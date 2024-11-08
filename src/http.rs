use log::{debug, error};


pub fn is_http_request(buf: &[u8]) -> bool {
    // Ensure the buffer is large enough for the smallest possible request (3 bytes for "GET")
    if buf.len() < 3 {
        return false;
    }

    // Directly compare the first few bytes for common HTTP methods
    match buf[0] {
        b'G' if buf.len() >= 3 && buf[1] == b'E' && buf[2] == b'T' => true,        // GET
        b'P' if buf.len() >= 3 && buf[1] == b'O' && buf[2] == b'S' => {
            if buf.len() >= 4 && buf[3] == b'T' { true } else { false }            // POST
        },
        b'H' if buf.len() >= 4 && buf[1] == b'E' && buf[2] == b'A' && buf[3] == b'D' => true,  // HEAD
        b'D' if buf.len() >= 6 && buf[1] == b'E' && buf[2] == b'L' && buf[3] == b'E' && buf[4] == b'T' && buf[5] == b'E' => true, // DELETE
        b'T' if buf.len() >= 5 && buf[1] == b'R' && buf[2] == b'A' && buf[3] == b'C' && buf[4] == b'E' => true, // TRACE
        b'O' if buf.len() >= 7 && buf[1] == b'P' && buf[2] == b'T' && buf[3] == b'I' && buf[4] == b'O' && buf[5] == b'N' && buf[6] == b'S' => true, // OPTIONS
        b'C' if buf.len() >= 7 && buf[1] == b'O' && buf[2] == b'N' && buf[3] == b'N' && buf[4] == b'E' && buf[5] == b'C' && buf[6] == b'T' => true, // CONNECT
        b'P' if buf.len() >= 3 && buf[1] == b'U' && buf[2] == b'T' => true, // PUT
        b'P' if buf.len() >= 5 && buf[1] == b'A' && buf[2] == b'T' && buf[3] == b'C' && buf[4] == b'H' => true, // PATCH
        _ => false,
    }
}

pub fn modify_user_agent(buf: &mut Vec<u8>, user_agent: &str) {
    let target = b"User-Agent: ";
    let len = buf.len();

    // 定位 User-Agent 的起始位置
    let start = buf.windows(target.len()).position(|window| window == target);
    if start.is_none() {
        error!("User-Agent header not found");
        return;
    }
    let start = start.unwrap() + target.len();

    // 定位 User-Agent 的结束位置，以 `\r\n` 为结束标记
    let end = buf[start..]
        .windows(2)
        .position(|window| window == b"\r\n")
        .map(|pos| start + pos)
        .unwrap_or(len); // 如果找不到结束标记，默认使用缓冲区末尾

    // 检查 User-Agent 是否在白名单中
    if check_is_in_whitelist(&buf[start..end]) {
        debug!("User-Agent 在白名单中，无需修改");
        return;
    }
    debug!("正在替换 User-Agent，从位置 {} 到 {}", start, end);
    debug!("修改前 HTTP 请求:\n{}", String::from_utf8_lossy(&buf));

    // 执行修改
    buf.splice(start..end, user_agent.bytes());
    // 输出修改后的完整请求头，确保格式不被破坏
    debug!("修改后的完整 HTTP 请求:\n{}", String::from_utf8_lossy(&buf));
}


fn check_is_in_whitelist(buf: &[u8]) -> bool {
    static WHITELIST: &[&str] = &["micromessenger client", "bilibili"];

    let buf = std::str::from_utf8(buf).unwrap_or("").to_lowercase();
    WHITELIST.iter().any(|&item| buf.contains(item))
}
