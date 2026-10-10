//! Resolution at the proxy. System resolver by default, user DoH when set.
//! DoH failure falls back to OS. DNS never blocks, `decide` does.

use std::net::SocketAddr;
use std::time::Duration;

const DOH_TIMEOUT: Duration = Duration::from_secs(2);
const DOH_MAX_BODY: usize = 64 * 1024;

/// How upstream hostnames resolve.
#[derive(Debug, Clone, Default)]
pub struct DnsConfig {
    /// `https://` DoH JSON endpoint. None means OS resolver. Invalid values
    /// are ignored at resolve time.
    pub doh_endpoint: Option<String>,
}

impl DnsConfig {
    pub fn system() -> Self {
        Self { doh_endpoint: None }
    }

    pub fn with_doh(endpoint: impl Into<String>) -> Self {
        Self {
            doh_endpoint: Some(endpoint.into()),
        }
    }

    /// Why the configured DoH endpoint is unusable, if it is. Pure and
    /// loud-by-design: callers validate at the settings boundary (once per
    /// import/spawn), never per dial, so a bad value is one warning, not a
    /// warning per request. `None` means unset-or-valid.
    pub fn endpoint_problem(&self) -> Option<&'static str> {
        let endpoint = self.doh_endpoint.as_deref()?;
        if endpoint.trim().is_empty() {
            return None;
        }
        let parsed = match url::Url::parse(endpoint) {
            Ok(u) => u,
            Err(_) => return Some("doh endpoint must be https"),
        };
        if parsed.scheme() != "https" {
            return Some("doh endpoint must be https");
        }
        if parsed.host_str().is_none_or(|h| h.is_empty()) {
            return Some("doh endpoint has no host");
        }
        None
    }

    /// Effective endpoint: the configured value when valid, `None` otherwise
    /// (OS resolver). Invalid values are refused here, never probed.
    pub fn effective_endpoint(&self) -> Option<&str> {
        if self.endpoint_problem().is_some() {
            return None;
        }
        self.doh_endpoint
            .as_deref()
            .filter(|e| !e.trim().is_empty())
    }

    /// Literal IPs skip resolution. DoH first when set, else OS. DoH failure
    /// falls back to OS, so a dead server never breaks browsing.
    pub async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        if let Some(endpoint) = self.effective_endpoint() {
            match doh_lookup(endpoint, host).await {
                Ok(ips) if !ips.is_empty() => {
                    return Ok(ips
                        .into_iter()
                        .map(|ip| SocketAddr::new(ip, port))
                        .collect());
                }
                Ok(_) => tracing::debug!(%host, "doh empty answer: os fallback"),
                Err(e) => tracing::warn!(%host, error = %e, "doh failed: os fallback"),
            }
        }
        tokio::net::lookup_host((host, port))
            .await
            .map(|v| v.collect())
    }
}

/// DoH JSON API over a fresh TLS connection. No proxy (we are the proxy).
/// Only `Answer[].data` IPs are read. Both A and AAAA are queried.
async fn doh_lookup(endpoint: &str, name: &str) -> std::io::Result<Vec<std::net::IpAddr>> {
    let parsed = url::Url::parse(endpoint).map_err(std::io::Error::other)?;
    if parsed.scheme() != "https" {
        return Err(std::io::Error::other("doh endpoint must be https"));
    }
    let doh_host = parsed
        .host_str()
        .ok_or_else(|| std::io::Error::other("doh no host"))?;
    let doh_port = parsed.port().unwrap_or(443);
    let base = parsed.path().to_owned();
    let mut out = Vec::new();
    for qtype in ["A", "AAAA"] {
        let path = format!("{base}?name={name}&type={qtype}");
        match doh_json_query(doh_host, doh_port, &path).await {
            Ok(mut ips) => out.append(&mut ips),
            Err(e) => tracing::debug!(%name, %qtype, error = %e, "doh query failed"),
        }
    }
    Ok(out)
}

async fn doh_json_query(
    doh_host: &str,
    doh_port: u16,
    path: &str,
) -> std::io::Result<Vec<std::net::IpAddr>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addrs = tokio::net::lookup_host((doh_host, doh_port))
        .await?
        .collect::<Vec<_>>();
    let addr = addrs
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("doh resolve empty"))?;
    let tcp = tokio::time::timeout(DOH_TIMEOUT, tokio::net::TcpStream::connect(addr))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "doh connect timeout"))??;
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // Explicit provider (see direct.rs): auto-detect panics under
    // feature-unification. Already-set error ignored, first wins.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = std::sync::Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let connector = tokio_rustls::TlsConnector::from(config);
    let server_name = doh_host
        .to_owned()
        .try_into()
        .map_err(std::io::Error::other)?;
    let mut tls = tokio::time::timeout(DOH_TIMEOUT, connector.connect(server_name, tcp))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "doh tls timeout"))??;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {doh_host}\r\nAccept: application/dns-json\r\nConnection: close\r\n\r\n"
    );
    tokio::time::timeout(DOH_TIMEOUT, tls.write_all(req.as_bytes()))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "doh write timeout"))??;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        if buf.len() > DOH_MAX_BODY + 1024 {
            return Err(std::io::Error::other("doh body too large"));
        }
        let n = tokio::time::timeout(DOH_TIMEOUT, tls.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "doh read timeout"))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("doh no head"))?
        + 4;
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other("doh bad status"))?;
    if status != 200 {
        return Err(std::io::Error::other(format!("doh status {status}")));
    }
    let body = &buf[split..];
    let json: serde_json::Value = serde_json::from_slice(body).map_err(std::io::Error::other)?;
    let mut ips = Vec::new();
    if let Some(answers) = json.get("Answer").and_then(|a| a.as_array()) {
        for a in answers {
            if let Some(data) = a.get("data").and_then(|d| d.as_str())
                && let Ok(ip) = data.parse::<std::net::IpAddr>()
            {
                ips.push(ip);
            }
        }
    }
    Ok(ips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn literal_ip_skips_resolution() {
        let dns = DnsConfig::system();
        let addrs = dns.resolve("127.0.0.1", 8080).await.unwrap();
        assert_eq!(addrs, vec![SocketAddr::from(([127, 0, 0, 1], 8080))]);
    }

    #[tokio::test]
    async fn invalid_doh_falls_back_to_os() {
        let dns = DnsConfig::with_doh("http://127.0.0.1/dns-query");
        let addrs = dns.resolve("localhost", 80).await.unwrap();
        assert!(!addrs.is_empty());
    }

    #[tokio::test]
    async fn unreachable_doh_falls_back_to_os() {
        let dns = DnsConfig::with_doh("https://127.0.0.1:1/dns-query");
        let addrs = dns.resolve("localhost", 80).await.unwrap();
        assert!(!addrs.is_empty());
    }

    #[test]
    fn endpoint_validation() {
        assert!(url::Url::parse("https://dns.example/dns-query").is_ok());
        let bad = url::Url::parse("http://dns.example/dns-query").unwrap();
        assert_ne!(bad.scheme(), "https");
    }

    #[test]
    fn endpoint_problem_names_bad_values() {
        assert_eq!(DnsConfig::system().endpoint_problem(), None);
        assert_eq!(
            DnsConfig::with_doh("https://dns.example/dns-query").endpoint_problem(),
            None
        );
        assert_eq!(
            DnsConfig::with_doh("http://dns.example/dns-query").endpoint_problem(),
            Some("doh endpoint must be https")
        );
        assert_eq!(
            DnsConfig::with_doh("not a url").endpoint_problem(),
            Some("doh endpoint must be https")
        );
        // `https://` with an empty host fails parsing in the `url` crate,
        // which reports as the https reason above; the explicit no-host arm
        // stays as defense-in-depth for parser behavior changes.
        // Effective endpoint refuses invalid values (OS fallback downstream).
        assert!(
            DnsConfig::with_doh("http://dns.example/dns-query")
                .effective_endpoint()
                .is_none()
        );
        assert_eq!(
            DnsConfig::with_doh("https://dns.example/dns-query").effective_endpoint(),
            Some("https://dns.example/dns-query")
        );
    }
}
