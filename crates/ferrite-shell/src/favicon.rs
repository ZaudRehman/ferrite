//! Tab favicons: engine-reported icon URLs, Rust-fetched bytes.
//!
//! The page reports its `link[rel=icon]` href (absolute, engine-resolved)
//! over the existing content IPC. Rust fetches the bytes on a worker thread
//! through blocking `ureq` (same thread-bridge as view-source/cert), sniffs
//! raster magic, and stores a capped `data:` URL for the chrome push.
//! SVG is deliberately refused (script-capable markup in an image slot).
//! All validation is here so the IPC handler stays a thin gate.

use std::io::Read;

pub const FAVICON_MAX_URL_LEN: usize = 2048;
/// Cap on fetched bytes (icons are small; anything larger is not an icon).
pub const FAVICON_MAX_BYTES: usize = 64 * 1024;
/// Cap on the stored data URL so tab pushes stay small.
pub const FAVICON_MAX_DATA_URL: usize = 32 * 1024;
/// Host cache bound (LRU-ish eviction by insertion order).
pub const FAVICON_CACHE_CAP: usize = 200;

/// Worker result: the fetch this icon URL produced for this page.
pub struct FaviconDone {
    pub tab_id: uuid::Uuid,
    pub page_url: String,
    pub icon_url: String,
    pub data_url: Option<String>,
}

/// Accept only `http(s)` icon URLs within the length cap.
pub fn valid_icon_url(url: &str) -> bool {
    if url.is_empty() || url.len() > FAVICON_MAX_URL_LEN {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Raster magic sniff. Returns the mime suffix for the data URL, or `None`
/// for anything non-raster (SVG, HTML error pages, text).
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 4 {
        return None;
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return Some("image/x-icon");
    }
    if bytes.starts_with(&[0x42, 0x4D]) {
        return Some("image/bmp");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Build the data URL when the bytes sniff as raster and fit the push cap.
/// Returns `None` otherwise (caller keeps the letter avatar).
pub fn data_url_for(bytes: &[u8]) -> Option<String> {
    let mime = sniff_image(bytes)?;
    // base64 expands ~4/3; reject early when the URL could not fit.
    if bytes.len() * 4 / 3 + mime.len() + 24 > FAVICON_MAX_DATA_URL {
        return None;
    }
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    let url = format!("data:{mime};base64,{encoded}");
    if url.len() > FAVICON_MAX_DATA_URL {
        return None;
    }
    Some(url)
}

/// Blocking fetch on a worker thread. Never on the event loop.
fn fetch_icon_bytes(icon_url: &str) -> Option<Vec<u8>> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(5)))
            .build(),
    );
    let mut resp = agent.get(icon_url).call().ok()?;
    if resp.status() != 200 {
        return None;
    }
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Refuse SVG and non-images before reading the body.
    if ctype.contains("svg") || (!ctype.is_empty() && !ctype.contains("image")) {
        return None;
    }
    let mut body = Vec::new();
    resp.body_mut()
        .as_reader()
        .take((FAVICON_MAX_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .ok()?;
    if body.len() > FAVICON_MAX_BYTES {
        return None;
    }
    Some(body)
}

/// Spawn the worker. Reports back on `tx` with the page URL for staleness
/// checks (a navigation racing the fetch must not paint a stale icon).
pub fn spawn_favicon_fetch(
    tab_id: uuid::Uuid,
    page_url: String,
    icon_url: String,
    tx: std::sync::mpsc::Sender<FaviconDone>,
) {
    std::thread::spawn(move || {
        let data_url = fetch_icon_bytes(&icon_url)
            .as_deref()
            .and_then(data_url_for);
        let _ = tx.send(FaviconDone {
            tab_id,
            page_url,
            icon_url,
            data_url,
        });
    });
}

/// System accent follow (Linux GNOME only, best-effort). Reads
/// `org.gnome.desktop.interface accent-color` via `gsettings` and maps the
/// named color to a hex accent. `FERRITE_ACCENT=#rrggbb` overrides (also the
/// test seam). `None` keeps the pack accent.
pub fn system_accent_hex() -> Option<String> {
    if let Ok(raw) = std::env::var("FERRITE_ACCENT") {
        let t = raw.trim();
        if t.len() == 7 && t.starts_with('#') && t[1..].chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(t.to_ascii_lowercase());
        }
    }
    #[cfg(target_os = "linux")]
    {
        let out = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "accent-color"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let name = String::from_utf8_lossy(&out.stdout)
            .trim()
            .trim_matches('\'')
            .to_owned();
        match name.as_str() {
            "blue" => Some("#3584e4".to_owned()),
            "teal" => Some("#2190a4".to_owned()),
            "green" => Some("#3a9104".to_owned()),
            "yellow" => Some("#e5a50a".to_owned()),
            "orange" => Some("#c64600".to_owned()),
            "red" => Some("#e01b24".to_owned()),
            "pink" => Some("#d56199".to_owned()),
            "purple" => Some("#9141ac".to_owned()),
            "slate" => Some("#6e6c6e".to_owned()),
            _ => None,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_url_gate() {
        assert!(valid_icon_url("https://example.com/i.png"));
        assert!(valid_icon_url("http://example.com/favicon.ico"));
        assert!(!valid_icon_url(""));
        assert!(!valid_icon_url("ferrite://newtab"));
        assert!(!valid_icon_url("data:image/png;base64,xx"));
        assert!(!valid_icon_url("javascript:alert(1)"));
        assert!(!valid_icon_url(&"x".repeat(FAVICON_MAX_URL_LEN + 1)));
    }

    #[test]
    fn sniff_accepts_raster_refuses_markup() {
        assert_eq!(
            sniff_image(&[0x89, b'P', b'N', b'G', 0, 0]),
            Some("image/png")
        );
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a..."), Some("image/gif"));
        assert_eq!(
            sniff_image(&[0x00, 0x00, 0x01, 0x00, 1, 0]),
            Some("image/x-icon")
        );
        assert!(sniff_image(b"<svg").is_none());
        assert!(sniff_image(b"<!DO").is_none());
        assert!(sniff_image(&[]).is_none());
        assert!(sniff_image(&[1, 2, 3]).is_none());
    }

    #[test]
    fn data_url_caps_push_size() {
        let png = vec![0x89, b'P', b'N', b'G', 0, 1, 2, 3];
        let url = data_url_for(&png).expect("small png");
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(data_url_for(&vec![0u8; FAVICON_MAX_BYTES]).is_none());
        assert!(data_url_for(b"<svg></svg>").is_none());
    }

    #[test]
    fn env_accent_override_is_validated() {
        unsafe { std::env::set_var("FERRITE_ACCENT", "#FF8C00") };
        assert_eq!(system_accent_hex(), Some("#ff8c00".to_owned()));
        unsafe { std::env::set_var("FERRITE_ACCENT", "red") };
        // On non-Linux this is None; on Linux gsettings may answer, but a
        // bare name is never accepted as hex here.
        let got = system_accent_hex();
        assert!(got.is_none() || got.unwrap().starts_with('#'));
        unsafe { std::env::remove_var("FERRITE_ACCENT") };
    }
}
