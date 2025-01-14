use tracing::{error, debug};
use memchr::memmem;

pub fn is_http_request(buf: &[u8]) -> bool {
    matches!(
        buf,
        [b'G', b'E', b'T', ..]
            | [b'P', b'O', b'S', b'T', ..]
            | [b'P', b'U', b'T', ..]
            | [b'P', b'A', b'T', b'C', b'H', ..]
            | [b'H', b'E', b'A', b'D', ..]
            | [b'D', b'E', b'L', b'E', b'T', b'E', ..]
            | [b'T', b'R', b'A', b'C', b'E', ..]
            | [b'O', b'P', b'T', b'I', b'O', b'N', b'S', ..]
            | [b'C', b'O', b'N', b'N', b'E', b'C', b'T', ..]
    )
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

    let end = match buf[start..].iter().position(|&b| b == b'\r') {
        Some(pos) => start + pos,
        None => {
            error!("未找到 User-Agent 结束符");
            return;
        }
    };

    if end - start > 1024 {
        error!("User-Agent 字段超长，无法修改");
        return;
    }
    /*

    if check_is_in_whitelist(&buf[start..end]) {
        debug!("User-Agent 在白名单中，无需修改。");
        return;
    }

    */
    buf.splice(start..end, user_agent.as_bytes().iter().copied());
    debug!(
        "User-Agent 已修改为: {}",
        String::from_utf8_lossy(&buf[start..start + user_agent.len()])
    );
}
/*
fn check_is_in_whitelist(buf: &[u8]) -> bool {
    const WHITELIST: &[&[u8]] = &[
        b"micromessenger client",
        b"bilibili",
    ];

    let lower_buf = buf.iter().map(|&b| b.to_ascii_lowercase());
    WHITELIST.iter().any(|&item| lower_buf.clone().eq(item.iter().copied()))
}
*/