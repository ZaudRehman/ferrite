//! Viewer sessions for `ferrite://pdf/`.
//!
//! Bytes live in RAM only, inside [`zeroize::Zeroizing`] buffers, and are
//! handed to the isolated renderer child exactly once per spawn. The shell
//! never parses PDF itself (that is the child's whole job); it only
//! classifies URLs, fetches remote bytes with caps, and translates viewer
//! posts into child RPCs. Passwords are never logged, never persisted,
//! and die with the session. Private tabs change nothing here because
//! nothing here ever touches the disk.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::Sender;
use std::time::Instant;

use uuid::Uuid;
use zeroize::Zeroizing;

use ferrite_archive::pdf::IsolatedPdfRenderer;

/// Session cap: each session can hold up to 64 MB in RAM plus a renderer
/// child. Eight is the same order as the tab discard budget; LRU eviction
/// keeps a foundry of viewers from eating the machine.
pub const MAX_SESSIONS: usize = 8;
pub const MAX_PDF_BYTES: usize = 64 * 1024 * 1024;

/// Navigation classification lives in [`ferrite_tabs::pdf`] (shared
/// with the engine link intercept); the shell adds file decoding and
/// display titles here.
/// Percent-decode a `file://` path (handles `%20` and friends). Malformed
/// escapes fail the whole decode: a half-decoded path is how traversal
/// bugs are born.
pub fn decode_file_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost/").unwrap_or(rest);
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Display title from a URL or path: last segment, de-truncated to a
/// sane length. Never trusted for anything but labels.
pub fn display_title(source: &str) -> String {
    let seg = source
        .split(['?', '#'])
        .next()
        .unwrap_or(source)
        .rsplit('/')
        .next()
        .unwrap_or(source);
    let seg = seg.trim();
    if seg.is_empty() {
        return "Document".to_owned();
    }
    const MAX: usize = 96;
    if seg.len() > MAX {
        format!("{}…", &seg[..MAX])
    } else {
        seg.to_owned()
    }
}

/// Completed remote fetch, back on the event loop.
pub struct PdfFetchDone {
    pub tab_id: Uuid,
    pub url: String,
    pub title: String,
    pub result: Result<Vec<u8>, String>,
}

/// Fetch a remote PDF on a worker thread. Caps: 64 MB total (declared or
/// actual), 10s connect, 120s global. Content-Type `text/*` fails fast
/// (usually an error page wearing a `.pdf` suffix); the `%PDF-` magic is
/// the final gate. Direct fetch, like the engine would do on navigation:
/// no warmer privacy claim than status quo, no colder either.
pub fn spawn_pdf_fetch(url: String, title: String, tab_id: Uuid, tx: Sender<PdfFetchDone>) {
    std::thread::spawn(move || {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let result = fetch_pdf_bytes(&url);
        let _ = tx.send(PdfFetchDone {
            tab_id,
            url,
            title,
            result,
        });
    });
}

fn fetch_pdf_bytes(url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(10)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(10)))
        .timeout_recv_body(Some(std::time::Duration::from_secs(120)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    if resp.status() != ureq::http::StatusCode::OK {
        return Err(format!("download status {}", resp.status()));
    }
    if let Some(ct) = resp
        .headers()
        .get(ureq::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        let ct = ct.to_ascii_lowercase();
        if ct.starts_with("text/") {
            return Err("server sent a text page, not a PDF".to_owned());
        }
    }
    let total: Option<u64> = resp
        .headers()
        .get(ureq::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());
    if total.is_some_and(|t| t > MAX_PDF_BYTES as u64) {
        return Err("PDF exceeds the 64 MB viewer cap".to_owned());
    }
    let reader = resp.into_body().into_reader();
    let mut limited = reader.take(MAX_PDF_BYTES as u64 + 1);
    let mut buf = Vec::new();
    limited.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    if buf.len() > MAX_PDF_BYTES {
        return Err("PDF exceeds the 64 MB viewer cap".to_owned());
    }
    if buf.len() < 5 || &buf[..5] != b"%PDF-" {
        return Err("not a PDF file".to_owned());
    }
    Ok(buf)
}

/// One open document: bytes plus a renderer child that owns the parse.
/// `bytes`/`password` are re-sent on child respawn and dropped (zeroized)
/// on close. Nothing here is serialized, logged, or written anywhere.
pub struct PdfDocSession {
    pub tab: Uuid,
    pub title: String,
    pub source: String,
    bytes: Zeroizing<Vec<u8>>,
    password: Option<Zeroizing<String>>,
    renderer: IsolatedPdfRenderer,
    opened: bool,
    pub pages: usize,
    pub locked: bool,
    last_used: Instant,
}

impl PdfDocSession {
    /// Was the renderer child lost (crash, kill)? The next op re-opens
    /// transparently from retained bytes.
    fn ensure_open_blocking(&mut self) -> Result<(), String> {
        if self.opened && self.renderer.is_alive() {
            return Ok(());
        }
        self.renderer.shutdown();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        // Re-open replays the stored password: the user is never asked
        // twice for one session.
        let pwd = self.password.as_deref().map(|s| s.as_str()).unwrap_or("");
        let pwd_opt = if pwd.is_empty() { None } else { Some(pwd) };
        let bytes = self.bytes.clone();
        let (pages, locked) = rt
            .block_on(self.renderer.open_document(&bytes, pwd_opt))
            .map_err(|e| e.to_string())?;
        self.opened = !locked;
        self.pages = pages;
        self.locked = locked;
        Ok(())
    }
}

pub struct PdfSessions {
    map: HashMap<Uuid, PdfDocSession>,
    order: VecDeque<Uuid>,
}

impl PdfSessions {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Open bytes already in hand. Returns (session id, pages, locked).
    /// A locked document keeps its session: the viewer prompts, then
    /// `unlock` retries with the password.
    pub fn open_bytes(
        &mut self,
        tab: Uuid,
        title: String,
        source: String,
        bytes: Vec<u8>,
        password: Option<String>,
    ) -> Result<(Uuid, usize, bool), String> {
        if bytes.is_empty() || bytes.len() > MAX_PDF_BYTES {
            return Err("PDF length out of bounds".to_owned());
        }
        if bytes.len() < 5 || &bytes[..5] != b"%PDF-" {
            return Err("not a PDF file".to_owned());
        }
        if let Some(p) = &password
            && p.len() > 1024
        {
            return Err("password too long".to_owned());
        }
        while self.len() >= MAX_SESSIONS {
            self.evict_oldest();
        }
        let mut renderer =
            IsolatedPdfRenderer::spawn().map_err(|e| format!("renderer unavailable: {e}"))?;
        // Open inline: a missing renderer binary fails here, loudly, at
        // open time: never as a spinner in the viewer.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let pwd_opt = password.as_deref();
        let (pages, locked) = rt
            .block_on(renderer.open_document(&bytes, pwd_opt))
            .map_err(|e| e.to_string())?;
        let id = Uuid::new_v4();
        self.order.push_back(id);
        self.map.insert(
            id,
            PdfDocSession {
                tab,
                title,
                source,
                bytes: Zeroizing::new(bytes),
                password: password.map(Zeroizing::new),
                renderer,
                opened: !locked,
                pages,
                locked,
                last_used: Instant::now(),
            },
        );
        Ok((id, pages, locked))
    }

    /// Retry with a password. Wrong passwords keep `locked`: the viewer
    /// prompts again, the bytes never move.
    pub fn unlock(&mut self, id: Uuid, password: String) -> Result<(usize, bool), String> {
        if password.len() > 1024 {
            return Err("password too long".to_owned());
        }
        let s = self.get_mut(id)?;
        s.renderer.shutdown();
        s.opened = false;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let bytes = s.bytes.clone();
        let (pages, locked) = rt
            .block_on(s.renderer.open_document(&bytes, Some(&password)))
            .map_err(|e| e.to_string())?;
        if !locked {
            s.password = Some(Zeroizing::new(password));
            s.opened = true;
            s.pages = pages;
        }
        s.locked = locked;
        s.last_used = Instant::now();
        Ok((pages, locked))
    }

    pub fn get_mut(&mut self, id: Uuid) -> Result<&mut PdfDocSession, String> {
        let s = self
            .map
            .get_mut(&id)
            .ok_or_else(|| "unknown document".to_owned())?;
        s.last_used = Instant::now();
        Ok(s)
    }

    /// Touch-free lookup for authorization checks: reading must not
    /// refresh LRU (a probe loop would otherwise pin a session alive).
    pub fn get(&self, id: Uuid) -> Option<&PdfDocSession> {
        self.map.get(&id)
    }

    /// Drop a session: child killed, bytes and password zeroized on drop.
    /// Returns true when something was actually closed.
    pub fn close(&mut self, id: Uuid) -> bool {
        if let Some(mut s) = self.map.remove(&id) {
            s.renderer.shutdown();
            self.order.retain(|x| *x != id);
            true
        } else {
            false
        }
    }

    /// Drop every session owned by a tab. Called on tab close and when a
    /// tab navigates away from the viewer: no orphan children, no orphan
    /// bytes.
    pub fn close_tab_sessions(&mut self, tab: Uuid) {
        if self.is_empty() {
            return;
        }
        let ids: Vec<Uuid> = self
            .map
            .iter()
            .filter(|(_, s)| s.tab == tab)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.close(id);
        }
    }

    fn evict_oldest(&mut self) {
        let mut oldest: Option<(Uuid, Instant)> = None;
        for (id, s) in &self.map {
            if oldest.is_none_or(|(_, t)| s.last_used < t) {
                oldest = Some((*id, s.last_used));
            }
        }
        if let Some((id, _)) = oldest {
            self.close(id);
        }
    }

    /// Render one page to a PNG data URL, with true dimensions. The
    /// child hands back raw RGBA; PNG-encoding here keeps the viewer to
    /// a single `<img>` with no canvas marshalling and no dimension
    /// guessing (the viewer also falls back to natural size).
    pub fn render_page_png(
        &mut self,
        id: Uuid,
        page: usize,
        scale: f32,
    ) -> Result<(u32, u32, String), String> {
        use base64::Engine as _;
        let s = self.get_mut(id)?;
        s.ensure_open_blocking()?;
        if !s.opened {
            return Err("document is locked".to_owned());
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let fut = s.renderer.page_bitmap(page, scale);
        let (w, h, rgba) = rt.block_on(fut).map_err(|e| e.to_string())?;
        if rgba.len() != w as usize * h as usize * 4 {
            return Err("renderer returned a corrupt bitmap".to_owned());
        }
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().map_err(|e| e.to_string())?;
            writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        Ok((w, h, b64))
    }

    /// Shared runtime builder for child round trips.
    fn runtime() -> Result<tokio::runtime::Runtime, String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
    }

    /// Lock-state gate shared by every op below.
    fn ready(s: &mut PdfDocSession) -> Result<(), String> {
        s.ensure_open_blocking()?;
        if !s.opened {
            return Err("document is locked".to_owned());
        }
        Ok(())
    }

    pub fn doc_info(&mut self, id: Uuid) -> Result<serde_json::Value, String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.doc_info();
        rt.block_on(fut).map_err(|e| e.to_string())
    }

    pub fn search_all(
        &mut self,
        id: Uuid,
        query: String,
        case: bool,
    ) -> Result<serde_json::Value, String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.search_all(query, case);
        rt.block_on(fut).map_err(|e| e.to_string())
    }

    pub fn page_text(&mut self, id: Uuid, page: usize) -> Result<serde_json::Value, String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.page_text(page);
        rt.block_on(fut).map_err(|e| e.to_string())
    }

    pub fn page_links(&mut self, id: Uuid, page: usize) -> Result<serde_json::Value, String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.page_links(page);
        rt.block_on(fut).map_err(|e| e.to_string())
    }

    pub fn doc_outline(&mut self, id: Uuid) -> Result<serde_json::Value, String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.doc_outline();
        rt.block_on(fut).map_err(|e| e.to_string())
    }

    pub fn doc_forms(&mut self, id: Uuid) -> Result<serde_json::Value, String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.doc_forms();
        rt.block_on(fut).map_err(|e| e.to_string())
    }

    pub fn fill_text(&mut self, id: Uuid, index: usize, value: String) -> Result<(), String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.fill_text(index, value);
        rt.block_on(fut).map_err(|e| e.to_string())
    }

    pub fn fill_check(&mut self, id: Uuid, index: usize, checked: bool) -> Result<(), String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.fill_check(index, checked);
        rt.block_on(fut).map_err(|e| e.to_string())
    }

    pub fn fill_radio(&mut self, id: Uuid, index: usize) -> Result<(), String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.fill_radio(index);
        rt.block_on(fut).map_err(|e| e.to_string())
    }

    pub fn save_bytes(&mut self, id: Uuid) -> Result<Vec<u8>, String> {
        let rt = Self::runtime()?;
        let s = self.get_mut(id)?;
        Self::ready(s)?;
        let fut = s.renderer.save_document();
        rt.block_on(fut).map_err(|e| e.to_string())
    }
}

impl Default for PdfSessions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_file_url_handles_escapes() {
        assert_eq!(
            decode_file_url("file:///C:/my%20docs/a.pdf"),
            Some("/C:/my docs/a.pdf".to_owned())
        );
        assert_eq!(decode_file_url("file:///x.pdf"), Some("/x.pdf".to_owned()));
        // Truncated escape fails the whole decode, never half-decodes.
        assert_eq!(decode_file_url("file:///x%2.pdf"), None);
        assert_eq!(decode_file_url("https://h/x.pdf"), None);
    }

    #[test]
    fn display_title_takes_last_segment() {
        assert_eq!(
            display_title("https://example.com/a/b/report.pdf?dl=1"),
            "report.pdf"
        );
        assert_eq!(display_title(""), "Document");
    }

    #[test]
    fn open_rejects_garbage_before_touching_renderer() {
        let mut sessions = PdfSessions::new();
        assert!(
            sessions
                .open_bytes(Uuid::new_v4(), "t".into(), "s".into(), vec![], None)
                .is_err()
        );
        assert!(
            sessions
                .open_bytes(
                    Uuid::new_v4(),
                    "t".into(),
                    "s".into(),
                    b"GIF89a...".to_vec(),
                    None
                )
                .is_err()
        );
        assert!(sessions.is_empty());
    }

    #[test]
    fn close_unknown_is_false() {
        let mut sessions = PdfSessions::new();
        assert!(!sessions.close(Uuid::new_v4()));
    }
}
