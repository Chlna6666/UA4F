pub mod http;

use tokio::{net::{TcpListener, TcpStream}, io::{AsyncReadExt, AsyncWriteExt}, io, select};
use std::sync::Arc;
use std::time::{Duration, Instant};
use clap::{Parser, command};
use tracing::{info, warn, error, debug};
use socks5_server::{
    auth::NoAuth, connection::state::NeedAuthenticate,
    proto::{Address, Error, Reply},
    Command,
    IncomingConnection,
    connection::connect::{Connect, state::NeedReply}};
use once_cell::sync::OnceCell;
use ua4f::utils;

use moka::future::Cache;
use once_cell::sync::Lazy;
use tokio::io::{AsyncRead, AsyncWrite};
use bytes::BytesMut;

static USERAGENT: OnceCell<Arc<str>> = OnceCell::new();

// 新增全局缓存，用于记录目标地址非 HTTP 的情况
static NON_HTTP_CACHE: Lazy<Cache<String, ()>> = Lazy::new(|| {
    Cache::builder()
        .max_capacity(300)
        // 这里可根据需求调整非 HTTP 缓存的有效期
        .time_to_live(Duration::from_secs(600))
        .build()
});
#[derive(Parser, Debug)]
#[command(version, long_about = "")]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,

    #[arg(short, long, default_value = "1080")]
    port: String,

    #[arg(short('f'), long("user-agent"), default_value = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.5.1.4 Safari/537.36 Edg/114.5.1.4")]
    user_agent: String,

    #[arg(short('l'), long("log-level"), default_value = "info")]
    log_level: String,

    #[arg(long("no-file-log"))]
    no_file_log: bool,
}

fn main() {
    let cpu_cores = num_cpus::get();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(cpu_cores)
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime");

    let args = Args::parse();
    runtime.block_on(start_server(args));
}

async fn start_server(args: Args) {
    // 记录启动时间
    let start_time = Instant::now();

    USERAGENT.set(Arc::from(args.user_agent)).ok();

    // 绑定监听地址和端口
    let listener = TcpListener::bind(format!("{}:{}", args.bind, args.port))
        .await
        .unwrap_or_else(|err| {
            eprintln!("Failed to bind to {}:{}. Error: {}", args.bind, args.port, err);
            panic!("Server failed to start");
        });


    // 初始化日志
    utils::logger::init_logger(args.log_level.clone(), args.no_file_log);
    info!("UA4F started on {} cores", num_cpus::get());
    info!("Author: {}", env!("CARGO_PKG_AUTHORS"));
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("User-Agent: {}", USERAGENT.get().map(|s| &**s).unwrap_or("Unknown"));
    info!("Listening on {}:{}", args.bind, args.port);


    let auth = Arc::new(NoAuth);
    let server = socks5_server::Server::new(listener, auth);
    let elapsed_time = start_time.elapsed();
    info!("Server started in {}ms", elapsed_time.as_millis());

    loop {
        if let Ok((conn, _)) = server.accept().await {
            tokio::spawn( handler(conn));
        }
    }

}

async fn handler(conn: IncomingConnection<(), NeedAuthenticate>) -> Result<(), Error> {
    // 认证部分：认证失败时直接关闭连接并返回错误
    let conn = match conn.authenticate().await {
        Ok((conn, _)) => conn,
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await; // 忽略关闭错误
            return Err(err);
        }
    };

    // 打印出客户端地址（连接的来源地址）
    match conn.peer_addr() {
        Ok(addr) => debug!("来自客户端的连接，地址: {}", addr),
        Err(e) => warn!("无法获取客户端连接地址: {}", e),
    }

    // 封装错误处理，若等待命令出错，则关闭连接并返回错误
    let command = match conn.wait().await {
        Ok(cmd) => cmd,
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await; // 尝试关闭连接
            return Err(err);
        }
    };

    match command {
        Command::Bind(bind, _) => {
            warn!("收到绑定命令，拒绝处理");
            if let Ok(mut reply_conn) = bind.reply(Reply::CommandNotSupported, Address::unspecified()).await {
                let _ = reply_conn.close().await;
            }
        }
        Command::Connect(connect, addr) => {
            debug!("收到连接命令，尝试连接到目标地址: {}", addr);
            handle_tcp_connect(connect, addr).await?;
        }
        _ => {
            warn!("收到不支持的命令");
        }
    }

    Ok(())
}



pub async fn copy_bidirectional<A, B>(
    a: &mut A,
    b: &mut B,
) -> io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    const BUF_SIZE: usize = 5 * 1024;

    let mut buf_a = BytesMut::with_capacity(BUF_SIZE);
    buf_a.resize(BUF_SIZE, 0);

    let mut buf_b = BytesMut::with_capacity(BUF_SIZE);
    buf_b.resize(BUF_SIZE, 0);

    let mut a_to_b_bytes: u64 = 0;
    let mut b_to_a_bytes: u64 = 0;

    let mut a_closed = false;
    let mut b_closed = false;

    loop {
        select! {
            result = a.read(&mut *buf_a), if !a_closed => {
                match result {
                    Ok(n) if n > 0 => {
                        if let Err(e) = b.write_all(&buf_a[..n]).await {
                            if e.kind() == io::ErrorKind::BrokenPipe || e.kind() == io::ErrorKind::ConnectionReset {
                                b_closed = true;
                            } else {
                                return Err(e);
                            }
                        }
                        a_to_b_bytes += n as u64;
                    }
                    Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {
                        // 远端重置连接，直接关闭 a
                        a_closed = true;
                        let _ = b.shutdown().await;
                    }
                    _ => {
                        a_closed = true;
                        let _ = b.shutdown().await;
                    }
                }
            }

            result = b.read(&mut *buf_b), if !b_closed => {
                match result {
                    Ok(n) if n > 0 => {
                        if let Err(e) = a.write_all(&buf_b[..n]).await {
                            if e.kind() == io::ErrorKind::BrokenPipe || e.kind() == io::ErrorKind::ConnectionReset {
                                a_closed = true;
                            } else {
                                return Err(e);
                            }
                        }
                        b_to_a_bytes += n as u64;
                    }
                    Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {
                        // 远端重置连接，直接关闭 b
                        b_closed = true;
                        let _ = a.shutdown().await;
                    }
                    _ => {
                        b_closed = true;
                        let _ = a.shutdown().await;
                    }
                }
            }

            else => break, // 如果 a 和 b 都关闭了，则退出
        }
    }

    let _ = a.flush().await;
    let _ = b.flush().await;

    drop(buf_a);
    drop(buf_b);

    Ok((a_to_b_bytes, b_to_a_bytes))
}

async fn handle_tcp_connect(connect: Connect<NeedReply>, addr: Address) -> Result<(), Error> {
    let timeout = Duration::from_secs(30);
    let address_info = match &addr {
        Address::DomainAddress(domain, port) => {
            let domain = String::from_utf8_lossy(domain);
            format!("{domain}:{port}")
        }
        Address::SocketAddress(socket_addr) => socket_addr.to_string(),
    };


    let target = match addr {
        Address::DomainAddress(domain, port) => {
            let domain = String::from_utf8_lossy(&domain);
            tokio::time::timeout(timeout, TcpStream::connect((domain.as_ref(), port))).await
        }
        Address::SocketAddress(addr) => tokio::time::timeout(timeout, TcpStream::connect(addr)).await,

    };
    let mut target = match target {
        // 成功获取流直接返回
        Ok(Ok(stream)) => stream,

        // 处理目标不可达错误
        Ok(Err(err)) => {
            warn!(target = ?address_info, error = ?err, "无法连接到目标");
            let _ = connect.reply(Reply::HostUnreachable, Address::unspecified()).await;
            return Err(Error::Io(err));
        }

        // 处理连接超时错误
        Err(_) => {
            warn!("与目标的连接 {} 超时", address_info);
            let _ = connect.reply(Reply::TtlExpired,Address::unspecified()).await;
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "连接超时"
            )));
        }
    };

    if let Err(err) = target.set_nodelay(true) {
        warn!("设置 TCP_NODELAY 失败: {}", err);
    }

    let replied = connect.reply(Reply::Succeeded, Address::unspecified()).await;
    let mut conn = match replied {
        Ok(conn) => conn,
        Err((err, mut conn)) => {
            error!("回复失败 : {}", err);
            conn.shutdown().await?;
            target.shutdown().await?;
            return Err(Error::Io(err));
        }
    };

    // 根据目标地址判断是否已缓存为非 HTTP 连接，如果是则直接转发
    if NON_HTTP_CACHE.get(&address_info).await.is_some() {
        debug!("目标 {} 缓存为非 HTTP，直接转发流量", address_info);
        if let Err(e) = copy_bidirectional(&mut conn, &mut target).await {
            error!("双向复制失败: {:?}, 目标地址: {}", e, address_info);
        }
        conn.shutdown().await?;
        target.shutdown().await?;
        return Ok(());
    }

    // 先读取前 7 个字节到 small_buf
    let mut small_buf = [0u8; 7];
    let n = conn.read(&mut small_buf).await?;
    if n == 0 {
        // 连接已关闭，直接关闭所有连接并返回
        conn.shutdown().await?;
        target.shutdown().await?;
        return Ok(());
    }

    // 根据已读取的数据判断是否为 HTTP 请求
    if http::is_http_request(&small_buf[..n]) {
        debug!("检测到 HTTP 请求，进行 User-Agent 修改");

        let mut buf = BytesMut::with_capacity(4096);
        buf.resize(4096, 0);

        // 将已读取的 small_buf 数据拷贝到 buf 中
        buf[..n].copy_from_slice(&small_buf[..n]);

        // 继续读取剩余数据到 buf[n..]
        let _ = conn.read(&mut buf[n..]).await?;

        // 若配置了 User-Agent，则对 HTTP 请求中的 User-Agent 进行修改
        if let Some(user_agent) = USERAGENT.get().cloned() {
            http::modify_user_agent(&mut buf, &*user_agent);
        }

        // 将整个初始数据（已修改的部分）写入目标连接
        if let Err(err) = target.write_all(&mut buf).await {
            conn.shutdown().await?;
            target.shutdown().await?;
            conn.flush().await?;
            target.flush().await?;
            warn!("未能将初始数据写入目标 {}: {}", address_info, err);
        }
    } else {
        // 非 HTTP 请求：先写入已经读取的 small_buf，再直接转发后续数据
        NON_HTTP_CACHE.insert(address_info.clone(), ()).await;
        debug!("非 HTTP 请求 添加到缓存{}", address_info);
    }
    if let Err(e) = copy_bidirectional(&mut conn, &mut target).await {
        error!("双向复制失败: {:?}, 目标地址: {}", e, address_info);
    }
    conn.shutdown().await?;
    target.shutdown().await?;
    conn.flush().await?;
    target.flush().await?;
    Ok(())
}