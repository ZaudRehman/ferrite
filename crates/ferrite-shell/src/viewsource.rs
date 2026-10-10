//! View source fetcher. Re-requests with tab cookies. No capture path
//! exists (proxy tunnels TLS, wry exposes no source). GET replay only.
//! POST results are the known gap.

use std::sync::mpsc::Sender;

use serde::Serialize;
use uuid::Uuid;

/// Cap with truncation notice. Never stalls on huge bodies.
pub const MAX_BYTES: usize = 1_000_000;

/// Fetch result for one request.
#[derive(Debug, Clone, Serialize)]
pub struct VsReport {
    pub url: String,
    pub content_type: String,
    pub truncated: bool,
    pub escaped: String,
    pub error: String,
}

/// Entity escape in Rust. Page also uses textContent, so double safe.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Textual check. Fetch first, refuse garbled bytes after.
fn is_textual(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    ct.starts_with("text/")
        || ct.contains("xml")
        || ct.contains("json")
        || ct.contains("javascript")
        || ct.contains("html")
}

/// GET the URL with cookies, capped and escaped. Plain blocking ureq on a
/// spawned thread (no runtime needed) with a 10s global timeout.
pub fn fetch_source(url: String, cookies: Vec<(String, String)>) -> VsReport {
    use std::io::Read;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let err = |e: String| VsReport {
        url: url.clone(),
        content_type: String::new(),
        truncated: false,
        escaped: String::new(),
        error: e,
    };
    let cookie_header = cookies
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build(),
    );
    let mut req = agent.get(&url).header(
        "Accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    );
    if !cookie_header.is_empty() {
        req = req.header("Cookie", &cookie_header);
    }
    let resp = match req.call() {
        Ok(r) => r,
        Err(e) => return err(format!("request failed: {e}")),
    };
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !is_textual(&content_type) {
        return VsReport {
            url: url.clone(),
            content_type,
            truncated: false,
            escaped: String::new(),
            error: "not text: source view is for text responses".to_owned(),
        };
    }
    let mut body = Vec::new();
    match resp
        .into_body()
        .into_reader()
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut body)
    {
        Ok(_) => {}
        Err(e) => return err(format!("read failed: {e}")),
    }
    let truncated = body.len() > MAX_BYTES;
    body.truncate(MAX_BYTES);
    let text = String::from_utf8_lossy(&body).into_owned();
    VsReport {
        url: url.clone(),
        content_type,
        truncated,
        escaped: escape_html(&text),
        error: String::new(),
    }
}

/// Spawn the fetch off the winit loop. The report lands on `tx`.
pub fn spawn_viewsource(
    url: String,
    cookies: Vec<(String, String)>,
    tab_id: Uuid,
    tx: Sender<(Uuid, VsReport)>,
) {
    std::thread::spawn(move || {
        let rep = fetch_source(url, cookies);
        let _ = tx.send((tab_id, rep));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_neutralizes_markup() {
        assert_eq!(
            escape_html("<script>alert(\"x\")</script>&"),
            "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;&amp;"
        );
        assert_eq!(escape_html("plain"), "plain");
    }

    #[test]
    fn textual_gate() {
        assert!(is_textual("text/html; charset=utf-8"));
        assert!(is_textual("application/json"));
        assert!(is_textual("application/javascript"));
        assert!(!is_textual("image/png"));
        assert!(!is_textual("application/pdf"));
        assert!(!is_textual(""));
    }

    // Live fetch (ignored in CI: needs the network). Run manually to
    // verify the ureq plumbing end to end.
    #[test]
    #[ignore]
    fn live_fetch_example_com() {
        let rep = fetch_source("https://example.com".to_owned(), Vec::new());
        assert!(rep.error.is_empty(), "fetch failed: {}", rep.error);
        assert!(rep.escaped.contains("Example Domain"));
    }
}
