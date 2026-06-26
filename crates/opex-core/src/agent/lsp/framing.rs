//! LSP base-protocol framing: `Content-Length: N\r\n\r\n<body>`.

#[allow(dead_code)]
pub fn encode_message(payload: &str) -> Vec<u8> {
    format!("Content-Length: {}\r\n\r\n{}", payload.len(), payload).into_bytes()
}

#[allow(dead_code)]
pub fn try_decode(buf: &mut Vec<u8>) -> Option<String> {
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let header = std::str::from_utf8(&buf[..header_end]).ok()?;
    let len: usize = header.lines()
        .find_map(|l| l.strip_prefix("Content-Length:").map(str::trim))
        .and_then(|n| n.parse().ok())?;
    let body_start = header_end + 4;
    if buf.len() < body_start + len { return None; }
    let body = String::from_utf8(buf[body_start..body_start + len].to_vec()).ok()?;
    buf.drain(..body_start + len);
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encodes_with_header() {
        assert_eq!(encode_message("{}"), b"Content-Length: 2\r\n\r\n{}".to_vec());
    }
    #[test]
    fn decodes_and_consumes() {
        let mut b = b"Content-Length: 2\r\n\r\n{}rest".to_vec();
        assert_eq!(try_decode(&mut b).as_deref(), Some("{}"));
        assert_eq!(b, b"rest".to_vec());
    }
    #[test]
    fn none_on_incomplete() {
        let mut b = b"Content-Length: 10\r\n\r\n{}".to_vec();
        assert_eq!(try_decode(&mut b), None);
        assert_eq!(b.len(), 24);
    }
    #[test]
    fn skips_extra_headers() {
        let mut b = b"Content-Length: 2\r\nContent-Type: x\r\n\r\n{}".to_vec();
        assert_eq!(try_decode(&mut b).as_deref(), Some("{}"));
    }
}
