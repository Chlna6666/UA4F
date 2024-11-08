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
};

#[derive(Parser, Debug)]
#[command(version, long_about = "")]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,

    #[arg(short, long, default_value = "1080")]
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
    let args = Args::parse();
    start_server(args);
}

static mut USERAGENT: Option<String> = None;

#[tokio::main]
async fn start_server(args: Args) {
    // 初始化日志
    utils::init_logger(args.log_level, args.no_file_log);
    info!("UA4F started");
    info!("Author: {}", env!("CARGO_PKG_AUTHORS"));
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("Listening on {}:{}", args.bind, args.port);

    // 设置全局的用户代理（USERAGENT变量）
    unsafe {
        USERAGENT = Some(args.user_agent);
    }

    // 绑定监听地址和端口
    let listener = match TcpListener::bind(format!("{}:{}", args.bind, args.port)).await {
        Ok(listener) => listener,
        Err(err) => {
            error!("Failed to bind to {}:{}. Error: {}", args.bind, args.port, err);
            return;
        }
    };

    let auth = Arc::new(NoAuth);
    let server = socks5_server::Server::new(listener, auth);

    // 接受连接并使用 tokio::spawn 启动新的任务处理每个连接
    loop {
        match server.accept().await {
            Ok((conn, _)) => {
                tokio::spawn(async move {
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
                            error!("reply failed: {}", err);
                            let _ = conn.shutdown().await;
                            return Err(Error::Io(err));
                        }
                    };

                    let mut buf: Vec<u8> = vec![0; 8];
                    let n = match conn.read(&mut buf).await {
                        Ok(n) => n,
                        Err(err) => {
                            let _ = conn.shutdown().await;
                            let _ = target.shutdown().await;

                            error!("read failed: {}", err);
                            return Err(Error::Io(err));
                        }
                    };
                    debug!("read {} bytes", n);
                    if n == 0 {
                        let _ = conn.shutdown().await;
                        let _ = target.shutdown().await;
                        return Ok(());
                    }
                    let is_http = http::is_http_request(&mut buf[..n]);
                    debug!("is_http: {}", is_http);
                    if is_http {
                        let user_agent = unsafe { USERAGENT.as_ref().unwrap() };

                        let mut buf: Vec<u8> = vec![0; 4088];
                        let n = match conn.read(&mut buf).await {
                            Ok(n) => n,
                            Err(err) => {
                                let _ = conn.shutdown().await;
                                let _ = target.shutdown().await;

                                error!("read failed: {}", err);
                                return Err(Error::Io(err));
                            }
                        };
                        if n == 0 {
                            let _ = conn.shutdown().await;
                            let _ = target.shutdown().await;
                            return Ok(());
                        }

                        http::modify_user_agent(&mut buf, user_agent);
                    }

                    debug!("buf len: {}", buf.len());

                    target.write(&buf[..buf.len()]).await?;
                    target.flush().await?;

                    let res = io::copy_bidirectional(&mut target, &mut conn).await;
                    let _ = conn.shutdown().await;
                    let _ = target.shutdown().await;

                    res?;
                }
                Err(err) => {
                    warn!("connect failed: {}", err);

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