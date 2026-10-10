//! Realtime hash prefix lookup. Direct origin fetch, never the loopback proxy.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(800);
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Keyed SafeBrowsing URL. Pure for tests: prefix and key are already
/// validated hex/non-empty by the caller.
pub fn safebrowsing_url(prefix: &str, api_key: &str) -> String {
    let host = "safebrowsing.googleapis.com";
    format!("https://{host}/v4/threatMatches:find?prefix={prefix}&key={api_key}")
}

/// Direct lookup. Prefix only (hex, min 4 chars). Do not log the target.
/// Absent/empty key is an honest no-op: None without any network call.
pub async fn lookup_direct(prefix: &str, api_key: Option<&str>) -> Option<bool> {
    if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let key = api_key.map(str::trim).filter(|k| !k.is_empty())?;
    let target = safebrowsing_url(prefix, key);
    let resp = ferrite_proxy::direct_get(&target, MAX_HEAD_BYTES + MAX_BODY_BYTES).await?;
    if resp.status != 200 {
        return None;
    }
    Some(resp.body_str().contains(prefix))
}

/// Legacy proxy path. Tests and offline harnesses only.
pub async fn lookup(prefix: &str, port: u16) -> Option<bool> {
    if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let host = "safebrowsing.googleapis.com";
    let target = format!("https://{host}/v4/threatMatches:find?prefix={prefix}");
    let resp = get_via_proxy(&target, host, port).await?;
    if resp.status != 200 {
        return None;
    }
    Some(resp.body.contains(prefix))
}

struct ProxyResponse {
    status: u16,
    body: String,
}

async fn get_via_proxy(target: &str, host: &str, port: u16) -> Option<ProxyResponse> {
    let stream = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .ok()?
    .ok()?;
    let mut stream = stream;
    let req = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    tokio::time::timeout(IO_TIMEOUT, stream.write_all(req.as_bytes()))
        .await
        .ok()?
        .ok()?;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        if buf.len() > MAX_HEAD_BYTES + MAX_BODY_BYTES {
            return None;
        }
        let n = tokio::time::timeout(IO_TIMEOUT, stream.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_HEAD_BYTES + MAX_BODY_BYTES {
            return None;
        }
    }
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    if split > MAX_HEAD_BYTES + 4 {
        return None;
    }
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let status = parse_status(&head)?;
    let mut body = buf[split..].to_vec();
    if body.len() > MAX_BODY_BYTES {
        body.truncate(MAX_BODY_BYTES);
    }
    Some(ProxyResponse {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn parse_status(head: &str) -> Option<u16> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    parts.next()?;
    parts.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_url_carries_prefix_and_key() {
        let url = safebrowsing_url("abcd1234", "test-key-123");
        assert!(url.starts_with("https://safebrowsing.googleapis.com/"));
        assert!(url.contains("prefix=abcd1234"));
        assert!(url.contains("key=test-key-123"));
    }

    #[tokio::test]
    async fn absent_key_is_noop_without_network() {
        // No socket is opened: any well-formed prefix returns None fast.
        let start = std::time::Instant::now();
        assert_eq!(lookup_direct("abcd1234", None).await, None);
        assert_eq!(lookup_direct("abcd1234", Some("")).await, None);
        assert_eq!(lookup_direct("abcd1234", Some("   ")).await, None);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "absent key must not do network I/O"
        );
    }

    #[tokio::test]
    async fn invalid_prefix_returns_none_even_with_key() {
        assert_eq!(lookup_direct("xyz", Some("k")).await, None);
        assert_eq!(lookup_direct("", Some("k")).await, None);
    }
}
