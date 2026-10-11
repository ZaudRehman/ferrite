//! SQLite store for snapshots, quarantine rows, and the FTS5 index.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::error::{ArchiveError, ArchiveResult};

#[derive(Debug, Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
}

impl Storage {
    /// Open (or create) the file store under `key`. Mirrors the vault
    /// contract: legacy pre-SQLCipher files are rejected, never silently
    /// re-encrypted; a configured key without the `sqlcipher` feature
    /// fails fast instead of writing plaintext; wrong keys fail closed.
    pub fn open(path: impl AsRef<Path>, key: &[u8]) -> ArchiveResult<Self> {
        let p = path.as_ref();
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            }
        }
        // Header triage, mirroring the vault: only plaintext SQLite files
        // are identifiable by magic. Opaque bytes may be SQLCipher (whose
        // header page is encrypted too), legacy-XOR, or corruption: only a
        // key attempt distinguishes them, so magic alone never decides.
        // Plaintext files are refused (never silently adopted); opaque
        // files without a key to verify them are refused with guidance.
        let mut opaque_without_key = false;
        if p.exists() {
            if let Ok(data) = std::fs::read(p) {
                if !data.is_empty() && !data.starts_with(b"SQLite format 3\0") {
                    if key.is_empty() {
                        opaque_without_key = true;
                    }
                } else if !data.is_empty() {
                    return Err(ArchiveError::Storage(format!(
                        "legacy plaintext archive file at {}: move it aside to start fresh",
                        p.display()
                    )));
                }
            }
        }
        if opaque_without_key {
            return Err(ArchiveError::Storage(format!(
                "unreadable archive file at {} with no key to verify it (legacy format or wrong setup): move it aside to start fresh",
                p.display()
            )));
        }
        if !key.is_empty() && key.len() != 32 {
            return Err(ArchiveError::Crypto(
                "archive key must be exactly 32 bytes".to_owned(),
            ));
        }
        if !key.is_empty() && !cfg!(feature = "sqlcipher") {
            return Err(ArchiveError::Crypto(
                "archive key configured but built without `sqlcipher` feature: refusing plaintext fallback (build with --features sqlcipher)".to_owned(),
            ));
        }
        let conn = Connection::open(p).map_err(|e| ArchiveError::Storage(e.to_string()))?;
        if !key.is_empty() {
            let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
            let pragma = format!("PRAGMA key = \"x'{hex}'\"; PRAGMA cipher_compatibility = 4;");
            conn.execute_batch(&pragma)
                .map_err(|e| ArchiveError::Storage(format!("sqlcipher key: {e}")))?;
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| ArchiveError::Storage(format!("sqlcipher key rejected (NOTADB): {e}")))?;
        }
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;");
        let ok: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| ArchiveError::Storage(format!("integrity check failed: {e}")))?;
        if !ok.eq_ignore_ascii_case("ok") {
            return Err(ArchiveError::Storage(format!("database corrupt: {ok}")));
        }
        let s = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        s.init_schema()?;
        Ok(s)
    }

    pub fn open_in_memory() -> ArchiveResult<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let s = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        s.init_schema()?;
        Ok(s)
    }

    /// Re-encrypt this database under `new_key` (key rotation). Same
    /// contract as the vault's rekey: keyed databases and the `sqlcipher`
    /// feature only (rekeying an unkeyed file would silently encrypt it),
    /// verified under the new key before returning. Callers track
    /// keyed-ness (the store itself retains no key).
    pub fn rekey(&self, keyed: bool, new_key: &[u8]) -> ArchiveResult<()> {
        if new_key.len() != 32 {
            return Err(ArchiveError::Crypto(
                "rotation key must be exactly 32 bytes".to_owned(),
            ));
        }
        if !keyed {
            return Err(ArchiveError::Crypto(
                "rekey refused: database is not keyed (would encrypt plaintext)".to_owned(),
            ));
        }
        #[cfg(not(feature = "sqlcipher"))]
        {
            Err(ArchiveError::Crypto(
                "rekey requires the `sqlcipher` feature".to_owned(),
            ))
        }
        #[cfg(feature = "sqlcipher")]
        {
            let hex: String = new_key.iter().map(|b| format!("{b:02x}")).collect();
            let conn = self
                .conn
                .lock()
                .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            conn.execute_batch(&format!("PRAGMA rekey = \"x'{hex}'\";"))
                .map_err(|e| ArchiveError::Storage(format!("sqlcipher rekey: {e}")))?;
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| ArchiveError::Storage(format!("rekey verify failed: {e}")))?;
            Ok(())
        }
    }

    fn init_schema(&self) -> ArchiveResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        conn.execute_batch(
            r#"
            -- Incremental auto-vacuum for databases created from here on
            -- (same discipline as the vault: freed snapshot pages are
            -- tracked for `incremental_vacuum`, existing files convert
            -- on first fragmented open via `reclaim_space`).
            PRAGMA auto_vacuum=INCREMENTAL;
            CREATE TABLE IF NOT EXISTS web_snapshots(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                headers TEXT,
                zstd_blob BLOB NOT NULL,
                size INTEGER NOT NULL,
                is_pinned INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_snapshots_url_ts ON web_snapshots(url, timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_snapshots_pinned ON web_snapshots(is_pinned);

            CREATE TABLE IF NOT EXISTS web_snapshots_unsanitized(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                headers TEXT,
                zstd_blob BLOB NOT NULL,
                size INTEGER NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS web_snapshots_fts USING fts5(
                url, content, snapshot_id UNINDEXED, tokenize='porter unicode61'
            );
            "#,
        )
        .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Reclaim free pages (same contract as the vault's `reclaim_space`:
    /// bounded sweep, one-time conversion rebuild only when fragmented).
    /// Snapshots evict oldest-first elsewhere; this only compacts what
    /// eviction and expiry already freed.
    pub fn reclaim_space(&self) -> ArchiveResult<usize> {
        const CONVERT_FREELIST_PAGES: i64 = 2000;
        const SWEEP_PAGES: u32 = 500;
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let mode: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        if mode != 2 {
            let free: i64 = conn
                .query_row("PRAGMA freelist_count", [], |r| r.get(0))
                .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            if free < CONVERT_FREELIST_PAGES {
                return Ok(0);
            }
            conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")
                .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            conn.execute_batch("VACUUM;")
                .map_err(|e| ArchiveError::Storage(format!("vacuum rebuild: {e}")))?;
        }
        let before: i64 = conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        conn.execute_batch(&format!("PRAGMA incremental_vacuum({SWEEP_PAGES});"))
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let after: i64 = conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        Ok(before.saturating_sub(after).max(0) as usize)
    }

    /// Row plus FTS row in one transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_snapshot(
        &self,
        url: &str,
        timestamp: i64,
        headers: &str,
        zstd_blob: &[u8],
        size: i64,
        is_pinned: bool,
        fts_content: &str,
    ) -> ArchiveResult<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let id: ArchiveResult<i64> = (|| {
            conn.execute(
                "INSERT INTO web_snapshots(url, timestamp, headers, zstd_blob, size, is_pinned) VALUES(?1,?2,?3,?4,?5,?6)",
                params![url, timestamp, headers, zstd_blob, size, if is_pinned {1} else {0}],
            )
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            let id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO web_snapshots_fts(url, content, snapshot_id) VALUES(?1,?2,?3)",
                params![url, fts_content, id],
            )
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            Ok(id)
        })();
        match id {
            Ok(id) => {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| ArchiveError::Storage(e.to_string()))?;
                Ok(id)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Quarantine insert. Not indexed, not served. Keeps newest 1000.
    pub fn insert_unsanitized(
        &self,
        url: &str,
        timestamp: i64,
        headers: &str,
        zstd_blob: &[u8],
        size: i64,
    ) -> ArchiveResult<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO web_snapshots_unsanitized(url, timestamp, headers, zstd_blob, size) VALUES(?1,?2,?3,?4,?5)",
            params![url, timestamp, headers, zstd_blob, size],
        )
        .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let id = conn.last_insert_rowid();
        let _ = conn.execute(
            "DELETE FROM web_snapshots_unsanitized WHERE id NOT IN (SELECT id FROM web_snapshots_unsanitized ORDER BY id DESC LIMIT 1000)",
            [],
        );
        Ok(id)
    }

    #[allow(clippy::type_complexity)]
    pub fn get_latest(
        &self,
        url: &str,
    ) -> ArchiveResult<Option<(i64, Vec<u8>, String, i64, bool)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT id, zstd_blob, headers, timestamp, is_pinned FROM web_snapshots WHERE url=?1 ORDER BY timestamp DESC LIMIT 1")
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let row = stmt
            .query_row([url], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4).unwrap_or(0) != 0,
                ))
            })
            .optional()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        Ok(row)
    }

    /// LIKE wildcards in the input are escaped.
    #[allow(clippy::type_complexity)]
    pub fn query_fuzzy_domain(
        &self,
        domain: &str,
    ) -> ArchiveResult<Option<(i64, String, Vec<u8>, String, i64)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let pattern = format!("%{}%", escape_like(domain));
        let mut stmt = conn
            .prepare("SELECT id, url, zstd_blob, headers, timestamp FROM web_snapshots WHERE url LIKE ?1 ESCAPE '\\' ORDER BY timestamp DESC LIMIT 1")
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let row = stmt
            .query_row([pattern], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .optional()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        Ok(row)
    }

    /// Tokens are quoted, so operators match literally. Limit max 100.
    pub fn search_fts(&self, query: &str, limit: usize) -> ArchiveResult<Vec<(String, String)>> {
        let escaped = escape_fts_query(query);
        if escaped.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT url, snippet(web_snapshots_fts, -1, '<b>', '</b>', '...', 10) FROM web_snapshots_fts WHERE web_snapshots_fts MATCH ?1 LIMIT ?2")
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![escaped, limit.min(100) as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| ArchiveError::Storage(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn list_for_url(&self, url: &str) -> ArchiveResult<Vec<(i64, i64)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT id, timestamp FROM web_snapshots WHERE url=?1 ORDER BY timestamp DESC")
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([url], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| ArchiveError::Storage(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> ArchiveResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        conn.execute(
            "UPDATE web_snapshots SET is_pinned=?1 WHERE id=?2",
            params![if pinned { 1 } else { 0 }, id],
        )
        .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn total_size(&self) -> ArchiveResult<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let v: i64 = conn
            .query_row("SELECT COALESCE(SUM(size),0) FROM web_snapshots", [], |r| {
                r.get(0)
            })
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        Ok(v)
    }

    /// Delete oldest unpinned rows until under budget. Pinned rows exempt.
    /// Loops victim passes (bounded) so over-budget stores larger than one
    /// 10000-row window still converge instead of reporting success over
    /// budget.
    pub fn evict_lru(&self, max_bytes: i64) -> ArchiveResult<usize> {
        const MAX_PASSES: usize = 100;
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let mut total_evicted = 0usize;
        for _ in 0..MAX_PASSES {
            let total: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(size),0) FROM web_snapshots WHERE is_pinned=0",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            if total <= max_bytes {
                return Ok(total_evicted);
            }
            let mut to_free = total - max_bytes;
            let ids: Vec<(i64, i64)> = {
                let mut stmt = conn
                    .prepare("SELECT id, size FROM web_snapshots WHERE is_pinned=0 ORDER BY timestamp ASC LIMIT 10000")
                    .map_err(|e| ArchiveError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
                    .map_err(|e| ArchiveError::Storage(e.to_string()))?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|e| ArchiveError::Storage(e.to_string()))?
            };
            let mut victims: Vec<i64> = Vec::new();
            for (id, size) in &ids {
                if to_free <= 0 {
                    break;
                }
                victims.push(*id);
                to_free -= size;
            }
            if victims.is_empty() {
                // Under-budget check above still fails but nothing is
                // evictable (e.g. zero-size rows): stop instead of spinning.
                return Ok(total_evicted);
            }
            conn.execute_batch("BEGIN IMMEDIATE;")
                .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            let mut ok = true;
            for chunk in victims.chunks(100) {
                let mut ids_sql = String::from("(");
                for (i, id) in chunk.iter().enumerate() {
                    if i > 0 {
                        ids_sql.push(',');
                    }
                    ids_sql.push_str(&id.to_string());
                }
                ids_sql.push(')');
                // Ids come from the integer scan above, never user input.
                if conn
                    .execute(
                        &format!("DELETE FROM web_snapshots WHERE id IN {ids_sql}"),
                        [],
                    )
                    .is_err()
                    || conn
                        .execute(
                            &format!(
                                "DELETE FROM web_snapshots_fts WHERE snapshot_id IN {ids_sql}"
                            ),
                            [],
                        )
                        .is_err()
                {
                    ok = false;
                    break;
                }
            }
            if ok {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| ArchiveError::Storage(e.to_string()))?;
                total_evicted += victims.len();
            } else {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(ArchiveError::Storage("evict batch failed".to_owned()));
            }
        }
        Ok(total_evicted)
    }

    pub fn delete_snapshot(&self, id: i64) -> ArchiveResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let r = (|| {
            conn.execute("DELETE FROM web_snapshots WHERE id=?1", [id])
                .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            conn.execute("DELETE FROM web_snapshots_fts WHERE snapshot_id=?1", [id])
                .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            Ok(())
        })();
        match r {
            Ok(()) => {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| ArchiveError::Storage(e.to_string()))?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn is_pinned(&self, id: i64) -> ArchiveResult<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let v: i64 = conn
            .query_row(
                "SELECT is_pinned FROM web_snapshots WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        Ok(v != 0)
    }

    pub fn export_snapshot(&self, id: i64) -> ArchiveResult<(String, Vec<u8>)> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT url, zstd_blob FROM web_snapshots WHERE id=?1")
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let row = stmt
            .query_row([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        Ok(row)
    }
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '%' || c == '_' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn escape_fts_query(query: &str) -> String {
    let mut out = String::new();
    for token in query.split_whitespace().take(32) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push('"');
        for c in token.chars().take(100) {
            if c == '"' {
                out.push_str("\"\"");
            } else {
                out.push(c);
            }
        }
        out.push('"');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_escape_is_literal() {
        assert_eq!(escape_like("100%_x\\y"), "100\\%\\_x\\\\y");
    }

    #[test]
    fn fts_escape_neutralizes_operators() {
        assert_eq!(
            escape_fts_query("carbon OR capture"),
            "\"carbon\" \"OR\" \"capture\""
        );
        assert_eq!(escape_fts_query("a*b"), "\"a*b\"");
        assert_eq!(escape_fts_query("   "), "");
        assert_eq!(escape_fts_query("say \"hi\""), "\"say\" \"\"\"hi\"\"\"");
    }

    fn temp_db(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrite_test_{}_{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("archive.db")
    }

    #[test]
    fn keyed_file_requires_sqlcipher_without_plaintext_fallback() {
        let path = temp_db("keyed");
        let dir = path.parent().unwrap().to_owned();
        let key = vec![0xAA; 32];
        #[cfg(feature = "sqlcipher")]
        {
            let s = Storage::open(&path, &key).unwrap();
            drop(s);
            let data = std::fs::read(&path).unwrap();
            assert!(!data.starts_with(b"SQLite format 3\0"));
            // Wrong key fails closed, never decodes garbage.
            assert!(Storage::open(&path, &[0xBB; 32]).is_err());
            // Same key reopens.
            assert!(Storage::open(&path, &key).is_ok());
        }
        #[cfg(not(feature = "sqlcipher"))]
        {
            let err = Storage::open(&path, &key).unwrap_err().to_string();
            assert!(err.contains("sqlcipher"), "got: {err}");
            assert!(!path.exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rekey_refuses_unkeyed_and_short_keys() {
        // Memory stores are never keyed: rekey would encrypt, so refuse.
        let s = Storage::open_in_memory().unwrap();
        let err = s.rekey(false, &[0xAA; 32]).unwrap_err().to_string();
        assert!(err.contains("not keyed"), "got: {err}");
        let err = s.rekey(true, &[0xAA; 16]).unwrap_err().to_string();
        assert!(err.contains("32 bytes"), "got: {err}");
        // Keyed without the feature fails closed (never encrypts silently).
        #[cfg(not(feature = "sqlcipher"))]
        {
            let err = s.rekey(true, &[0xAA; 32]).unwrap_err().to_string();
            assert!(err.contains("sqlcipher"), "got: {err}");
        }
    }

    #[test]
    fn legacy_non_sqlite_file_is_rejected() {
        let path = temp_db("legacy");
        let dir = path.parent().unwrap().to_owned();
        std::fs::write(&path, b"LEGACY-BLOB").unwrap();
        let err = Storage::open(&path, &[]).unwrap_err().to_string();
        assert!(err.contains("legacy"), "got: {err}");
        // Bad key length fails before touching disk.
        let err = Storage::open(&path, &[0xAA; 16]).unwrap_err().to_string();
        assert!(
            err.contains("legacy") || err.contains("32 bytes"),
            "got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reclaim_space_is_safe_on_fresh_db() {
        let s = Storage::open_in_memory().unwrap();
        // Fresh database: nothing to reclaim, never an error.
        assert_eq!(s.reclaim_space().unwrap(), 0);
    }

    #[test]
    fn evict_loops_past_single_window_until_under_budget() {
        let s = Storage::open_in_memory().unwrap();
        // 11,000 rows x 1000 bytes = 11 MB; a 500 KB budget needs 10,500
        // evictions, more than one 10000-row window. Single-pass code
        // reports success still over budget.
        for i in 0..11_000 {
            s.insert_snapshot(
                &format!("https://evict.test/{i}"),
                i,
                "",
                &[0xAA; 16],
                1000,
                false,
                "text",
            )
            .unwrap();
        }
        assert_eq!(s.total_size().unwrap(), 11_000_000);
        let evicted = s.evict_lru(500_000).unwrap();
        assert!(evicted > 10_000, "must loop past one window, got {evicted}");
        assert!(
            s.total_size().unwrap() <= 500_000,
            "must reach budget, size={}",
            s.total_size().unwrap()
        );
    }
}
