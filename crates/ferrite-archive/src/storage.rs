//! SQLite storage for snapshots and FTS5.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::error::{ArchiveError, ArchiveResult};

#[derive(Debug, Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
}

impl Storage {
    pub fn open(path: impl AsRef<Path>) -> ArchiveResult<Self> {
        let p = path.as_ref();
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ArchiveError::Storage(e.to_string()))?;
            }
        }
        let conn = Connection::open(p).map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;");
        // Boot integrity gate: refuse corrupt files instead of half-working.
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

    fn init_schema(&self) -> ArchiveResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        conn.execute_batch(
            r#"
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
        // Single transaction: a crash between row and FTS inserts must not
        // leave orphan rows or missing index entries.
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
        // Bounded retention: unsanitized rows were previously never evicted
        // (unbounded growth). Keep the newest 1000.
        let _ = conn.execute(
            "DELETE FROM web_snapshots_unsanitized WHERE id NOT IN (SELECT id FROM web_snapshots_unsanitized ORDER BY id DESC LIMIT 1000)",
            [],
        );
        Ok(id)
    }

    #[allow(clippy::type_complexity)]
    pub fn get_latest(&self, url: &str) -> ArchiveResult<Option<(i64, Vec<u8>, String, i64)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT id, zstd_blob, headers, timestamp FROM web_snapshots WHERE url=?1 ORDER BY timestamp DESC LIMIT 1")
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let row = stmt
            .query_row([url], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .optional()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        Ok(row)
    }

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

    /// Escape FTS5 query syntax by quoting every token as a phrase.
    /// Raw user input like `OR`, `"`, `*`, `NEAR(` otherwise throws or
    /// changes query semantics (FTS injection).
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

    pub fn evict_lru(&self, max_bytes: i64) -> ArchiveResult<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(size),0) FROM web_snapshots WHERE is_pinned=0",
                [],
                |r| r.get(0),
            )
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        if total <= max_bytes {
            return Ok(0);
        }
        let mut to_free = total - max_bytes;
        // Bound the scan: oldest-first with a 10k-row cap keeps per-ingest
        // work flat instead of loading millions of rows into a Vec.
        let mut stmt = conn
            .prepare("SELECT id, size FROM web_snapshots WHERE is_pinned=0 ORDER BY timestamp ASC LIMIT 10000")
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        let ids: Vec<(i64, i64)> = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ArchiveError::Storage(e.to_string()))?;
        drop(stmt);
        // Select victims first (oldest-first until the budget is met), then
        // delete in bounded IN-chunks inside one transaction.
        let mut victims: Vec<i64> = Vec::new();
        for (id, size) in &ids {
            if to_free <= 0 {
                break;
            }
            victims.push(*id);
            to_free -= size;
        }
        if victims.is_empty() {
            return Ok(0);
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
            // Row + FTS delete in the same transaction (no orphans). IDs
            // come from our own integer query, never user input.
            if conn
                .execute(
                    &format!("DELETE FROM web_snapshots WHERE id IN {ids_sql}"),
                    [],
                )
                .is_err()
                || conn
                    .execute(
                        &format!("DELETE FROM web_snapshots_fts WHERE snapshot_id IN {ids_sql}"),
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
            Ok(victims.len())
        } else {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(ArchiveError::Storage("evict batch failed".to_owned()))
        }
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

/// Escape LIKE wildcards so fuzzy-directory input matches literally.
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

/// Quote every whitespace-separated token as an FTS5 phrase (`"a b"`),
/// doubling embedded quotes. Operators (`OR`, `*`, `NEAR(`, `-`, column
/// filters) become literals instead of syntax. Empty/whitespace input
/// yields an empty string (caller returns no rows).
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
}
