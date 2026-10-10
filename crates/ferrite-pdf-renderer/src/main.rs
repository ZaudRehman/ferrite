//! Isolated PDF renderer process.
//!
//! One process is one document session: the parent sends OPEN once, then
//! PAGE/INFO/SEARCH/TEXT/LINKS/OUTLINE/FORMS/FILL*/SAVE requests over the
//! same stdin/stdout pair until EOF. Bytes and passwords cross once and
//! never leave this process; every response is a u32 length prefix plus
//! payload (JSON, or raw bitmap bytes for PAGE, or raw PDF bytes for
//! SAVE). A failed request reports zero-length (PAGE) or an error object,
//! so the parent can never mistake failure for content.

use std::io::{Read, Write};

use ferrite_archive::pdf::{PdfDocument, PdfEngine};
use ferrite_archive::shm::ShmHandle;
use tracing::info;
use zeroize::{Zeroize, Zeroizing};

/// Caps hold even against a compromised parent. Headers stay unlogged.
const MAX_PDF_BYTES: usize = 64 * 1024 * 1024;
const MAX_PAGE: usize = 10_000;
const MAX_PWD_BYTES: usize = 1_024;
const MAX_QUERY_BYTES: usize = 256;
const MAX_VALUE_BYTES: usize = 16_384;
const MAX_HEADER_BYTES: usize = 1_024;
const MIN_SCALE_MILLI: usize = 50;
const MAX_SCALE_MILLI: usize = 8_000;
const MAX_BITMAP_BYTES: usize = 64 * 1024 * 1024;
const MAX_SEARCH_PAGES: usize = 1_000;

fn err_json(msg: &str) -> Vec<u8> {
    serde_json::json!({"ok": false, "error": msg})
        .to_string()
        .into_bytes()
}

/// Read one header line. `Ok(None)` means clean EOF: shut down.
fn read_header(stdin: &mut std::io::StdinLock) -> anyhow::Result<Option<String>> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stdin.read_exact(&mut byte) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && header.is_empty() => {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
        if byte[0] == b'\n' {
            break;
        }
        header.push(byte[0]);
        if header.len() > MAX_HEADER_BYTES {
            anyhow::bail!("header exceeds cap");
        }
    }
    if header.is_empty() {
        anyhow::bail!("empty header");
    }
    Ok(Some(String::from_utf8_lossy(&header).into_owned()))
}

fn read_exact(stdin: &mut std::io::StdinLock, len: usize, cap: usize) -> anyhow::Result<Vec<u8>> {
    if len > cap {
        anyhow::bail!("payload exceeds cap");
    }
    let mut buf = vec![0u8; len];
    stdin.read_exact(&mut buf)?;
    Ok(buf)
}

fn respond(stdout: &mut std::io::Stdout, payload: &[u8]) -> anyhow::Result<()> {
    stdout.write_all(&(payload.len() as u32).to_le_bytes())?;
    stdout.write_all(payload)?;
    stdout.flush()?;
    Ok(())
}

fn has_encrypt_marker(bytes: &[u8]) -> bool {
    bytes.windows(8).any(|w| w == b"/Encrypt")
}

/// Split a PAGE tail: page and scale are single tokens, the shm path is
/// everything after them, re-joined verbatim. Temp dirs routinely
/// contain spaces ("C:\Users\First Last\..."); whitespace-splitting the
/// path truncates it and fails every render for those users.
fn page_args(parts: &mut std::str::SplitWhitespace<'_>) -> (usize, usize, String) {
    let page = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let scale_milli = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let shm_raw = parts.collect::<Vec<_>>().join(" ");
    (page, scale_milli, shm_raw)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    info!("pdf renderer started");

    // Bound once: the session document reuses the library handle.
    let engine = PdfEngine::new().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut stdout = std::io::stdout();
    let mut doc: Option<PdfDocument> = None;

    while let Some(header) = read_header(&mut lock)? {
        // One bad request must not poison the session: PAGE reports
        // zero-length, everything else an error object.
        let mut parts = header.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let is_page = cmd == "PAGE";
        let result = serve(cmd, &mut parts, &mut lock, &mut stdout, &engine, &mut doc);
        if let Err(e) = result {
            tracing::warn!(error = %e, cmd, "pdf render request failed");
            if is_page {
                stdout.write_all(&0u32.to_le_bytes())?;
                stdout.flush()?;
            } else {
                let msg = format!("{e:#}");
                let capped = msg.chars().take(256).collect::<String>();
                respond(&mut stdout, &err_json(&capped))?;
            }
        }
    }
    info!("pdf renderer stdin closed, exiting");
    Ok(())
}

fn serve(
    cmd: &str,
    parts: &mut std::str::SplitWhitespace<'_>,
    stdin: &mut std::io::StdinLock,
    stdout: &mut std::io::Stdout,
    engine: &PdfEngine,
    doc: &mut Option<PdfDocument>,
) -> anyhow::Result<()> {
    match cmd {
        "OPEN" => {
            let len: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let pwd_len: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if len == 0 || len > MAX_PDF_BYTES {
                anyhow::bail!("pdf length out of bounds");
            }
            if pwd_len > MAX_PWD_BYTES {
                anyhow::bail!("password length out of bounds");
            }
            let mut buf = read_exact(stdin, len, MAX_PDF_BYTES)?;
            let mut pwd_buf = Zeroizing::new(vec![0u8; pwd_len]);
            let password: Option<Zeroizing<String>> = if pwd_len > 0 {
                stdin.read_exact(&mut pwd_buf)?;
                let s = Zeroizing::new(String::from_utf8_lossy(&pwd_buf).into_owned());
                if s.is_empty() { None } else { Some(s) }
            } else {
                None
            };
            let payload: Vec<u8> = match engine.load(&buf, password.as_deref().map(|s| s.as_str()))
            {
                Ok(loaded) => {
                    let pages = loaded.info().page_count;
                    *doc = Some(loaded);
                    serde_json::json!({"ok": true, "pages": pages, "locked": false})
                        .to_string()
                        .into_bytes()
                }
                Err(_) if has_encrypt_marker(&buf) => {
                    *doc = None;
                    serde_json::json!({"ok": false, "locked": true})
                        .to_string()
                        .into_bytes()
                }
                Err(e) => {
                    *doc = None;
                    let msg = format!("{e}").chars().take(256).collect::<String>();
                    err_json(&msg)
                }
            };
            buf.zeroize();
            respond(stdout, &payload)
        }
        "PAGE" => {
            let (page, scale_milli, shm_raw) = page_args(&mut *parts);
            if page > MAX_PAGE {
                anyhow::bail!("page index out of range");
            }
            if !(MIN_SCALE_MILLI..=MAX_SCALE_MILLI).contains(&scale_milli) {
                anyhow::bail!("render scale out of range");
            }
            let Some(doc) = doc.as_ref() else {
                anyhow::bail!("no document open");
            };
            // Bitmap lands only inside temp_dir.
            let shm_path = ShmHandle::validate_path(&shm_raw)
                .ok_or_else(|| anyhow::anyhow!("shm path rejected"))?;
            let scale = scale_milli as f32 / 1000.0;
            let bmp = doc
                .render_page(page, scale)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            if bmp.is_empty() || bmp.len() > MAX_BITMAP_BYTES || bmp.len() % 4 != 0 {
                anyhow::bail!("rendered bitmap out of bounds");
            }
            // Dimensions ride along so the parent can integrity-check the
            // file (w*h*4 must equal the byte count) and PNG-encode it.
            let dims = doc
                .page_size(page)
                .ok_or_else(|| anyhow::anyhow!("page unreadable"))?;
            let (w, h) = (
                (dims.0 * scale).round() as u32,
                (dims.1 * scale).round() as u32,
            );
            if w == 0 || h == 0 || w > 8192 || h > 8192 {
                anyhow::bail!("rendered dimensions out of bounds");
            }
            if w as usize * h as usize * 4 != bmp.len() {
                anyhow::bail!("bitmap does not match page dimensions");
            }
            std::fs::write(&shm_path, &bmp)?;
            // Header-only on stdout: the bitmap travels via the file, so
            // a stalled pipe can never deliver a truncated image the
            // parent would mistake for a page.
            stdout.write_all(&w.to_le_bytes())?;
            stdout.write_all(&h.to_le_bytes())?;
            stdout.write_all(&(bmp.len() as u32).to_le_bytes())?;
            stdout.flush()?;
            Ok(())
        }
        "INFO" => {
            let Some(doc) = doc.as_ref() else {
                anyhow::bail!("no document open");
            };
            let meta = doc.metadata_full();
            let perms = doc
                .permissions_summary()
                .unwrap_or(ferrite_archive::pdf::DocPermissions {
                    print: true,
                    copy: true,
                    fill: false,
                });
            respond(
                stdout,
                serde_json::json!({
                    "ok": true,
                    "engine": ferrite_archive::pdf::HAS_ENGINE,
                    "meta": meta,
                    "perms": perms,
                })
                .to_string()
                .as_bytes(),
            )
        }
        "SEARCH" => {
            let case_flag: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let qlen: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let query = read_exact(stdin, qlen, MAX_QUERY_BYTES)?;
            let Some(doc) = doc.as_ref() else {
                anyhow::bail!("no document open");
            };
            let query = String::from_utf8_lossy(&query);
            let pages = doc.info().page_count.min(MAX_SEARCH_PAGES);
            let mut hits = Vec::new();
            let mut total = 0usize;
            for page in 0..pages {
                let rects = doc.search_page(page, &query, case_flag == 1);
                if !rects.is_empty() {
                    total += rects.len();
                    hits.push(serde_json::json!({"page": page, "rects": rects}));
                }
                if total > 2_000 {
                    break;
                }
            }
            respond(
                stdout,
                serde_json::json!({
                    "ok": true,
                    "hits": hits,
                    "total": total,
                    "truncated": doc.info().page_count > pages,
                })
                .to_string()
                .as_bytes(),
            )
        }
        "TEXT" => {
            let page: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if page > MAX_PAGE {
                anyhow::bail!("page index out of range");
            }
            let Some(doc) = doc.as_ref() else {
                anyhow::bail!("no document open");
            };
            respond(
                stdout,
                serde_json::json!({"ok": true, "lines": doc.page_lines(page)})
                    .to_string()
                    .as_bytes(),
            )
        }
        "LINKS" => {
            let page: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if page > MAX_PAGE {
                anyhow::bail!("page index out of range");
            }
            let Some(doc) = doc.as_ref() else {
                anyhow::bail!("no document open");
            };
            respond(
                stdout,
                serde_json::json!({"ok": true, "links": doc.page_links(page)})
                    .to_string()
                    .as_bytes(),
            )
        }
        "OUTLINE" => {
            let Some(doc) = doc.as_ref() else {
                anyhow::bail!("no document open");
            };
            respond(
                stdout,
                serde_json::json!({"ok": true, "entries": doc.outline()})
                    .to_string()
                    .as_bytes(),
            )
        }
        "FORMS" => {
            let Some(doc) = doc.as_ref() else {
                anyhow::bail!("no document open");
            };
            respond(
                stdout,
                serde_json::json!({"ok": true, "fields": doc.form_state()})
                    .to_string()
                    .as_bytes(),
            )
        }
        "FILLTEXT" => {
            let index: usize = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(usize::MAX);
            let vlen: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let value = read_exact(stdin, vlen, MAX_VALUE_BYTES)?;
            let Some(doc) = doc.as_mut() else {
                anyhow::bail!("no document open");
            };
            let value = String::from_utf8_lossy(&value);
            match doc.fill_text(index, &value) {
                Ok(()) => respond(
                    stdout,
                    serde_json::json!({"ok": true, "dirty": doc.is_dirty()})
                        .to_string()
                        .as_bytes(),
                ),
                Err(e) => {
                    let msg = format!("{e}").chars().take(256).collect::<String>();
                    respond(stdout, &err_json(&msg))
                }
            }
        }
        "FILLCHECK" => {
            let index: usize = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(usize::MAX);
            let flag: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let Some(doc) = doc.as_mut() else {
                anyhow::bail!("no document open");
            };
            match doc.fill_check(index, flag == 1) {
                Ok(()) => respond(
                    stdout,
                    serde_json::json!({"ok": true, "dirty": doc.is_dirty()})
                        .to_string()
                        .as_bytes(),
                ),
                Err(e) => {
                    let msg = format!("{e}").chars().take(256).collect::<String>();
                    respond(stdout, &err_json(&msg))
                }
            }
        }
        "FILLRADIO" => {
            let index: usize = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(usize::MAX);
            let Some(doc) = doc.as_mut() else {
                anyhow::bail!("no document open");
            };
            match doc.fill_radio(index) {
                Ok(()) => respond(
                    stdout,
                    serde_json::json!({"ok": true, "dirty": doc.is_dirty()})
                        .to_string()
                        .as_bytes(),
                ),
                Err(e) => {
                    let msg = format!("{e}").chars().take(256).collect::<String>();
                    respond(stdout, &err_json(&msg))
                }
            }
        }
        "SAVE" => {
            let Some(doc) = doc.as_ref() else {
                anyhow::bail!("no document open");
            };
            let bytes = doc
                .save_bytes()
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            respond(stdout, &bytes)
        }
        _ => anyhow::bail!("unknown command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_args_keep_spaced_paths_whole() {
        let mut parts = "3 1000 C:\\Users\\First Last\\Temp\\ferrite-pdf-1.shm".split_whitespace();
        let (page, scale, path) = page_args(&mut parts);
        assert_eq!((page, scale), (3, 1000));
        assert_eq!(path, "C:\\Users\\First Last\\Temp\\ferrite-pdf-1.shm");
    }

    #[test]
    fn page_args_tolerate_garbage_numbers() {
        let mut parts = "oops huge C:\\Temp\\a.shm".split_whitespace();
        let (page, scale, path) = page_args(&mut parts);
        assert_eq!(
            (page, scale),
            (0, 0),
            "unparseable falls back to rejectable zeros"
        );
        assert_eq!(path, "C:\\Temp\\a.shm");
    }
}
