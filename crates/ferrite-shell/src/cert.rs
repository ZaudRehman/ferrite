//! Per tab certificate viewer. Opens our own TLS handshake on explicit
//! request and reports the presented chain, never the tab connection.

// ponytail: platform hooks (WebView2 events, WK challenge, WebKitGTK info)
// are not surfaced by wry, so a second implementation waits on that.

use std::sync::mpsc::Sender;

use serde::Serialize;
use uuid::Uuid;

/// One certificate, display ready. Unsafe labels are flagged, not converted.
#[derive(Debug, Clone, Serialize)]
pub struct CertSummary {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    pub sig_alg: String,
    pub key_alg: String,
    pub key_bits: u32,
    pub sha256: String,
    pub sans: Vec<String>,
    pub san_suspicious: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CertReport {
    pub host: String,
    pub verified: bool,
    pub verify_error: String,
    pub leaf: Option<CertSummary>,
    pub chain: Vec<CertSummary>,
    /// Live per site exception for this host and leaf. Engine truth untouched.
    pub exception_trusted: bool,
    pub exception_expires_at: Option<i64>,
}

/// Viewer grant lifetime: one year. Revocable anytime.
pub const EXCEPTION_TTL_MS: i64 = 365 * 24 * 3600 * 1000;

/// One exception row as the fetch thread sees it. Never touches the DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionEntry {
    pub host: String,
    pub sha256: String,
    pub expires_at: i64,
}

/// Whether a live exception covers `host` presenting `leaf_sha256` at
/// `now_ms`. Exact host, exact fingerprint, unexpired.
pub fn exception_matches(
    host: &str,
    leaf_sha256: &str,
    exceptions: &[ExceptionEntry],
    now_ms: i64,
) -> Option<i64> {
    let host = host.trim().to_lowercase();
    let fp = leaf_sha256.to_lowercase();
    exceptions
        .iter()
        .filter(|e| e.host == host && e.sha256 == fp && e.expires_at > now_ms)
        .map(|e| e.expires_at)
        .max()
}

/// True when any SAN label is non-ASCII (possible homograph: verify the
/// punycode form out-of-band, not here).
pub fn san_suspicious(sans: &[String]) -> bool {
    sans.iter().any(|s| !s.is_ascii())
}

/// SHA-256 fingerprint, lowercase hex.
pub fn fingerprint_hex(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn summarize(der: &[u8]) -> Option<CertSummary> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    let validity = cert.validity();
    let tbs = &cert.tbs_certificate;
    let key_bits = tbs
        .subject_pki
        .subject_public_key
        .data
        .len()
        .saturating_mul(8) as u32;
    let mut sans: Vec<String> = Vec::new();
    if let Ok(Some(ext)) = cert.subject_alternative_name() {
        for g in &ext.value.general_names {
            match g {
                x509_parser::extensions::GeneralName::DNSName(n)
                | x509_parser::extensions::GeneralName::RFC822Name(n)
                | x509_parser::extensions::GeneralName::URI(n) => {
                    sans.push(n.to_string());
                }
                x509_parser::extensions::GeneralName::IPAddress(b) => {
                    sans.push(
                        b.iter()
                            .map(|x| x.to_string())
                            .collect::<Vec<_>>()
                            .join("."),
                    );
                }
                _ => {}
            }
        }
    }
    Some(CertSummary {
        subject: cert.subject().to_string(),
        issuer: cert.issuer().to_string(),
        serial: cert.serial.to_string(),
        not_before: validity.not_before.to_rfc2822().unwrap_or_default(),
        not_after: validity.not_after.to_rfc2822().unwrap_or_default(),
        sig_alg: cert.signature.algorithm.to_id_string(),
        key_alg: tbs.subject_pki.algorithm.algorithm.to_id_string(),
        key_bits,
        sha256: fingerprint_hex(der),
        san_suspicious: san_suspicious(&sans),
        sans,
    })
}

/// TLS handshake to `host:port`, returning verification status plus the
/// presented DER chain. Direct connection (same destination the tab
/// visits, no new privacy class), 6s timeout, sent back over `tx`.
pub async fn fetch_chain(host: String, port: u16) -> Result<(bool, String, Vec<Vec<u8>>), String> {
    // Explicit provider: feature-unification across the tree can enable
    // both ring and aws-lc-rs, which makes auto-detect panic. The
    // already-set error is ignored (first install wins process-wide).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
    let server_name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|e| format!("bad hostname: {e}"))?;
    let addr = format!("{host}:{port}");
    let tcp = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .map_err(|_| "connection timed out".to_owned())?
    .map_err(|e| format!("connection failed: {e}"))?;
    tcp.set_nodelay(true).ok();
    match tokio::time::timeout(
        std::time::Duration::from_secs(6),
        connector.connect(server_name, tcp),
    )
    .await
    {
        Ok(Ok(tls)) => {
            let certs: Vec<Vec<u8>> = tls
                .get_ref()
                .1
                .peer_certificates()
                .unwrap_or_default()
                .iter()
                .map(|c| c.to_vec())
                .collect();
            Ok((true, String::new(), certs))
        }
        Ok(Err(e)) => Ok((false, format!("not trusted here: {e}"), Vec::new())),
        Err(_) => Ok((false, "handshake timed out".to_owned(), Vec::new())),
    }
}

/// Spawn the fetch on a thread (never block the winit loop). The report
/// lands on `tx` for the tick drain. `exceptions` is a Vault snapshot.
pub fn spawn_cert_fetch(
    host: String,
    port: u16,
    tab_id: Uuid,
    tx: Sender<(Uuid, CertReport)>,
    exceptions: Vec<ExceptionEntry>,
    now_ms: i64,
) {
    std::thread::spawn(move || {
        let blank = |host: String, verify_error: String| CertReport {
            host,
            verified: false,
            verify_error,
            leaf: None,
            chain: Vec::new(),
            exception_trusted: false,
            exception_expires_at: None,
        };
        let report = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => match rt.block_on(fetch_chain(host.clone(), port)) {
                Ok((verified, verify_error, chain)) => {
                    let summaries: Vec<CertSummary> =
                        chain.iter().filter_map(|d| summarize(d)).collect();
                    let leaf = summaries.first().cloned();
                    let (exception_trusted, exception_expires_at) = leaf
                        .as_ref()
                        .and_then(|l| exception_matches(&host, &l.sha256, &exceptions, now_ms))
                        .map(|exp| (true, Some(exp)))
                        .unwrap_or((false, None));
                    CertReport {
                        host: host.clone(),
                        verified,
                        verify_error,
                        leaf,
                        chain: summaries,
                        exception_trusted,
                        exception_expires_at,
                    }
                }
                Err(e) => blank(host.clone(), e),
            },
            Err(e) => blank(host.clone(), format!("runtime failed: {e}")),
        };
        let _ = tx.send((tab_id, report));
    });
}

/// Split an `https://host[:port]/...` URL for fetching. Anything else is
/// "not encrypted": never fetched, never parsed.
pub fn https_host(url: &str) -> Option<(String, u16)> {
    let rest = url.strip_prefix("https://")?;
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("");
    if hostport.is_empty() {
        return None;
    }
    if let Some((h, p)) = hostport.rsplit_once(':')
        && let Ok(port) = p.parse::<u16>()
    {
        return Some((h.to_owned(), port));
    }
    Some((hostport.to_owned(), 443))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_sha256_hex() {
        // sha256("abc"): canonical vector.
        assert_eq!(
            fingerprint_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn suspicious_flags_non_ascii_san() {
        assert!(!san_suspicious(&["example.com".to_owned()]));
        assert!(san_suspicious(&["exаmple.com".to_owned()]));
        assert!(!san_suspicious(&[]));
    }

    // Live handshake check (ignored in CI: needs the network). Run
    // manually to verify the fetch+parse path end to end.
    #[test]
    #[ignore]
    fn live_fetch_example_com() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (verified, err, chain) = rt
            .block_on(fetch_chain("example.com".to_owned(), 443))
            .unwrap();
        assert!(verified, "unexpected verify failure: {err}");
        assert!(!chain.is_empty(), "empty chain");
        let leaf = summarize(&chain[0]).expect("leaf parses");
        assert!(!leaf.sha256.is_empty());
        assert!(!leaf.not_after.is_empty());
    }

    #[test]
    fn https_host_splits_port() {
        assert_eq!(
            https_host("https://example.com:8443/a?b#c"),
            Some(("example.com".to_owned(), 8443))
        );
        assert_eq!(
            https_host("https://example.com"),
            Some(("example.com".to_owned(), 443))
        );
        assert_eq!(https_host("http://example.com"), None);
        assert_eq!(https_host("ferrite://settings"), None);
        assert_eq!(https_host("https://"), None);
    }

    #[test]
    fn exception_match_is_exact_host_fp_and_live() {
        let fp = "ab".repeat(32);
        let rows = vec![
            ExceptionEntry {
                host: "router.lan".to_owned(),
                sha256: fp.clone(),
                expires_at: 2000,
            },
            ExceptionEntry {
                host: "old.lan".to_owned(),
                sha256: fp.clone(),
                expires_at: 1000,
            },
        ];
        assert_eq!(
            exception_matches("router.lan", &fp, &rows, 1000),
            Some(2000)
        );
        assert_eq!(
            exception_matches("ROUTER.lan", &fp.to_uppercase(), &rows, 1999),
            Some(2000)
        );
        // Expired, wrong fp, wrong host, subdomain, superdomain: all None.
        assert_eq!(exception_matches("router.lan", &fp, &rows, 2000), None);
        assert_eq!(
            exception_matches("router.lan", &"cd".repeat(32), &rows, 1000),
            None
        );
        assert_eq!(exception_matches("sub.router.lan", &fp, &rows, 1000), None);
        assert_eq!(exception_matches("old.lan", &fp, &rows, 1000), None);
        assert_eq!(exception_matches("router.lan", &fp, &[], 1000), None);
    }
}
