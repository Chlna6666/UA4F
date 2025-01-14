pub mod http;

use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{command, Parser};
use tracing::{info, warn, error, debug};

use socks5_server::{
    auth::NoAuth,
    connection::state::NeedAuthenticate,
    proto::{Address, Error, Reply},
    Command, IncomingConnection,
    connection::connect::{Connect, state::NeedReply}
};

use tokio::{io, io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}};
use once_cell::sync::OnceCell;

use ua4f::utils;

static USERAGENT: OnceCell<Arc<String>> = OnceCell::new();

async fn  set_user_agent(agent: String) {
    USERAGENT.set(Arc::new(agent)).ok();
}

async fn get_user_agent() -> Option<Arc<String>> {
    USERAGENT.get().cloned()
}

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

    set_user_agent(args.user_agent).await;

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

    loop {
        match server.accept().await {
            Ok((conn, _)) => {
                tokio::spawn(handler(conn));
            }
            Err(err) => error!("无法接受连接: {}", err),
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

        Err((err, mut conn)) => {
            // 集中处理错误，关闭连接
            conn.shutdown().await?;
            return Err(err);
        }
        _ => {}
    }
    Ok(())
}


async fn handle_tcp_connect(connect: Connect<NeedReply>, addr: Address) -> Result<(), Error> {
    let timeout = Duration::from_secs(30);
    let address_info = match &addr {
        Address::DomainAddress(domain, port) => format!("{}:{}", String::from_utf8_lossy(domain), port),
        Address::SocketAddress(socket_addr) => format!("{}", socket_addr),
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
            warn!("无法连接到目标 {}: {}", address_info, err);
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


    let mut buf = vec![0; 4088];

    let initial_read = conn.read(&mut buf).await?;
    if initial_read == 0 {
        conn.shutdown().await?;
        target.shutdown().await?;
        return Ok(());
    }

    if http::is_http_request(&buf[..initial_read]) {
        debug!("检测到HTTP请求");
        if let Some(user_agent) = get_user_agent().await {
            http::modify_user_agent(&mut buf, &*user_agent);
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
