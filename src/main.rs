pub mod http;
pub mod utils;
use std::sync::Arc;
use std::time::Instant;

use clap::{command, Parser};
use tracing::{info, warn, error, debug};

use socks5_server::{
    auth::NoAuth,
    connection::state::NeedAuthenticate,
    proto::{Address, Error, Reply},
    Command, IncomingConnection,
    connection::connect::{Connect, state::NeedReply}
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio::sync::Mutex;

use once_cell::sync::Lazy;
use socks5_server::connection::connect::state::Ready;

static USERAGENT: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

async fn set_user_agent(agent: String) {
    let mut ua = USERAGENT.lock().await;
    *ua = Some(agent);
}

// 获取全局 User-Agent
async fn get_user_agent() -> Option<String> {
    USERAGENT.lock().await.clone()
}



struct BufferPool {
    buffer: Mutex<Vec<u8>>,
}

impl BufferPool {
    fn new(size: usize) -> Self {
        Self {
            buffer: Mutex::new(vec![0; size]),
        }
    }

    async fn get_buffer(&self) -> tokio::sync::MutexGuard<'_, Vec<u8>> {
        self.buffer.lock().await
    }
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
    utils::init_logger(args.log_level.clone(), args.no_file_log);
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
            Err(err) => error!("Failed to accept connection: {}", err),
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

async fn copy_bidirectional_optimized(
    mut conn: Connect<Ready>,
    mut target: TcpStream,
    buffer_size: usize,
) -> Result<(), Error> {
    let mut conn_buf = vec![0u8; buffer_size];
    let mut target_buf = vec![0u8; buffer_size];

    loop {
        tokio::select! {
            result = conn.read(&mut conn_buf) => match result {
                Ok(0) => break, // Connection closed
                Ok(n) => {
                    target.write_all(&conn_buf[..n]).await?;
                }
                Err(e) => {
                    error!("从连接读取失败: {}", e);
                    return Err(Error::Io(e));
                }
            },
            result = target.read(&mut target_buf) => match result {
                Ok(0) => break, // Target closed
                Ok(n) => {
                    conn.write_all(&target_buf[..n]).await?;
                }
                Err(e) => {
                    error!("从目标读取失败: {}", e);
                    return Err(Error::Io(e));
                }
            },
        }
    }
    Ok(())
}


async fn handle_tcp_connect(connect: Connect<NeedReply>, addr: Address) -> Result<(), Error> {
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
                    error!("Reply failed: {}", err);
                    conn.shutdown().await?;
                    return Err(Error::Io(err));
                }
            };

            let buffer_pool = Arc::new(BufferPool::new(16 * 1024)); // 增大缓冲区大小
            let mut buf = buffer_pool.get_buffer().await;
            let initial_read = conn.read(&mut buf).await?;
            if initial_read == 0 {
                conn.shutdown().await?;
                target.shutdown().await?;
                return Ok(());
            }

            let is_http = http::is_http_request(&buf[..initial_read]);
            debug!("is_http: {}", is_http);
            if is_http {
                if let Some(user_agent) = get_user_agent().await {
                    http::modify_user_agent(&mut buf, &*user_agent);
                }
            }

            target.write_all(&buf[..initial_read]).await?;
            target.flush().await?;
            debug!("写入目标连接耗时");

            let spawn_start = Instant::now();
            // 设置优化参数
            target.set_nodelay(true)?;


            // 自定义双向转发逻辑
            copy_bidirectional_optimized(conn, target, 16 * 1024).await?;

            let spawn_duration = spawn_start.elapsed();
            debug!("双向传输耗时: {}ms", spawn_duration.as_millis());
        }

        Err(err) => {
            warn!("Connection failed: {}", err);
            if let Ok(mut conn) = connect.reply(Reply::HostUnreachable, Address::unspecified()).await {
                conn.shutdown().await?;
            }
            return Err(Error::Io(err));
        }
    }

    Ok(())
}
