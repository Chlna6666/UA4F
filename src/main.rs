pub mod http;
pub mod utils;

use std::sync::Arc;
use std::thread;
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
};
use tokio::runtime::Builder;


static mut USERAGENT: Option<String> = None;

#[derive(Parser, Debug)]
#[command(version, long_about = "")]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,

    #[arg(short, long, default_value = "1090")]
    port: String,

    #[arg(
        short('f'),
        long("user-agent"),
        default_value = "FFFF"
    )]
    user_agent: String,

    #[arg(short('l'), long("log-level"), default_value = "info")]
    log_level: String,

    #[arg(long("no-file-log"))]
    no_file_log: bool,
}


fn main() {
    // 动态获取 CPU 核心数
    let cpu_cores = num_cpus::get();
    println!("Detected CPU cores: {}", cpu_cores);

    // 手动创建 Tokio 多线程运行时
    let runtime = Builder::new_multi_thread()
        .worker_threads(cpu_cores)
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime");

    // 在运行时内启动服务器
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

    // 接受连接并使用 tokio::spawn 启动新的任务处理每个连接
    loop {
        match server.accept().await {
            Ok((conn, _)) => {
                tokio::spawn(async move {
                    // 添加线程 ID 的日志信息
                    debug!("Handling connection on thread {:?}", thread::current().id());

                    if let Err(err) = handler(conn).await {
                        error!("Connection handling error: {}", err);
                    }
                });
            }
            Err(err) => {
                error!("Failed to accept connection: {}", err);
                // 可以考虑是否在这里加入断开重试逻辑
            }
        }
    }
}


async fn handler(conn: IncomingConnection<(), NeedAuthenticate>) -> Result<(), Error> {
    let conn = match conn.authenticate().await {
        Ok((conn, _)) => conn,
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await;
            return Err(err);
        }
    };

    match conn.wait().await {
        // 单独处理 Associate 和 Bind 命令，避免类型不匹配的问题
        Ok(Command::Associate(associate, _)) => {
            warn!("received associate command, rejecting");
            let replied = associate
                .reply(Reply::CommandNotSupported, Address::unspecified())
                .await;

            let mut conn = match replied {
                Ok(conn) => conn,
                Err((err, mut conn)) => {
                    let _ = conn.shutdown().await;
                    return Err(Error::Io(err));
                }
            };

            let _ = conn.close().await;
        }
        Ok(Command::Bind(bind, _)) => {
            warn!("received bind command, rejecting");
            let replied = bind
                .reply(Reply::CommandNotSupported, Address::unspecified())
                .await;

            let mut conn = match replied {
                Ok(conn) => conn,
                Err((err, mut conn)) => {
                    let _ = conn.shutdown().await;
                    return Err(Error::Io(err));
                }
            };

            let _ = conn.close().await;
        }
        Ok(Command::Connect(connect, addr)) => {
            // 原有 Connect 命令处理逻辑保持不变
            let target = match addr {
                Address::DomainAddress(domain, port) => {
                    let domain = String::from_utf8_lossy(&domain);
                    TcpStream::connect((domain.as_ref(), port)).await
                }
                Address::SocketAddress(addr) => TcpStream::connect(addr).await,
            };

            match target {
                Ok(mut target) => {
                    let replied = connect
                        .reply(Reply::Succeeded, Address::unspecified())
                        .await;

                    let mut conn = match replied {
                        Ok(conn) => conn,
                        Err((err, mut conn)) => {
                            error!("回复失败: {}", err);
                            let _ = conn.shutdown().await;
                            return Err(Error::Io(err));
                        }
                    };

                    // 初始缓冲区设为 1024 字节
                    let mut buf: Vec<u8> = vec![0; 1024];
                    let mut n = 0; // 实际读取的字节数

                    loop {
                        let bytes_read = match conn.read(&mut buf[n..]).await {
                            Ok(0) => break, // 读取完成
                            Ok(bytes) => bytes,
                            Err(err) => {
                                let _ = conn.shutdown().await;
                                let _ = target.shutdown().await;
                                error!("读取失败: {}", err);
                                return Err(Error::Io(err));
                            }
                        };

                        n += bytes_read;

                        // 检查是否已经读取到完整的请求头（即 "\r\n\r\n"）
                        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }

                        // 若缓冲区已满且还没有读取到完整请求头，则扩展缓冲区
                        if n == buf.len() {
                            buf.resize(buf.len() + 1024, 0); // 每次扩展 1024 字节
                        }
                    }

                    debug!("读取了 {} 字节", n);

                    // 判断是否为 HTTP 请求
                    let is_http = http::is_http_request(&buf[..n]);
                    debug!("is_http: {}", is_http);

                    if is_http {
                        let user_agent = unsafe { USERAGENT.as_ref().unwrap() };

                        // 修改 User-Agent
                        http::modify_user_agent(&mut buf, user_agent);
                        // 将修改后的请求转发到目标服务器
                        target.write_all(&buf[..n]).await?;
                        target.flush().await?;
                    } else {
                        // 若非 HTTP 请求，直接转发
                        target.write_all(&buf[..n]).await?;
                        target.flush().await?;
                    }

                    // 双向数据转发
                    let res = io::copy_bidirectional(&mut target, &mut conn).await;
                    let _ = conn.shutdown().await;
                    let _ = target.shutdown().await;

                    res?;
                }
                Err(err) => {
                    warn!("连接失败: {}", err);

                    let replied = connect
                        .reply(Reply::HostUnreachable, Address::unspecified())
                        .await;

                    let mut conn = match replied {
                        Ok(conn) => conn,
                        Err((err, mut conn)) => {
                            let _ = conn.shutdown().await;
                            return Err(Error::Io(err));
                        }
                    };

                    let _ = conn.shutdown().await;
                }
            }

        }
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await;
            return Err(err);
        }
    };

    Ok(())
}