pub mod http;

use tokio::{net::{TcpListener, TcpStream}, io::{AsyncReadExt, AsyncWriteExt},io};
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

static USERAGENT: OnceCell<Arc<str>> = OnceCell::new();

// 新增全局缓存，用于记录目标地址非 HTTP 的情况
static NON_HTTP_CACHE: Lazy<Cache<String, ()>> = Lazy::new(|| {
    Cache::builder()
        .max_capacity(600)
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
    // 尝试认证
    let conn = match conn.authenticate().await {
        Ok((conn, _)) => conn,
        Err((err, mut conn)) => {
            conn.shutdown().await?; // 立即关闭连接
            return Err(err);
        }
    };

    // 处理连接中的命令
    match conn.wait().await {
        Ok(command) => match command {
            Command::Bind(bind, _) => {
                warn!("Received bind command, rejecting");
                if let Ok(mut conn) = bind.reply(Reply::CommandNotSupported, Address::unspecified()).await {
                    conn.close().await?; // 关闭连接
                }
            }
            Command::Connect(connect, addr) => {
                handle_tcp_connect(connect, addr).await?; // 处理 TCP 连接
            }
            _ => {
                warn!("Unsupported command received");
            }
        },

        Err((err, mut conn)) => {
            // 集中处理错误，关闭连接
            conn.shutdown().await?;
            return Err(err);
        }
    }

    Ok(())
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
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            warn!(target = ?address_info, error = ?err, "无法连接到目标");
            if let Ok(mut conn) = connect.reply(Reply::HostUnreachable, Address::unspecified()).await {
                conn.shutdown().await?;
            }
            return Err(Error::Io(err));
        }
        Err(_) => {
            warn!("与目标的连接 {} 超时", address_info);
            if let Ok(mut conn) = connect.reply(Reply::TtlExpired, Address::unspecified()).await {
                conn.shutdown().await?;
            }
            return Err(Error::Io(io::Error::new(io::ErrorKind::TimedOut, "连接超时")));
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
            return Err(Error::Io(err));
        }
    };

    let mut buf = vec![0; 4096];

    let initial_read = conn.read(&mut buf).await?;
    if initial_read == 0 {
        conn.shutdown().await?;
        target.shutdown().await?;
        return Ok(());
    }

    if http::is_http_request(&buf[..initial_read]) {
        debug!("检测到 HTTP 请求，进行 User-Agent 修改");
        if let Some(user_agent) = USERAGENT.get().cloned() {
            http::modify_user_agent(&mut buf, user_agent.as_ref());
        }
    } else {
        NON_HTTP_CACHE.insert(address_info.clone(), ()).await;
    }

    if let Err(err) = target.write_all(&buf[..initial_read]).await {
        warn!("未能将初始数据写入目标 {}: {}", address_info, err);
    }

    if let Err(err) = target.flush().await {
        warn!("数据 flush 到目标失败 {}: {}", address_info, err);
    }



    let result = io::copy_bidirectional_with_sizes(&mut conn, &mut target, buf.len(), buf.len())
        .await
        .map_err(|err| {
            error!("双向传输失败：{}，目标地址：{}", err, address_info);
            err
        });

    match result {
        Ok((from_conn, from_target)) => {
            debug!(
                "传输完成：从客户端读取 {} 字节，从目标读取 {} 字节，目标地址：{}",
                from_conn, from_target, address_info
            );
        }
        Err(err) => return Err(Error::Io(err)),
    }

    conn.shutdown().await?;
    target.shutdown().await?;
    Ok(())
}