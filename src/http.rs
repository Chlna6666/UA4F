use log::{debug, error};
use std::collections::HashSet;

pub fn is_http_request(buf: &[u8]) -> bool {
    matches!(
        buf.get(0..),
        Some([b'G', b'E', b'T', ..])
            | Some([b'P', b'O', b'S', b'T', ..])
            | Some([b'H', b'E', b'A', b'D', ..])
            | Some([b'D', b'E', b'L', b'E', b'T', b'E', ..])
            | Some([b'T', b'R', b'A', b'C', b'E', ..])
            | Some([b'O', b'P', b'T', b'I', b'O', b'N', b'S', ..])
            | Some([b'C', b'O', b'N', b'N', b'E', b'C', b'T', ..])
            | Some([b'P', b'U', b'T', ..])
            | Some([b'P', b'A', b'T', b'C', b'H', ..])
    )
}

pub fn modify_user_agent(buf: &mut Vec<u8>, user_agent: &str) {
    let target = b"User-Agent: ";
    let len = buf.len();
    let mut pos = 0;

    // 查找User-Agent头的起始位置
    while pos + target.len() < len {
        if &buf[pos..pos + target.len()] == target {
            break;
        }
        pos += 1;
    }

    if pos + target.len() >= len {
        error!("User-Agent not found, start not found");
        return;
    }
    let start = pos + target.len();

    // 查找User-Agent头的结束位置
    while pos < len {
        if buf.get(pos..pos + 2) == Some(&[b'\r', b'\n']) {
            break;
        }
        pos += 1;
    }

    if pos >= len {
        error!("User-Agent not found, end not found");
        return;
    }

    let end = pos;
    debug!("start: {}, end: {}", start, end);

    if check_is_in_whitelist(&buf[start..end]) {
        return;
    }

    buf.splice(start..end, user_agent.bytes());
    debug!(
        "new user_agent: {}",
        String::from_utf8_lossy(&buf[start..start + user_agent.len()])
    );
}

fn check_is_in_whitelist(buf: &[u8]) -> bool {
    static WHITELIST: &[&str] = &["micromessenger client", "bilibili"];

    let buf = std::str::from_utf8(buf).unwrap_or("").to_lowercase();
    WHITELIST.iter().any(|&item| buf.contains(item))
}
