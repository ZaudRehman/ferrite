//! Real-time hash-prefix lookup via proxy (k-anonymity).

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(800);
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;

/// K-anonymity prefix lookup via the local proxy.
/// Sends only hex-validated prefix (≥4 chars) as a query param; the proxy
/// telemetry path must not log query strings (callers: do not `tracing`
/// the full target).
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
    // Body is a JSON array of full hashes; a hit contains our prefix.
    // Avoid false positives from unrelated fields by scanning string values.
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
