//! Isolated PDF renderer process.

use std::io::{Read, Write};

use ferrite_archive::pdf::PdfEngine;
use ferrite_archive::shm::ShmHandle;
use tracing::info;
use zeroize::{Zeroize, Zeroizing};

/// Protocol caps: the parent is trusted but headers are validated anyway so
/// a compromised parent (or fuzzer) cannot OOM this process.
const MAX_PDF_BYTES: usize = 64 * 1024 * 1024;
const MAX_PAGE: usize = 10_000;
const MAX_PWD_BYTES: usize = 1024;
const MAX_HEADER_BYTES: usize = 1024;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    info!("pdf renderer started");

    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while std::io::stdin().read_exact(&mut byte).is_ok() {
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
    let header_str = String::from_utf8_lossy(&header);
    let mut parts = header_str.split_whitespace();
    let len: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let page: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let shm_raw = parts.next().unwrap_or("");
    let pwd_len: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    if len == 0 || len > MAX_PDF_BYTES {
        anyhow::bail!("pdf length out of bounds");
    }
    if page > MAX_PAGE {
        anyhow::bail!("page index out of range");
    }
    if pwd_len > MAX_PWD_BYTES {
        anyhow::bail!("password length out of bounds");
    }
    // Arbitrary file write closed: the bitmap lands only inside temp_dir.
    let shm_path =
        ShmHandle::validate_path(shm_raw).ok_or_else(|| anyhow::anyhow!("shm path rejected"))?;

    let mut buf = vec![0u8; len];
    std::io::stdin().read_exact(&mut buf)?;
    let mut pwd_buf = Zeroizing::new(vec![0u8; pwd_len]);
    let password: Option<Zeroizing<String>> = if pwd_len > 0 {
        std::io::stdin().read_exact(&mut pwd_buf)?;
        let s = Zeroizing::new(String::from_utf8_lossy(&pwd_buf).into_owned());
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    };
    // never log the password or the raw header
    let engine = PdfEngine::new().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let doc = engine
        .load(&buf, password.as_deref().map(|s| s.as_str()))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    drop(password);
    buf.zeroize();

    let bmp = doc
        .render_page(page, 1.0)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    std::fs::write(&shm_path, &bmp)?;
    std::io::stdout().write_all(&(bmp.len() as u32).to_le_bytes())?;
    std::io::stdout().write_all(&bmp)?;
    std::io::stdout().flush()?;
    Ok(())
}
