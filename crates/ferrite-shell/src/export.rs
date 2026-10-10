//! Export-my-data: portable plaintext tarball, or passphrase-sealed backup
//! for a new device. Restore reuses the import write paths. Tar is a minimal
//! USTAR subset, no new dependencies.

/// Largest single archive this module will build (three small text files).
/// Anything bigger is a bug upstream, not a bigger export.
pub const MAX_TAR_BYTES: usize = 64 * 1024 * 1024;

/// One history row in a backup envelope. `visited_ms`/`visits` are
/// optional so backups written before timestamps existed still restore
/// (the row lands with "now", exactly like the old behavior).
#[derive(Debug, Clone)]
pub struct HistoryRow {
    pub url: String,
    pub title: String,
    pub visited_ms: Option<i64>,
    pub visits: i64,
}

/// Parts of a native backup envelope. Shapes mirror the portable formats
/// so restore reuses the import write paths instead of inventing new ones.
pub struct EnvelopeParts {
    pub created_ms: i64,
    pub bookmarks_html: String,
    pub passwords: Vec<(String, String, String)>,
    pub history: Vec<HistoryRow>,
    pub cards: Vec<serde_json::Value>,
    pub addresses: Vec<serde_json::Value>,
    pub settings: serde_json::Value,
}

/// Assemble the envelope object: still plaintext until the caller seals it
/// with `ferrite_sync::backup::seal_backup_file` before it touches disk.
pub fn build_envelope(parts: &EnvelopeParts) -> serde_json::Value {
    serde_json::json!({
        "format": ferrite_sync::backup::BACKUP_FILE_FORMAT,
        "created_ms": parts.created_ms,
        "bookmarks_html": parts.bookmarks_html,
        "passwords": parts.passwords.iter().map(|(u, n, _)| {
            serde_json::json!({"url": u, "username": n})
        }).collect::<Vec<_>>(),
        // Passwords travel separately below so the metadata array stays
        // safe to log. The secrets array is the sensitive half.
        "secrets": parts.passwords.iter().map(|(_, _, p)| {
            serde_json::json!({"password": p})
        }).collect::<Vec<_>>(),
        "history": parts.history.iter().map(|h| {
            serde_json::json!({"url": h.url, "title": h.title, "visited_ms": h.visited_ms, "visits": h.visits})
        }).collect::<Vec<_>>(),
        "cards": parts.cards,
        "addresses": parts.addresses,
        "settings": parts.settings,
    })
}

pub struct SplitPasswords {
    pub rows: Vec<(String, String)>,
    pub secrets: Vec<String>,
}

/// Split an envelope back into metadata rows and secret rows (indices
/// align: `passwords[i]` pairs with `secrets[i]`).
pub fn split_envelope(env: &serde_json::Value) -> Result<SplitPasswords, String> {
    if env.get("format").and_then(|f| f.as_str()) != Some(ferrite_sync::backup::BACKUP_FILE_FORMAT)
    {
        return Err("not a Ferrite backup envelope".to_owned());
    }
    let meta = env
        .get("passwords")
        .and_then(|p| p.as_array())
        .ok_or("backup envelope has no passwords")?;
    let secrets = env
        .get("secrets")
        .and_then(|s| s.as_array())
        .ok_or("backup envelope has no secrets")?;
    if meta.len() != secrets.len() || meta.len() > 10_000 {
        return Err("backup envelope passwords are malformed".to_owned());
    }
    let mut rows = Vec::with_capacity(meta.len());
    let mut pws = Vec::with_capacity(secrets.len());
    for (m, s) in meta.iter().zip(secrets.iter()) {
        let url = m
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or("backup envelope password lacks url")?;
        let username = m
            .get("username")
            .and_then(|u| u.as_str())
            .ok_or("backup envelope password lacks username")?;
        let pw = s
            .get("password")
            .and_then(|p| p.as_str())
            .ok_or("backup envelope secret lacks password")?;
        rows.push((url.to_owned(), username.to_owned()));
        pws.push(pw.to_owned());
    }
    Ok(SplitPasswords { rows, secrets: pws })
}

/// Minimal USTAR writer (regular files only, no pax, no links). Names are
/// short ASCII-relative paths. Sizes bounded by [`MAX_TAR_BYTES`].
pub fn write_tar(files: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
    let mut total: usize = 0;
    for (name, data) in files {
        archive_name(name)?;
        total = total
            .checked_add(data.len())
            .filter(|&t| t <= MAX_TAR_BYTES)
            .ok_or("export too large")?;
    }
    let mut out = Vec::new();
    for (name, data) in files {
        out.extend_from_slice(&tar_header(name, data.len() as u64)?);
        out.extend_from_slice(data);
        let pad = (512 - data.len() % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    // Two zero blocks terminate the archive.
    out.extend(std::iter::repeat_n(0u8, 1024));
    Ok(out)
}

fn archive_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 90 || name.starts_with('/') || name.contains("..") {
        return Err(format!("bad archive name: {name}"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
    {
        return Err(format!("bad archive name: {name}"));
    }
    Ok(())
}

fn octal(buf: &mut [u8], value: u64) {
    // NUL-terminated octal, right-aligned in the field.
    let s = format!("{value:o}");
    let digits = s.as_bytes();
    let start = buf.len().saturating_sub(digits.len() + 1);
    buf[start..start + digits.len()].copy_from_slice(digits);
}

fn tar_header(name: &str, size: u64) -> Result<[u8; 512], String> {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], 0o644);
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], 0);
    h[156] = b'0';
    h[257..262].copy_from_slice(b"ustar");
    h[263..265].copy_from_slice(b"00");
    // Checksum over the header with the checksum field blank (spaces).
    for b in h[148..156].iter_mut() {
        *b = b' ';
    }
    let sum: u64 = h.iter().map(|&b| b as u64).sum();
    octal(&mut h[148..156], sum);
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_tar(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
        // Minimal reader, just enough to verify the writer round-trips.
        let mut out = Vec::new();
        let mut off = 0;
        while off + 512 <= archive.len() {
            let h = &archive[off..off + 512];
            if h.iter().all(|&b| b == 0) {
                break;
            }
            let end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
            let name = String::from_utf8(h[..end].to_vec()).unwrap();
            let size_str = String::from_utf8(h[124..136].to_vec()).unwrap();
            let size = usize::from_str_radix(size_str.trim_matches(['\0', ' ']), 8).unwrap();
            let data = archive[off + 512..off + 512 + size].to_vec();
            // Verify the stored checksum.
            let stored = usize::from_str_radix(
                String::from_utf8(h[148..156].to_vec())
                    .unwrap()
                    .trim_matches(['\0', ' ']),
                8,
            )
            .unwrap();
            let mut blanked = h.to_vec();
            for b in blanked[148..156].iter_mut() {
                *b = b' ';
            }
            let sum: usize = blanked.iter().map(|&b| b as usize).sum();
            assert_eq!(stored, sum, "checksum for {name}");
            out.push((name, data));
            off += 512 + size.div_ceil(512) * 512;
        }
        out
    }

    #[test]
    fn tar_round_trips_and_terminates() {
        let files: &[(&str, &[u8])] = &[
            ("ferrite-bookmarks.html", b"<DL><p>" as &[u8]),
            ("ferrite-passwords.csv", b"url\n" as &[u8]),
            ("ferrite-settings.json", b"{}" as &[u8]),
        ];
        let archive = write_tar(files).unwrap();
        assert!(archive.len().is_multiple_of(512));
        assert!(archive.ends_with(&[0u8; 1024]));
        let back = read_tar(&archive);
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].0, "ferrite-bookmarks.html");
        assert_eq!(back[0].1, b"<DL><p>");
        assert_eq!(back[2].1, b"{}");
    }

    #[test]
    fn tar_rejects_bad_names() {
        assert!(write_tar(&[("/abs", b"x")]).is_err());
        assert!(write_tar(&[("../esc", b"x")]).is_err());
        assert!(write_tar(&[("sp ace", b"x")]).is_err());
        assert!(write_tar(&[("", b"x")]).is_err());
        assert!(write_tar(&[("ok-name_v1.json", b"x")]).is_ok());
    }

    #[test]
    fn envelope_splits_rows_from_secrets() {
        let parts = EnvelopeParts {
            created_ms: 1,
            bookmarks_html: String::new(),
            passwords: vec![("https://a.test".to_owned(), "u".to_owned(), "p".to_owned())],
            history: vec![HistoryRow {
                url: "https://h.test".to_owned(),
                title: "H".to_owned(),
                visited_ms: Some(1_700_000_000_000),
                visits: 3,
            }],
            cards: Vec::new(),
            addresses: Vec::new(),
            settings: serde_json::json!({}),
        };
        let env = build_envelope(&parts);
        // Metadata array carries no secret. Secrets array does.
        assert!(env["passwords"][0].get("password").is_none());
        assert_eq!(env["secrets"][0]["password"], "p");
        // History keeps its timestamp and count through the envelope.
        assert_eq!(
            env["history"][0]["visited_ms"].as_i64(),
            Some(1_700_000_000_000)
        );
        assert_eq!(env["history"][0]["visits"].as_i64(), Some(3));
        let split = split_envelope(&env).unwrap();
        assert_eq!(
            split.rows,
            vec![("https://a.test".to_owned(), "u".to_owned())]
        );
        assert_eq!(split.secrets, vec!["p".to_owned()]);
        let mut bad = env.clone();
        bad["secrets"] = serde_json::json!([]);
        assert!(split_envelope(&bad).is_err());
    }
}
