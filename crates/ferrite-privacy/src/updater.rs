//! Filter list fetching. Direct origin fetch is primary, proxy path is
//! legacy. None means retain last known good.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 1;

/// Normalize to the load contract: `trim_end + "\n"`.
pub fn normalize_filter_text(text: &str) -> String {
    format!("{}\n", text.trim_end_matches(['\n', '\r']))
}

/// Fetch through the local proxy. Legacy path, tests only. One same host
/// redirect, caps, strict status, optional `X-SHA256` over the normalized
/// body. Checksum detects truncation, not authenticity.
pub async fn fetch_via_proxy(url: &str, port: u16) -> Option<(String, Option<String>)> {
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let parsed = url::Url::parse(&current).ok()?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return None;
        }
        let host = parsed.host_str()?.to_owned();
        let resp = get_via_proxy(&current, &host, port).await?;
        match resp.status {
            200 => {
                let body = resp.body;
                let sha = resp.sha256;
                if let Some(expected) = &sha {
                    let normalized = normalize_filter_text(&body);
                    if !verify_normalized(&normalized, expected) {
                        return None;
                    }
                }
                return Some((body, sha));
            }
            301 | 302 | 303 | 307 | 308 => {
                let loc = resp.location?;
                let next = url::Url::parse(&loc)
                    .ok()
                    .and_then(|u| {
                        if u.host_str() == parsed.host_str() {
                            Some(u.to_string())
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                        if loc.starts_with('/') {
                            Some(format!(
                                "{}://{}{}",
                                parsed.scheme(),
                                parsed.host_str()?,
                                loc
                            ))
                        } else {
                            None
                        }
                    })?;
                current = next;
                continue;
            }
            _ => return None,
        }
    }
    None
}

struct ProxyResponse {
    status: u16,
    body: String,
    sha256: Option<String>,
    location: Option<String>,
}

/// Fetch direct from origin. Same contract as the proxy path.
/// None means retain last known good.
pub async fn fetch_direct(url: &str) -> Option<(String, Option<String>)> {
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let parsed = url::Url::parse(&current).ok()?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return None;
        }
        let resp = ferrite_proxy::direct_get(&current, MAX_HEAD_BYTES + MAX_BODY_BYTES).await?;
        match resp.status {
            200 => {
                let body = resp.body_str().into_owned();
                let sha = resp
                    .header("x-sha256")
                    .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit()))
                    .map(|s| s.to_owned());
                if let Some(expected) = &sha {
                    let normalized = normalize_filter_text(&body);
                    if !verify_normalized(&normalized, expected) {
                        return None;
                    }
                }
                return Some((body, sha));
            }
            301 | 302 | 303 | 307 | 308 => {
                let loc = resp.header("location")?.to_owned();
                let next = url::Url::parse(&loc)
                    .ok()
                    .and_then(|u| {
                        if u.host_str() == parsed.host_str() {
                            Some(u.to_string())
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                        if loc.starts_with('/') {
                            Some(format!(
                                "{}://{}{}",
                                parsed.scheme(),
                                parsed.host_str()?,
                                loc
                            ))
                        } else {
                            None
                        }
                    })?;
                current = next;
                continue;
            }
            _ => return None,
        }
    }
    None
}

/// Fetch raw bytes direct from origin (dictionary blobs are binary).
/// Same discipline as [`fetch_direct`]: scheme gate, one same-host
/// redirect, strict 200, 8MB cap. None means retain/skip: the caller must
/// fail soft (spellcheck continues without that dictionary).
pub async fn fetch_dict_bytes(url: &str) -> Option<Vec<u8>> {
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let parsed = url::Url::parse(&current).ok()?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return None;
        }
        let resp = ferrite_proxy::direct_get(&current, MAX_HEAD_BYTES + MAX_BODY_BYTES).await?;
        match resp.status {
            200 => return Some(resp.body),
            301 | 302 | 303 | 307 | 308 => {
                let loc = resp.header("location")?.to_owned();
                let next = url::Url::parse(&loc)
                    .ok()
                    .and_then(|u| {
                        if u.host_str() == parsed.host_str() {
                            Some(u.to_string())
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                        if loc.starts_with('/') {
                            Some(format!(
                                "{}://{}{}",
                                parsed.scheme(),
                                parsed.host_str()?,
                                loc
                            ))
                        } else {
                            None
                        }
                    })?;
                current = next;
                continue;
            }
            _ => return None,
        }
    }
    None
}

fn verify_normalized(normalized: &str, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    let actual: String = Sha256::digest(normalized.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    actual.to_lowercase() == expected.to_lowercase()
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
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];
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
    }
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    if split > MAX_HEAD_BYTES + 4 {
        return None;
    }
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let status = parse_status(&head)?;
    let body = buf[split..].to_vec();
    if body.len() > MAX_BODY_BYTES {
        return None;
    }
    let sha256 = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("x-sha256:"))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_owned()))
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit()));
    let location = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("location:"))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_owned()));
    Some(ProxyResponse {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
        sha256,
        location,
    })
}

fn parse_status(head: &str) -> Option<u16> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    parts.next()?;
    parts.next()?.parse().ok()
}
