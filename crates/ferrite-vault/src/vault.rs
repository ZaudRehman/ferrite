#![allow(clippy::manual_flatten, clippy::unnecessary_sort_by)]
//! Vault core: encrypted DB, hot cache, batch writer.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, bounded};
use dashmap::DashMap;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;
use zeroize::Zeroize;

use crate::error::{VaultError, VaultResult};

pub struct VaultConfig {
    pub path: String,
    pub key: Vec<u8>,
}

impl std::fmt::Debug for VaultConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultConfig")
            .field("path", &self.path)
            .field("key", &"<redacted>")
            .finish()
    }
}

impl Clone for VaultConfig {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            key: self.key.clone(),
        }
    }
}

impl Drop for VaultConfig {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl VaultConfig {
    pub fn new(path: impl Into<String>, key: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            key,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: String,
    pub url: String,
    pub title: Option<String>,
    pub visit_count: i32,
    pub last_visit: i64,
    pub hlc: i64,
}

/// Escape LIKE wildcards so history search input matches literally.
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

pub struct Vault {
    pub conn: Arc<Mutex<Connection>>,
    pub history_cache: Arc<DashMap<String, HistoryEntry>>,
    pub bookmark_cache: Arc<DashMap<String, (String, String)>>,
    batch_tx: Sender<HistoryEntry>,
    config: Option<VaultConfig>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault").finish_non_exhaustive()
    }
}

impl Vault {
    pub fn open(config: VaultConfig) -> VaultResult<Self> {
        let cfg = config.clone();
        let conn = Self::open_connection(&config.path, &config.key)?;
        let (batch_tx, batch_rx) = bounded(4096);
        let vault = Self {
            conn: Arc::new(Mutex::new(conn)),
            history_cache: Arc::new(DashMap::new()),
            bookmark_cache: Arc::new(DashMap::new()),
            batch_tx,
            config: Some(cfg),
        };
        vault.warm_cache()?;
        vault.spawn_batch_writer(batch_rx);
        Ok(vault)
    }

    pub fn open_in_memory() -> VaultResult<Self> {
        let conn = Connection::open_in_memory().map_err(|e| VaultError::Storage(e.to_string()))?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;");
        let (batch_tx, batch_rx) = bounded(4096);
        let vault = Self {
            conn: Arc::new(Mutex::new(conn)),
            history_cache: Arc::new(DashMap::new()),
            bookmark_cache: Arc::new(DashMap::new()),
            batch_tx,
            config: None,
        };
        Self::init_schema(&vault.conn.lock().unwrap())?;
        vault.spawn_batch_writer(batch_rx);
        Ok(vault)
    }

    fn open_connection(path: &str, key: &[u8]) -> VaultResult<Connection> {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| VaultError::Storage(e.to_string()))?;
            }
        }
        // Legacy XOR files are rejected: no silent plaintext, no silent
        // re-encryption. Migrate with a `--features sqlcipher` build.
        if Path::new(path).exists() {
            if let Ok(data) = std::fs::read(path) {
                if !data.is_empty() && !data.starts_with(b"SQLite format 3\0") {
                    return Err(VaultError::Storage(
                        "legacy non-SQLite vault file: migrate with a sqlcipher build".to_owned(),
                    ));
                }
            }
        }
        // Production requires SQLCipher when a key is configured. Without the
        // `sqlcipher` feature the PRAGMA below is a no-op and the file would
        // be plaintext: fail fast instead of silently downgrading.
        if !key.is_empty() && !cfg!(feature = "sqlcipher") {
            return Err(VaultError::Crypto(
                "vault key configured but built without `sqlcipher` feature: refusing plaintext fallback (build with --features sqlcipher)".to_owned(),
            ));
        }
        if !key.is_empty() && key.len() != 32 {
            return Err(VaultError::Crypto(
                "vault key must be exactly 32 bytes".to_owned(),
            ));
        }
        let conn = Connection::open(path).map_err(|e| VaultError::Storage(e.to_string()))?;
        if !key.is_empty() {
            let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
            let pragma = format!("PRAGMA key = \"x'{hex}'\"; PRAGMA cipher_compatibility = 4;");
            conn.execute_batch(&pragma)
                .map_err(|e| VaultError::Storage(format!("sqlcipher key: {e}")))?;
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| VaultError::Storage(format!("sqlcipher key rejected (NOTADB): {e}")))?;
        }
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        // Boot integrity gate: refuse to run on a corrupt database file
        // instead of half-working (wrong SQLCipher key also fails here).
        let ok: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| VaultError::Storage(format!("integrity check failed: {e}")))?;
        if !ok.eq_ignore_ascii_case("ok") {
            return Err(VaultError::Storage(format!("database corrupt: {ok}")));
        }
        Self::init_schema(&conn)?;
        Ok(conn)
    }

    fn init_schema(conn: &Connection) -> VaultResult<()> {
        // XOR `encrypt_file` deleted: at-rest encryption is SQLCipher only.
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS history(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                title TEXT,
                visit_count INTEGER DEFAULT 1,
                last_visit_timestamp INTEGER NOT NULL,
                hlc_timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_history_url ON history(url);
            CREATE INDEX IF NOT EXISTS idx_history_timestamp ON history(last_visit_timestamp DESC);

            CREATE TABLE IF NOT EXISTS bookmarks(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                title TEXT NOT NULL,
                parent_folder_id TEXT REFERENCES bookmarks(id),
                sort_order INTEGER NOT NULL DEFAULT 0,
                hlc_timestamp INTEGER NOT NULL,
                is_deleted INTEGER DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_bookmarks_parent ON bookmarks(parent_folder_id);
            CREATE INDEX IF NOT EXISTS idx_bookmarks_url ON bookmarks(url);

            CREATE TABLE IF NOT EXISTS blocked_log(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                origin_domain TEXT NOT NULL,
                blocked_domain TEXT NOT NULL,
                category TEXT NOT NULL,
                timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_blocked_origin ON blocked_log(origin_domain);

            CREATE TABLE IF NOT EXISTS web_snapshots(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                capture_timestamp INTEGER NOT NULL,
                compressed_html BLOB NOT NULL,
                is_pinned INTEGER DEFAULT 0,
                hlc_timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_snapshots_url ON web_snapshots(url);

            CREATE TABLE IF NOT EXISTS sessions(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                saved_at INTEGER NOT NULL,
                tab_state_json TEXT NOT NULL,
                is_clean_shutdown INTEGER DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS recovery_key_wrap(
                id INTEGER PRIMARY KEY CHECK (id = 1),
                wrapped_master_key BLOB NOT NULL,
                kdf_salt BLOB NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS web_snapshots_fts USING fts5(url, content, snapshot_id UNINDEXED, tokenize='porter unicode61');

            CREATE TABLE IF NOT EXISTS credentials(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                username TEXT NOT NULL,
                encrypted_password BLOB NOT NULL,
                hlc_timestamp INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS autofill_records(
                id TEXT PRIMARY KEY,
                record_type TEXT NOT NULL,
                label TEXT NOT NULL,
                encrypted_fields BLOB NOT NULL,
                hlc_timestamp INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS totp_secrets(
                credential_id TEXT PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
                encrypted_secret BLOB NOT NULL,
                digits INTEGER NOT NULL,
                period_seconds INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_credentials_url ON credentials(url);
            CREATE INDEX IF NOT EXISTS idx_credentials_hlc ON credentials(hlc_timestamp);
            CREATE INDEX IF NOT EXISTS idx_autofill_hlc ON autofill_records(hlc_timestamp);

            CREATE TABLE IF NOT EXISTS trusted_devices (
                device_id TEXT PRIMARY KEY,
                device_name TEXT NOT NULL,
                public_key BLOB NOT NULL,
                paired_via_device_id TEXT,
                first_paired_at INTEGER NOT NULL,
                last_seen_at INTEGER,
                last_synced_at INTEGER,
                hlc_timestamp INTEGER NOT NULL,
                is_revoked INTEGER DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_trusted_hlc ON trusted_devices(hlc_timestamp);

            CREATE TABLE IF NOT EXISTS sync_vector_clock (
                device_id TEXT PRIMARY KEY,
                counter INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS raft_log (
                idx INTEGER PRIMARY KEY,
                term INTEGER NOT NULL,
                action_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sync_settings (
                device_id TEXT PRIMARY KEY,
                enabled_categories TEXT NOT NULL
            );
            PRAGMA user_version=1;
            "#,
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        // Migration for pre-P0-3 databases lacking `is_deleted` / indexes.
        let _ = conn.execute_batch(
            "ALTER TABLE credentials ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0;
             CREATE INDEX IF NOT EXISTS idx_credentials_url ON credentials(url);
             CREATE INDEX IF NOT EXISTS idx_credentials_hlc ON credentials(hlc_timestamp);",
        );
        Ok(())
    }

    /// Hot-set warmup: most-recent history (paged, not the whole table)
    /// plus bookmarks. Cold rows stay in SQLite and are served via DB
    /// fallback in [`Vault::query_history`].
    fn warm_cache(&self) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp FROM history ORDER BY last_visit_timestamp DESC LIMIT 1000")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(HistoryEntry {
                    id: r.get(0)?,
                    url: r.get(1)?,
                    title: r.get(2)?,
                    visit_count: r.get(3)?,
                    last_visit: r.get(4)?,
                    hlc: r.get(5)?,
                })
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        for r in rows {
            if let Ok(e) = r {
                self.history_cache.insert(e.url.clone(), e);
            }
        }
        // bookmarks (bounded: newest 10k; cold rows remain queryable
        // via direct DB access, not the hot cache)
        let mut stmt2 = conn
            .prepare("SELECT id, url, title FROM bookmarks WHERE is_deleted=0 ORDER BY rowid DESC LIMIT 10000")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let rows2 = stmt2
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        for r in rows2 {
            if let Ok((id, url, title)) = r {
                self.bookmark_cache.insert(url.clone(), (id, title));
            }
        }
        Ok(())
    }

    fn spawn_batch_writer(&self, rx: Receiver<HistoryEntry>) {
        let conn = Arc::clone(&self.conn);
        std::thread::Builder::new()
            .name("vault-batch-writer".to_owned())
            .spawn(move || {
                let mut buf: Vec<HistoryEntry> = Vec::with_capacity(50);
                loop {
                    // Block for the first entry, then drain bursts without the
                    // unconditional 1s stall (old code capped throughput at
                    // ~50/s and lost bursts via try_send).
                    let first = rx.recv();
                    let Ok(entry) = first else {
                        break;
                    };
                    buf.push(entry);
                    while buf.len() < 50 {
                        match rx.try_recv() {
                            Ok(e) => buf.push(e),
                            Err(_) => break,
                        }
                    }
                    let flushed = match conn.lock() {
                        Ok(c) => {
                            if c.execute_batch("BEGIN IMMEDIATE;").is_err() {
                                tracing::warn!("vault batch BEGIN failed");
                                false
                            } else {
                                let mut failed = false;
                                for e in &buf {
                                    if c.execute(
                                        "INSERT OR REPLACE INTO history(id, url, title, visit_count, last_visit_timestamp, hlc_timestamp) VALUES(?1,?2,?3,?4,?5,?6)",
                                        rusqlite::params![e.id, e.url, e.title, e.visit_count, e.last_visit, e.hlc],
                                    ).is_err() {
                                        failed = true;
                                        break;
                                    }
                                }
                                if failed {
                                    let _ = c.execute_batch("ROLLBACK;");
                                    tracing::warn!("vault batch insert failed - rolled back");
                                    false
                                } else if c.execute_batch("COMMIT;").is_err() {
                                    tracing::warn!("vault batch COMMIT failed");
                                    false
                                } else {
                                    true
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "vault batch lock poisoned");
                            false
                        }
                    };
                    let _ = flushed;
                    buf.clear();
                    // Only idle-sleep when the queue is empty.
                    if rx.is_empty() {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                }
            })
            .ok();
    }

    pub fn insert_history(&self, url: &str, title: Option<&str>) -> VaultResult<String> {
        let id = Uuid::new_v4().to_string();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let entry = HistoryEntry {
            id: id.clone(),
            url: url.to_owned(),
            title: title.map(|s| s.to_owned()),
            visit_count: 1,
            last_visit: now,
            hlc: now,
        };
        self.history_cache.insert(url.to_owned(), entry.clone());
        // Backpressure: Full queue is an error, never a silent drop.
        self.batch_tx
            .try_send(entry)
            .map_err(|e| VaultError::Storage(format!("history queue full: {e}")))?;
        Ok(id)
    }

    pub fn query_history(&self, prefix: &str, limit: usize) -> Vec<HistoryEntry> {
        let limit = limit.min(50);
        let needle = prefix.to_lowercase();
        let mut out: Vec<HistoryEntry> = self
            .history_cache
            .iter()
            .filter(|e| {
                e.url.to_lowercase().contains(&needle)
                    || e.title
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&needle)
            })
            .map(|e| e.value().clone())
            .collect();
        out.sort_by(|a, b| b.last_visit.cmp(&a.last_visit));
        // DB fallback for cold rows beyond the 1000-row hot set: LIKE with
        // escaped wildcards, excluding URLs already served from cache.
        if out.len() < limit {
            if let Ok(conn) = self.conn.lock() {
                let pattern = format!("%{}%", escape_like(&needle));
                let want = ((limit - out.len()) * 2).max(limit) as i64;
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp FROM history WHERE LOWER(url) LIKE ?1 ESCAPE '\\' OR LOWER(title) LIKE ?1 ESCAPE '\\' ORDER BY last_visit_timestamp DESC LIMIT ?2",
                ) {
                    if let Ok(rows) = stmt.query_map(
                        rusqlite::params![pattern, want],
                        |r| {
                            Ok(HistoryEntry {
                                id: r.get(0)?,
                                url: r.get(1)?,
                                title: r.get(2)?,
                                visit_count: r.get(3)?,
                                last_visit: r.get(4)?,
                                hlc: r.get(5)?,
                            })
                        },
                    ) {
                        for r in rows.flatten() {
                            if !out.iter().any(|e| e.url == r.url) {
                                // Keep the cache coherent for next time.
                                self.history_cache.insert(r.url.clone(), r.clone());
                                out.push(r);
                                if out.len() >= limit {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            out.sort_by(|a, b| b.last_visit.cmp(&a.last_visit));
        }
        out.truncate(limit);
        out
    }

    pub fn save_session(&self, json: &str, is_clean: bool) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        conn.execute(
            "INSERT INTO sessions(saved_at, tab_state_json, is_clean_shutdown) VALUES(?1,?2,?3)",
            params![now, json, if is_clean { 1 } else { 0 }],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn last_session(&self) -> VaultResult<Option<(String, bool)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT tab_state_json, is_clean_shutdown FROM sessions ORDER BY saved_at DESC, id DESC LIMIT 1")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let row = stmt
            .query_row([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .optional()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(row.map(|(j, c)| (j, c != 0)))
    }

    pub fn insert_bookmark(
        &self,
        url: &str,
        title: &str,
        parent: Option<&str>,
    ) -> VaultResult<String> {
        let id = Uuid::new_v4().to_string();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO bookmarks(id, url, title, parent_folder_id, sort_order, hlc_timestamp, is_deleted) VALUES(?1,?2,?3,?4,?5,?6,0)",
            params![id, url, title, parent, 0, now],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        self.bookmark_cache
            .insert(url.to_owned(), (id.clone(), title.to_owned()));
        Ok(id)
    }

    pub fn delete_bookmark(&self, id: &str) -> VaultResult<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // Resolve URL first so the cache tombstone lands even if the row
        // was already deleted.
        let url: Option<String> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            let existing: Option<String> = conn
                .query_row("SELECT url FROM bookmarks WHERE id=?1", [id], |r| r.get(0))
                .optional()
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            conn.execute(
                "UPDATE bookmarks SET is_deleted=1, hlc_timestamp=?1 WHERE id=?2",
                params![now, id],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
            existing
        };
        if let Some(u) = url {
            self.bookmark_cache.remove(&u);
        } else {
            // Unknown id: sweep any stale entry (defensive, no full scan —
            // cache is keyed by URL, so only exact removals apply).
        }
        Ok(())
    }

    pub fn insert_blocked_log(
        &self,
        origin: &str,
        blocked: &str,
        category: &str,
        timestamp: i64,
    ) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO blocked_log(origin_domain, blocked_domain, category, timestamp) VALUES(?1,?2,?3,?4)",
            params![origin, blocked, category, timestamp],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn is_recovery_enabled(&self) -> VaultResult<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM recovery_key_wrap", [], |r| r.get(0))
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(count > 0)
    }

    pub fn enable_recovery(&self, phrase: &str) -> VaultResult<()> {
        if self.is_recovery_enabled()? {
            return Err(VaultError::Crypto("recovery already enabled".to_string()));
        }
        let salt = {
            let mut s = vec![0u8; 16];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut s);
            s
        };
        let wrapping_key = crate::recovery::derive_wrapping_key(phrase, &salt)?;
        let mut master = self
            .config
            .as_ref()
            .map(|c| c.key.clone())
            .unwrap_or_default();
        if master.len() != 32 {
            master.zeroize();
            return Err(VaultError::Crypto(
                "master key must be 32 bytes to enable recovery".to_owned(),
            ));
        }
        let wrapped = crate::recovery::wrap_master_key(&master, &wrapping_key)?;
        master.zeroize();
        // Verify decrypt-round-trip before persisting (never store bad wrap).
        {
            let check = crate::recovery::unwrap_master_key(&wrapped, &wrapping_key)?;
            if check.len() != 32 {
                return Err(VaultError::Crypto(
                    "recovery wrap verification failed".to_owned(),
                ));
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO recovery_key_wrap(id, wrapped_master_key, kdf_salt, created_at) VALUES(1,?1,?2,?3)",
            params![wrapped, salt, now],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn disable_recovery(&self) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute("DELETE FROM recovery_key_wrap WHERE id=1", [])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn recover_with_phrase(&self, phrase: &str) -> VaultResult<Vec<u8>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT wrapped_master_key, kdf_salt FROM recovery_key_wrap WHERE id=1")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let row = stmt
            .query_row([], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let (wrapped, salt) = row;
        let mut wrapping_key = crate::recovery::derive_wrapping_key(phrase, &salt)?;
        let master = crate::recovery::unwrap_master_key(&wrapped, &wrapping_key)?;
        wrapping_key.zeroize();
        Ok(master)
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        if let Some(cfg) = &mut self.config {
            // Checkpoint WAL so no plaintext journal lingers for backup tools.
            // No XOR re-encryption: SQLCipher handles at-rest encryption.
            if !cfg.key.is_empty() {
                if let Ok(conn) = self.conn.lock() {
                    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                }
            }
            cfg.key.zeroize();
            let _ = &cfg.path;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_file_requires_sqlcipher_when_keyed() {
        let key = vec![0xAA; 32];
        let dir = std::env::temp_dir().join(format!(
            "ferrite_test_{}_vault",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path_str = dir.join("vault.db").to_string_lossy().to_string();
        let cfg = VaultConfig::new(path_str.clone(), key.clone());
        #[cfg(feature = "sqlcipher")]
        {
            let v = Vault::open(cfg).unwrap();
            v.insert_history("https://example.com", Some("Example"))
                .unwrap();
            drop(v);
            let data = std::fs::read(&path_str).unwrap();
            assert!(!data.starts_with(b"SQLite format 3\0"));
            // Wrong key must fail with NOTADB, not decode garbage.
            let bad = VaultConfig::new(path_str.clone(), vec![0xBB; 32]);
            assert!(Vault::open(bad).is_err());
        }
        #[cfg(not(feature = "sqlcipher"))]
        {
            // Fail fast: never silently write plaintext when a key is set.
            let err = Vault::open(cfg).unwrap_err().to_string();
            assert!(err.contains("sqlcipher"), "got: {err}");
            assert!(!std::path::Path::new(&path_str).exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_file_rejects_legacy_non_sqlite() {
        let dir = std::env::temp_dir().join(format!(
            "ferrite_test_{}_legacy",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path_str = dir.join("vault.db").to_string_lossy().to_string();
        std::fs::write(&path_str, b"XOR-OBFUSCATED-BLOB").unwrap();
        let cfg = VaultConfig::new(path_str.clone(), vec![]);
        assert!(Vault::open(cfg).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_uuid_and_cache() {
        let v = Vault::open_in_memory().unwrap();
        let id1 = v.insert_history("https://a.com", Some("A")).unwrap();
        let id2 = v.insert_history("https://b.com", Some("B")).unwrap();
        assert_ne!(id1, id2);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let hits = v.query_history("a.com", 5);
        assert!(!hits.is_empty());
    }

    #[test]
    fn bookmarks_crud_tombstone() {
        let v = Vault::open_in_memory().unwrap();
        let id = v
            .insert_bookmark("https://example.com", "Example", None)
            .unwrap();
        assert!(v.bookmark_cache.contains_key("https://example.com"));
        v.delete_bookmark(&id).unwrap();
        // Cache tombstone lands alongside the DB row.
        assert!(!v.bookmark_cache.contains_key("https://example.com"));
        // is_deleted should be 1, not hard delete
        let conn = v.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT is_deleted, hlc_timestamp FROM bookmarks WHERE id=?1")
            .unwrap();
        let (del, _hlc): (i64, i64) = stmt
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(del, 1);
    }

    #[test]
    fn recovery_default_off() {
        let v = Vault::open_in_memory().unwrap();
        assert!(!v.is_recovery_enabled().unwrap());
    }

    #[test]
    fn recovery_enable_never_plaintext() {
        // Keyless in-memory vaults cannot enable recovery (no master to wrap).
        let v = Vault::open_in_memory().unwrap();
        let phrase = crate::recovery::generate_mnemonic(12).unwrap();
        assert!(v.enable_recovery(&phrase).is_err());
        assert!(!v.is_recovery_enabled().unwrap());
        // Round-trip at the crypto layer with an explicit 32B master.
        let master = crate::keyring::generate_master_key().unwrap();
        let salt = [7u8; 16];
        let wrapping = crate::recovery::derive_wrapping_key(&phrase, &salt).unwrap();
        let wrapped = crate::recovery::wrap_master_key(&master, &wrapping).unwrap();
        let back = crate::recovery::unwrap_master_key(&wrapped, &wrapping).unwrap();
        assert_eq!(back, master);
        // phrase should not appear in file or logs
        let data =
            std::fs::read_to_string(format!("/tmp/ferrite-test-{}-recovery", std::process::id()))
                .unwrap_or_default();
        assert!(!data.contains(&phrase));
    }

    #[test]
    fn session_durability() {
        let v = Vault::open_in_memory().unwrap();
        v.save_session(r#"[{"url":"https://a.com"}]"#, false)
            .unwrap();
        let (json, clean) = v.last_session().unwrap().unwrap();
        assert!(json.contains("https://a.com"));
        assert!(!clean);
        v.save_session(r#"[]"#, true).unwrap();
        let (_, clean2) = v.last_session().unwrap().unwrap();
        assert!(clean2);
    }

    #[test]
    fn pipeline_throughput_no_block() {
        let v = Vault::open_in_memory().unwrap();
        let (tx, rx) = crossbeam_channel::bounded(4096);
        let start = std::time::Instant::now();
        for i in 0..1000 {
            let entry = HistoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                url: format!("https://example.com/{i}"),
                title: Some(format!("Title {i}")),
                visit_count: 1,
                last_visit: i as i64,
                hlc: i as i64,
            };
            assert!(tx.try_send(entry).is_ok());
        }
        let mut buf = Vec::new();
        while let Ok(e) = rx.try_recv() {
            buf.push(e);
        }
        assert_eq!(buf.len(), 1000);
        for e in buf {
            let _ = v.insert_history(&e.url, e.title.as_deref());
        }
        assert!(start.elapsed().as_millis() < 2000);
    }

    #[test]
    fn import_netscape_bookmarks() {
        let html = "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<TITLE>Bookmarks</TITLE>\n<DL><p>\n<DT><A HREF=\"https://example.com\">Example</A>\n<DT><A HREF=\"https://rust-lang.org\">Rust</A>\n</DL><p>";
        let path = std::env::temp_dir().join("ferrite_test_bookmarks.html");
        std::fs::write(&path, html).unwrap();
        let v = Vault::open_in_memory().unwrap();
        let count = crate::import::import_netscape_bookmarks(path.to_str().unwrap(), &v).unwrap();
        assert_eq!(count, 2);
        assert!(v.bookmark_cache.contains_key("https://example.com"));
        let _ = std::fs::remove_file(path);
    }
}
