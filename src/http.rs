use log::{debug, error};

// 判断是否为 HTTP 请求
pub fn is_http_request(buf: &[u8]) -> bool {
    if buf.len() < 3 {
        return false;
    }

    match buf[0] {
        b'G' if buf.len() >= 3 && buf[1] == b'E' && buf[2] == b'T' => true,
        b'P' if buf.len() >= 4 && buf[1] == b'O' && buf[2] == b'S' && buf[3] == b'T' => true,
        b'P' if buf.len() >= 3 && buf[1] == b'U' && buf[2] == b'T' => true,
        b'P' if buf.len() >= 5 && buf[1] == b'A' && buf[2] == b'T' && buf[3] == b'C' && buf[4] == b'H' => true,
        b'H' if buf.len() >= 4 && buf[1] == b'E' && buf[2] == b'A' && buf[3] == b'D' => true,
        b'D' if buf.len() >= 6 && buf[1] == b'E' && buf[2] == b'L' && buf[3] == b'E' && buf[4] == b'T' && buf[5] == b'E' => true,
        b'T' if buf.len() >= 5 && buf[1] == b'R' && buf[2] == b'A' && buf[3] == b'C' && buf[4] == b'E' => true,
        b'O' if buf.len() >= 7 && buf[1] == b'P' && buf[2] == b'T' && buf[3] == b'I' && buf[4] == b'O' && buf[5] == b'N' && buf[6] == b'S' => true,
        b'C' if buf.len() >= 7 && buf[1] == b'O' && buf[2] == b'N' && buf[3] == b'N' && buf[4] == b'E' && buf[5] == b'C' && buf[6] == b'T' => true,
        _ => false,
    }
}

// 修改 User-Agent
pub fn modify_user_agent(buf: &mut Vec<u8>, user_agent: &str) {
    let target = b"User-Agent: ";
    let len = buf.len();

    debug!("初始 HTTP 请求:\n{}", String::from_utf8_lossy(&buf));

    // 定位 User-Agent 头的起始位置
    let start = buf.windows(target.len()).position(|window| window == target);
    if start.is_none() {
        error!("未找到 User-Agent 头");
        return;
    }
    let start = start.unwrap() + target.len();

    // 使用 `\r\n` 定位 User-Agent 头的结束位置
    let end = buf[start..]
        .windows(2)
        .position(|window| window == b"\r\n")
        .map(|pos| start + pos)
        .unwrap_or(len); // 如果找不到结束标记，默认使用缓冲区末尾

    // 检查 User-Agent 是否在白名单中
    if check_is_in_whitelist(&buf[start..end]) {
        debug!("User-Agent 在白名单中，无需修改。");
        debug!("未修改的 HTTP 请求:\n{}", String::from_utf8_lossy(&buf));
        return;
    }
    debug!("替换 User-Agent，从位置 {} 到 {}", start, end);

    // 执行修改
    buf.splice(start..end, user_agent.bytes());

    // 输出修改后的请求
    debug!("修改后的 HTTP 请求:\n{}", String::from_utf8_lossy(&buf));
}

// 检查 User-Agent 是否在白名单中
fn check_is_in_whitelist(buf: &[u8]) -> bool {
    static WHITELIST: &[&str] = &["micromessenger client", "bilibili"];

    let buf = std::str::from_utf8(buf).unwrap_or("").to_lowercase();
    WHITELIST.iter().any(|&item| buf.contains(item))
}
