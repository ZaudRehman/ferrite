//! Ferrite's own outbound client. Never routed through the loopback proxy.
//!
//! ponytail: OS resolver here, not `DnsConfig`. Plumb DoH through only if a
//! DoH only network ever requires it.

use std::collections::HashMap;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
pub const DIRECT_MAX_HEAD: usize = 16 * 1024;
pub const DIRECT_MAX_BODY: usize = 1024 * 1024;

pub struct DirectResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl DirectResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == lower)
            .map(|(_, v)| v.as_str())
    }

    pub fn body_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// Direct origin GET. No redirects. None on any failure, cap breach, pin
/// mismatch, or parse rejection. Trust is roots plus exact host leaf pins.
pub async fn direct_get(url: &str, max_body: usize) -> Option<DirectResponse> {
    let parsed = url::Url::parse(url).ok()?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_owned();
    let port = parsed
        .port()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    let mut path = parsed.path().to_owned();
    if path.is_empty() {
        path.push('/');
    }
    if let Some(q) = parsed.query() {
        path.push('?');
        path.push_str(q);
    }
    // Never ask the loopback proxy (that would recurse).
    let addrs: Vec<std::net::SocketAddr> = if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        vec![std::net::SocketAddr::new(ip, port)]
    } else {
        tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio::net::lookup_host((host.as_str(), port)),
        )
        .await
        .ok()?
        .ok()?
        .collect()
    };
    let addr = addrs.into_iter().next()?;
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr))
        .await
        .ok()?
        .ok()?;
    let _ = tcp.set_nodelay(true);
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    if scheme == "https" {
        // Explicit provider: feature-unification across the tree can enable
        // both ring and aws-lc-rs, which makes auto-detect panic. The
        // already-set error is ignored (first install wins process-wide).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let connector = tokio_rustls::TlsConnector::from(config);
        let server_name = host.clone().try_into().ok()?;
        let mut tls = tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(server_name, tcp))
            .await
            .ok()?
            .ok()?;
        // Pin check before any request bytes. Mismatch fails closed.
        let peer_leaf: Option<Vec<u8>> = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .map(|c| c.to_vec());
        if !pin_allows(&host, peer_leaf.as_deref()) {
            tracing::warn!(%host, "tls pin mismatch: connection refused");
            return None;
        }
        tokio::time::timeout(IO_TIMEOUT, tls.write_all(req.as_bytes()))
            .await
            .ok()?
            .ok()?;
        let raw = read_capped(&mut tls, max_body).await?;
        parse_response(raw, max_body)
    } else {
        let mut stream = tcp;
        tokio::time::timeout(IO_TIMEOUT, stream.write_all(req.as_bytes()))
            .await
            .ok()?
            .ok()?;
        let raw = read_capped(&mut stream, max_body).await?;
        parse_response(raw, max_body)
    }
}

async fn read_capped<S>(stream: &mut S, max_body: usize) -> Option<Vec<u8>>
where
    S: AsyncReadExt + Unpin,
{
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];
    loop {
        if buf.len() > DIRECT_MAX_HEAD + max_body + 4 {
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
    Some(buf)
}

fn parse_response(raw: Vec<u8>, max_body: usize) -> Option<DirectResponse> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    if split > DIRECT_MAX_HEAD + 4 {
        return None;
    }
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.lines();
    let status_line = lines.next()?;
    let mut parts = status_line.split_whitespace();
    parts.next()?;
    let status: u16 = parts.next()?.parse().ok()?;
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(':')?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
    }
    let mut body = raw[split..].to_vec();
    if body.len() > max_body {
        return None;
    }
    body.truncate(max_body);
    Some(DirectResponse {
        status,
        headers,
        body,
    })
}

/// Operator pins for Ferrite's own fetches. Leaf SHA-256 hex per exact host.
/// Empty registry means root verification only. Format is `host=hex64,...`.
/// Malformed entries are skipped and counted.
pub fn leaf_pin_hex(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Parse one `host=hex` entry. Exact host only, 64 hex chars.
pub fn parse_pin_entry(s: &str) -> Option<(String, String)> {
    let (host, hex) = s.split_once('=')?;
    let host = host.trim().to_lowercase();
    let hex = hex.trim().to_lowercase();
    if host.is_empty() || host.contains(char::is_whitespace) || host.contains('/') {
        return None;
    }
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((host, hex))
}

/// Map plus skipped count.
pub fn parse_pin_list(s: &str) -> (HashMap<String, Vec<String>>, usize) {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    let mut skipped = 0;
    for entry in s.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        match parse_pin_entry(entry) {
            Some((host, hex)) => map.entry(host).or_default().push(hex),
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(skipped, "tls pin list entries skipped (malformed)");
    }
    (map, skipped)
}

static TLS_PINS: std::sync::OnceLock<std::sync::RwLock<HashMap<String, Vec<String>>>> =
    std::sync::OnceLock::new();

fn pin_registry() -> &'static std::sync::RwLock<HashMap<String, Vec<String>>> {
    TLS_PINS.get_or_init(|| std::sync::RwLock::new(HashMap::new()))
}

/// Install the pin set. Replaces any previous set. Empty clears.
pub fn set_tls_pins(map: HashMap<String, Vec<String>>) {
    if let Ok(mut reg) = pin_registry().write() {
        *reg = map;
    }
}

/// Pinned leaf hexes for a host. Empty means unpinned.
pub fn tls_pins_for(host: &str) -> Vec<String> {
    pin_registry()
        .read()
        .map(|reg| reg.get(&host.to_lowercase()).cloned().unwrap_or_default())
        .unwrap_or_default()
}

/// Unpinned hosts pass. Pinned hosts must present a listed leaf.
pub fn pin_allows_with(host: &str, leaf_der: &[u8], pins: &HashMap<String, Vec<String>>) -> bool {
    match pins.get(&host.to_lowercase()) {
        Some(list) if !list.is_empty() => {
            let hex = leaf_pin_hex(leaf_der);
            list.iter().any(|p| p == &hex)
        }
        // Unpinned or empty: standard verification only.
        _ => true,
    }
}

/// Registry-backed decision for a live handshake. No peer certificates
/// fails closed (TLS always presents at least a leaf; absence means
/// something is already wrong).
fn pin_allows(host: &str, peer_leaf: Option<&[u8]>) -> bool {
    let Ok(reg) = pin_registry().read() else {
        return false;
    };
    match reg.get(&host.to_lowercase()) {
        Some(list) if !list.is_empty() => match peer_leaf {
            Some(der) => {
                let hex = leaf_pin_hex(der);
                list.iter().any(|p| p == &hex)
            }
            // Pinned host presenting nothing fails closed.
            None => false,
        },
        // Unpinned or empty: standard verification only.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        assert!(direct_get("ferrite://newtab", 1024).await.is_none());
        assert!(direct_get("ftp://example.com/x", 1024).await.is_none());
    }

    #[tokio::test]
    async fn rejects_unparsable_url() {
        assert!(direct_get("not a url", 1024).await.is_none());
        assert!(direct_get("https://", 1024).await.is_none());
    }

    #[test]
    fn strict_status_and_head_cap() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".to_vec();
        let r = parse_response(raw, 1024).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-length"), Some("2"));
        assert_eq!(r.body_str(), "hi");
        assert!(parse_response(b"GARBAGE\r\n\r\nx".to_vec(), 1024).is_none());
        let big = vec![b'A'; DIRECT_MAX_HEAD + 8];
        assert!(parse_response([big, b"\r\n\r\n".to_vec()].concat(), 1024).is_none());
    }

    #[test]
    fn body_over_cap_rejected() {
        let raw = b"HTTP/1.1 200 OK\r\n\r\n0123456789".to_vec();
        assert!(parse_response(raw, 4).is_none());
    }

    #[test]
    fn pin_entries_validate_strictly() {
        let good = "ab".repeat(32);
        assert_eq!(
            parse_pin_entry(&format!("Example.COM={good}")),
            Some(("example.com".to_owned(), good.clone()))
        );
        assert_eq!(parse_pin_entry("a.test="), None);
        assert_eq!(parse_pin_entry("=abcd"), None);
        assert_eq!(parse_pin_entry("no-equals-here"), None);
        assert_eq!(parse_pin_entry("a.test=xyz"), None);
        assert_eq!(parse_pin_entry("a b.test=abcd"), None);
        assert_eq!(parse_pin_entry("a.test/b=abcd"), None);
    }

    #[test]
    fn pin_list_skips_malformed_and_counts() {
        let good = "ab".repeat(32);
        let (map, skipped) =
            parse_pin_list(&format!("a.test={good}, junk, b.test={good}, , a.test="));
        assert_eq!(skipped, 2);
        assert_eq!(map.get("a.test").unwrap(), &vec![good.clone()]);
        assert_eq!(map.get("b.test").unwrap(), &vec![good]);
    }

    #[test]
    fn pin_decision_is_exact_host_and_fail_closed() {
        use std::collections::HashMap;
        let der = b"fake-der-bytes";
        let hex = leaf_pin_hex(der);
        assert_eq!(hex.len(), 64);
        let mut pins = HashMap::new();
        // Unpinned hosts always pass (default registry state).
        assert!(pin_allows_with("example.com", der, &pins));
        pins.insert("example.com".to_owned(), vec![hex.clone()]);
        assert!(pin_allows_with("EXAMPLE.com", der, &pins));
        assert!(pin_allows_with("example.com", der, &pins));
        // Listed leaf, wrong host, no suffix inheritance.
        assert!(pin_allows_with("sub.example.com", der, &pins));
        assert!(pin_allows_with("example.com.evil.com", der, &pins));
        // Pinned host, unlisted leaf, closed.
        assert!(!pin_allows_with("example.com", b"other-der", &pins));
        // Empty pin list behaves as unpinned.
        pins.insert("empty.test".to_owned(), vec![]);
        assert!(pin_allows_with("empty.test", der, &pins));
    }

    #[test]
    fn pin_registry_roundtrips() {
        let good = "cd".repeat(32);
        let mut map = HashMap::new();
        map.insert("reg.test".to_owned(), vec![good.clone()]);
        set_tls_pins(map);
        assert_eq!(tls_pins_for("REG.test"), vec![good]);
        assert!(tls_pins_for("other.test").is_empty());
        set_tls_pins(HashMap::new());
        assert!(tls_pins_for("reg.test").is_empty());
    }
}
