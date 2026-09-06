//! Download events: wry hooks report here, the shell drains per frame.
//!
//! Files land in the platform Downloads directory with sanitized names
//! (no path separators, control chars, leading dots, or reserved Windows
//! stems). Events are bounded (256) and dropped with a warning past that.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_channel::{Receiver, Sender, bounded};

#[derive(Debug, Clone)]
pub struct DownloadEvent {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub bytes_total: Option<u64>,
    pub bytes_received: u64,
    pub state: DownloadStateKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStateKind {
    Started,
    Progress,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug)]
struct DownloadInner {
    tx: Sender<DownloadEvent>,
    rx: Receiver<DownloadEvent>,
    next_id: AtomicUsize,
}

/// Cloneable download sink shared between the shell and every webview.
#[derive(Debug, Clone)]
pub struct DownloadManager {
    inner: Arc<DownloadInner>,
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadManager {
    pub fn new() -> Self {
        let (tx, rx) = bounded(256);
        Self {
            inner: Arc::new(DownloadInner {
                tx,
                rx,
                next_id: AtomicUsize::new(1),
            }),
        }
    }

    pub fn start(&self, url: &str, filename: &str) -> String {
        let id = format!("dl-{}", self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let ev = DownloadEvent {
            id: id.clone(),
            url: url.to_owned(),
            filename: filename.to_owned(),
            bytes_total: None,
            bytes_received: 0,
            state: DownloadStateKind::Started,
        };
        if self.inner.tx.try_send(ev).is_err() {
            tracing::warn!("download queue full: dropping start event");
        }
        id
    }

    pub fn progress(&self, id: &str, url: &str, filename: &str, received: u64, total: Option<u64>) {
        let ev = DownloadEvent {
            id: id.to_owned(),
            url: url.to_owned(),
            filename: filename.to_owned(),
            bytes_total: total,
            bytes_received: received,
            state: DownloadStateKind::Progress,
        };
        let _ = self.inner.tx.try_send(ev);
    }

    pub fn complete(&self, id: &str, url: &str, filename: &str) {
        let ev = DownloadEvent {
            id: id.to_owned(),
            url: url.to_owned(),
            filename: filename.to_owned(),
            bytes_total: None,
            bytes_received: 0,
            state: DownloadStateKind::Completed,
        };
        let _ = self.inner.tx.try_send(ev);
    }

    pub fn fail(&self, id: &str, url: &str, filename: &str) {
        let ev = DownloadEvent {
            id: id.to_owned(),
            url: url.to_owned(),
            filename: filename.to_owned(),
            bytes_total: None,
            bytes_received: 0,
            state: DownloadStateKind::Failed,
        };
        let _ = self.inner.tx.try_send(ev);
    }

    pub fn receiver(&self) -> Receiver<DownloadEvent> {
        self.inner.rx.clone()
    }

    /// Non-blocking drain for the per-frame pump (cap keeps one burst from
    /// starving the frame).
    pub fn drain(&self, cap: usize) -> Vec<DownloadEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.inner.rx.try_recv() {
            out.push(ev);
            if out.len() >= cap {
                break;
            }
        }
        out
    }

    /// SSE handler: writes `text/event-stream` to a `TcpStream` for `ferrite://downloads` polling.
    pub fn sse_payload(events: &[DownloadEvent]) -> String {
        let mut out = String::new();
        for ev in events {
            let data = serde_json::json!({
                "id": ev.id,
                "url": ev.url,
                "filename": ev.filename,
                "bytes_total": ev.bytes_total,
                "bytes_received": ev.bytes_received,
                "state": format!("{:?}", ev.state),
            });
            out.push_str(&format!("data: {data}\n\n"));
        }
        out
    }
}

/// Sanitize a server-suggested filename: no directories, control chars,
/// leading dots, or Windows reserved stems; capped at 128 chars.
pub fn sanitize_filename(raw: &str) -> String {
    // Take only the final path segment (kills `../../etc/passwd` and
    // `C:\Windows\...` shapes), decoding percent-escapes first.
    let decoded = percent_decode(raw);
    let base = decoded.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let mut name: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .collect();
    // Strip leading dots (hidden files) and trailing dots/spaces (Windows).
    while name.starts_with('.') {
        name.remove(0);
    }
    while name.ends_with(['.', ' ']) {
        name.pop();
    }
    // Truncate to 128 chars on a char boundary.
    if name.chars().count() > 128 {
        name = name.chars().take(128).collect();
    }
    // Windows reserved stems (case-insensitive, pre-extension).
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        name = format!("_{name}");
    }
    if name.is_empty() {
        name = "download".to_owned();
    }
    name
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.as_bytes().iter().peekable();
    while let Some(&b) = bytes.next() {
        if b == b'%' {
            let hi = bytes.next().copied().unwrap_or(b'0');
            let lo = bytes.next().copied().unwrap_or(b'0');
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push((hex(hi) << 4 | hex(lo)) as char);
        } else {
            out.push(b as char);
        }
    }
    out
}

/// Platform Downloads directory (created on demand), falling back to the
/// temp dir when unavailable.
pub fn default_download_dir() -> PathBuf {
    let base = std::env::var("USERPROFILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    // Only append `Downloads` when we started from a home dir.
    let dir = if base == std::env::temp_dir() {
        base
    } else {
        base.join("Downloads")
    };
    if std::fs::create_dir_all(&dir).is_ok() {
        dir
    } else {
        std::env::temp_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sse_format() {
        let m = DownloadManager::new();
        let id = m.start("https://example.com/file.zip", "file.zip");
        let ev = DownloadEvent {
            id,
            url: "https://example.com/file.zip".to_owned(),
            filename: "file.zip".to_owned(),
            bytes_total: Some(1000),
            bytes_received: 500,
            state: DownloadStateKind::Progress,
        };
        let s = DownloadManager::sse_payload(&[ev]);
        assert!(s.starts_with("data: "));
        assert!(s.contains("file.zip"));
    }

    #[test]
    fn sanitize_kills_traversal_and_reserved() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("C:\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_filename("..."), "download");
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("nul"), "_nul");
        assert_eq!(sanitize_filename("ok name.zip"), "ok name.zip");
        assert_eq!(sanitize_filename("%2e%2e%2fsecret"), "secret");
        assert!(sanitize_filename(&"x".repeat(500)).chars().count() <= 128);
    }

    #[test]
    fn fail_and_drain_roundtrip() {
        let m = DownloadManager::new();
        let id = m.start("https://example.com/a", "a");
        m.fail(&id, "https://example.com/a", "a");
        let evs = m.drain(10);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].state, DownloadStateKind::Failed);
    }
}
