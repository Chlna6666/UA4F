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


pub fn modify_user_agent(buf: &mut Vec<u8>, user_agent: &str) {
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
        error!("User-Agent 结束符超出缓冲区范围");
        return;
    }

    let old_len = end - start;
    let new_len = user_agent.len();

    if old_len > 1024 {
        error!("User-Agent 字段超长，无法修改");
        return;
    }

    if check_is_in_whitelist(&buf[start..end]) {
        debug!("User-Agent 在白名单中，无需修改。");
        return;
    }

    // 使用 splice 并立即消费返回的迭代器，确保替换立即生效
    let _ = buf.splice(start..end, user_agent.bytes()).collect::<Vec<_>>();

    match std::str::from_utf8(&buf[start..start + new_len]) {
        Ok(ua) => debug!("User-Agent 已修改为: {}", ua),
        Err(_) => error!("修改后的 User-Agent 不是有效的 UTF-8"),
    };
}

/// **优化白名单检查**
fn check_is_in_whitelist(buf: &[u8]) -> bool {
    const WHITELIST: [&[u8]; 3] = [
        b"MicroMessenger Client",
        b"bilibili",
        b"Go-http-client/1.1",
    ];
    WHITELIST.iter().any(|&item| buf.eq_ignore_ascii_case(item))
}