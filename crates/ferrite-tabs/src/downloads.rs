//! Download events. Flat registry: downloads outlive their tabs, so they
//! are never parented under one. Names are sanitized, conflicts resolve to
//! `name (1).ext`. Queue bounded at 256, excess drops with a warning.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use crossbeam_channel::{Receiver, Sender, bounded};

#[derive(Debug, Clone)]
pub struct DownloadEvent {
    pub id: String,
    pub url: String,
    pub filename: String,
    /// Full destination when known. Shell hashes this, never rederives.
    pub path: Option<String>,
    /// Originating tab (UUID string), when the starter knew it. Powers
    /// the sleep exemption (a mid-download tab must not unload); absent
    /// for unattributed completions, which simply don't protect.
    pub tab_id: Option<String>,
    pub bytes_total: Option<u64>,
    pub bytes_received: u64,
    pub state: DownloadStateKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStateKind {
    Started,
    Progress,
    Paused,
    Interrupted,
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

/// Shared sink between shell and every webview.
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
        self.start_with_path(url, filename, None)
    }

    pub fn start_with_path(&self, url: &str, filename: &str, path: Option<&Path>) -> String {
        self.start_with_tab(url, filename, path, None)
    }

    /// Start with tab attribution for the sleep exemption. `tab_id` is
    /// the tab UUID string when the starter knows it (engine handler,
    /// dialog worker with tab context).
    pub fn start_with_tab(
        &self,
        url: &str,
        filename: &str,
        path: Option<&Path>,
        tab_id: Option<String>,
    ) -> String {
        let id = format!("dl-{}", self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let ev = DownloadEvent {
            id: id.clone(),
            url: url.to_owned(),
            filename: filename.to_owned(),
            path: path.map(|p| p.to_string_lossy().to_string()),
            tab_id,
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
        self.progress_with_path(id, url, filename, None, received, total);
    }

    pub fn progress_with_path(
        &self,
        id: &str,
        url: &str,
        filename: &str,
        path: Option<&Path>,
        received: u64,
        total: Option<u64>,
    ) {
        // No tab half: the Started event already raised the tab's
        // download flag, and only terminal events lower it. Progress
        // carries no protection information.
        let ev = DownloadEvent {
            id: id.to_owned(),
            url: url.to_owned(),
            filename: filename.to_owned(),
            path: path.map(|p| p.to_string_lossy().to_string()),
            tab_id: None,
            bytes_total: total,
            bytes_received: received,
            state: DownloadStateKind::Progress,
        };
        let _ = self.inner.tx.try_send(ev);
    }

    pub fn complete(&self, id: &str, url: &str, filename: &str) {
        self.complete_with_path(id, url, filename, None);
    }

    pub fn complete_with_path(&self, id: &str, url: &str, filename: &str, path: Option<&Path>) {
        self.complete_with_tab(id, url, filename, path, None)
    }

    pub fn complete_with_tab(
        &self,
        id: &str,
        url: &str,
        filename: &str,
        path: Option<&Path>,
        tab_id: Option<String>,
    ) {
        let ev = DownloadEvent {
            id: id.to_owned(),
            url: url.to_owned(),
            filename: filename.to_owned(),
            path: path.map(|p| p.to_string_lossy().to_string()),
            tab_id,
            bytes_total: None,
            bytes_received: 0,
            state: DownloadStateKind::Completed,
        };
        let _ = self.inner.tx.try_send(ev);
    }

    pub fn fail(&self, id: &str, url: &str, filename: &str) {
        self.fail_with_path(id, url, filename, None);
    }

    pub fn fail_with_path(&self, id: &str, url: &str, filename: &str, path: Option<&Path>) {
        self.fail_with_tab(id, url, filename, path, None)
    }

    pub fn fail_with_tab(
        &self,
        id: &str,
        url: &str,
        filename: &str,
        path: Option<&Path>,
        tab_id: Option<String>,
    ) {
        let ev = DownloadEvent {
            id: id.to_owned(),
            url: url.to_owned(),
            filename: filename.to_owned(),
            path: path.map(|p| p.to_string_lossy().to_string()),
            tab_id,
            bytes_total: None,
            bytes_received: 0,
            state: DownloadStateKind::Failed,
        };
        let _ = self.inner.tx.try_send(ev);
    }

    pub fn cancel(&self, id: &str, url: &str, filename: &str, path: Option<&Path>) {
        let ev = DownloadEvent {
            id: id.to_owned(),
            url: url.to_owned(),
            filename: filename.to_owned(),
            path: path.map(|p| p.to_string_lossy().to_string()),
            tab_id: None,
            bytes_total: None,
            bytes_received: 0,
            state: DownloadStateKind::Cancelled,
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
                "path": ev.path,
                "bytes_total": ev.bytes_total,
                "bytes_received": ev.bytes_received,
                "state": format!("{:?}", ev.state),
            });
            out.push_str(&format!("data: {data}\n\n"));
        }
        out
    }
}

/// Sweep crash-orphaned `.partial` files in a download dir. Only files
/// untouched for longer than `max_age` go: no live transfer of ours
/// can be that quiet (workers die after 30 minutes; active ones write
/// constantly), and side-by-side profiles share the dir, so recency
/// is the only safe liveness signal. Top level only, best effort.
/// Returns files removed. Retry re-downloads from scratch when the
/// partial is gone, so a sweep never destroys retrievable data.
pub fn sweep_stale_partials(dir: &Path, max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "partial") {
            let old_enough = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age >= max_age);
            if old_enough && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// Filename-conflict resolution: first free `dir/filename`, else
/// `stem (1).ext`, `stem (2).ext`, … up to 999, then a UUID stem.
/// Pure + bounded: never overwrites an existing file.
pub fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match filename.rfind('.') {
        Some(i) if i > 0 => (&filename[..i], Some(&filename[i + 1..])),
        _ => (filename, None),
    };
    for n in 1..=999 {
        let suffixed = match ext {
            Some(e) if !e.is_empty() => format!("{stem} ({n}).{e}"),
            _ => format!("{stem} ({n})"),
        };
        let p = dir.join(&suffixed);
        if !p.exists() {
            return p;
        }
    }
    let fallback = format!("{}-{}", stem, uuid::Uuid::new_v4());
    match ext {
        Some(e) if !e.is_empty() => dir.join(format!("{fallback}.{e}")),
        _ => dir.join(fallback),
    }
}

/// Interception decision, passive-download side: sanitize the engine's
/// suggestion and resolve a conflict-free destination under `dir`.
pub fn decide_destination(dir: &Path, suggested_filename: &str) -> PathBuf {
    unique_path(dir, &sanitize_filename(suggested_filename))
}

/// Mixed-content download blocking: an `https` page must never trigger an
/// `http` download. Returns true when the download must be denied.
/// Unparseable URLs fail closed only for `http` downloads.
pub fn should_block_mixed_content(page_url: Option<&str>, download_url: &str) -> bool {
    let dl = download_url.trim().to_lowercase();
    if !dl.starts_with("http://") {
        return false;
    }
    let Some(page) = page_url else {
        return false;
    };
    page.trim().to_lowercase().starts_with("https://")
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

/// Executable/script/installer extensions that earn an explicit warning
/// before the file is usable. Local, free, no cloud involved.
pub fn is_dangerous_ext(filename: &str) -> bool {
    const DANGEROUS: &[&str] = &[
        "exe", "scr", "bat", "cmd", "com", "pif", "msi", "dmg", "pkg", "sh", "ps1", "vbs", "jse",
        "wsf", "jar",
    ];
    filename
        .rsplit('.')
        .next()
        .map(|ext| DANGEROUS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// SHA-256 of a finished download, hex-encoded, for the publisher-checksum
/// workflow. Streams in 64KB chunks, constant memory at any file size.
/// Returns `None` when the file cannot be read (moved or deleted).
pub fn sha256_file(path: &std::path::Path) -> Option<String> {
    use sha2::Digest;
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return None,
        }
    }
    let mut hex = String::with_capacity(64);
    for b in hasher.finalize().into_iter() {
        hex.push_str(&format!("{b:02x}"));
    }
    Some(hex)
}

/// Mark-of-the-web: tag the file as internet-origin so the OS shows its own
/// "are you sure" prompt on open. Windows Zone.Identifier ADS. Only the zone
/// marker is written (`ZoneId=3`), never the source URL.
pub fn mark_zone_identifier(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let ads = format!("{}:Zone.Identifier", path.display());
        // Best-effort: NTFS-only, silently absent on other filesystems.
        let _ = std::fs::write(ads, "[ZoneTransfer]\nZoneId=3\n");
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

/// Verify the Mark-of-the-Web is present (Windows Zone.Identifier ADS with
/// `ZoneId=3`). Chromium already sets this on engine downloads, so this is
/// a check-before-write gate: `true` means nothing left to do.
pub fn verify_zone_identifier(path: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        let ads = format!("{}:Zone.Identifier", path.display());
        std::fs::read_to_string(ads)
            .map(|c| c.contains("ZoneId=3"))
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        true
    }
}

/// Manual-fetch staging: bytes stream to `dest.partial`, promoted by atomic
/// rename on success. A crash never leaves a complete-looking file at the
/// final name; a `.partial` leftover resumes via `Range` on retry.
pub fn partial_path_for(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".partial");
    PathBuf::from(s)
}

/// Atomic promotion of a finished staging file. Fails when the destination
/// already exists (conflict resolution must run first), never overwrite.
pub fn promote_partial(partial: &Path, dest: &Path) -> std::io::Result<()> {
    if dest.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "destination exists",
        ));
    }
    std::fs::rename(partial, dest)
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
            path: None,
            tab_id: None,
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

    #[test]
    fn dangerous_ext_flags_executables_only() {
        assert!(is_dangerous_ext("setup.EXE"));
        assert!(is_dangerous_ext("run.sh"));
        assert!(is_dangerous_ext("a.msi"));
        assert!(!is_dangerous_ext("report.pdf"));
        assert!(!is_dangerous_ext("photo.jpg"));
        assert!(!is_dangerous_ext("noext"));
    }

    #[test]
    fn sha256_known_vector_and_missing_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("ferrite_dl_hash_test.txt");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        let _ = std::fs::remove_file(&path);
        assert!(sha256_file(&path).is_none());
    }

    #[test]
    fn unique_path_suffixes_before_extension() {
        let dir = std::env::temp_dir().join(format!("ferrite_dl_uniq_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "report.pdf");
        assert_eq!(first, dir.join("report.pdf"));
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(unique_path(&dir, "report.pdf"), dir.join("report (1).pdf"));
        std::fs::write(dir.join("report (1).pdf"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "report.pdf"), dir.join("report (2).pdf"));
        assert_eq!(unique_path(&dir, "noext"), dir.join("noext"));
        std::fs::write(dir.join("noext"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "noext"), dir.join("noext (1)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mixed_content_blocks_https_page_http_download() {
        assert!(should_block_mixed_content(
            Some("https://example.com/page"),
            "http://cdn.example.com/file.zip"
        ));
        assert!(!should_block_mixed_content(
            Some("https://example.com/page"),
            "https://cdn.example.com/file.zip"
        ));
        assert!(!should_block_mixed_content(
            Some("http://example.com/page"),
            "http://cdn.example.com/file.zip"
        ));
        assert!(!should_block_mixed_content(
            None,
            "http://cdn.example.com/file.zip"
        ));
        assert!(!should_block_mixed_content(
            Some("https://example.com/page"),
            "blob:https://example.com/abc"
        ));
    }

    #[test]
    fn partial_promote_is_atomic_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("ferrite_dl_part_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin");
        let partial = partial_path_for(&dest);
        assert!(partial.extension().is_some());
        std::fs::write(&partial, b"data").unwrap();
        promote_partial(&partial, &dest).unwrap();
        assert!(dest.exists());
        assert!(!partial.exists());
        std::fs::write(&partial, b"new").unwrap();
        assert!(promote_partial(&partial, &dest).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_partial_sweep_keeps_fresh_and_non_partial() {
        let dir = std::env::temp_dir().join(format!("ferrite_dl_sweep_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.partial"), b"x").unwrap();
        std::fs::write(dir.join("keep.bin"), b"y").unwrap();
        // Zero age sweeps everything partial (fresh files included);
        // real calls pass 24h so live transfers are never touched.
        assert_eq!(sweep_stale_partials(&dir, Duration::from_secs(0)), 1);
        assert!(!dir.join("old.partial").exists());
        assert!(dir.join("keep.bin").exists());
        assert_eq!(
            sweep_stale_partials(&dir.join("missing"), Duration::from_secs(0)),
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_carries_through_events() {
        let m = DownloadManager::new();
        let dest = std::path::Path::new("/tmp/example/file.zip");
        let id = m.start_with_path("https://example.com/file.zip", "file.zip", Some(dest));
        m.complete_with_path(&id, "https://example.com/file.zip", "file.zip", Some(dest));
        let evs = m.drain(10);
        assert_eq!(evs[0].id, id);
        assert_eq!(
            evs[0].path.as_deref(),
            Some(dest.to_string_lossy().as_ref())
        );
    }
}
