pub mod http;
pub mod utils;

use std::sync::Arc;
use clap::{command, Parser};
use log::{debug, error, info, warn};
use socks5_server::{
    auth::NoAuth,
    connection::state::NeedAuthenticate,
    proto::{Address, Error, Reply},
    Command, IncomingConnection,
};
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    runtime::Builder,
};

static mut USERAGENT: Option<String> = None;

#[derive(Parser, Debug)]
#[command(version, long_about = "")]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,

    #[arg(short, long, default_value = "1090")]
    port: String,

    #[arg(short('f'), long("user-agent"), default_value = "FFFF")]
    user_agent: String,

    #[arg(short('l'), long("log-level"), default_value = "info")]
    log_level: String,

    #[arg(long("no-file-log"))]
    no_file_log: bool,
}

fn main() {
    // 获取 CPU 核心数并创建 Tokio 多线程运行时
    let cpu_cores = num_cpus::get();
    println!("Detected CPU cores: {}", cpu_cores);

    let runtime = Builder::new_multi_thread()
        .worker_threads(cpu_cores)
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime");

    let args = Args::parse();
    runtime.block_on(start_server(args));
}

async fn start_server(args: Args) {
    // 初始化日志
    utils::init_logger(args.log_level, args.no_file_log);
    info!("UA4F started on {} cores", num_cpus::get());
    info!("Author: {}", env!("CARGO_PKG_AUTHORS"));
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("Listening on {}:{}", args.bind, args.port);

    // 绑定监听地址和端口
    let listener = match TcpListener::bind(format!("{}:{}", args.bind, args.port)).await {
        Ok(listener) => listener,
        Err(err) => {
            error!("Failed to bind to {}:{}. Error: {}", args.bind, args.port, err);
            return;
        }
    };
    unsafe {
        USERAGENT = Some(args.user_agent);
    }

    let auth = Arc::new(NoAuth);
    let server = socks5_server::Server::new(listener, auth);

    // 接受连接并使用 tokio::spawn 启动新任务处理每个连接
    loop {
        match server.accept().await {
            Ok((conn, _)) => {
                tokio::spawn(async move {
                    if let Err(err) = handler(conn).await {
                        error!("Connection handling error: {}", err);
                    }
                });
            }
            Err(err) => error!("Failed to accept connection: {}", err),
        }
    }
}

async fn handler(conn: IncomingConnection<(), NeedAuthenticate>) -> Result<(), Error> {
    let conn = match conn.authenticate().await {
        Ok((conn, _)) => conn,
        Err((err, mut conn)) => {
            conn.shutdown().await?;
            return Err(err);
        }
    };

    match conn.wait().await {
        // 独立处理 Associate 命令
        Ok(Command::Associate(associate, _)) => {
            warn!("received associate command, rejecting");
            let replied = associate
                .reply(Reply::CommandNotSupported, Address::unspecified())
                .await;

            if let Ok(mut conn) = replied {
                conn.close().await?;
            }
        }
        // 独立处理 Bind 命令
        Ok(Command::Bind(bind, _)) => {
            warn!("received bind command, rejecting");
            let replied = bind
                .reply(Reply::CommandNotSupported, Address::unspecified())
                .await;

            if let Ok(mut conn) = replied {
                conn.close().await?;
            }
        }
        // 保持 Connect 命令的原有处理逻辑
        Ok(Command::Connect(connect, addr)) => {
            // 这里是 Connect 命令的处理逻辑
            let target = match addr {
                Address::DomainAddress(domain, port) => {
                    let domain = String::from_utf8_lossy(&domain);
                    TcpStream::connect((domain.as_ref(), port)).await
                }
                Address::SocketAddress(addr) => TcpStream::connect(addr).await,
            };

            match target {
                Ok(mut target) => {
                    let replied = connect.reply(Reply::Succeeded, Address::unspecified()).await;

                    let mut conn = match replied {
                        Ok(conn) => conn,
                        Err((err, mut conn)) => {
                            error!("reply failed: {}", err);
                            conn.shutdown().await?;
                            return Err(Error::Io(err));
                        }
                    };

                    // 合并的缓冲区，避免多次分配
                    let mut buf: Vec<u8> = vec![0; 4096];
                    let initial_read = conn.read(&mut buf[..8]).await?;

                    if initial_read == 0 {
                        conn.shutdown().await?;
                        target.shutdown().await?;
                        return Ok(());
                    }

                    let is_http = http::is_http_request(&buf[..initial_read]);
                    debug!("is_http: {}", is_http);

                    if is_http {
                        let user_agent = unsafe { USERAGENT.as_ref().unwrap() };
                        let additional_read = conn.read(&mut buf[initial_read..]).await?;
                        let total_read = initial_read + additional_read;

                        http::modify_user_agent(&mut buf, user_agent);
                        target.write_all(&buf[..total_read]).await?;
                    } else {
                        target.write_all(&buf[..initial_read]).await?;
                    }
                    target.flush().await?;

                    // 使用 tokio::io::split 进行双向数据传输
                    let (mut conn_r, mut conn_w) = io::split(conn);
                    let (mut target_r, mut target_w) = io::split(target);
                    tokio::try_join!(
                    io::copy(&mut conn_r, &mut target_w),
                    io::copy(&mut target_r, &mut conn_w)
                )?;
                }
                Err(err) => {
                    warn!("connection failed: {}", err);
                    let replied = connect.reply(Reply::HostUnreachable, Address::unspecified()).await;

                    if let Ok(mut conn) = replied {
                        conn.shutdown().await?;
                    }
                    return Err(Error::Io(err));
                }
            }
        }
        Err((err, mut conn)) => {
            conn.shutdown().await?;
            return Err(err);
        }
    };


    Ok(())
}
