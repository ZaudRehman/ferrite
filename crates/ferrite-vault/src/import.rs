#![allow(clippy::manual_flatten)]
//! Import: Netscape Bookmark File Format and history DB parsing.

use regex::Regex;
use rusqlite::Connection;

use crate::error::{VaultError, VaultResult};
use crate::vault::Vault;

pub fn import_netscape_bookmarks(path: &str, vault: &Vault) -> VaultResult<usize> {
    let data = std::fs::read_to_string(path).map_err(|e| VaultError::Import(e.to_string()))?;
    // Very small parser: <A HREF="url">title</A>
    let re = Regex::new(r#"(?i)<a\s+href="([^"]+)"[^>]*>([^<]+)</a>"#)
        .map_err(|e| VaultError::Import(format!("bookmark pattern: {e}")))?;
    let mut count = 0;
    for cap in re.captures_iter(&data) {
        let url = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title = cap.get(2).map(|m| m.as_str()).unwrap_or(url);
        if url.is_empty() {
            continue;
        }
        let _ = vault.insert_bookmark(url, title, None);
        count += 1;
    }
    Ok(count)
}

pub fn import_chrome_history(path: &str, vault: &Vault) -> VaultResult<usize> {
    let conn = Connection::open(path).map_err(|e| VaultError::Import(e.to_string()))?;
    let mut stmt = conn
        .prepare("SELECT url, title FROM urls")
        .map_err(|e| VaultError::Import(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| VaultError::Import(e.to_string()))?;
    let mut count = 0;
    for r in rows {
        if let Ok((url, title)) = r {
            let _ = vault.insert_history(&url, Some(&title));
            count += 1;
        }
    }
    Ok(count)
}

pub fn import_firefox_history(path: &str, vault: &Vault) -> VaultResult<usize> {
    let conn = Connection::open(path).map_err(|e| VaultError::Import(e.to_string()))?;
    let mut stmt = conn
        .prepare("SELECT url, title FROM moz_places WHERE url IS NOT NULL")
        .map_err(|e| VaultError::Import(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })
        .map_err(|e| VaultError::Import(e.to_string()))?;
    let mut count = 0;
    for r in rows {
        if let Ok((url, title)) = r {
            if url.is_empty() {
                continue;
            }
            let _ = vault.insert_history(&url, Some(&title));
            count += 1;
        }
    }
    Ok(count)
}

pub fn export_bookmarks_netscape(vault: &Vault) -> String {
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n",
    );
    for entry in vault.bookmark_cache.iter() {
        let url = entry.key();
        let (_, title) = entry.value();
        out.push_str(&format!("<DT><A HREF=\"{url}\">{title}</A>\n"));
    }
    out.push_str("</DL><p>\n");
    out
}
