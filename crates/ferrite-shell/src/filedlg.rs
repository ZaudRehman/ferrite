//! Native file dialogs off the UI thread, plus filename sanitization.
//! Pickers run on a worker and report over a channel drained in tick.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

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

/// GET a URL to disk through the save dialog, with cookies and progress.
/// Worker thread only. Bytes stage to `.partial`, promote by rename, one
/// resume retry. Failures go back over `notice_tx`.
/// Ask-divert refetch with a save dialog, unless `force_dir` is set: then
/// the dialog is skipped and the file lands sanitized and de-duplicated
/// inside it (guest temp dir). The dir is created best-effort; failure to
/// create fails the download visibly, never silently elsewhere.
/// Fetch a URL to disk on a worker (ask-every-time refetch path).
/// Eight params, each used; a struct would just move the arity.
#[allow(clippy::too_many_arguments)]
pub fn spawn_fetch_save(
    url: String,
    filename: String,
    cookies: Vec<(String, String)>,
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
        let fail_clean = |msg: String| {
            let _ = std::fs::remove_file(&partial);
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
        // Attempt loop: at most one resume retry (refused-Range restart),
        // then terminal. No unbounded retry: a flapping server must fail
        // visibly, not spin a worker forever.
        for attempt in 0..=1 {
            let mut req = agent.get(&url);
            if let Some(ref header) = cookie_header {
                req = req.header("Cookie", header);
            }
            if resume_from > 0 {
                req = req.header("Range", &format!("bytes={resume_from}-"));
            }
            let resp = match req.call() {
                Ok(r) => r,
                Err(e) => {
                    fail_keep_partial(format!("Couldn't save {dest_filename}: {e}"));
                    return;
                }
            };
            let status = resp.status();
            if status == 416 || (resume_from > 0 && status == 200) {
                // Range refused: restart from zero (stale partial).
                let _ = std::fs::remove_file(&partial);
                resume_from = 0;
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
            let total = resp
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|n| n + resume_from);
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
                    }
                    Err(e) => {
                        failed = Some(format!("Couldn't save {dest_filename}: {e}"));
                        break;
                    }
                }
            }
            if let Some(msg) = failed {
                let _ = writer.flush();
                fail_keep_partial(msg);
                return;
            }
            if writer.flush().is_err() {
                fail_keep_partial(format!("Couldn't write {dest_filename}: disk error"));
                return;
            }
            drop(writer);
            match ferrite_tabs::promote_partial(&partial, &dest) {
                Ok(()) => {
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
}
