//! Native file dialogs off the UI thread, plus filename sanitization.
//! Pickers run on a worker and report over a channel drained in tick.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum PickTag {
    WelcomeBookmarks,
    WelcomePasswords,
    ExportSettings,
    ImportSettings,
    BookmarkExport,
    HistoryExport,
    BookmarkKind { kind: String },
    PasswordExport,
    ExportAll,
    BackupExport,
    BackupImport,
}

/// None path means cancelled.
#[derive(Debug, Clone)]
pub struct PickDone {
    pub tag: PickTag,
    pub tab_id: Uuid,
    pub path: Option<PathBuf>,
}

pub struct PickSpec {
    pub save: bool,
    pub filters: Vec<(String, Vec<String>)>,
    pub filename: Option<String>,
}

impl PickSpec {
    pub fn open(filters: Vec<(&str, &[&str])>) -> Self {
        Self {
            save: false,
            filters: filters
                .into_iter()
                .map(|(n, e)| (n.to_owned(), e.iter().map(|s| s.to_string()).collect()))
                .collect(),
            filename: None,
        }
    }

    pub fn save(filename: &str) -> Self {
        Self {
            save: true,
            filters: Vec::new(),
            filename: Some(filename.to_owned()),
        }
    }
}

/// Show the dialog on a worker thread. The picked path (if any) lands on
/// `tx` for the tick drain. `start_dir` seeds the dialog. The drain
/// persists the picked parent back to settings.
pub fn spawn_pick(
    spec: PickSpec,
    tag: PickTag,
    tab_id: Uuid,
    start_dir: Option<PathBuf>,
    tx: Sender<PickDone>,
) {
    std::thread::spawn(move || {
        let mut dlg = rfd::FileDialog::new();
        if let Some(dir) = start_dir {
            dlg = dlg.set_directory(dir);
        }
        for (name, exts) in &spec.filters {
            dlg = dlg.add_filter(name, exts);
        }
        let path = if spec.save {
            dlg.set_file_name(spec.filename.unwrap_or_else(|| "download".to_owned()))
                .save_file()
        } else {
            dlg.pick_file()
        };
        let _ = tx.send(PickDone { tag, tab_id, path });
    });
}

/// Sanitize a page- or header-suggested filename for a save dialog.
/// Strip traversal, kill controls and Windows-invalid glyphs, trim trailing
/// dots, cap length. Double extensions stay visible, renaming would lie.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if base.is_empty() || base == "." || base == ".." {
        return "download".to_owned();
    }
    let mut clean: String = base
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Trailing dots and spaces are stripped by Windows (a "file." lands as
    // "file"). Trim here so the dialog shows the real name.
    while clean.ends_with(['.', ' ']) {
        clean.pop();
    }
    if clean.is_empty() {
        return "download".to_owned();
    }
    // Cap length, keeping the tail (extension side) intact.
    const MAX: usize = 180;
    if clean.chars().count() > MAX {
        clean = clean.chars().skip(clean.chars().count() - MAX).collect();
    }
    clean
}

/// Suggested filename from a `Content-Disposition` value (`filename=`
/// quoted or `filename*=UTF-8`). Else the last URL path segment.
/// Returns the raw candidate. Callers sanitize before display.
pub fn attachment_filename(content_disposition: &str, url: &str) -> String {
    for part in content_disposition.split(';') {
        let part = part.trim();
        if let Some(rest) = part
            .strip_prefix("filename*=")
            .or_else(|| part.strip_prefix("Filename*="))
        {
            // RFC 5987: charset'lang'value: only UTF-8 honored, %XX
            // decoded to bytes first (multibyte stays intact), then cut.
            if let Some((charset, value)) = rest
                .split_once('\'')
                .and_then(|(c, r)| r.split_once('\'').map(|(_, v)| (c, v)))
                && charset.eq_ignore_ascii_case("utf-8")
            {
                let mut raw: Vec<u8> = Vec::with_capacity(value.len());
                let mut bytes = value.as_bytes().iter().peekable();
                while let Some(&b) = bytes.next() {
                    if b == b'%' {
                        let h: Vec<u8> = bytes.by_ref().take(2).copied().collect();
                        if h.len() == 2
                            && let Ok(hex) = std::str::from_utf8(&h)
                            && let Ok(byte) = u8::from_str_radix(hex, 16)
                        {
                            raw.push(byte);
                            continue;
                        }
                        raw.push(b'%');
                        raw.extend(h);
                    } else if b == b'+' {
                        raw.push(b' ');
                    } else {
                        raw.push(b);
                    }
                }
                let out = String::from_utf8_lossy(&raw).into_owned();
                if !out.trim().is_empty() {
                    return out;
                }
            }
        }
        if let Some(name) = part
            .strip_prefix("filename=")
            .or_else(|| part.strip_prefix("Filename="))
        {
            let name = name.trim().trim_matches('"').trim();
            if !name.is_empty() {
                return name.to_owned();
            }
        }
    }
    // Fallback: URL path tail.
    url.split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_owned()
}

/// Sidecar schema version. A mismatch discards the partial rather than
/// risking an append against unknown metadata.
const RESUME_META_VERSION: u8 = 1;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ResumeMeta {
    version: u8,
    /// SHA-256 of the URL, not the URL itself: query strings can contain
    /// bearer tokens and the sidecar sits beside the download.
    url_tag: String,
    /// Strong ETag or Last-Modified, sent back as `If-Range`.
    validator: String,
    /// Full entity length when the origin disclosed it.
    total: Option<u64>,
    /// Bytes known to be durable when this sidecar was last written.
    partial_len: u64,
}

/// Checkpoint at most this many un-checkpointed bytes. A crash can lose
/// this window; every checkpoint flushes and `sync_data`s before the
/// sidecar records the durable length, so recovery never appends a byte
/// that was not synced. Tests use a tiny window so the failure path can
/// exercise the same code without writing 64 MiB.
#[cfg(not(test))]
const CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(test)]
const CHECKPOINT_BYTES: u64 = 16 * 1024;

fn checkpoint_due(received: u64, durable: u64) -> bool {
    received.saturating_sub(durable) >= CHECKPOINT_BYTES
}

/// A page URL may be used as `Referer` only for http(s) documents, without
/// its fragment (referrers never carry one). Anything else sends no header.
fn referer_value(page: &str) -> Option<String> {
    let mut url = url::Url::parse(page).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    // Referrers never carry a fragment or URL credentials. A page URL with
    // embedded userinfo must not leak it to the download host.
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Some(url.to_string())
}

fn header_value_ok(value: &str) -> bool {
    !value.is_empty() && !value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentRange {
    start: u64,
    end: u64,
    total: Option<u64>,
}

/// Parse `Content-Range: bytes <start>-<end>/<total|*>`. A total of `*`
/// stays `None`; a finite total must contain the whole range.
fn parse_content_range(value: &str) -> Option<ContentRange> {
    let (unit, rest) = value.trim().split_once(char::is_whitespace)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let rest = rest.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        n => {
            let n = n.parse::<u64>().ok()?;
            if end >= n {
                return None;
            }
            Some(n)
        }
    };
    Some(ContentRange { start, end, total })
}

/// Prefer a strong ETag, then Last-Modified. Weak ETags cannot be used in
/// `If-Range`; a malformed/unquoted ETag falls back rather than being sent.
fn choose_validator(etag: Option<&str>, last_modified: Option<&str>) -> Option<String> {
    if let Some(tag) = etag.map(str::trim).filter(|t| !t.is_empty())
        && tag.len() >= 2
        && tag.starts_with('"')
        && tag.ends_with('"')
    {
        return Some(tag.to_owned());
    }
    last_modified
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Validate a `206` before appending. Returns the finite total.
/// Any error means "discard the partial and restart from zero"; it must
/// never mean "append anyway".
fn check_resume_response(
    resume_from: u64,
    meta: &ResumeMeta,
    content_range: Option<ContentRange>,
    content_length: Option<u64>,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<u64, &'static str> {
    let cr = content_range.ok_or("missing content-range")?;
    if cr.start != resume_from {
        return Err("content-range start mismatch");
    }
    let validator_matches = |v: &str| {
        let v = v.trim();
        !v.is_empty()
            && (etag.is_some_and(|e| e.trim() == v)
                || last_modified.is_some_and(|lm| lm.trim() == v))
    };
    if !validator_matches(&meta.validator) {
        return Err("validator mismatch");
    }
    let total = match (meta.total, cr.total) {
        (Some(a), Some(b)) if a != b => return Err("total mismatch"),
        (Some(a), _) => a,
        (None, Some(b)) => b,
        (None, None) => return Err("unknown total"),
    };
    let expected = cr
        .end
        .checked_sub(cr.start)
        .and_then(|n| n.checked_add(1))
        .ok_or("range overflow")?;
    // A missing Content-Length means ureq decompressed the body or the
    // origin used chunked framing. Either way the prefix no longer maps
    // to representation byte offsets, so resume must restart.
    let content_length = content_length.ok_or("missing content-length")?;
    if content_length != expected {
        return Err("content-length mismatch");
    }
    if resume_from >= total || cr.end >= total {
        return Err("range beyond total");
    }
    Ok(total)
}

fn resume_meta_path(partial: &Path) -> PathBuf {
    ferrite_tabs::partial_meta_path_for(partial)
}

fn url_tag(url: &str) -> String {
    let mut hex = String::with_capacity(64);
    for byte in sha2::Sha256::digest(url.as_bytes()) {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn load_resume_meta(partial: &Path, url: &str) -> Option<ResumeMeta> {
    let path = resume_meta_path(partial);
    let len = std::fs::metadata(&path).ok()?.len();
    if len > 4096 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let meta: ResumeMeta = serde_json::from_slice(&bytes).ok()?;
    if meta.version != RESUME_META_VERSION
        || meta.url_tag != url_tag(url)
        || meta.validator.trim().is_empty()
        || meta.validator.len() > 1024
        || meta
            .validator
            .bytes()
            .any(|b| b == b'\r' || b == b'\n' || b == 0)
    {
        return None;
    }
    Some(meta)
}

fn store_resume_meta(partial: &Path, meta: &ResumeMeta) -> std::io::Result<()> {
    use std::io::Write as _;
    let bytes = serde_json::to_vec(meta).map_err(std::io::Error::other)?;
    let path = resume_meta_path(partial);
    // Replace the sidecar atomically: a crash mid-write must leave either
    // the old durable length or the new one, never truncated JSON that
    // would force the next attempt to discard a perfectly good prefix.
    let tmp = path.with_extension(format!("ferrite-resume-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_data()?;
        std::fs::rename(&tmp, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn clear_resume_meta(partial: &Path) {
    let _ = std::fs::remove_file(resume_meta_path(partial));
}

/// Drop both the bytes and their validator: used whenever a resume is
/// refused, stale, or unverifiable.
fn clear_partial(partial: &Path) {
    let _ = std::fs::remove_file(partial);
    clear_resume_meta(partial);
}

/// Record the durable prefix after a flushed and synced failure so a later
/// attempt can resume. A failed flush/sync leaves the old length in place,
/// which makes the next attempt discard rather than append to unknown bytes.
fn checkpoint_partial(partial: &Path, meta: &mut Option<ResumeMeta>, received: u64) {
    if let Some(meta) = meta.as_mut() {
        meta.partial_len = received;
        let _ = store_resume_meta(partial, meta);
    }
}

/// GET a URL to disk through the save dialog, with cookies and progress.
/// Worker thread only. Bytes stage to `.partial`, promote by rename, one
/// resume retry. Failures go back over `notice_tx`.
/// Ask-divert refetch with a save dialog, unless `force_dir` is set: then
/// the dialog is skipped and the file lands sanitized and de-duplicated
/// inside it (guest temp dir). The dir is created best-effort; failure to
/// create fails the download visibly, never silently elsewhere.
/// Fetch a URL to disk on a worker (ask-every-time refetch path).
/// Ten params, each used; a struct would just move the arity.
#[allow(clippy::too_many_arguments)]
pub fn spawn_fetch_save(
    url: String,
    filename: String,
    cookies: Vec<(String, String)>,
    page_url: Option<String>,
    user_agent: Option<String>,
    manager: ferrite_tabs::DownloadManager,
    start_dir: Option<PathBuf>,
    force_dir: Option<PathBuf>,
    notice_tx: Sender<String>,
    tab_id: Option<Uuid>,
) {
    let tab_id = tab_id.map(|t| t.to_string());
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let _ = rustls::crypto::ring::default_provider().install_default();
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_connect(Some(std::time::Duration::from_secs(30)))
                .timeout_global(Some(std::time::Duration::from_secs(1800)))
                .build(),
        );
        // The engine already applied the server's filename. Sanitize the
        // suggestion (page-influenced, traversal-checked) for the dialog.
        let suggested = sanitize_filename(&filename);
        let dest = match force_dir {
            Some(dir) => {
                if std::fs::create_dir_all(&dir).is_err() {
                    let _ = notice_tx
                        .send("Couldn't save file: temporary folder unavailable.".to_owned());
                    return;
                }
                ferrite_tabs::decide_destination(&dir, &suggested)
            }
            None => {
                let mut dlg = rfd::FileDialog::new().set_file_name(suggested.clone());
                if let Some(dir) = start_dir {
                    dlg = dlg.set_directory(dir);
                }
                let Some(dest) = dlg.save_file() else {
                    return;
                };
                dest
            }
        };
        let dest_filename = dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&suggested)
            .to_owned();
        let id = manager.start_with_tab(&url, &dest_filename, Some(&dest), tab_id.clone());
        let partial = ferrite_tabs::partial_path_for(&dest);
        let mut resume_from: u64 = std::fs::metadata(&partial)
            .ok()
            .map(|m| m.len())
            .unwrap_or(0);
        // A partial is resumable only with a sidecar that matches this URL
        // and a still-plausible total. If the file is longer than the
        // sidecar's durable length, the extra tail was written after the
        // last sync (or during a crash): truncate it back to the verified
        // durable prefix and resume from there. Never append unverified
        // bytes. A sidecar claiming more bytes than the file has, or a
        // zero-length durable prefix, is discarded.
        let mut resume_meta = if resume_from > 0 {
            match load_resume_meta(&partial, &url) {
                Some(meta)
                    if meta.total.is_none_or(|t| meta.partial_len < t)
                        && meta.partial_len > 0
                        && meta.partial_len <= resume_from =>
                {
                    if meta.partial_len < resume_from {
                        match std::fs::OpenOptions::new().write(true).open(&partial) {
                            Ok(file) if file.set_len(meta.partial_len).is_ok() => {
                                tracing::info!(
                                    from = resume_from,
                                    durable = meta.partial_len,
                                    "truncating unverified partial tail"
                                );
                                resume_from = meta.partial_len;
                                Some(meta)
                            }
                            _ => {
                                clear_partial(&partial);
                                resume_from = 0;
                                None
                            }
                        }
                    } else {
                        Some(meta)
                    }
                }
                _ => {
                    clear_partial(&partial);
                    resume_from = 0;
                    None
                }
            }
        } else {
            clear_partial(&partial);
            None
        };
        let fail_clean = |msg: String| {
            clear_partial(&partial);
            manager.fail_with_tab(&id, &url, &dest_filename, Some(&dest), tab_id.clone());
            let _ = notice_tx.send(msg);
        };
        let fail_keep_partial = |msg: String| {
            manager.fail_with_tab(&id, &url, &dest_filename, Some(&dest), tab_id.clone());
            let _ = notice_tx.send(msg);
        };
        let cookie_header = if cookies.is_empty() {
            None
        } else {
            Some(
                cookies
                    .iter()
                    .map(|(n, v)| format!("{n}={v}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        };
        // The engine normally owns this download; the ask-divert refetch
        // must carry the same page context or authenticated/anti-hotlink
        // servers reject it. Only a real http(s) page becomes Referer.
        let referer = page_url.as_deref().and_then(referer_value);
        let user_agent = user_agent.filter(|v| header_value_ok(v));
        // Attempt loop: at most one resume retry (refused-Range restart),
        // then terminal. No unbounded retry: a flapping server must fail
        // visibly, not spin a worker forever.
        for attempt in 0..=1 {
            if resume_from > 0 && resume_meta.is_none() {
                clear_partial(&partial);
                resume_from = 0;
                if attempt == 0 {
                    continue;
                }
                fail_clean(format!(
                    "Couldn't save {dest_filename}: resume metadata missing"
                ));
                return;
            }
            let mut req = agent.get(&url);
            if let Some(ref header) = cookie_header {
                req = req.header("Cookie", header);
            }
            if let Some(ref header) = user_agent {
                req = req.header("User-Agent", header);
            }
            if let Some(ref header) = referer {
                req = req.header("Referer", header);
            }
            // Ranges count representation bytes. Ureq otherwise negotiates
            // gzip and transparently strips Content-Length, which makes a
            // decompressed prefix unsafe to resume.
            req = req.header("Accept-Encoding", "identity");
            if resume_from > 0 {
                let Some(meta) = resume_meta.as_ref() else {
                    continue;
                };
                req = req
                    .header("Range", &format!("bytes={resume_from}-"))
                    .header("If-Range", &meta.validator);
            }
            let resp = match req.call() {
                Ok(r) => r,
                Err(e) => {
                    fail_keep_partial(format!("Couldn't save {dest_filename}: {e}"));
                    return;
                }
            };
            let status = resp.status();
            let etag = resp.headers().get("etag").and_then(|v| v.to_str().ok());
            let last_modified = resp
                .headers()
                .get("last-modified")
                .and_then(|v| v.to_str().ok());
            let content_length = resp
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok());
            let content_range = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range);
            // A decoded representation no longer maps byte offsets to the
            // origin's entity. `identity` is the only encoding that can be
            // resumed safely; a server that ignores it must restart.
            let encoded = resp.headers().get("content-encoding").is_some_and(|v| {
                v.to_str()
                    .map(|s| !s.trim().eq_ignore_ascii_case("identity"))
                    .unwrap_or(true)
            });
            if status == 416 || (resume_from > 0 && status == 200) {
                // Range refused or ignored: restart from zero. A 200 body
                // is a fresh entity, so no prefix is reused.
                clear_partial(&partial);
                resume_from = 0;
                resume_meta = None;
                if attempt == 0 {
                    continue;
                }
                fail_clean(format!(
                    "Couldn't save {dest_filename}: server refused resume"
                ));
                return;
            }
            if status != 200 && status != 206 {
                fail_clean(format!(
                    "Couldn't save {dest_filename}: server answered {status}"
                ));
                return;
            }
            if status == 206 && resume_from == 0 {
                fail_clean(format!(
                    "Couldn't save {dest_filename}: unexpected partial content"
                ));
                return;
            }
            if status == 206 && encoded {
                clear_partial(&partial);
                resume_from = 0;
                resume_meta = None;
                if attempt == 0 {
                    continue;
                }
                fail_clean(format!(
                    "Couldn't save {dest_filename}: encoded resume unsupported"
                ));
                return;
            }
            let total = if status == 206 {
                let Some(meta) = resume_meta.as_ref() else {
                    clear_partial(&partial);
                    resume_from = 0;
                    resume_meta = None;
                    if attempt == 0 {
                        continue;
                    }
                    fail_clean(format!(
                        "Couldn't save {dest_filename}: resume metadata missing"
                    ));
                    return;
                };
                match check_resume_response(
                    resume_from,
                    meta,
                    content_range,
                    content_length,
                    etag,
                    last_modified,
                ) {
                    Ok(total) => {
                        // A finite Content-Range total may be the first one
                        // we have; persist it for later validation.
                        if let Some(meta) = resume_meta.as_mut()
                            && meta.total.is_none()
                        {
                            meta.total = Some(total);
                            let _ = store_resume_meta(&partial, meta);
                        }
                        Some(total)
                    }
                    Err(reason) => {
                        tracing::warn!(reason, "resume rejected");
                        clear_partial(&partial);
                        resume_from = 0;
                        resume_meta = None;
                        if attempt == 0 {
                            continue;
                        }
                        fail_clean(format!(
                            "Couldn't save {dest_filename}: server changed the file"
                        ));
                        return;
                    }
                }
            } else {
                content_length
            };
            if status == 200 {
                // A fresh full response is the only point at which a new
                // validator can be recorded. Without a validator or a
                // finite length (chunked/decoded body), a later attempt
                // must restart rather than guess.
                resume_meta = if encoded {
                    None
                } else {
                    content_length.and_then(|total| {
                        choose_validator(etag, last_modified).map(|validator| ResumeMeta {
                            version: RESUME_META_VERSION,
                            url_tag: url_tag(&url),
                            validator,
                            total: Some(total),
                            partial_len: 0,
                        })
                    })
                };
                if let Some(meta) = resume_meta.as_ref() {
                    let _ = store_resume_meta(&partial, meta);
                }
            }
            if resume_from > 0 {
                // Recheck immediately before append: a file changed between
                // metadata load and open must not receive a suffix.
                let actual = std::fs::metadata(&partial)
                    .ok()
                    .map(|m| m.len())
                    .unwrap_or(0);
                if actual != resume_from {
                    clear_partial(&partial);
                    resume_from = 0;
                    resume_meta = None;
                    if attempt == 0 {
                        continue;
                    }
                    fail_clean(format!(
                        "Couldn't save {dest_filename}: partial file changed"
                    ));
                    return;
                }
            }
            let file = match std::fs::OpenOptions::new()
                .create(true)
                .append(resume_from > 0)
                .write(true)
                .truncate(resume_from == 0)
                .open(&partial)
            {
                Ok(f) => f,
                Err(e) => {
                    fail_clean(format!("Couldn't write {dest_filename}: {e}"));
                    return;
                }
            };
            // Append mode positions at end already. No seek needed.
            let mut reader = resp.into_body().into_reader();
            let mut writer = std::io::BufWriter::new(file);
            let mut buf = [0u8; 65536];
            let mut received: u64 = resume_from;
            let mut failed: Option<String> = None;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if writer.write_all(&buf[..n]).is_err() {
                            failed = Some(format!("Couldn't write {dest_filename}: disk error"));
                            break;
                        }
                        received += n as u64;
                        manager.progress_with_path(
                            &id,
                            &url,
                            &dest_filename,
                            Some(&dest),
                            received,
                            total,
                        );
                        let due = resume_meta
                            .as_ref()
                            .is_some_and(|m| checkpoint_due(received, m.partial_len));
                        if due && writer.flush().is_ok() && writer.get_ref().sync_data().is_ok() {
                            checkpoint_partial(&partial, &mut resume_meta, received);
                        }
                    }
                    Err(e) => {
                        failed = Some(format!("Couldn't save {dest_filename}: {e}"));
                        break;
                    }
                }
            }
            if let Some(msg) = failed {
                // Only a successful flush+sync makes `received` a durable
                // prefix. A failed write leaves the old sidecar length, so
                // the next attempt discards instead of appending.
                let flushed = writer.flush().is_ok() && writer.get_ref().sync_data().is_ok();
                if flushed {
                    checkpoint_partial(&partial, &mut resume_meta, received);
                }
                fail_keep_partial(msg);
                return;
            }
            if writer.flush().is_err() || writer.get_ref().sync_data().is_err() {
                fail_keep_partial(format!("Couldn't write {dest_filename}: disk error"));
                return;
            }
            drop(writer);
            if let Some(expected) = total {
                if received < expected {
                    checkpoint_partial(&partial, &mut resume_meta, received);
                    fail_keep_partial(format!("Couldn't save {dest_filename}: incomplete"));
                    return;
                }
                if received > expected {
                    fail_clean(format!(
                        "Couldn't save {dest_filename}: server sent too much data"
                    ));
                    return;
                }
            }
            match ferrite_tabs::promote_partial(&partial, &dest) {
                Ok(()) => {
                    clear_resume_meta(&partial);
                    manager.complete_with_tab(
                        &id,
                        &url,
                        &dest_filename,
                        Some(&dest),
                        tab_id.clone(),
                    );
                    return;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // User picked an existing name between dialog and
                    // promote: suffix and retry the rename once, no re-fetch.
                    let parent = dest
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(std::env::temp_dir);
                    let fresh = ferrite_tabs::unique_path(&parent, &dest_filename);
                    match std::fs::rename(&partial, &fresh) {
                        Ok(()) => {
                            clear_resume_meta(&partial);
                            let fresh_name = fresh
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(&dest_filename)
                                .to_owned();
                            manager.complete_with_tab(
                                &id,
                                &url,
                                &fresh_name,
                                Some(&fresh),
                                tab_id.clone(),
                            );
                            return;
                        }
                        Err(e2) => {
                            fail_keep_partial(format!("Couldn't save {dest_filename}: {e2}"));
                            return;
                        }
                    }
                }
                Err(e) => {
                    fail_keep_partial(format!("Couldn't save {dest_filename}: {e}"));
                    return;
                }
            }
        }
    });
}

/// GET a URL to disk through the save dialog (Save Link and Image As).
/// Buffer-first for the `Content-Disposition` name, cookies for auth.
/// Failures go back over `notice_tx` (no download-manager row here).
// ponytail: 250MB cap keeps one fetch from eating the machine. Raise
// alongside a progress surface, not before.
pub fn spawn_save_url(
    url: String,
    page_url: Option<String>,
    user_agent: Option<String>,
    cookies: Vec<(String, String)>,
    start_dir: Option<PathBuf>,
    force_dir: Option<PathBuf>,
    notice_tx: Sender<String>,
) {
    std::thread::spawn(move || {
        use std::io::Read;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(std::time::Duration::from_secs(60)))
                .build(),
        );
        let mut req = agent.get(&url);
        if let Some(header) = page_url.as_deref().and_then(referer_value) {
            req = req.header("Referer", &header);
        }
        if let Some(header) = user_agent.filter(|v| header_value_ok(v)) {
            req = req.header("User-Agent", &header);
        }
        if !cookies.is_empty() {
            let header = cookies
                .iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            req = req.header("Cookie", &header);
        }
        let fail = |msg: String| {
            let _ = notice_tx.send(msg);
        };
        let resp = match req.call() {
            Ok(r) => r,
            Err(e) => {
                fail(format!("Couldn't save file: {e}"));
                return;
            }
        };
        if resp.status() != 200 {
            fail(format!(
                "Couldn't save file: server answered {}",
                resp.status()
            ));
            return;
        }
        let cd = resp
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mut body = Vec::new();
        if resp
            .into_body()
            .into_reader()
            .take(250 * 1024 * 1024)
            .read_to_end(&mut body)
            .is_err()
        {
            fail("Couldn't save file: interrupted download".to_owned());
            return;
        }
        if body.len() >= 250 * 1024 * 1024 {
            fail("Couldn't save file: over the 250MB save-as cap".to_owned());
            return;
        }
        let suggested = sanitize_filename(&{
            let cand = attachment_filename(&cd, &url);
            if cand.is_empty() {
                "download".to_owned()
            } else {
                cand
            }
        });
        let mut dlg = rfd::FileDialog::new().set_file_name(suggested.clone());
        if let Some(dir) = start_dir {
            dlg = dlg.set_directory(dir);
        }
        let dest = match force_dir {
            // Guest temp dir: no picker by design (any permanent path it
            // could offer would violate ephemerality).
            Some(dir) => {
                if std::fs::create_dir_all(&dir).is_err() {
                    fail("Couldn't save file: temporary folder unavailable.".to_owned());
                    return;
                }
                ferrite_tabs::decide_destination(&dir, &suggested)
            }
            None => {
                let Some(dest) = dlg.save_file() else {
                    return;
                };
                dest
            }
        };
        if let Err(e) = std::fs::write(&dest, &body) {
            fail(format!("Couldn't write {suggested}: {e}"));
        }
    });
}

/// Write already-held bytes through the save dialog (decoded data URLs,
/// reassembled blob transfers). Worker thread, failures via notice.
/// `force_dir` skips the dialog into a fixed dir (guest temp dir).
pub fn spawn_save_bytes(
    bytes: Vec<u8>,
    filename: String,
    start_dir: Option<PathBuf>,
    force_dir: Option<PathBuf>,
    notice_tx: Sender<String>,
) {
    std::thread::spawn(move || {
        save_bytes_dialog(&bytes, &filename, start_dir, force_dir, &notice_tx);
    });
}

/// Blocking dialog-plus-write core shared by the byte-saving workers (each
/// already runs on its own thread, this never touches the loop).
fn save_bytes_dialog(
    bytes: &[u8],
    filename: &str,
    start_dir: Option<PathBuf>,
    force_dir: Option<PathBuf>,
    notice_tx: &Sender<String>,
) {
    let suggested = sanitize_filename(filename);
    let dest = match force_dir {
        Some(dir) => {
            if std::fs::create_dir_all(&dir).is_err() {
                let _ =
                    notice_tx.send("Couldn't save file: temporary folder unavailable.".to_owned());
                return;
            }
            ferrite_tabs::decide_destination(&dir, &suggested)
        }
        None => {
            let mut dlg = rfd::FileDialog::new().set_file_name(suggested.clone());
            if let Some(dir) = start_dir {
                dlg = dlg.set_directory(dir);
            }
            // Cancelled dialogs record nothing (no row, no file, no notice).
            let Some(dest) = dlg.save_file() else {
                return;
            };
            dest
        }
    };
    if let Err(e) = std::fs::write(&dest, bytes) {
        let _ = notice_tx.send(format!("Couldn't write {suggested}: {e}"));
    }
}

/// Extension for a MIME type (save-dialog defaults). Unknown reads as bin.
pub fn mime_ext(mime: &str) -> &'static str {
    let base = mime.split(';').next().unwrap_or("").trim().to_lowercase();
    match base.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/avif" => "avif",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/css" => "css",
        "text/csv" => "csv",
        "application/pdf" => "pdf",
        "application/json" => "json",
        "application/zip" => "zip",
        "application/gzip" => "gz",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        _ => "bin",
    }
}

/// Decode a data: URL to (bytes, dialog filename). Percent-encoded or
/// base64 bodies. Caps at 100MB decoded. Page-embedded payloads bigger
/// than that are pathological (the page itself already holds them).
pub fn decode_data_url(url: &str) -> Option<(Vec<u8>, String)> {
    const CAP: usize = 100 * 1024 * 1024;
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let (mime, base64) = match meta.split_once(';') {
        Some((m, f)) => (m, f.eq_ignore_ascii_case("base64")),
        None => (meta, false),
    };
    let ext = if mime.trim().is_empty() {
        "txt"
    } else {
        mime_ext(mime)
    };
    let bytes = if base64 {
        use base64::Engine as _;
        let compact: String = data.chars().filter(|c| !c.is_whitespace()).collect();
        base64::engine::general_purpose::STANDARD
            .decode(compact.as_bytes())
            .ok()?
    } else {
        let mut out = Vec::with_capacity(data.len().min(CAP));
        let mut it = data.as_bytes().iter().peekable();
        while let Some(&b) = it.next() {
            if b == b'%' {
                let h: Vec<u8> = it.by_ref().take(2).copied().collect();
                if h.len() == 2
                    && let Ok(hex) = std::str::from_utf8(&h)
                    && let Ok(byte) = u8::from_str_radix(hex, 16)
                {
                    out.push(byte);
                    continue;
                }
                out.push(b'%');
                out.extend(h);
            } else {
                // In data: bodies '+' is literal (no form encoding).
                out.push(b);
            }
        }
        out
    };
    if bytes.is_empty() || bytes.len() > CAP {
        return None;
    }
    Some((bytes, format!("download.{ext}")))
}

/// Decode plus save a data: URL (fully worker-side, the loop only passed
/// the URL string). `force_dir` skips the dialog (guest temp dir).
pub fn spawn_save_data_url(
    url: String,
    start_dir: Option<PathBuf>,
    force_dir: Option<PathBuf>,
    notice_tx: Sender<String>,
) {
    std::thread::spawn(move || match decode_data_url(&url) {
        Some((bytes, filename)) => {
            save_bytes_dialog(&bytes, &filename, start_dir, force_dir, &notice_tx);
        }
        None => {
            let _ = notice_tx.send("Couldn't save file: bad or oversize data URL".to_owned());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn sanitize_strips_traversal_and_controls() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("C:\\Users\\a\\x.txt"), "x.txt");
        assert_eq!(sanitize_filename("a\x00b"), "a_b");
        assert_eq!(sanitize_filename("a<b>c:d\"e/f\\g|h?i*j"), "g_h_i_j");
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename(".."), "download");
        assert_eq!(sanitize_filename("file."), "file");
        // Double extensions stay visible (no silent rename).
        assert_eq!(sanitize_filename("invoice.pdf.exe"), "invoice.pdf.exe");
        assert_eq!(sanitize_filename("résumé.pdf"), "résumé.pdf");
    }

    #[test]
    fn attachment_prefers_disposition_over_url() {
        assert_eq!(
            attachment_filename(
                r#"attachment; filename="report q1.pdf""#,
                "https://a.test/x"
            ),
            "report q1.pdf"
        );
        assert_eq!(
            attachment_filename(
                "attachment; filename*=UTF-8''%E2%82%ACrates.pdf",
                "https://a.test/x"
            ),
            "€rates.pdf"
        );
        assert_eq!(
            attachment_filename("", "https://a.test/files/a.bin?x=1"),
            "a.bin"
        );
        assert_eq!(attachment_filename("", "https://a.test/"), "");
    }

    #[test]
    fn data_url_decodes_both_shapes() {
        let (bytes, name) = decode_data_url("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(name, "download.png");
        assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G']));
        let (bytes, name) = decode_data_url("data:,Hello%20World").unwrap();
        assert_eq!(name, "download.txt");
        assert_eq!(bytes, b"Hello World");
        // Literal '+' stays (no form encoding in data: bodies).
        let (bytes, _) = decode_data_url("data:,a+b").unwrap();
        assert_eq!(bytes, b"a+b");
        // Reject: empty, missing comma, oversize shape.
        assert!(decode_data_url("data:image/png;base64,").is_none());
        assert!(decode_data_url("data:image/png").is_none());
        assert!(decode_data_url("https://a.test/x").is_none());
    }

    #[test]
    fn mime_ext_covers_common_types() {
        assert_eq!(mime_ext("image/jpeg"), "jpg");
        assert_eq!(mime_ext("application/pdf; charset=binary"), "pdf");
        assert_eq!(mime_ext("application/x-unknown"), "bin");
    }

    fn temp_test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ferrite-filedlg-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn content_range_parser_handles_known_unknown_and_bad_totals() {
        assert_eq!(
            parse_content_range("bytes 100-199/200"),
            Some(ContentRange {
                start: 100,
                end: 199,
                total: Some(200)
            })
        );
        assert_eq!(
            parse_content_range("Bytes 0-0/*"),
            Some(ContentRange {
                start: 0,
                end: 0,
                total: None
            })
        );
        assert_eq!(parse_content_range("bytes */200"), None);
        assert_eq!(parse_content_range("bytes 5-4/200"), None);
        assert_eq!(parse_content_range("bytes 0-200/200"), None);
        assert_eq!(parse_content_range("bytes 0-1/1"), None);
        assert_eq!(parse_content_range("nibbles 0-1/2"), None);
    }

    #[test]
    fn validator_prefers_strong_etag_and_rejects_weak_or_malformed() {
        assert_eq!(
            choose_validator(Some("\"v1\""), Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            Some("\"v1\"".to_owned())
        );
        assert_eq!(
            choose_validator(Some("W/\"v1\""), Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            Some("Wed, 21 Oct 2015 07:28:00 GMT".to_owned())
        );
        assert_eq!(choose_validator(Some("v1"), None), None);
        assert_eq!(choose_validator(None, None), None);
    }

    #[test]
    fn resume_check_rejects_mismatched_range_validator_or_total() {
        let meta = ResumeMeta {
            version: RESUME_META_VERSION,
            url_tag: url_tag("http://origin/file.bin"),
            validator: "\"v1\"".to_owned(),
            total: Some(200),
            partial_len: 100,
        };
        let good = parse_content_range("bytes 100-199/200");
        assert_eq!(
            check_resume_response(100, &meta, good, Some(100), Some("\"v1\""), None),
            Ok(200)
        );
        assert!(
            check_resume_response(
                100,
                &meta,
                parse_content_range("bytes 0-99/200"),
                Some(100),
                Some("\"v1\""),
                None
            )
            .is_err()
        );
        assert!(check_resume_response(100, &meta, good, Some(100), Some("\"v2\""), None).is_err());
        assert!(
            check_resume_response(
                100,
                &meta,
                parse_content_range("bytes 100-199/201"),
                Some(100),
                Some("\"v1\""),
                None
            )
            .is_err()
        );
        assert!(check_resume_response(100, &meta, good, Some(99), Some("\"v1\""), None).is_err());
        assert!(check_resume_response(100, &meta, None, Some(100), Some("\"v1\""), None).is_err());
        assert!(check_resume_response(100, &meta, good, None, Some("\"v1\""), None).is_err());

        let last_modified = ResumeMeta {
            validator: "Wed, 21 Oct 2015 07:28:00 GMT".to_owned(),
            total: Some(200),
            ..meta
        };
        assert_eq!(
            check_resume_response(
                100,
                &last_modified,
                parse_content_range("bytes 100-199/*"),
                Some(100),
                Some("\"new\""),
                Some("Wed, 21 Oct 2015 07:28:00 GMT")
            ),
            Ok(200)
        );
        let unknown_total = ResumeMeta {
            total: None,
            ..last_modified
        };
        assert!(
            check_resume_response(
                100,
                &unknown_total,
                parse_content_range("bytes 100-199/*"),
                Some(100),
                None,
                Some("Wed, 21 Oct 2015 07:28:00 GMT")
            )
            .is_err()
        );
    }

    #[test]
    fn resume_meta_round_trips_and_requires_matching_url() {
        let dir = temp_test_dir("meta");
        let partial = dir.join("file.bin.partial");
        std::fs::write(&partial, b"prefix").unwrap();
        let meta = ResumeMeta {
            version: RESUME_META_VERSION,
            url_tag: url_tag("http://origin/file.bin"),
            validator: "\"v1\"".to_owned(),
            total: Some(200),
            partial_len: 6,
        };
        store_resume_meta(&partial, &meta).unwrap();
        assert_eq!(
            load_resume_meta(&partial, "http://origin/file.bin"),
            Some(meta)
        );
        assert_eq!(load_resume_meta(&partial, "http://origin/other.bin"), None);
        assert!(
            !String::from_utf8_lossy(&std::fs::read(resume_meta_path(&partial)).unwrap())
                .contains("origin/file.bin")
        );
        let bad_validator = ResumeMeta {
            version: RESUME_META_VERSION,
            url_tag: url_tag("http://origin/file.bin"),
            validator: "bad\r\nX-Injected: yes".to_owned(),
            total: Some(200),
            partial_len: 6,
        };
        store_resume_meta(&partial, &bad_validator).unwrap();
        assert_eq!(load_resume_meta(&partial, "http://origin/file.bin"), None);
        std::fs::write(resume_meta_path(&partial), vec![b' '; 4097]).unwrap();
        assert_eq!(load_resume_meta(&partial, "http://origin/file.bin"), None);
        clear_partial(&partial);
        assert!(!partial.exists());
        assert!(!resume_meta_path(&partial).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_replacement_is_atomic_and_leaves_no_temp() {
        let dir = temp_test_dir("sidecar-atomic");
        let partial = dir.join("file.bin.partial");
        let url = "https://example.com/file.bin";
        let first = ResumeMeta {
            version: RESUME_META_VERSION,
            url_tag: url_tag(url),
            validator: "\"v1\"".to_owned(),
            total: Some(4096),
            partial_len: 1024,
        };
        store_resume_meta(&partial, &first).unwrap();
        let second = ResumeMeta {
            partial_len: 2048,
            ..first
        };
        store_resume_meta(&partial, &second).unwrap();
        let loaded = load_resume_meta(&partial, url).unwrap();
        assert_eq!(loaded, second);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp sidecars: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkpoint_cadence_uses_durable_length_not_elapsed_time() {
        assert!(!checkpoint_due(0, 0));
        assert!(!checkpoint_due(CHECKPOINT_BYTES - 1, 0));
        assert!(checkpoint_due(CHECKPOINT_BYTES, 0));
        assert!(!checkpoint_due(CHECKPOINT_BYTES + 10, CHECKPOINT_BYTES));
        assert!(checkpoint_due(CHECKPOINT_BYTES * 2, CHECKPOINT_BYTES));
        // A sidecar ahead of the file (bad state) must not underflow into
        // an immediate checkpoint.
        assert!(!checkpoint_due(10, CHECKPOINT_BYTES));
    }

    #[test]
    fn referer_is_http_only_and_fragmentless() {
        assert_eq!(
            referer_value("https://example.com/a/b?q=1#frag"),
            Some("https://example.com/a/b?q=1".to_owned())
        );
        assert_eq!(
            referer_value("http://example.com/"),
            Some("http://example.com/".to_owned())
        );
        assert_eq!(
            referer_value("https://user:secret@example.com/a"),
            Some("https://example.com/a".to_owned()),
            "URL credentials must never leak through Referer"
        );
        assert_eq!(referer_value("ferrite://settings"), None);
        assert_eq!(referer_value("data:text/plain,hello"), None);
        assert_eq!(referer_value("not a url"), None);
        assert!(header_value_ok("Ferrite/1.0"));
        assert!(!header_value_ok(""));
        assert!(!header_value_ok("Ferrite\r\nX-Injected: yes"));
    }

    type RequestHeaders = Vec<(String, String)>;

    #[derive(Clone)]
    struct OriginReply {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    struct TestOrigin {
        addr: std::net::SocketAddr,
        requests: Arc<Mutex<Vec<RequestHeaders>>>,
    }

    impl TestOrigin {
        fn start(replies: Vec<OriginReply>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&requests);
            std::thread::spawn(move || {
                for reply in replies {
                    let Ok((mut stream, _)) = listener.accept() else {
                        return;
                    };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while let Ok(n) = stream.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
                            break;
                        }
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let mut lines = text.split("\r\n");
                    let _request_line = lines.next();
                    let mut headers = Vec::new();
                    for line in lines {
                        if line.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            headers
                                .push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
                        }
                    }
                    log.lock().unwrap().push(headers);
                    let reason = match reply.status {
                        200 => "OK",
                        206 => "Partial Content",
                        416 => "Range Not Satisfiable",
                        _ => "Test",
                    };
                    let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reason);
                    for (name, value) in &reply.headers {
                        head.push_str(&format!("{name}: {value}\r\n"));
                    }
                    head.push_str(&format!(
                        "Content-Length: {}\r\nConnection: close\r\n\r\n",
                        reply.body.len()
                    ));
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&reply.body);
                    let _ = stream.flush();
                }
            });
            Self { addr, requests }
        }

        fn url(&self) -> String {
            format!("http://{}/file.bin", self.addr)
        }

        fn requests(&self) -> Vec<RequestHeaders> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn reply(status: u16, headers: &[(&str, &str)], body: Vec<u8>) -> OriginReply {
        OriginReply {
            status,
            headers: headers
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
            body,
        }
    }

    fn header<'a>(request: &'a RequestHeaders, name: &str) -> Option<&'a str> {
        request
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn seed_resume_with_total(
        dir: &Path,
        url: &str,
        body: &[u8],
        prefix: usize,
        validator: &str,
        total: Option<u64>,
    ) -> PathBuf {
        let dest = dir.join("file.bin");
        let partial = ferrite_tabs::partial_path_for(&dest);
        std::fs::write(&partial, &body[..prefix]).unwrap();
        store_resume_meta(
            &partial,
            &ResumeMeta {
                version: RESUME_META_VERSION,
                url_tag: url_tag(url),
                validator: validator.to_owned(),
                total,
                partial_len: prefix as u64,
            },
        )
        .unwrap();
        dest
    }

    fn seed_resume(dir: &Path, url: &str, body: &[u8], prefix: usize, validator: &str) -> PathBuf {
        seed_resume_with_total(dir, url, body, prefix, validator, Some(body.len() as u64))
    }

    fn run_fetch_with_context(
        url: &str,
        dir: &Path,
        page_url: Option<&str>,
        user_agent: Option<&str>,
    ) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        spawn_fetch_save(
            url.to_owned(),
            "file.bin".to_owned(),
            Vec::new(),
            page_url.map(str::to_owned),
            user_agent.map(str::to_owned),
            ferrite_tabs::DownloadManager::new(),
            None,
            Some(dir.to_path_buf()),
            tx,
            None,
        );
        rx
    }

    fn run_fetch(url: &str, dir: &Path) -> mpsc::Receiver<String> {
        run_fetch_with_context(url, dir, None, None)
    }

    fn wait_for_dest(path: &Path) -> Vec<u8> {
        let start = Instant::now();
        loop {
            if let Ok(bytes) = std::fs::read(path) {
                return bytes;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "download did not finish: {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn test_body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn resume_appends_only_a_validated_range() {
        let dir = temp_test_dir("resume-ok");
        let body = test_body(96 * 1024);
        let prefix = 4096;
        let content_range = format!("bytes {prefix}-{}/{}", body.len() - 1, body.len());
        let origin = TestOrigin::start(vec![reply(
            206,
            &[
                ("Content-Range", &content_range),
                ("ETag", "\"v1\""),
                ("Accept-Ranges", "bytes"),
            ],
            body[prefix..].to_vec(),
        )]);
        let url = origin.url();
        let dest = seed_resume(&dir, &url, &body, prefix, "\"v1\"");
        let _rx = run_fetch(&url, &dir);
        assert_eq!(wait_for_dest(&dest), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(header(&requests[0], "range"), Some("bytes=4096-"));
        assert_eq!(header(&requests[0], "if-range"), Some("\"v1\""));
        assert_eq!(header(&requests[0], "accept-encoding"), Some("identity"));
        let partial = ferrite_tabs::partial_path_for(&dest);
        assert!(!partial.exists());
        assert!(!ferrite_tabs::partial_meta_path_for(&partial).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignored_range_restarts_without_reusing_the_prefix() {
        let dir = temp_test_dir("resume-ignored");
        let body = test_body(64 * 1024);
        let prefix = 4096;
        let origin = TestOrigin::start(vec![
            reply(200, &[("ETag", "\"v2\"")], body.clone()),
            reply(200, &[("ETag", "\"v2\"")], body.clone()),
        ]);
        let url = origin.url();
        let dest = seed_resume(&dir, &url, &body, prefix, "\"v1\"");
        let _rx = run_fetch(&url, &dir);
        assert_eq!(wait_for_dest(&dest), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(header(&requests[0], "range"), Some("bytes=4096-"));
        assert_eq!(header(&requests[0], "if-range"), Some("\"v1\""));
        assert_eq!(header(&requests[1], "range"), None);
        assert_eq!(header(&requests[1], "if-range"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encoded_range_restarts_instead_of_appending() {
        let dir = temp_test_dir("resume-encoded");
        let body = test_body(64 * 1024);
        let prefix = 4096;
        let origin = TestOrigin::start(vec![
            reply(
                206,
                &[
                    ("Content-Range", "bytes 4096-65535/65536"),
                    ("ETag", "\"v1\""),
                    ("Content-Encoding", "x-ferrite-test"),
                ],
                body[prefix..].to_vec(),
            ),
            reply(200, &[("ETag", "\"v1\"")], body.clone()),
        ]);
        let url = origin.url();
        let dest = seed_resume(&dir, &url, &body, prefix, "\"v1\"");
        let _rx = run_fetch(&url, &dir);
        assert_eq!(wait_for_dest(&dest), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(header(&requests[0], "range"), Some("bytes=4096-"));
        assert_eq!(header(&requests[1], "range"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mismatched_content_range_restarts_instead_of_appending() {
        let dir = temp_test_dir("resume-bad-range");
        let body = test_body(64 * 1024);
        let prefix = 4096;
        let origin = TestOrigin::start(vec![
            reply(
                206,
                &[("Content-Range", "bytes 0-4095/65536"), ("ETag", "\"v1\"")],
                body[..prefix].to_vec(),
            ),
            reply(200, &[("ETag", "\"v1\"")], body.clone()),
        ]);
        let url = origin.url();
        let dest = seed_resume(&dir, &url, &body, prefix, "\"v1\"");
        let _rx = run_fetch(&url, &dir);
        assert_eq!(wait_for_dest(&dest), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(header(&requests[1], "range"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_without_sidecar_is_discarded_not_appended() {
        let dir = temp_test_dir("resume-no-sidecar");
        let body = test_body(64 * 1024);
        let origin = TestOrigin::start(vec![reply(200, &[("ETag", "\"v1\"")], body.clone())]);
        let url = origin.url();
        let dest = dir.join("file.bin");
        let partial = ferrite_tabs::partial_path_for(&dest);
        std::fs::write(&partial, &body[..4096]).unwrap();
        let _rx = run_fetch(&url, &dir);
        assert_eq!(wait_for_dest(&dest), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(header(&requests[0], "range"), None);
        assert!(!partial.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_longer_than_sidecar_is_truncated_and_resumed() {
        let dir = temp_test_dir("resume-salvage");
        let body = test_body(64 * 1024);
        let prefix = 4096;
        let durable = 2048;
        let origin = TestOrigin::start(vec![reply(
            206,
            &[
                (
                    "Content-Range",
                    &format!("bytes {durable}-{}/{}", body.len() - 1, body.len()),
                ),
                ("ETag", "\"v1\""),
            ],
            body[durable..].to_vec(),
        )]);
        let url = origin.url();
        let dest = seed_resume(&dir, &url, &body, prefix, "\"v1\"");
        let partial = ferrite_tabs::partial_path_for(&dest);
        let mut meta = load_resume_meta(&partial, &url).unwrap();
        meta.partial_len = durable as u64;
        store_resume_meta(&partial, &meta).unwrap();
        let _rx = run_fetch(&url, &dir);
        assert_eq!(wait_for_dest(&dest), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            header(&requests[0], "range"),
            Some("bytes=2048-"),
            "resume must start at the last synced byte, not the file's tail"
        );
        assert!(!partial.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_ahead_of_partial_discards_the_prefix() {
        let dir = temp_test_dir("resume-sidecar-ahead");
        let body = test_body(64 * 1024);
        let prefix = 4096;
        let origin = TestOrigin::start(vec![reply(200, &[("ETag", "\"v1\"")], body.clone())]);
        let url = origin.url();
        let dest = seed_resume(&dir, &url, &body, prefix, "\"v1\"");
        let partial = ferrite_tabs::partial_path_for(&dest);
        let mut meta = load_resume_meta(&partial, &url).unwrap();
        meta.partial_len = prefix as u64 + 1;
        store_resume_meta(&partial, &meta).unwrap();
        let _rx = run_fetch(&url, &dir);
        assert_eq!(wait_for_dest(&dest), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(header(&requests[0], "range"), None);
        assert!(!partial.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_link_as_sends_page_referer_and_engine_user_agent() {
        let dir = temp_test_dir("save-headers");
        let body = test_body(32 * 1024);
        let origin = TestOrigin::start(vec![reply(200, &[], body.clone())]);
        let url = origin.url();
        let (tx, _rx) = mpsc::channel();
        spawn_save_url(
            url,
            Some("https://page.example/dir/doc.html#section".to_owned()),
            Some("FerriteTest/1.0".to_owned()),
            vec![("session".to_owned(), "abc".to_owned())],
            None,
            Some(dir.clone()),
            tx,
        );
        let dest = dir.join("file.bin");
        assert_eq!(wait_for_dest(&dest), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            header(&requests[0], "referer"),
            Some("https://page.example/dir/doc.html")
        );
        assert_eq!(header(&requests[0], "user-agent"), Some("FerriteTest/1.0"));
        assert_eq!(header(&requests[0], "cookie"), Some("session=abc"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ask_divert_refetch_sends_page_context() {
        let dir = temp_test_dir("fetch-headers");
        let body = test_body(32 * 1024);
        let origin = TestOrigin::start(vec![reply(200, &[("ETag", "\"v1\"")], body.clone())]);
        let url = origin.url();
        let _rx = run_fetch_with_context(
            &url,
            &dir,
            Some("http://page.example/downloads"),
            Some("FerriteTest/1.0"),
        );
        assert_eq!(wait_for_dest(&dir.join("file.bin")), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            header(&requests[0], "referer"),
            Some("http://page.example/downloads")
        );
        assert_eq!(header(&requests[0], "user-agent"), Some("FerriteTest/1.0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_total_resume_restarts_instead_of_promoting_a_prefix() {
        let dir = temp_test_dir("resume-unknown-total");
        let body = test_body(64 * 1024);
        let prefix = 4096;
        let origin = TestOrigin::start(vec![
            reply(
                206,
                &[("Content-Range", "bytes 4096-8191/*"), ("ETag", "\"v1\"")],
                body[prefix..prefix + 4096].to_vec(),
            ),
            reply(200, &[("ETag", "\"v1\"")], body.clone()),
        ]);
        let url = origin.url();
        let dest = seed_resume_with_total(&dir, &url, &body, prefix, "\"v1\"", None);
        let _rx = run_fetch(&url, &dir);
        assert_eq!(wait_for_dest(&dest), body);
        let requests = origin.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(header(&requests[0], "range"), Some("bytes=4096-"));
        assert_eq!(header(&requests[1], "range"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_validated_range_keeps_partial_and_does_not_promote() {
        let dir = temp_test_dir("resume-short-range");
        let body = test_body(64 * 1024);
        let prefix = 4096;
        let origin = TestOrigin::start(vec![reply(
            206,
            &[
                ("Content-Range", "bytes 4096-8191/65536"),
                ("ETag", "\"v1\""),
            ],
            body[prefix..prefix + 4096].to_vec(),
        )]);
        let url = origin.url();
        let dest = seed_resume(&dir, &url, &body, prefix, "\"v1\"");
        let rx = run_fetch(&url, &dir);
        let notice = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("download should fail visibly");
        assert!(notice.contains("incomplete"), "{notice}");
        assert!(!dest.exists());
        let partial = ferrite_tabs::partial_path_for(&dest);
        assert_eq!(std::fs::metadata(&partial).unwrap().len(), 8192);
        let meta = load_resume_meta(&partial, &url).unwrap();
        assert_eq!(meta.partial_len, 8192);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
