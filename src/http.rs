use bytes::BytesMut;
use tracing::{error, debug};
use memchr::{memmem};

pub fn is_http_request(buf: &[u8]) -> bool {
    buf.starts_with(b"GET ") ||
        buf.starts_with(b"POST ") ||
        buf.starts_with(b"HEAD ") ||
        buf.starts_with(b"PUT ") ||
        buf.starts_with(b"DELETE ") ||
        buf.starts_with(b"OPTIONS ") ||
        buf.starts_with(b"CONNECT ")
}


pub fn modify_user_agent(buf: &mut BytesMut, user_agent: &str) {
    const USER_AGENT_HEADER: &[u8] = b"User-Agent: ";

    let start = match memmem::find(buf, USER_AGENT_HEADER) {
        Some(pos) => pos + USER_AGENT_HEADER.len(),
        None => {
            error!("未找到 User-Agent 头");
            return;
        }
    };

    let end = match memchr::memchr(b'\r', &buf[start..]) {
        Some(pos) => start + pos,
        None => {
            error!("未找到 User-Agent 结束符");
            return;
        }
    };

    if end > buf.len() {
        error!("User-Agent 结束符超出缓冲区范围: end={} > buf.len()={}", end, buf.len());
        return;
    }

    let old_len = end - start;
    let new_len = user_agent.len();

    // 打印修改前的 User-Agent
    match std::str::from_utf8(&buf[start..end]) {
        Ok(ua) => debug!("修改前的 User-Agent: {}", ua),
        Err(_) => error!("修改前的 User-Agent 不是有效的 UTF-8"),
    };

    if old_len > 1024 {
        error!("User-Agent 字段超长，无法修改");
        return;
    }

    if check_is_in_whitelist(&buf[start..end]) {
        debug!("User-Agent 在白名单中，无需修改。");
        return;
    }

    // **修正部分**
    let mut new_buf = BytesMut::with_capacity(buf.len() - old_len + new_len);
    new_buf.extend_from_slice(&buf[..start]);  // 复制 User-Agent 之前的部分
    new_buf.extend_from_slice(user_agent.as_bytes());  // 插入新的 User-Agent
    new_buf.extend_from_slice(&buf[end..]);  // 复制 User-Agent 之后的部分

    // 替换 buf
    *buf = new_buf;

    match std::str::from_utf8(&buf[start..start + new_len]) {
        Ok(ua) => debug!("User-Agent 已修改为: {}", ua),
        Err(_) => error!("修改后的 User-Agent 不是有效的 UTF-8"),
    };
}

fn check_is_in_whitelist(buf: &[u8]) -> bool {
    const WHITELIST: &[&[u8]] = &[
        b"MicroMessenger Client",
        b"ByteDancePcdn",
        b"Go-http-client/1.1",
        b"Bilibili Freedoooooom/MarkII",
    ];
    for &item in WHITELIST {
        if item.len() == buf.len() && buf.eq_ignore_ascii_case(item) {
            return true;
        }
    }
    false
}
