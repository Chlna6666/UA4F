pub mod http;


use tokio::{net::{TcpListener, TcpStream, UdpSocket}, io::{AsyncReadExt, AsyncWriteExt},io};
use std::sync::Arc;
use std::time::{Duration, Instant};
use clap::{Parser, command};
use tracing::{info, warn, error, debug};
use socks5_server::{auth::NoAuth, connection::state::NeedAuthenticate, proto::{Address, Error, Reply}, Command, IncomingConnection, connection::connect::{Connect, state::NeedReply}, Associate};
use once_cell::sync::OnceCell;
use tokio::sync::Semaphore;
use ua4f::utils;


static USERAGENT: OnceCell<Arc<String>> = OnceCell::new();


#[derive(Parser, Debug)]
#[command(version, long_about = "")]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,

    #[arg(short, long, default_value = "1080")]
    port: String,

    #[arg(short('f'), long("user-agent"), default_value = "FFFF")]
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

    USERAGENT.set(Arc::new(args.user_agent)).ok();

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
    info!("Listening on {}:{}", args.bind, args.port);
    let elapsed_time = start_time.elapsed();
    info!("Server started in {}ms", elapsed_time.as_millis());


    let auth = Arc::new(NoAuth);
    let server = socks5_server::Server::new(listener, auth);

    let max_concurrent_connections = 1000;
    let semaphore = Arc::new(Semaphore::new(max_concurrent_connections));

    loop {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let connection = server.accept().await;
        tokio::spawn(async move {
            let _permit = permit; // 保持 permit 的生命周期
            if let Ok((conn, _)) = connection {
                handler(conn).await.unwrap_or_else(|err| error!("处理连接时出错: {:?}", err));
            }
        });
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
        Ok(Command::Bind(bind, _)) => {
            warn!("Received bind command, rejecting");
            let replied = bind.reply(Reply::CommandNotSupported, Address::unspecified()).await;
            if let Ok(mut conn) = replied {
                conn.close().await?; // 关闭连接
            }
        }

        Ok(Command::Connect(connect, addr)) => {
            handle_tcp_connect(connect, addr).await?; // 处理 TCP 连接
        }

        Ok(Command::Associate(udp_associate, addr)) => {
            handle_udp_associate(udp_associate, addr).await?; // 处理 UDP 连接
        }

        Err((err, mut conn)) => {
            // 集中处理错误，关闭连接
            conn.shutdown().await?;
            return Err(err);
        }
    }
    Ok(())
}

async fn handle_udp_associate(
    udp_associate: Associate<socks5_server::connection::associate::state::NeedReply>,
    addr: Address,
) -> Result<(), Error> {
    let timeout = Duration::from_secs(30);

    // 格式化目标地址信息
    let address_info = match &addr {
        Address::DomainAddress(domain, port) => format!("{}:{}", String::from_utf8_lossy(domain), port),
        Address::SocketAddress(socket_addr) => socket_addr.to_string(),
    };

    // 创建 UDP 套接字
    let target = match addr {
        Address::DomainAddress(domain, _) => {
            let domain = String::from_utf8_lossy(&domain);
            tokio::time::timeout(timeout, UdpSocket::bind(domain.as_ref())).await
        }
        Address::SocketAddress(addr) => tokio::time::timeout(timeout, UdpSocket::bind(addr)).await,
    };

    let  target = match target {
        Ok(Ok(socket)) => socket,
        Ok(Err(err)) => {
            warn!("无法连接到目标 {}: {}", address_info, err);
            udp_associate
                .reply(Reply::HostUnreachable, Address::unspecified())
                .await
                .ok(); // 忽略错误
            return Err(Error::Io(err));
        }
        Err(_) => {
            warn!("与目标的连接 {} 超时", address_info);
            udp_associate
                .reply(Reply::TtlExpired, Address::unspecified())
                .await
                .ok(); // 忽略错误
            return Err(Error::Io(io::Error::new(io::ErrorKind::TimedOut, "连接超时")));
        }
    };

    // 成功绑定后发送成功响应
    if let Err((err, _)) = udp_associate.reply(Reply::Succeeded, Address::unspecified()).await {
        error!("发送成功响应失败: {}", err);
        return Err(Error::Io(err));
    }

    let mut buf = vec![0; 4096];

    // 开始转发 UDP 数据
    loop {
        match target.recv_from(&mut buf).await {
            Ok((n, src_addr)) => {
                debug!("收到来自 {} 的 {} 字节数据", src_addr, n);

                if let Err(err) = target.send_to(&buf[..n], src_addr).await {
                    warn!("转发 UDP 数据失败: {}", err);
                    return Err(Error::Io(err));
                }
            }
            Err(err) => {
                warn!("接收 UDP 数据失败: {}", err);
                return Err(Error::Io(err));
            }
        }
    }
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
        debug!("检测到HTTP请求");
        if let Some(user_agent) = USERAGENT.get().cloned() {
            http::modify_user_agent(&mut buf, user_agent.as_ref());
        }
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
