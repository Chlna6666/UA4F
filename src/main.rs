pub mod http;
pub mod utils;
use std::net::SocketAddr;
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
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::Semaphore,
};
use bytes::BytesMut;

static mut USERAGENT: Option<String> = None;

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

    // 初始化日志
    utils::init_logger(args.log_level.clone(), args.no_file_log);
    info!("UA4F started on {} cores", num_cpus::get());
    info!("Author: {}", env!("CARGO_PKG_AUTHORS"));
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("Listening on {}:{}", args.bind, args.port);
    let elapsed_time = start_time.elapsed();
    info!("Server started in {}ms", elapsed_time.as_millis());

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

    let max_connections = Arc::new(Semaphore::new(500)); // 控制最大并发连接数
    loop {
        let permit = max_connections.clone().acquire_owned().await.unwrap();
        match server.accept().await {
            Ok((conn, _)) => {
                // 使用 tokio::spawn 直接异步处理连接
                tokio::spawn(async move {
                    let _permit = permit; // 确保 permit 在异步任务生命周期内不会被提前释放
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
        Ok(Command::Associate(associate, client_addr)) => {
            info!("Received UDP Associate command from {:?}", client_addr);

            // 确保 client_addr 是 SocketAddr 类型，如果不是，提前返回
            let client_addr = if let Address::SocketAddress(addr) = client_addr {
                addr
            } else {
                warn!("Received unsupported address type for UDP associate");
                let reply = associate.reply(Reply::AddressTypeNotSupported, Address::unspecified()).await;
                if let Ok(mut conn) = reply {
                    conn.close().await?;
                }
                return Ok(()); // 直接结束处理
            };

            // 创建 UDP 套接字
            let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
            let local_addr = udp_socket.local_addr()?;
            info!("UDP socket bound on {}", local_addr);

            // 向客户端回复代理的 UDP 地址
            let reply = associate.reply(Reply::Succeeded, Address::SocketAddress(local_addr)).await;
            if let Ok(mut conn) = reply {
                conn.close().await?;
            }

            // 启动 UDP 数据传输处理任务
            tokio::spawn(handle_udp_traffic(udp_socket, client_addr));
        }

        Ok(Command::Bind(bind, _)) => {
            warn!("Received bind command, rejecting");
            let replied = bind.reply(Reply::CommandNotSupported, Address::unspecified()).await;
            if let Ok(mut conn) = replied {
                conn.close().await?;
            }
        }

        Ok(Command::Connect(connect, addr)) => {
            handle_tcp_connect(connect, addr).await?;
        }

        Err((err, mut conn)) => {
            // 集中处理错误，关闭连接
            conn.shutdown().await?;
            return Err(err);
        }
    }

    Ok(())
}


async fn handle_udp_traffic(udp_socket: UdpSocket, client_addr: SocketAddr) {
    // 预分配最大 UDP 数据包长度的缓冲区
    let mut buf = [0u8; 65507];
    let mut send_buf = BytesMut::with_capacity(65507);

    loop {
        match udp_socket.recv_from(&mut buf).await {
            Ok((len, src_addr)) => {
                debug!("Received {} bytes from {:?}", len, src_addr);

                // 解析 UDP 数据包
                if let Some((target_addr, data)) = parse_socks5_udp_packet(&buf[..len]) {
                    // 清空并重用发送缓冲区
                    send_buf.clear();
                    send_buf.extend_from_slice(data);

                    // 发送解封装后的数据到目标地址
                    if let Err(e) = udp_socket.send_to(send_buf.as_ref(), target_addr).await {
                        warn!("Failed to send UDP packet to {}: {:?}", target_addr, e);
                        continue; // 跳过这次循环
                    }

                    // 从目标地址接收响应数据并封装发送回客户端
                    let response = encapsulate_socks5_udp_packet(&src_addr, data);
                    if let Err(e) = udp_socket.send_to(&response, client_addr).await {
                        error!("Failed to send UDP response to client: {:?}", e);
                    }
                }
            }
            Err(e) => {
                error!("Error receiving UDP packet: {:?}", e);
            }
        }
    }
}

// 解析 SOCKS5 UDP 数据包
fn parse_socks5_udp_packet(packet: &[u8]) -> Option<(SocketAddr, &[u8])> {
    // 检查长度和标识符
    if packet.len() < 10 || packet[1] != 0x00 {
        return None; // 非法的 UDP 数据包
    }

    // 地址类型
    let address_type = packet[3];
    let (addr, offset) = match address_type {
        0x01 => { // IPv4 地址
            // 解析 IPv4 地址和端口
            if packet.len() < 10 {
                return None;
            }
            let ip = &packet[4..8];
            let port = u16::from_be_bytes([packet[8], packet[9]]);
            let socket_addr = SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3])),
                port,
            );
            (socket_addr, &packet[10..])
        }
        0x03 => { // 域名地址
            let domain_len = packet[4] as usize;
            // 检查数据长度
            if packet.len() < 5 + domain_len + 2 {
                return None;
            }
            // 提取端口信息
            let port = u16::from_be_bytes([packet[5 + domain_len], packet[6 + domain_len]]);
            let socket_addr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), port);
            (socket_addr, &packet[5 + domain_len + 2..])
        }
        _ => return None, // 不支持的地址类型
    };

    Some((addr, offset))
}

// 封装 SOCKS5 UDP 数据包
fn encapsulate_socks5_udp_packet(addr: &SocketAddr, data: &[u8]) -> Vec<u8> {
    // 预先分配准确的缓冲区大小，3字节保留字段 + 地址信息 + 数据长度
    let addr_len = match addr {
        SocketAddr::V4(_) => 7, // 1 byte 地址类型 + 4 byte IPv4 + 2 byte 端口
        SocketAddr::V6(_) => 19, // 1 byte 地址类型 + 16 byte IPv6 + 2 byte 端口
    };
    let mut packet = Vec::with_capacity(3 + addr_len + data.len());

    // 插入 SOCKS5 UDP 头部
    packet.extend_from_slice(&[0x00, 0x00, 0x00]);

    // 插入目标地址
    match addr {
        SocketAddr::V4(addr) => {
            packet[2] = 0x01; // 地址类型: IPv4
            packet.extend_from_slice(&addr.ip().octets());
            packet.extend_from_slice(&addr.port().to_be_bytes());
        }
        SocketAddr::V6(addr) => {
            packet[2] = 0x04; // 地址类型: IPv6
            packet.extend_from_slice(&addr.ip().octets());
            packet.extend_from_slice(&addr.port().to_be_bytes());
        }
    }

    // 插入数据
    packet.extend_from_slice(data);
    packet
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

            match io::copy_bidirectional(&mut conn, &mut target).await {
                Ok((from_conn, from_target)) => {
                    debug!("已从连接传输 {} 字节，并从目标传输 {} 字节", from_conn, from_target);
                }
                Err(err) => {
                    error!("双向传输失败：{}", err);
                }
            }


        }
        Err(err) => {
            warn!("Connection failed: {}", err);
            let replied = connect.reply(Reply::HostUnreachable, Address::unspecified()).await;
            if let Ok(mut conn) = replied {
                conn.shutdown().await?;
            }
            return Err(Error::Io(err));
        }
    }

    Ok(())
}