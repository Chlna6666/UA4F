use log::{debug, error};

// 判断是否为 HTTP 请求
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

// 修改 User-Agent
pub fn modify_user_agent(buf: &mut Vec<u8>, user_agent: &str) {
    const USER_AGENT_HEADER: &[u8] = b"User-Agent: ";
    let buf_len = buf.len();

    // 定位 User-Agent 头的起始位置
    if let Some(start) = buf.windows(USER_AGENT_HEADER.len()).position(|window| window == USER_AGENT_HEADER) {
        let start = start + USER_AGENT_HEADER.len();

        // 使用 `\r\n` 定位 User-Agent 头的结束位置
        let end = buf[start..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|pos| start + pos)
            .unwrap_or(buf_len); // 如果找不到结束标记，默认使用缓冲区末尾

        // 检查 User-Agent 是否在白名单中
        if check_is_in_whitelist(&buf[start..end]) {
            debug!("User-Agent 在白名单中，无需修改。");
            return;
        }

        // 替换 User-Agent 内容
        buf.splice(start..end, user_agent.as_bytes().iter().copied());
        debug!("修改后的 HTTP 请求:\n{}", String::from_utf8_lossy(&buf));
    } else {
        error!("未找到 User-Agent 头");
    }
}

// 检查 User-Agent 是否在白名单中
fn check_is_in_whitelist(buf: &[u8]) -> bool {
    const WHITELIST: &[&[u8]] = &[b"micromessenger client", b"bilibili"];

    WHITELIST.iter().any(|&item| buf.windows(item.len()).any(|window| window.eq_ignore_ascii_case(item)))
}
