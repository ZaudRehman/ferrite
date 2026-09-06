//! Breach check via k-anonymity through the local proxy.

use sha1::{Digest, Sha1};
use zeroize::Zeroize;

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;

pub fn hash_prefix(password: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(password.as_bytes());
    let result = hasher.finalize();
    let hex: String = result.iter().map(|b| format!("{b:02X}")).collect();
    hex[..5].to_owned()
}

pub fn full_sha1(password: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(password.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02X}")).collect()
}

/// Local suffix match against HIBP range tails.
/// `full_hash` must be the 40-char SHA-1; `tails` are `TAIL:count` lines
/// (or bare tails) where TAIL is `full_hash[5..]`. Short prefixes alone
/// cannot decide membership and return false. Never panics.
pub fn check_prefix_local(full_hash: &str, full_hash_suffixes: &[String]) -> bool {
    let h = full_hash.trim().to_ascii_uppercase();
    if h.len() != 40 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let want = &h[5..];
    full_hash_suffixes.iter().any(|s| {
        let tail = s
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        !tail.is_empty() && tail == want
    })
}

/// Precise tail check used by `check_breach_via_proxy`.
pub fn tail_matches_suffix(suffix_upper: &str, line_tail_upper: &str) -> bool {
    suffix_upper.eq_ignore_ascii_case(line_tail_upper)
}

/// HIBP k-anonymity range query via `https://` through the local proxy.
/// Sends only the 5-char prefix, never the password or full hash. Password
/// material is hashed then zeroized; responses are capped and timed out.
pub async fn check_breach_via_proxy(password: &str, proxy_port: u16) -> Option<bool> {
    let prefix = hash_prefix(password);
    let full = full_sha1(password);
    let mut full_buf = full.clone().into_bytes();
    let suffix = full[5..].to_owned();
    full_buf.zeroize();
    let host = "api.pwnedpasswords.com";
    let target = format!("https://{host}/range/{prefix}");
    let body = get_via_proxy(&target, host, proxy_port).await?;
    for line in body.lines() {
        if let Some((hash_tail, _count)) = line.split_once(':') {
            if tail_matches_suffix(&suffix, hash_tail.trim()) {
                return Some(true);
            }
        }
    }
    Some(false)
}

async fn get_via_proxy(target: &str, host: &str, port: u16) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
    if parse_status(&head)? != 200 {
        return None;
    }
    let body = buf[split..].to_vec();
    if body.len() > MAX_BODY_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(&body).into_owned())
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
    fn prefix_length() {
        let p = hash_prefix("password");
        assert_eq!(p.len(), 5);
        assert!(p.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn local_check_matches_tail_without_panic() {
        let full = full_sha1("password");
        let tail = full[5..].to_owned();
        assert!(check_prefix_local(
            &full,
            &[
                format!("{tail}:2"),
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:1".to_owned()
            ]
        ));
        assert!(!check_prefix_local(
            &full,
            &["BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB:1".to_owned()]
        ));
        assert!(!check_prefix_local("", &[]));
        assert!(!check_prefix_local("ABCDE", &[tail]));
    }

    #[test]
    fn tail_match_is_case_insensitive() {
        assert!(tail_matches_suffix("ABCDEF", "abcdef"));
        assert!(!tail_matches_suffix("ABCDEF", "ABCDE0"));
    }
}
