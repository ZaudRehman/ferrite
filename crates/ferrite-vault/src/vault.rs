#![allow(clippy::manual_flatten, clippy::unnecessary_sort_by)]
//! Encrypted store: SQLCipher file, hot cache, batch writer. Keys zeroize.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use dashmap::DashMap;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;
use zeroize::Zeroize;

use crate::bookmarks::{BookmarkEvent, BookmarkEventKind, ORDER_STEP, position_between};
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
    /// How the visit happened (`typed`, `link`). Stored for future ranking.
    pub transition: String,
}

/// This device's sync identity: stable id, 32-byte Ed25519 seed, display
/// name. The seed is a secret (encrypted at rest by the vault itself);
/// holders zeroize their copies after building the keypair.
#[derive(Debug, Clone)]
pub struct DeviceIdentityRow {
    pub device_id: String,
    pub seed: Vec<u8>,
    pub device_name: String,
}

/// Display name for a fresh device identity. OS computer name when set
/// (sanitized to one printable line, capped), else a plain default. Pure.
pub fn default_device_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_default();
    let clean: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(64)
        .collect();
    if clean.is_empty() {
        "This device".to_owned()
    } else {
        clean
    }
}

/// One download row for `ferrite://downloads`.
#[derive(Debug, Clone)]
pub struct DownloadRecord {
    pub id: String,
    pub url: String,
    pub filename: String,
    /// Full destination when known. Null for pre-path rows.
    pub path: Option<String>,
    pub size_bytes: Option<u64>,
    pub sha256_hex: Option<String>,
    pub state: String,
    pub downloaded_at: i64,
    pub is_dangerous: bool,
}

/// One site permission row. Mirrors the broker shape without depending on
/// `ferrite-tabs` (that edge would cycle). Shell maps between the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePermissionRow {
    pub origin: String,
    pub kind: String,
    /// `user` or `auto_cooldown`. Validated on write, never defaulted.
    pub source: String,
    pub granted_at: i64,
    pub expires_at: Option<i64>,
}

/// One cert exception: exact host plus exact leaf fingerprint. Never a CA
/// grant, never a wildcard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertExceptionRow {
    pub host: String,
    pub sha256: String,
    pub expires_at: i64,
    pub created_at: i64,
}

/// Bare lowercase host for exception keys. Unparseable reads as None.
pub fn cert_exception_host(input: &str) -> Option<String> {
    let t = input.trim().to_lowercase();
    let t = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
        .unwrap_or(&t);
    let host = t.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = match host.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.is_empty() => h,
        _ => host,
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    Some(host.to_owned())
}

/// True for a 64-char lowercase hex SHA-256 fingerprint.
pub fn valid_cert_fingerprint(fp: &str) -> bool {
    fp.len() == 64 && fp.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One bookmark tree node for pickers, the bar, and the manager page.
/// Folders are rows with an empty URL; `None` parent means a bar root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkNode {
    pub id: String,
    pub title: String,
    pub url: String,
    pub is_folder: bool,
}

/// One folder for the manager tree (parent ids let the page nest
/// client-side; the bar never needs this shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkFolder {
    pub id: String,
    pub title: String,
    pub parent: Option<String>,
}

/// One manager table row: child detail with date-added for sorting.
/// Separate from [`BookmarkNode`] so bar/panel/picker call sites stay
/// untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkDetail {
    pub id: String,
    pub title: String,
    pub url: String,
    pub is_folder: bool,
    pub added: i64,
}

/// One duplicate-URL group for the manager (merge/delete review).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    pub url: String,
    pub items: Vec<DuplicateItem>,
}

/// One member of a duplicate group (parent id resolves to a folder name
/// from the tree data, no join needed here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateItem {
    pub id: String,
    pub title: String,
    pub parent: Option<String>,
}

/// One reading-list entry: something to read once, then mark done.
/// No cached article snapshot yet (needs the reader-extraction backend);
/// the row stores location + intent, encrypted like everything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingItem {
    pub id: String,
    pub url: String,
    pub title: String,
    pub added_at: i64,
    pub is_read: bool,
}

/// One new-tab tile override: pin/custom holds a grid slot, removed hides
/// a URL from tiles. History rows are never touched by either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtpTile {
    pub url: String,
    pub title: String,
    pub pinned: bool,
    pub removed: bool,
    pub position: i64,
}

/// Hot history cache bound. The DB fallback in `query_history` covers
/// misses, so evicting the stalest entries (by `last_visit`) is safe.
const HISTORY_CACHE_CAP: usize = 20_000;

/// Evict stalest history cache entries (by `last_visit`) down to the cap.
/// DB fallback covers misses, so eviction never loses queryable rows.
/// Over-evicts in batches: a single overflow removes up to `BATCH` stalest
/// rows, so sustained inserts pay one scan per thousand, not per row.
fn evict_history_cache_stalest(cache: &DashMap<String, HistoryEntry>) {
    const BATCH: usize = 1000;
    if cache.len() <= HISTORY_CACHE_CAP {
        return;
    }
    let overflow = cache.len() - HISTORY_CACHE_CAP;
    let take = overflow.max(BATCH).min(cache.len());
    let mut entries: Vec<(String, i64)> = cache
        .iter()
        .map(|e| (e.key().clone(), e.last_visit))
        .collect();
    entries.sort_by_key(|(_, lv)| *lv);
    for (url, _) in entries.into_iter().take(take) {
        cache.remove(&url);
    }
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

/// Lowercased URL host for origin matching (exclusions, site deletes).
/// Returns "" when the URL does not parse.
fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
        .unwrap_or_default()
}

/// Shared row mapping for the downloads table (list, snapshot, restore
/// paths must read identical columns in identical order).
fn download_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<DownloadRecord> {
    Ok(DownloadRecord {
        id: r.get(0)?,
        url: r.get(1)?,
        filename: r.get(2)?,
        path: r.get::<_, Option<String>>(3).unwrap_or(None),
        size_bytes: r.get::<_, Option<i64>>(4)?.map(|s| s as u64),
        sha256_hex: r.get(5)?,
        state: r.get(6)?,
        downloaded_at: r.get(7)?,
        is_dangerous: r.get::<_, i64>(8).unwrap_or(0) != 0,
    })
}

pub struct Vault {
    pub conn: Arc<Mutex<Connection>>,
    pub history_cache: Arc<DashMap<String, HistoryEntry>>,
    pub bookmark_cache: Arc<DashMap<String, (String, String)>>,
    batch_tx: Sender<HistoryEntry>,
    bookmark_subs: Arc<Mutex<Vec<Sender<BookmarkEvent>>>>,
    /// Retained open config (path for diagnostics, key for rotation-aware
    /// callers). Behind a mutex so `rekey` keeps it coherent on a shared
    /// handle; the key zeroizes with the vault on drop.
    config: Mutex<Option<VaultConfig>>,
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
            bookmark_subs: Arc::new(Mutex::new(Vec::new())),
            config: Mutex::new(Some(cfg)),
        };
        vault.warm_cache()?;
        vault.spawn_batch_writer(batch_rx, Arc::clone(&vault.history_cache));
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
            bookmark_subs: Arc::new(Mutex::new(Vec::new())),
            config: Mutex::new(None),
        };
        {
            let conn = vault
                .conn
                .lock()
                .map_err(|e| VaultError::Storage(format!("vault lock poisoned: {e}")))?;
            Self::init_schema(&conn)?;
        }
        vault.spawn_batch_writer(batch_rx, Arc::clone(&vault.history_cache));
        Ok(vault)
    }

    /// Whether this vault persists to disk. The shell only migrates legacy
    /// plaintext settings into Vault (deleting the originals) when this is
    /// true, migrating into an in-memory fallback would silently lose them.
    pub fn is_persistent(&self) -> bool {
        self.config.lock().map(|c| c.is_some()).unwrap_or(false)
    }

    /// This device's sync identity row. Created once (stable id + 32-byte
    /// CSPRNG seed + display name) and returned on every later boot, so the
    /// device keeps one keypair for its lifetime. In-memory vaults get a
    /// session-scoped identity (dies with the process, like everything else
    /// ephemeral). Corrupt rows fail closed instead of minting a twin.
    pub fn load_or_create_device_identity(&self) -> VaultResult<DeviceIdentityRow> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(format!("vault lock poisoned: {e}")))?;
        if let Some(row) = Self::read_identity_row(&conn)? {
            return Ok(row);
        }
        let mut seed = [0u8; 32];
        {
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut seed);
        }
        let row = DeviceIdentityRow {
            device_id: uuid::Uuid::new_v4().to_string(),
            seed: seed.to_vec(),
            device_name: default_device_name(),
        };
        conn.execute(
            "INSERT INTO device_identity(id, device_id, seed, device_name) VALUES('self',?1,?2,?3)",
            rusqlite::params![row.device_id, row.seed, row.device_name],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        seed.zeroize();
        Self::read_identity_row(&conn)?
            .ok_or_else(|| VaultError::Storage("device identity write did not persist".to_owned()))
    }

    fn read_identity_row(conn: &Connection) -> VaultResult<Option<DeviceIdentityRow>> {
        let mut stmt = conn
            .prepare("SELECT device_id, seed, device_name FROM device_identity WHERE id='self'")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut rows = stmt
            .query_map([], |r| {
                Ok(DeviceIdentityRow {
                    device_id: r.get(0)?,
                    seed: r.get(1)?,
                    device_name: r.get(2)?,
                })
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        match rows.next() {
            None => Ok(None),
            Some(Ok(row))
                if !row.device_id.is_empty()
                    && row.seed.len() == 32
                    && !row.device_name.is_empty() =>
            {
                Ok(Some(row))
            }
            Some(Ok(_)) => Err(VaultError::Storage(
                "device identity row corrupt: move the vault aside to re-pair".to_owned(),
            )),
            Some(Err(e)) => Err(VaultError::Storage(e.to_string())),
        }
    }

    fn open_connection(path: &str, key: &[u8]) -> VaultResult<Connection> {
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| VaultError::Storage(e.to_string()))?;
        }
        // Header triage. Only a plaintext SQLite file is identifiable by
        // magic ("SQLite format 3"). Opaque bytes are ambiguous by
        // construction: SQLCipher encrypts the header page too, so an
        // encrypted database is indistinguishable from legacy-XOR or
        // corruption without trying a key. Never guess from magic alone:
        // - plaintext magic: legacy plaintext DB. Refuse (never silently
        //   adopt foreign files); the remedy is to move it aside.
        // - opaque + key: attempt the keyed open below; success means ours,
        //   NOTADB means wrong key or unreadable bytes (same remedy).
        // - opaque + no key: nothing can verify it. Refuse with guidance
        //   instead of creating beside it or adopting it blind.
        let mut opaque_without_key = false;
        if Path::new(path).exists()
            && let Ok(data) = std::fs::read(path)
        {
            if !data.is_empty() && !data.starts_with(b"SQLite format 3\0") {
                if key.is_empty() {
                    opaque_without_key = true;
                }
            } else if !data.is_empty() {
                return Err(VaultError::Storage(format!(
                    "legacy plaintext vault file at {path}: move it aside to start fresh"
                )));
            }
        }
        if opaque_without_key {
            return Err(VaultError::Storage(format!(
                "unreadable vault file at {path} with no key to verify it (legacy format or wrong setup): move it aside to start fresh"
            )));
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
            -- Incremental auto-vacuum for databases created from here on:
            -- freed pages are tracked for `incremental_vacuum` instead of
            -- lingering forever. Must precede any CREATE to take effect.
            -- Existing databases convert on first fragmented open (see
            -- `reclaim_space`); the pragma is a no-op otherwise.
            PRAGMA auto_vacuum=INCREMENTAL;
            PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS history(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                title TEXT,
                visit_count INTEGER DEFAULT 1,
                last_visit_timestamp INTEGER NOT NULL,
                hlc_timestamp INTEGER NOT NULL,
                transition TEXT NOT NULL DEFAULT 'link'
            );
            CREATE INDEX IF NOT EXISTS idx_history_url ON history(url);
            CREATE INDEX IF NOT EXISTS idx_history_timestamp ON history(last_visit_timestamp DESC);

            CREATE TABLE IF NOT EXISTS excluded_origins(
                origin TEXT PRIMARY KEY
            );

            CREATE TABLE IF NOT EXISTS bookmarks(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                title TEXT NOT NULL,
                parent_folder_id TEXT REFERENCES bookmarks(id),
                -- Fractional positions: a move rewrites only the moved row
                -- (midpoint of its new neighbors). REAL affinity reads old
                -- integer rows back as x.0, so no migration is needed.
                sort_order REAL NOT NULL DEFAULT 0,
                hlc_timestamp INTEGER NOT NULL,
                is_deleted INTEGER DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_bookmarks_parent ON bookmarks(parent_folder_id);
            CREATE INDEX IF NOT EXISTS idx_bookmarks_url ON bookmarks(url);
            -- Pinned favicon bytes. Bookmark references are permanent (unlike
            -- tab-strip cache entries), so orphans are swept explicitly via
            -- `prune_orphan_favicons` instead of growing forever.
            CREATE TABLE IF NOT EXISTS favicons(
                url TEXT PRIMARY KEY,
                image BLOB NOT NULL,
                updated_at INTEGER NOT NULL
            );

            -- Permissions truth: permanent user grants plus active
            -- auto-blocks (source distinguishes them; the native profile
            -- store only holds allow/deny). One-time grants vanish after
            -- use and are never stored. `hlc_timestamp` rides along for a
            -- future sync consumer; `expires_at` is NULL for user grants.
            CREATE TABLE IF NOT EXISTS site_permissions(
                origin TEXT NOT NULL,
                kind TEXT NOT NULL,
                source TEXT NOT NULL,
                granted_at INTEGER NOT NULL,
                expires_at INTEGER,
                hlc_timestamp INTEGER NOT NULL,
                PRIMARY KEY (origin, kind)
            );
            CREATE INDEX IF NOT EXISTS idx_site_permissions_origin ON site_permissions(origin);

            -- Per-site certificate exceptions: trusting one specific
            -- self-signed cert on one exact host (router admin, local dev).
            -- Narrower than a CA grant and never touching root trust.
            -- `host` is a bare lowercase host (no scheme/port/path);
            -- `sha256` is the leaf's lowercase hex fingerprint.
            CREATE TABLE IF NOT EXISTS cert_exceptions(
                host TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (host, sha256)
            );
            CREATE INDEX IF NOT EXISTS idx_cert_exceptions_host ON cert_exceptions(host);

            -- Threat overrides: "I trust this site, don't warn me again"
            -- (host-scoped, permanent until revoked, the fast local fix
            -- for a false positive, versus the report path below which
            -- feeds list quality on a longer timescale). No expiry: the
            -- grant is explicit and revocable, like a permission grant.
            CREATE TABLE IF NOT EXISTS threat_exceptions(
                host TEXT PRIMARY KEY,
                created_at INTEGER NOT NULL
            );
            -- User block reports: host + trigger + time, recorded locally
            -- for future list-quality export. Never transmitted by this
            -- crate, collection without exfiltration.
            CREATE TABLE IF NOT EXISTS threat_reports(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                host TEXT NOT NULL,
                why TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_threat_reports_host ON threat_reports(host);

            CREATE TABLE IF NOT EXISTS blocked_log(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                origin_domain TEXT NOT NULL,
                blocked_domain TEXT NOT NULL,
                category TEXT NOT NULL,
                timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_blocked_origin ON blocked_log(origin_domain);

            CREATE TABLE IF NOT EXISTS form_history(
                origin TEXT NOT NULL,
                field_key TEXT NOT NULL,
                value TEXT NOT NULL,
                use_count INTEGER DEFAULT 1,
                last_used INTEGER NOT NULL,
                PRIMARY KEY (origin, field_key, value)
            );
            CREATE INDEX IF NOT EXISTS idx_form_history_lookup ON form_history(origin, field_key, use_count DESC, last_used DESC);

            CREATE TABLE IF NOT EXISTS zoom_levels(
                origin TEXT PRIMARY KEY,
                factor REAL NOT NULL
            );

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

            CREATE TABLE IF NOT EXISTS sync_settings (
                device_id TEXT PRIMARY KEY,
                enabled_categories TEXT NOT NULL
            );

            -- This device's sync identity (Ed25519 seed + stable id). One
            -- singleton row (`id='self'`), created on first use. The seed is
            -- a secret: it lives here (encrypted at rest with everything
            -- else), never in a sidecar, never in logs.
            CREATE TABLE IF NOT EXISTS device_identity(
                id TEXT PRIMARY KEY,
                device_id TEXT NOT NULL,
                seed BLOB NOT NULL,
                device_name TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS downloads(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                filename TEXT NOT NULL,
                path TEXT,
                size_bytes INTEGER,
                sha256_hex TEXT,
                state TEXT NOT NULL,
                downloaded_at INTEGER NOT NULL,
                is_dangerous INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_downloads_time ON downloads(downloaded_at DESC);

            CREATE TABLE IF NOT EXISTS reading_list(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                added_at INTEGER NOT NULL,
                is_read INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_reading_time ON reading_list(added_at DESC);
            -- New-tab overrides: pins/customs hold grid position, removed
            -- blocklists a URL from tiles (history itself is untouched).
            CREATE TABLE IF NOT EXISTS ntp_tiles(
                url TEXT PRIMARY KEY,
                title TEXT NOT NULL DEFAULT '',
                pinned INTEGER NOT NULL DEFAULT 0,
                removed INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0
            );
            -- Honest block counts: one row per UTC day, flushed from the
            -- session verdict counters (never per-request rows, never
            -- estimated "time/data saved" math).
            CREATE TABLE IF NOT EXISTS blocked_days(
                day INTEGER PRIMARY KEY,
                blocked INTEGER NOT NULL DEFAULT 0
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
        // Migration for DBs created before the credential `origin` lookup
        // column (same column as `ferrite-credentials` init_schema, so
        // both init paths converge). Additive ALTER only.
        let _ = conn
            .execute_batch("ALTER TABLE credentials ADD COLUMN origin TEXT NOT NULL DEFAULT '';");
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_credentials_origin ON credentials(origin);",
        );
        // Migration for DBs created before autofill tombstones
        // (`ferrite-sync` crdt_engine emits `is_deleted`). Additive only.
        let _ = conn.execute_batch(
            "ALTER TABLE autofill_records ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0;",
        );
        // History v2: transition column for typed-ranking, plus dedupe of
        // pre-upsert duplicate rows (reloads used to mint a row per visit).
        // Keeps the newest row per URL. Best-effort like the above.
        let _ = conn.execute_batch(
            "ALTER TABLE history ADD COLUMN transition TEXT NOT NULL DEFAULT 'link';
             DELETE FROM history WHERE EXISTS (
               SELECT 1 FROM history h2 WHERE h2.url = history.url AND (
                 h2.last_visit_timestamp > history.last_visit_timestamp OR
                 (h2.last_visit_timestamp = history.last_visit_timestamp AND h2.rowid > history.rowid)
               )
             );",
        );
        // Downloads v2: full destination path (ask-every-time files live
        // outside the downloads dir, hashing must use the real path).
        let _ = conn.execute_batch("ALTER TABLE downloads ADD COLUMN path TEXT;");
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
            .prepare("SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition FROM history ORDER BY last_visit_timestamp DESC LIMIT 1000")
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
                    transition: r
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "link".to_owned()),
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
                // Folders (empty URL) would collide on one cache key.
                if url.is_empty() {
                    continue;
                }
                self.bookmark_cache.insert(url.clone(), (id, title));
            }
        }
        Ok(())
    }

    fn spawn_batch_writer(
        &self,
        rx: Receiver<HistoryEntry>,
        cache: Arc<DashMap<String, HistoryEntry>>,
    ) {
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
                                    // Skip upserts the cache no longer wants:
                                    // a delete_* call purges the cache
                                    // synchronously, so in-flight entries for
                                    // a purged URL must not resurrect cleared
                                    // rows. A newer visit re-adds the cache
                                    // entry and its own message proceeds.
                                    let keep = cache
                                        .get(&e.url)
                                        .map(|c| e.last_visit >= c.last_visit)
                                        .unwrap_or(false);
                                    if !keep {
                                        continue;
                                    }
                                    if c.execute(
                                        "INSERT OR IGNORE INTO history(id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition) VALUES(?1,?2,?3,0,?4,?5,?6)",
                                        rusqlite::params![e.id, e.url, e.title, e.last_visit, e.hlc, e.transition],
                                    ).is_err() {
                                        failed = true;
                                        break;
                                    }
                                    if c.execute(
                                        "UPDATE history SET visit_count=visit_count+1, last_visit_timestamp=?1, hlc_timestamp=?2, title=COALESCE(NULLIF(?3,''),title), transition=?4 WHERE url=?5",
                                        rusqlite::params![e.last_visit, e.hlc, e.title.as_deref().unwrap_or(""), e.transition, e.url],
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

    pub fn insert_history(
        &self,
        url: &str,
        title: Option<&str>,
        transition: &str,
    ) -> VaultResult<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // Hot cache upserts by URL: revisits bump the count, never duplicate.
        let mut entry = self
            .history_cache
            .entry(url.to_owned())
            .or_insert_with(|| HistoryEntry {
                id: Uuid::new_v4().to_string(),
                url: url.to_owned(),
                title: None,
                visit_count: 0,
                last_visit: now,
                hlc: now,
                transition: "link".to_owned(),
            });
        let id = entry.id.clone();
        entry.visit_count = entry.visit_count.saturating_add(1);
        entry.last_visit = now;
        entry.hlc = now;
        if let Some(t) = title
            && !t.trim().is_empty()
        {
            entry.title = Some(t.to_owned());
        }
        entry.transition = transition.to_owned();
        let msg = entry.clone();
        drop(entry);
        evict_history_cache_stalest(&self.history_cache);
        // Backpressure: Full queue is an error, never a silent drop.
        self.batch_tx
            .try_send(msg)
            .map_err(|e| VaultError::Storage(format!("history queue full: {e}")))?;
        Ok(id)
    }

    /// Explicit-history import write: one source row lands with its ORIGINAL
    /// timestamp and visit count instead of "now". Merges with an existing
    /// row (counts add, latest visit wins, empty titles never clobber).
    /// Returns true when the URL is new to the store. Wild timestamps are
    /// clamped, never trusted: pre-2000 sorts as ancient (2000-01-01, never
    /// faked as recent), more than a day in the future lands on tomorrow.
    /// Bypasses the batch queue with a single transaction; imports are rare
    /// and explicit.
    pub fn import_history_row(
        &self,
        url: &str,
        title: Option<&str>,
        transition: &str,
        visited_ms: Option<i64>,
        visits: i64,
    ) -> VaultResult<bool> {
        let url = url.trim();
        if url.is_empty() {
            return Err(VaultError::Storage("empty history url".to_owned()));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // 2000-01-01T00:00Z; upper bound is tomorrow (clock skew, never far).
        let seen = visited_ms
            .map(|t| t.clamp(946_684_800_000, now.saturating_add(86_400_000)))
            .unwrap_or(now);
        let add = visits.max(1).min(i32::MAX as i64) as i32;
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let result: VaultResult<bool> = (|| {
            let existing: Option<(i32, i64, Option<String>)> = conn
                .query_row(
                    "SELECT visit_count, last_visit_timestamp, title FROM history WHERE url=?1",
                    [url],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            let is_new = existing.is_none();
            let (count, last, old_title) = match existing {
                Some((c, l, t)) => (c.saturating_add(add), l.max(seen), t),
                None => (add, seen, None),
            };
            let new_title = match title {
                Some(t) if !t.trim().is_empty() => Some(t.trim().to_owned()),
                _ => old_title,
            };
            let id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT OR REPLACE INTO history(id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![id, url, new_title, count, last, now, transition],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
            Ok(is_new)
        })();
        match result {
            Ok(is_new) => {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| VaultError::Storage(e.to_string()))?;
                // Merge into the hot cache the way insert_history does:
                // counts add, latest visit wins, empty titles never clobber.
                let mut entry =
                    self.history_cache
                        .entry(url.to_owned())
                        .or_insert_with(|| HistoryEntry {
                            id: Uuid::new_v4().to_string(),
                            url: url.to_owned(),
                            title: None,
                            visit_count: 0,
                            last_visit: seen,
                            hlc: now,
                            transition: transition.to_owned(),
                        });
                entry.visit_count = entry.visit_count.saturating_add(add);
                entry.last_visit = entry.last_visit.max(seen);
                entry.hlc = now;
                if let Some(t) = title
                    && !t.trim().is_empty()
                {
                    entry.title = Some(t.trim().to_owned());
                }
                entry.transition = transition.to_owned();
                drop(entry);
                evict_history_cache_stalest(&self.history_cache);
                Ok(is_new)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Full history read for exports and sealed backups, newest first.
    /// Unlike [`Self::query_history`] this is not capped at 50 rows and not
    /// served from the hot cache: exports must see the whole table.
    pub fn export_history_rows(&self, limit: usize) -> Vec<HistoryEntry> {
        let limit = limit.min(100_000) as i64;
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare(
            "SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition FROM history ORDER BY last_visit_timestamp DESC LIMIT ?1",
        )
        .and_then(|mut s| {
            s.query_map([limit], |r| {
                Ok(HistoryEntry {
                    id: r.get(0)?,
                    url: r.get(1)?,
                    title: r.get(2)?,
                    visit_count: r.get(3)?,
                    last_visit: r.get(4)?,
                    hlc: r.get(5)?,
                    transition: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
    }

    /// True when a live (non-deleted) bookmark already holds this URL.
    /// Checks the hot cache first, then the table: the cache only warms the
    /// newest rows, so the DB is the verdict for cold URLs.
    pub fn bookmark_exists(&self, url: &str) -> bool {
        if self.bookmark_cache.contains_key(url) {
            return true;
        }
        let Ok(conn) = self.conn.lock() else {
            return false;
        };
        conn.query_row(
            "SELECT 1 FROM bookmarks WHERE url=?1 AND is_deleted=0 LIMIT 1",
            [url],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// Id of a live child folder with this exact title under `parent`
    /// (`None` = bar roots). Used by importers to reuse folders so repeat
    /// imports stay idempotent instead of cloning the tree every run.
    pub fn child_folder(&self, parent: Option<&str>, title: &str) -> Option<String> {
        self.list_bookmark_children(parent)
            .into_iter()
            .find(|n| n.is_folder && n.title == title)
            .map(|n| n.id)
    }

    /// Lock/prepare/map ladder stays nested: each step fails open
    /// independently, and chaining them buries which step failed.
    #[allow(clippy::collapsible_if)]
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
                    "SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition FROM history WHERE LOWER(url) LIKE ?1 ESCAPE '\\' OR LOWER(title) LIKE ?1 ESCAPE '\\' ORDER BY last_visit_timestamp DESC LIMIT ?2",
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
                                transition: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "link".to_owned()),
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
                        evict_history_cache_stalest(&self.history_cache);
                    }
                }
            }
            out.sort_by(|a, b| b.last_visit.cmp(&a.last_visit));
        }
        out.truncate(limit);
        out
    }

    /// Delete history at or after `since_ms`, plus matching hot-cache rows.
    /// Powers time-ranged "clear browsing data". Returns rows removed.
    pub fn delete_history_since(&self, since_ms: i64) -> VaultResult<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let n = conn
            .execute(
                "DELETE FROM history WHERE last_visit_timestamp >= ?1",
                [since_ms],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        self.history_cache.retain(|_, e| e.last_visit < since_ms);
        Ok(n as u64)
    }

    /// Delete one history row by id (hot cache swept alongside).
    pub fn delete_history_id(&self, id: &str) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute("DELETE FROM history WHERE id=?1", [id])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        self.history_cache.retain(|_, e| e.id != id);
        Ok(())
    }

    /// Delete one history row by exact URL (hot cache swept alongside).
    /// Backs per-row removal from the omnibox dropdown.
    pub fn delete_history_url(&self, url: &str) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute("DELETE FROM history WHERE url=?1", [url])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        self.history_cache.retain(|_, e| e.url != url);
        Ok(())
    }

    /// Exact-host match for per-origin work (site view, forget-site).
    /// Substring matching would nuke `evil-example.com` when asked for
    /// `example.com`; parse every candidate instead. Case-insensitive,
    /// port-agnostic (ports are ephemeral, hosts are identity).
    fn url_host_is(url: &str, origin: &str) -> bool {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_lowercase));
        host.as_deref() == Some(origin)
    }

    /// (visits, last-visit-ms) for one exact host. Zeroes when absent.
    pub fn history_origin_stats(&self, origin: &str) -> VaultResult<(u64, i64)> {
        let origin = origin.to_lowercase();
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT url, visit_count, last_visit_timestamp FROM history WHERE url LIKE '%'||?1||'%' LIMIT 5000")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([origin.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let (mut visits, mut last) = (0u64, 0i64);
        for r in rows.flatten() {
            if Self::url_host_is(&r.0, &origin) {
                visits = visits.saturating_add(r.1.max(0) as u64);
                last = last.max(r.2);
            }
        }
        Ok((visits, last))
    }

    /// Delete every history row of one exact host. Returns rows removed;
    /// cache mirrors each delete (same consistency as single deletes).
    /// Bounded (5000 candidates): pathological hosts stop, never hang.
    pub fn delete_history_origin(&self, origin: &str) -> VaultResult<u64> {
        let origin = origin.to_lowercase();
        let urls: Vec<String> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            let mut stmt = conn
                .prepare("SELECT url FROM history WHERE url LIKE '%'||?1||'%' LIMIT 5000")
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            let rows = stmt
                .query_map([origin.as_str()], |r| r.get::<_, String>(0))
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            rows.flatten()
                .filter(|u| Self::url_host_is(u, &origin))
                .collect()
        };
        let mut n = 0u64;
        for u in urls {
            // delete_history_url re-locks per row (fine at this scale)
            // and keeps the hot cache consistent.
            if self.delete_history_url(&u).is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Snapshot one history row by URL for undo (read-before-delete).
    /// Checks the hot cache first, then the database.
    pub fn history_row_by_url(&self, url: &str) -> Option<HistoryEntry> {
        if let Some(e) = self.history_cache.get(url) {
            return Some(e.value().clone());
        }
        let Ok(conn) = self.conn.lock() else {
            return None;
        };
        conn.query_row(
            "SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition FROM history WHERE url=?1",
            [url],
            |r| {
                Ok(HistoryEntry {
                    id: r.get(0)?,
                    url: r.get(1)?,
                    title: r.get(2)?,
                    visit_count: r.get(3)?,
                    last_visit: r.get(4)?,
                    hlc: r.get(5)?,
                    transition: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "link".to_owned()),
                })
            },
        )
        .ok()
    }

    /// Snapshot one history row by id for undo (read-before-delete).
    pub fn history_row_by_id(&self, id: &str) -> Option<HistoryEntry> {
        if let Some(e) = self.history_cache.iter().find(|e| e.id == id) {
            return Some(e.value().clone());
        }
        let Ok(conn) = self.conn.lock() else {
            return None;
        };
        conn.query_row(
            "SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition FROM history WHERE id=?1",
            [id],
            |r| {
                Ok(HistoryEntry {
                    id: r.get(0)?,
                    url: r.get(1)?,
                    title: r.get(2)?,
                    visit_count: r.get(3)?,
                    last_visit: r.get(4)?,
                    hlc: r.get(5)?,
                    transition: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "link".to_owned()),
                })
            },
        )
        .ok()
    }

    /// Re-insert history rows verbatim (undo path). Same ids, same counts,
    /// same timestamps, a restore, not a re-visit. Hot cache refilled
    /// alongside so suggestions and the manager see them immediately.
    pub fn restore_history_rows(&self, rows: &[HistoryEntry]) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        for e in rows {
            conn.execute(
                "INSERT OR REPLACE INTO history(id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![e.id, e.url, e.title, e.visit_count, e.last_visit, e.hlc, e.transition],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
            self.history_cache.insert(e.url.clone(), e.clone());
        }
        evict_history_cache_stalest(&self.history_cache);
        Ok(())
    }

    /// Record a typed form value, per-origin scoped (never global by
    /// field name, that classic behavior leaks values across sites).
    /// Callers filter sensitive keys/values first; this stays dumb.
    pub fn record_form_value(
        &self,
        origin: &str,
        field_key: &str,
        value: &str,
        now_ms: i64,
    ) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO form_history(origin, field_key, value, use_count, last_used)
             VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(origin, field_key, value)
             DO UPDATE SET use_count = use_count + 1, last_used = excluded.last_used",
            rusqlite::params![origin, field_key, value, now_ms],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Past values for one origin + field, most-used first.
    pub fn form_values(&self, origin: &str, field_key: &str, limit: usize) -> Vec<String> {
        let limit = limit.min(50) as i64;
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare(
            "SELECT value FROM form_history WHERE origin=?1 AND field_key=?2
             ORDER BY use_count DESC, last_used DESC LIMIT ?3",
        )
        .and_then(|mut s| {
            s.query_map(rusqlite::params![origin, field_key, limit], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()
        })
        .unwrap_or_default()
    }

    /// Wipe all remembered form values (toggle-off path).
    pub fn clear_form_history(&self) -> VaultResult<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let n = conn
            .execute("DELETE FROM form_history", [])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(n as u64)
    }

    /// Form rows for one canonical origin (forget-site path). Exact key
    /// match: writers store `origin_of(tab_url)`, the same normalizer
    /// the site view validates against, so no host parsing is needed.
    pub fn delete_form_history_origin(&self, origin: &str) -> VaultResult<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let n = conn
            .execute("DELETE FROM form_history WHERE origin=?1", [origin])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(n as u64)
    }

    /// Count form rows for one canonical origin (site view display).
    pub fn count_form_history_origin(&self, origin: &str) -> u64 {
        let Ok(conn) = self.conn.lock() else {
            return 0;
        };
        conn.query_row(
            "SELECT COUNT(*) FROM form_history WHERE origin=?1",
            [origin],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0) as u64
    }

    /// Saved page-zoom factor for one origin, if any (100% = no row).
    pub fn zoom_for(&self, origin: &str) -> Option<f64> {
        let Ok(conn) = self.conn.lock() else {
            return None;
        };
        conn.query_row(
            "SELECT factor FROM zoom_levels WHERE origin=?1",
            [origin],
            |r| r.get(0),
        )
        .ok()
    }

    /// Save a page-zoom factor; 100% deletes the row (the default needs
    /// no storage). Factors clamp to the 25 to 500 percent preset range.
    pub fn set_zoom(&self, origin: &str, factor: f64) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        if (factor - 1.0).abs() < f64::EPSILON {
            conn.execute("DELETE FROM zoom_levels WHERE origin=?1", [origin])
                .map_err(|e| VaultError::Storage(e.to_string()))?;
        } else {
            let factor = factor.clamp(0.25, 5.0);
            conn.execute(
                "INSERT INTO zoom_levels(origin, factor) VALUES(?1,?2)
                 ON CONFLICT(origin) DO UPDATE SET factor=excluded.factor",
                rusqlite::params![origin, factor],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    /// Delete every visit whose URL belongs to `host` (exact or subdomain).
    /// Backs "delete all visits from this site".
    pub fn delete_history_site(&self, host: &str) -> VaultResult<u64> {
        let host = host.to_lowercase();
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        // Wildcards are structural; only the host itself is escaped.
        let h = escape_like(&host);
        let pat_exact = format!("%://{h}");
        let pat_sub = format!("%://{h}/%");
        let pat_dot = format!("%.{h}/%");
        let n = conn
            .execute(
                "DELETE FROM history WHERE LOWER(url) LIKE ?1 ESCAPE '\\' OR LOWER(url) LIKE ?2 ESCAPE '\\' OR LOWER(url) LIKE ?3 ESCAPE '\\'",
                rusqlite::params![pat_exact, pat_sub, pat_dot],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        self.history_cache.retain(|_, e| {
            let h = host_of(&e.url);
            h != host && !h.ends_with(&format!(".{host}"))
        });
        Ok(n as u64)
    }

    /// Snapshot every visit belonging to `host` for undo. Patterns mirror
    /// [`Self::delete_history_site`] exactly, snapshot and delete must
    /// cover the same rows or the undo would lie. Merges the hot cache
    /// (the batch writer flushes asynchronously, so fresh rows may not
    /// be in the database yet) with the database, deduplicated by id.
    /// Returns up to `cap + 1` rows; callers treat overflow (more than
    /// `cap`) as not-undoable rather than partially restoring.
    /// Lock/prepare/map ladder stays nested: each step fails open
    /// independently, and chaining them buries which step failed.
    #[allow(clippy::collapsible_if)]
    pub fn history_rows_for_site(&self, host: &str, cap: usize) -> Vec<HistoryEntry> {
        let host = host.to_lowercase();
        let mut out: Vec<HistoryEntry> = self
            .history_cache
            .iter()
            .filter(|e| {
                let h = host_of(&e.url);
                h == host || h.ends_with(&format!(".{host}"))
            })
            .map(|e| e.value().clone())
            .collect();
        if out.len() <= cap {
            let h = escape_like(&host);
            let pat_exact = format!("%://{h}");
            let pat_sub = format!("%://{h}/%");
            let pat_dot = format!("%.{h}/%");
            let limit = cap.saturating_add(1) as i64;
            if let Ok(conn) = self.conn.lock() {
                if let Ok(rows) = conn
                    .prepare(
                        "SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition FROM history WHERE LOWER(url) LIKE ?1 ESCAPE '\\' OR LOWER(url) LIKE ?2 ESCAPE '\\' OR LOWER(url) LIKE ?3 ESCAPE '\\' LIMIT ?4",
                    )
                    .and_then(|mut s| {
                        s.query_map(
                            rusqlite::params![pat_exact, pat_sub, pat_dot, limit],
                            |r| {
                                Ok(HistoryEntry {
                                    id: r.get(0)?,
                                    url: r.get(1)?,
                                    title: r.get(2)?,
                                    visit_count: r.get(3)?,
                                    last_visit: r.get(4)?,
                                    hlc: r.get(5)?,
                                    transition: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "link".to_owned()),
                                })
                            },
                        )?
                        .collect::<Result<Vec<_>, _>>()
                    })
                {
                    for r in rows {
                        if !out.iter().any(|e| e.id == r.id) {
                            out.push(r);
                            if out.len() > cap {
                                break;
                            }
                        }
                    }
                }
            }
        }
        out.truncate(cap.saturating_add(1));
        out
    }

    /// Delete download rows at or after `since_ms` (history entries only;
    /// files on disk are never touched by clearing).
    pub fn delete_downloads_since(&self, since_ms: i64) -> VaultResult<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let n = conn
            .execute(
                "DELETE FROM downloads WHERE downloaded_at >= ?1",
                [since_ms],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(n as u64)
    }

    /// Standing per-site history exclusion ("never save history here").
    pub fn add_excluded_origin(&self, origin: &str) -> VaultResult<()> {
        let origin = origin.trim().to_lowercase();
        if origin.is_empty() {
            return Err(VaultError::Storage("empty origin".to_owned()));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT OR IGNORE INTO excluded_origins(origin) VALUES(?1)",
            [origin],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn remove_excluded_origin(&self, origin: &str) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "DELETE FROM excluded_origins WHERE origin=?1",
            [origin.trim().to_lowercase()],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn list_excluded_origins(&self) -> Vec<String> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare("SELECT origin FROM excluded_origins ORDER BY origin")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default()
    }

    /// True when `url`'s host is excluded exactly or as a subdomain.
    pub fn history_excluded(&self, url: &str) -> bool {
        let host = host_of(url);
        if host.is_empty() {
            return false;
        }
        self.list_excluded_origins()
            .iter()
            .any(|o| host == *o || host.ends_with(&format!(".{o}")))
    }

    /// Persist a finished download (Completed/Failed/Cancelled). Terminal
    /// rows upsert by id so late progress events never resurrect a row.
    /// History for the `ferrite://downloads` page, which survives restarts
    /// unlike the shell's in-memory ring.
    /// Nine row columns mirror the downloads table 1:1; bundling them
    /// into a struct would separate the schema mapping from the INSERT.
    #[allow(clippy::too_many_arguments)]
    pub fn record_download(
        &self,
        id: &str,
        url: &str,
        filename: &str,
        path: Option<&str>,
        size_bytes: Option<u64>,
        sha256_hex: Option<&str>,
        state: &str,
        is_dangerous: bool,
    ) -> VaultResult<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO downloads(id, url, filename, path, size_bytes, sha256_hex, state, downloaded_at, is_dangerous)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(id) DO UPDATE SET state=excluded.state, sha256_hex=excluded.sha256_hex,
             size_bytes=excluded.size_bytes, downloaded_at=excluded.downloaded_at, is_dangerous=excluded.is_dangerous,
             path=excluded.path, filename=excluded.filename",
            params![
                id,
                url,
                filename,
                path,
                size_bytes.map(|s| s as i64),
                sha256_hex,
                state,
                now,
                if is_dangerous { 1 } else { 0 },
            ],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Newest-first download rows, capped (page renders grouped by day).
    pub fn list_downloads(&self, limit: usize) -> Vec<DownloadRecord> {
        let limit = limit.min(200) as i64;
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT id, url, filename, path, size_bytes, sha256_hex, state, downloaded_at, is_dangerous
             FROM downloads ORDER BY downloaded_at DESC LIMIT ?1",
        ) else {
            return Vec::new();
        };
        stmt.query_map([limit], download_row)
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// "Remove from list": deletes the history row, leaves the file on disk.
    /// (File deletion is a separate, confirmed action at the shell layer.)
    pub fn remove_download(&self, id: &str) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute("DELETE FROM downloads WHERE id=?1", [id])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Snapshot history rows at or after `since_ms` for undo (clear-data
    /// path). Merges hot cache with the database like
    /// [`Self::history_rows_for_site`]; see there for the overflow rule.
    /// Lock/prepare/map ladder stays nested: each step fails open
    /// independently, and chaining them buries which step failed.
    #[allow(clippy::collapsible_if)]
    pub fn history_rows_since(&self, since_ms: i64, cap: usize) -> Vec<HistoryEntry> {
        let mut out: Vec<HistoryEntry> = self
            .history_cache
            .iter()
            .filter(|e| e.last_visit >= since_ms)
            .map(|e| e.value().clone())
            .collect();
        if out.len() <= cap {
            let limit = cap.saturating_add(1) as i64;
            if let Ok(conn) = self.conn.lock() {
                if let Ok(rows) = conn
                    .prepare(
                        "SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition FROM history WHERE last_visit_timestamp >= ?1 LIMIT ?2",
                    )
                    .and_then(|mut s| {
                        s.query_map(rusqlite::params![since_ms, limit], |r| {
                            Ok(HistoryEntry {
                                id: r.get(0)?,
                                url: r.get(1)?,
                                title: r.get(2)?,
                                visit_count: r.get(3)?,
                                last_visit: r.get(4)?,
                                hlc: r.get(5)?,
                                transition: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "link".to_owned()),
                            })
                        })?
                        .collect::<Result<Vec<_>, _>>()
                    })
                {
                    for r in rows {
                        if !out.iter().any(|e| e.id == r.id) {
                            out.push(r);
                            if out.len() > cap {
                                break;
                            }
                        }
                    }
                }
            }
        }
        out.truncate(cap.saturating_add(1));
        out
    }

    /// Snapshot download rows at or after `since_ms` for undo.
    pub fn download_rows_since(&self, since_ms: i64, cap: usize) -> Vec<DownloadRecord> {
        let limit = cap.saturating_add(1) as i64;
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare(
            "SELECT id, url, filename, path, size_bytes, sha256_hex, state, downloaded_at, is_dangerous FROM downloads WHERE downloaded_at >= ?1 LIMIT ?2",
        )
        .and_then(|mut s| {
            s.query_map(rusqlite::params![since_ms, limit], download_row)?
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
    }

    /// Snapshot one download row for undo (read-before-remove).
    pub fn download_by_id(&self, id: &str) -> Option<DownloadRecord> {
        let Ok(conn) = self.conn.lock() else {
            return None;
        };
        conn.query_row(
            "SELECT id, url, filename, path, size_bytes, sha256_hex, state, downloaded_at, is_dangerous FROM downloads WHERE id=?1",
            [id],
            download_row,
        )
        .ok()
    }

    /// Re-insert one download row verbatim (undo path). Keeps the original
    /// `downloaded_at`, [`Self::record_download`] would stamp now, which
    /// would rewrite history instead of restoring it.
    pub fn restore_download_row(&self, row: &DownloadRecord) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO downloads(id, url, filename, path, size_bytes, sha256_hex, state, downloaded_at, is_dangerous) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                row.id,
                row.url,
                row.filename,
                row.path,
                row.size_bytes.map(|s| s as i64),
                row.sha256_hex,
                row.state,
                row.downloaded_at,
                if row.is_dangerous { 1 } else { 0 },
            ],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
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

    /// Subscribe to bookmark-tree mutations. The bar, sidebar, and manager
    /// page each hold a receiver and re-read only the event's `parent`,
    /// never the database directly, so the three surfaces can't drift apart.
    /// The channel is bounded (256); a slow subscriber drops an event rather
    /// than blocking writers, and a dropped event just means "re-read".
    pub fn subscribe_bookmarks(&self) -> Receiver<BookmarkEvent> {
        let (tx, rx) = bounded(256);
        if let Ok(mut subs) = self.bookmark_subs.lock() {
            subs.push(tx);
        }
        rx
    }

    /// Fan out one event. Disconnected receivers are pruned; full ones skip
    /// this event (see [`Vault::subscribe_bookmarks`]).
    fn emit_bookmark(&self, event: BookmarkEvent) {
        if let Ok(mut subs) = self.bookmark_subs.lock() {
            subs.retain(|s| {
                !matches!(
                    s.try_send(event.clone()),
                    Err(TrySendError::Disconnected(_))
                )
            });
        }
    }

    /// Live sibling `(id, position)` rows under `parent`, ascending.
    /// Callers hold the connection lock; pure read, no transaction needed.
    fn sibling_orders(conn: &Connection, parent: Option<&str>) -> VaultResult<Vec<(String, f64)>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, sort_order FROM bookmarks WHERE (parent_folder_id = ?1 OR (parent_folder_id IS NULL AND ?1 IS NULL)) AND is_deleted=0 ORDER BY sort_order, rowid",
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![parent], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| VaultError::Storage(e.to_string()))?);
        }
        Ok(out)
    }

    /// Spread siblings across `ORDER_STEP` spacing, preserving order. Runs
    /// inside the caller's `BEGIN IMMEDIATE`/`COMMIT`, the only write path
    /// that touches more than one row, and only when a gap crowds out.
    fn rebalance_siblings(conn: &Connection, parent: Option<&str>) -> VaultResult<()> {
        let orders = Self::sibling_orders(conn, parent)?;
        for (i, (id, _)) in orders.iter().enumerate() {
            conn.execute(
                "UPDATE bookmarks SET sort_order=?1 WHERE id=?2",
                rusqlite::params![(i as f64 + 1.0) * ORDER_STEP, id],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    /// Fractional position for a drop at index `at` (`None` = append).
    /// Rebalances once and retries when the target gap is crowded; a second
    /// miss means genuine corruption, not contention.
    fn order_for_insert(
        conn: &Connection,
        parent: Option<&str>,
        at: Option<usize>,
    ) -> VaultResult<f64> {
        let place = |orders: &[(String, f64)]| -> (Option<f64>, Option<f64>) {
            match at {
                None if orders.is_empty() => (None, None),
                None => (orders.last().map(|(_, p)| *p), None),
                Some(i) => {
                    let i = i.min(orders.len());
                    (
                        if i == 0 { None } else { Some(orders[i - 1].1) },
                        orders.get(i).map(|(_, p)| *p),
                    )
                }
            }
        };
        let orders = Self::sibling_orders(conn, parent)?;
        let (prev, next) = place(&orders);
        if let Some(p) = position_between(prev, next) {
            return Ok(p);
        }
        Self::rebalance_siblings(conn, parent)?;
        let orders = Self::sibling_orders(conn, parent)?;
        let (prev, next) = place(&orders);
        position_between(prev, next).ok_or_else(|| {
            VaultError::Storage("bookmark order gap still crowded after rebalance".to_owned())
        })
    }

    pub fn insert_bookmark(
        &self,
        url: &str,
        title: &str,
        parent: Option<&str>,
    ) -> VaultResult<String> {
        self.insert_bookmark_at(url, title, parent, None)
    }

    /// Insert at index `at` (`None` = append). Only the new row is written
    /// on the common path, neighbors are never renumbered.
    pub fn insert_bookmark_at(
        &self,
        url: &str,
        title: &str,
        parent: Option<&str>,
        at: Option<usize>,
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
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let result: VaultResult<()> = (|| {
            let order = Self::order_for_insert(&conn, parent, at)?;
            conn.execute(
                "INSERT INTO bookmarks(id, url, title, parent_folder_id, sort_order, hlc_timestamp, is_deleted) VALUES(?1,?2,?3,?4,?5,?6,0)",
                params![id, url, title, parent, order, now],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| VaultError::Storage(e.to_string()))?;
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(e);
            }
        }
        // Folders (empty URL) stay out of the URL-keyed hot cache.
        if !url.is_empty() {
            self.bookmark_cache
                .insert(url.to_owned(), (id.clone(), title.to_owned()));
        }
        self.emit_bookmark(BookmarkEvent::new(
            &id,
            parent.map(str::to_owned),
            BookmarkEventKind::Inserted,
        ));
        Ok(id)
    }

    /// Folders for pickers and submenus, in creation order.
    pub fn list_folders(&self) -> Vec<BookmarkNode> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare("SELECT id, title FROM bookmarks WHERE url='' AND is_deleted=0 ORDER BY rowid")
            .and_then(|mut s| {
                s.query_map([], |r| {
                    Ok(BookmarkNode {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        url: String::new(),
                        is_folder: true,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default()
    }

    /// Children of `parent` (`None` = bar roots), in explicit order.
    pub fn list_bookmark_children(&self, parent: Option<&str>) -> Vec<BookmarkNode> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare(
            "SELECT id, title, url FROM bookmarks WHERE (parent_folder_id = ?1 OR (parent_folder_id IS NULL AND ?1 IS NULL)) AND is_deleted=0 ORDER BY sort_order, rowid",
        )
        .and_then(|mut s| {
            s.query_map([parent], |r| {
                let url: String = r.get(2)?;
                Ok(BookmarkNode {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    is_folder: url.is_empty(),
                    url,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
    }

    /// Create a folder. Empty titles are rejected (nameless folders are a
    /// navigation dead end).
    pub fn add_folder(&self, title: &str, parent: Option<&str>) -> VaultResult<String> {
        let title = title.trim();
        if title.is_empty() {
            return Err(VaultError::Storage("empty folder name".to_owned()));
        }
        self.insert_bookmark("", title, parent)
    }

    /// Rewrite sibling order (and parent) from an ordered id list, e.g. a
    /// bar drag-reorder. One transaction: all rows land or none do, so two
    /// surfaces reordering at once can't leave a half-applied mix. Ids
    /// outside the set keep their rows untouched. Prefer
    /// [`Vault::move_bookmark_to`] for single-item moves, it touches one
    /// row instead of the whole sibling set.
    pub fn reorder_bookmarks(
        &self,
        parent: Option<&str>,
        ordered_ids: &[String],
    ) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut failed: Option<VaultError> = None;
        for (pos, id) in ordered_ids.iter().enumerate() {
            let order = (pos as f64 + 1.0) * ORDER_STEP;
            if let Err(e) = conn.execute(
                "UPDATE bookmarks SET parent_folder_id=?1, sort_order=?2 WHERE id=?3 AND is_deleted=0",
                rusqlite::params![parent, order, id],
            ) {
                failed = Some(VaultError::Storage(e.to_string()));
                break;
            }
        }
        match failed {
            None => {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| VaultError::Storage(e.to_string()))?;
            }
            Some(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(e);
            }
        }
        self.emit_bookmark(BookmarkEvent::new(
            "",
            parent.map(str::to_owned),
            BookmarkEventKind::Reordered,
        ));
        Ok(())
    }

    pub fn delete_bookmark(&self, id: &str) -> VaultResult<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // Resolve URL + parent first so the cache tombstone and the change
        // event land even if the row was already deleted.
        let (url, parent): (Option<String>, Option<String>) = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            let existing: Option<(String, Option<String>)> = conn
                .query_row(
                    "SELECT url, parent_folder_id FROM bookmarks WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            conn.execute(
                "UPDATE bookmarks SET is_deleted=1, hlc_timestamp=?1 WHERE id=?2",
                params![now, id],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
            match existing {
                Some((u, p)) => (Some(u), p),
                None => (None, None),
            }
        };
        if let Some(u) = url {
            if !u.is_empty() {
                self.bookmark_cache.remove(&u);
            }
        } else {
            // Unknown id: sweep any stale entry (defensive, no full scan,
            // cache is keyed by URL, so only exact removals apply).
        }
        self.emit_bookmark(BookmarkEvent::new(id, parent, BookmarkEventKind::Deleted));
        Ok(())
    }

    /// Clear a bookmark tombstone (undo path). The row was never hard
    /// deleted, so this restores parent, order, and title exactly; the
    /// URL-keyed hot cache is refilled alongside.
    pub fn restore_bookmark(&self, id: &str) -> VaultResult<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let row: Option<(String, String)> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            conn.execute(
                "UPDATE bookmarks SET is_deleted=0, hlc_timestamp=?1 WHERE id=?2",
                params![now, id],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
            conn.query_row("SELECT url, title FROM bookmarks WHERE id=?1", [id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()
            .map_err(|e| VaultError::Storage(e.to_string()))?
        };
        if let Some((url, title)) = row
            && !url.is_empty()
        {
            self.bookmark_cache.insert(url, (id.to_owned(), title));
        }
        let parent: Option<String> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            conn.query_row(
                "SELECT parent_folder_id FROM bookmarks WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| VaultError::Storage(e.to_string()))?
            .flatten()
        };
        self.emit_bookmark(BookmarkEvent::new(id, parent, BookmarkEventKind::Restored));
        Ok(())
    }

    /// Move a node into another folder (or the bar root). Lands at the end
    /// of the target's order. Moving a folder into itself or a descendant
    /// is refused (cycle walk in Rust, no recursive CTE).
    pub fn move_bookmark(&self, id: &str, parent: Option<&str>) -> VaultResult<()> {
        self.move_bookmark_to(id, parent, None)
    }

    /// Move a node to index `at` under `parent` (`None` = append at end).
    /// Exactly one row is rewritten on the common path, the fractional
    /// position absorbs the drop, neighbors are untouched. A crowded gap
    /// triggers a single-transaction rebalance of that sibling set only.
    pub fn move_bookmark_to(
        &self,
        id: &str,
        parent: Option<&str>,
        at: Option<usize>,
    ) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let result: VaultResult<bool> = (|| {
            let current: Option<Option<String>> = conn
                .query_row(
                    "SELECT parent_folder_id FROM bookmarks WHERE id=?1 AND is_deleted=0",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            let Some(cur) = current else {
                return Err(VaultError::Storage("unknown bookmark".to_owned()));
            };
            if let Some(p) = parent {
                let is_folder: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM bookmarks WHERE id=?1 AND url='' AND is_deleted=0",
                        [p],
                        |r| r.get(0),
                    )
                    .map_err(|e| VaultError::Storage(e.to_string()))?;
                if is_folder == 0 {
                    return Err(VaultError::Storage("target is not a folder".to_owned()));
                }
                let mut up: Option<String> = Some(p.to_owned());
                while let Some(u) = up {
                    if u == id {
                        return Err(VaultError::Storage("folder cycle refused".to_owned()));
                    }
                    up = conn
                        .query_row(
                            "SELECT parent_folder_id FROM bookmarks WHERE id=?1 AND is_deleted=0",
                            [&u],
                            |r| r.get::<_, Option<String>>(0),
                        )
                        .optional()
                        .map_err(|e| VaultError::Storage(e.to_string()))?
                        .flatten();
                }
            }
            // Same-folder no-op only when no explicit index was requested.
            if cur.as_deref() == parent && at.is_none() {
                return Ok(false);
            }
            // Exclude self so a within-folder move measures the gap it will
            // actually land in.
            let mut orders = Self::sibling_orders(&conn, parent)?;
            orders.retain(|(oid, _)| oid != id);
            let i = at.unwrap_or(orders.len()).min(orders.len());
            let prev = if i == 0 { None } else { Some(orders[i - 1].1) };
            let next = orders.get(i).map(|(_, p)| *p);
            let mut order = position_between(prev, next);
            if order.is_none() {
                Self::rebalance_siblings(&conn, parent)?;
                let mut orders = Self::sibling_orders(&conn, parent)?;
                orders.retain(|(oid, _)| oid != id);
                let i = at.unwrap_or(orders.len()).min(orders.len());
                let prev = if i == 0 { None } else { Some(orders[i - 1].1) };
                let next = orders.get(i).map(|(_, p)| *p);
                order = position_between(prev, next);
            }
            let Some(order) = order else {
                return Err(VaultError::Storage(
                    "bookmark order gap still crowded after rebalance".to_owned(),
                ));
            };
            conn.execute(
                "UPDATE bookmarks SET parent_folder_id=?1, sort_order=?2 WHERE id=?3",
                rusqlite::params![parent, order, id],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
            Ok(true)
        })();
        match result {
            Ok(moved) => {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| VaultError::Storage(e.to_string()))?;
                if moved {
                    self.emit_bookmark(BookmarkEvent::new(
                        id,
                        parent.map(str::to_owned),
                        BookmarkEventKind::Moved,
                    ));
                }
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Rename a bookmark or folder. Empty titles rejected (nameless nodes
    /// are navigation dead ends); the hot cache follows the new title.
    pub fn rename_bookmark(&self, id: &str, title: &str) -> VaultResult<()> {
        let title = title.trim();
        if title.is_empty() {
            return Err(VaultError::Storage("empty title".to_owned()));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let changed = conn
            .execute(
                "UPDATE bookmarks SET title=?1 WHERE id=?2 AND is_deleted=0",
                rusqlite::params![title, id],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        if changed == 0 {
            return Err(VaultError::Storage("unknown bookmark".to_owned()));
        }
        if let Ok(url) = conn.query_row("SELECT url FROM bookmarks WHERE id=?1", [id], |r| {
            r.get::<_, String>(0)
        }) && !url.is_empty()
        {
            self.bookmark_cache
                .insert(url, (id.to_owned(), title.to_owned()));
        }
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_folder_id FROM bookmarks WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| VaultError::Storage(e.to_string()))?
            .flatten();
        self.emit_bookmark(BookmarkEvent::new(id, parent, BookmarkEventKind::Renamed));
        Ok(())
    }

    /// Sort position of one bookmark (manager drag math, sync deltas).
    /// `None` when the row is missing or deleted.
    pub fn bookmark_order(&self, id: &str) -> Option<f64> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT sort_order FROM bookmarks WHERE id=?1 AND is_deleted=0",
            [id],
            |r| r.get(0),
        )
        .ok()
    }

    /// Store favicon bytes for a URL (upsert). Blobs over
    /// `MAX_FAVICON_BYTES` are refused, icons are tiny; anything bigger is
    /// a mislabeled download. Empty URLs and empty images are rejected.
    pub fn upsert_favicon(&self, url: &str, image: &[u8]) -> VaultResult<()> {
        if url.is_empty() {
            return Err(VaultError::Storage("empty favicon url".to_owned()));
        }
        if image.is_empty() || image.len() > crate::bookmarks::MAX_FAVICON_BYTES {
            return Err(VaultError::Storage(
                "favicon blob size out of range".to_owned(),
            ));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO favicons(url, image, updated_at) VALUES(?1,?2,?3) ON CONFLICT(url) DO UPDATE SET image=excluded.image, updated_at=excluded.updated_at",
            rusqlite::params![url, image, now],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Favicon bytes for a URL, if pinned.
    pub fn favicon_for(&self, url: &str) -> Option<Vec<u8>> {
        let conn = self.conn.lock().ok()?;
        conn.query_row("SELECT image FROM favicons WHERE url=?1", [url], |r| {
            r.get::<_, Vec<u8>>(0)
        })
        .ok()
    }

    /// Delete cached favicons no live bookmark or history row references.
    /// Returns the reclaimed row count. Call on startup and after bulk
    /// deletes/imports, bookmarks pin icons permanently (unlike transient
    /// tab-strip entries), so without this the table only grows.
    ///
    /// History counts two ways: committed rows AND the hot cache. The batch
    /// writer flushes asynchronously, so a just-visited URL may live only
    /// in memory, sweeping its icon before the flush lands would thrash.
    pub fn prune_orphan_favicons(&self) -> VaultResult<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut live: std::collections::HashSet<String> = conn
            .prepare("SELECT url FROM bookmarks WHERE url<>'' AND is_deleted=0")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<std::collections::HashSet<_>, _>>()
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let committed: std::collections::HashSet<String> = conn
            .prepare("SELECT url FROM history")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<std::collections::HashSet<_>, _>>()
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        live.extend(committed);
        for e in self.history_cache.iter() {
            live.insert(e.key().clone());
        }
        let all: Vec<String> = conn
            .prepare("SELECT url FROM favicons")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let orphans: Vec<&String> = all.iter().filter(|u| !live.contains(*u)).collect();
        let mut n = 0;
        for u in orphans {
            n += conn
                .execute("DELETE FROM favicons WHERE url=?1", [u])
                .map_err(|e| VaultError::Storage(e.to_string()))?;
        }
        Ok(n)
    }

    /// Delete history rows older than `cutoff_ms` (retention). Returns
    /// rows removed. Hot cache mirrors the delete. Same hard-delete
    /// semantics as user deletes (no tombstones: sync treats absent
    /// rows as absent, matching the existing delete paths).
    pub fn prune_history_older_than(&self, cutoff_ms: i64) -> VaultResult<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let n = conn
            .execute(
                "DELETE FROM history WHERE last_visit_timestamp < ?1",
                [cutoff_ms],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        self.history_cache.retain(|_, e| e.last_visit >= cutoff_ms);
        Ok(n)
    }

    /// Reclaim free pages. Bounded sweep first (`incremental_vacuum`,
    /// a few MB at most, fast): a no-op unless the database runs
    /// incremental mode. Conversion second: when fragmentation is
    /// significant (freelist past the threshold), switch to INCREMENTAL
    /// and rebuild once with VACUUM, then sweep. The rebuild rewrites
    /// the whole file, so it runs only when worth it, never
    /// unconditionally at boot. Returns reclaimed pages. SQLCipher-safe
    /// (VACUUM preserves encryption).
    pub fn reclaim_space(&self) -> VaultResult<usize> {
        /// Free pages below this: not worth a rebuild, sweep only.
        const CONVERT_FREELIST_PAGES: i64 = 2000;
        /// Pages reclaimed per sweep (4KB pages: ~2MB bound).
        const SWEEP_PAGES: u32 = 500;
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mode: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        // 2 == INCREMENTAL. 0 (none) and 1 (full) convert below.
        if mode != 2 {
            let free: i64 = conn
                .query_row("PRAGMA freelist_count", [], |r| r.get(0))
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            if free < CONVERT_FREELIST_PAGES {
                return Ok(0);
            }
            conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            conn.execute_batch("VACUUM;")
                .map_err(|e| VaultError::Storage(format!("vacuum rebuild: {e}")))?;
        }
        let before: i64 = conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute_batch(&format!("PRAGMA incremental_vacuum({SWEEP_PAGES});"))
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let after: i64 = conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(before.saturating_sub(after).max(0) as usize)
    }

    /// Upsert one site-permission row (user grant or active auto-block).
    /// `source` must be `"user"` or `"auto_cooldown"`, anything else is
    /// rejected rather than stored as a mystery grant. Empty origins and
    /// non-http(s) origins are rejected: internal pages never hold grants.
    pub fn save_permission_grant(
        &self,
        origin: &str,
        kind: &str,
        source: &str,
        granted_at: i64,
        expires_at: Option<i64>,
    ) -> VaultResult<()> {
        if source != "user" && source != "auto_cooldown" {
            return Err(VaultError::Storage("unknown permission source".to_owned()));
        }
        if kind.is_empty() || !(origin.starts_with("http://") || origin.starts_with("https://")) {
            return Err(VaultError::Storage(
                "permission grant needs an http(s) origin and kind".to_owned(),
            ));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO site_permissions(origin, kind, source, granted_at, expires_at, hlc_timestamp) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(origin, kind) DO UPDATE SET source=excluded.source, granted_at=excluded.granted_at, expires_at=excluded.expires_at, hlc_timestamp=excluded.hlc_timestamp",
            rusqlite::params![origin, kind, source, granted_at, expires_at, granted_at],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// All stored rows (user grants plus whatever auto-blocks are still
    /// recorded; the broker drops expired ones on load).
    pub fn list_permission_grants(&self) -> VaultResult<Vec<SitePermissionRow>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT origin, kind, source, granted_at, expires_at FROM site_permissions ORDER BY origin, kind")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SitePermissionRow {
                    origin: r.get(0)?,
                    kind: r.get(1)?,
                    source: r.get(2)?,
                    granted_at: r.get(3)?,
                    expires_at: r.get(4)?,
                })
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| VaultError::Storage(e.to_string()))?);
        }
        Ok(out)
    }

    /// Delete one row (settings reset action). Returns whether it existed.
    pub fn remove_permission_grant(&self, origin: &str, kind: &str) -> VaultResult<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let n = conn
            .execute(
                "DELETE FROM site_permissions WHERE origin=?1 AND kind=?2",
                rusqlite::params![origin, kind],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(n > 0)
    }

    /// Store one certificate exception (viewer "trust this cert" action).
    /// `host` is normalized to a bare host; fingerprints must be 64 hex
    /// chars. Duplicate (host, fingerprint) pairs refresh the expiry.
    /// `now_ms` stamps creation (rejects clock garbage at zero/negative).
    pub fn save_cert_exception(
        &self,
        host: &str,
        sha256: &str,
        expires_at: i64,
        now_ms: i64,
    ) -> VaultResult<()> {
        let Some(host) = cert_exception_host(host) else {
            return Err(VaultError::Storage(
                "exception needs a bare host".to_owned(),
            ));
        };
        if !valid_cert_fingerprint(sha256) {
            return Err(VaultError::Storage(
                "exception needs a 64-hex SHA-256 fingerprint".to_owned(),
            ));
        }
        if expires_at <= now_ms || now_ms <= 0 {
            return Err(VaultError::Storage(
                "exception expiry must be in the future".to_owned(),
            ));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO cert_exceptions(host, sha256, expires_at, created_at) VALUES(?1,?2,?3,?4) ON CONFLICT(host, sha256) DO UPDATE SET expires_at=excluded.expires_at, created_at=excluded.created_at",
            rusqlite::params![host, sha256.to_lowercase(), expires_at, now_ms],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Whether a live exception covers `host` presenting `sha256` at
    /// `now_ms`. Exact host, exact fingerprint, unexpired, no wildcards,
    /// no prefix matching, no CA semantics.
    pub fn cert_exception_match(&self, host: &str, sha256: &str, now_ms: i64) -> bool {
        let (Some(host), fp) = (cert_exception_host(host), sha256.to_lowercase()) else {
            return false;
        };
        if !valid_cert_fingerprint(&fp) {
            return false;
        }
        let Ok(conn) = self.conn.lock() else {
            return false;
        };
        conn.query_row(
            "SELECT COUNT(*) FROM cert_exceptions WHERE host=?1 AND sha256=?2 AND expires_at>?3",
            rusqlite::params![host, fp, now_ms],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    }

    /// All exceptions for the viewer/settings surface, oldest first.
    pub fn list_cert_exceptions(&self) -> VaultResult<Vec<CertExceptionRow>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT host, sha256, expires_at, created_at FROM cert_exceptions ORDER BY created_at, host")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CertExceptionRow {
                    host: r.get(0)?,
                    sha256: r.get(1)?,
                    expires_at: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| VaultError::Storage(e.to_string()))?);
        }
        Ok(out)
    }

    /// Delete one exception (viewer/settings revoke). Returns existence.
    pub fn remove_cert_exception(&self, host: &str, sha256: &str) -> VaultResult<bool> {
        let (Some(host), fp) = (cert_exception_host(host), sha256.to_lowercase()) else {
            return Ok(false);
        };
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let n = conn
            .execute(
                "DELETE FROM cert_exceptions WHERE host=?1 AND sha256=?2",
                rusqlite::params![host, fp],
            )
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(n > 0)
    }

    /// Delete expired exceptions. Returns the reclaimed count. Called on
    /// boot and after viewer revokes, expiry is enforced on read, so this
    /// is hygiene, not security.
    pub fn prune_expired_cert_exceptions(&self, now_ms: i64) -> VaultResult<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let n = conn
            .execute("DELETE FROM cert_exceptions WHERE expires_at<=?1", [now_ms])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(n)
    }

    /// Top history hosts by visit count (lookalike comparison set).
    /// Bare lowercase hosts, visit counts summed across the host's URLs,
    /// most-visited first, capped at `limit`. Unions the hot cache with a
    /// DB GROUP BY so cold rows (e.g. after a restart, before warmup
    /// covers them) still count. Read-only: never mutates the cache.
    /// Lock/prepare/map ladder stays nested: each step fails open
    /// independently, and chaining them buries which step failed.
    #[allow(clippy::collapsible_if)]
    pub fn top_hosts(&self, limit: usize) -> Vec<String> {
        let limit = limit.min(1000);
        let mut sums: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        let mut cached_urls: std::collections::HashSet<String> = std::collections::HashSet::new();
        for e in self.history_cache.iter() {
            cached_urls.insert(e.key().clone());
            if let Ok(url) = url::Url::parse(&e.url)
                && let Some(host) = url.host_str()
                && !host.is_empty()
            {
                *sums.entry(host.to_lowercase()).or_insert(0) += e.visit_count as i64;
            }
        }
        // Cold rows bypassing the cache (direct SQL, pre-warmup, evicted):
        // count them without double-counting cached URLs.
        if let Ok(conn) = self.conn.lock() {
            if let Ok(mut stmt) =
                conn.prepare("SELECT url, SUM(visit_count) FROM history GROUP BY url LIMIT 50000")
            {
                if let Ok(rows) = stmt.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
                }) {
                    for r in rows.flatten() {
                        let (url, count) = r;
                        if cached_urls.contains(&url) {
                            continue;
                        }
                        if let Ok(parsed) = url::Url::parse(&url)
                            && let Some(host) = parsed.host_str()
                            && !host.is_empty()
                        {
                            *sums.entry(host.to_lowercase()).or_insert(0) +=
                                count.unwrap_or(0).max(0);
                        }
                    }
                }
            }
        }
        let mut pairs: Vec<(i64, String)> = sums.into_iter().map(|(h, n)| (n, h)).collect();
        pairs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        pairs.into_iter().take(limit).map(|(_, h)| h).collect()
    }

    /// Store one threat override (blocked-page "trust this site" action).
    /// Bare host only; permanent until revoked via
    /// [`Vault::remove_threat_exception`].
    pub fn save_threat_exception(&self, host: &str, now_ms: i64) -> VaultResult<()> {
        let Some(host) = cert_exception_host(host) else {
            return Err(VaultError::Storage(
                "exception needs a bare host".to_owned(),
            ));
        };
        if now_ms <= 0 {
            return Err(VaultError::Storage("clock garbage rejected".to_owned()));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO threat_exceptions(host, created_at) VALUES(?1,?2) ON CONFLICT(host) DO UPDATE SET created_at=excluded.created_at",
            rusqlite::params![host, now_ms],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// All override hosts (engine snapshot at boot + after every change).
    pub fn list_threat_exceptions(&self) -> VaultResult<Vec<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT host FROM threat_exceptions ORDER BY host")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| VaultError::Storage(e.to_string()))?);
        }
        Ok(out)
    }

    /// Delete one override (re-arm warnings for the host). Returns existence.
    pub fn remove_threat_exception(&self, host: &str) -> VaultResult<bool> {
        let Some(host) = cert_exception_host(host) else {
            return Ok(false);
        };
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let n = conn
            .execute("DELETE FROM threat_exceptions WHERE host=?1", [host])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(n > 0)
    }

    /// Record a user block report (blocked-page "report" action): host,
    /// trigger (`malware`/`lookalike`/`form`), time. Local only, this
    /// crate transmits nothing; export is a future consumer's job.
    pub fn save_threat_report(&self, host: &str, why: &str, now_ms: i64) -> VaultResult<()> {
        let Some(host) = cert_exception_host(host) else {
            return Err(VaultError::Storage("report needs a bare host".to_owned()));
        };
        if why != "malware" && why != "lookalike" && why != "form" {
            return Err(VaultError::Storage("unknown report trigger".to_owned()));
        }
        if now_ms <= 0 {
            return Err(VaultError::Storage("clock garbage rejected".to_owned()));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO threat_reports(host, why, created_at) VALUES(?1,?2,?3)",
            rusqlite::params![host, why, now_ms],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Report count for one host (settings/debugging surface).
    pub fn threat_report_count(&self, host: &str) -> usize {
        let Some(host) = cert_exception_host(host) else {
            return 0;
        };
        self.conn
            .lock()
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM threat_reports WHERE host=?1",
                    [host],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            })
            .unwrap_or(0) as usize
    }

    /// Replace the whole permission table with a snapshot (broker write-through: the
    /// snapshot is the entire truth, so no row can drift). One transaction.
    pub fn replace_permission_grants(&self, rows: &[SitePermissionRow]) -> VaultResult<()> {
        for r in rows {
            if r.source != "user" && r.source != "auto_cooldown" {
                return Err(VaultError::Storage("unknown permission source".to_owned()));
            }
            if r.kind.is_empty()
                || !(r.origin.starts_with("http://") || r.origin.starts_with("https://"))
            {
                return Err(VaultError::Storage(
                    "permission grant needs an http(s) origin and kind".to_owned(),
                ));
            }
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        let result: VaultResult<()> = (|| {
            conn.execute("DELETE FROM site_permissions", [])
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            for r in rows {
                conn.execute(
                    "INSERT INTO site_permissions(origin, kind, source, granted_at, expires_at, hlc_timestamp) VALUES(?1,?2,?3,?4,?5,?6)",
                    rusqlite::params![
                        r.origin,
                        r.kind,
                        r.source,
                        r.granted_at,
                        r.expires_at,
                        r.granted_at
                    ],
                )
                .map_err(|e| VaultError::Storage(e.to_string()))?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| VaultError::Storage(e.to_string()))?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Every live folder (page nests client-side from parent ids).
    pub fn bookmark_folders(&self) -> Vec<BookmarkFolder> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare(
            "SELECT id, title, parent_folder_id FROM bookmarks WHERE url='' AND is_deleted=0 ORDER BY sort_order, rowid",
        )
        .and_then(|mut s| {
            s.query_map([], |r| {
                Ok(BookmarkFolder {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    parent: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
    }

    /// Full child rows for the manager table (date-added included for
    /// sorting, the bar/panel node shape stays untouched).
    pub fn bookmark_details(&self, parent: Option<&str>) -> Vec<BookmarkDetail> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare(
            "SELECT id, title, url, hlc_timestamp FROM bookmarks WHERE (parent_folder_id = ?1 OR (parent_folder_id IS NULL AND ?1 IS NULL)) AND is_deleted=0 ORDER BY sort_order, rowid",
        )
        .and_then(|mut s| {
            s.query_map([parent], |r| {
                let url: String = r.get(2)?;
                Ok(BookmarkDetail {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    is_folder: url.is_empty(),
                    url,
                    added: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
    }

    /// Substring search across titles + URLs (recent first). Backs the
    /// manager's `?q=`, folder names resolve client-side from the tree.
    pub fn search_bookmarks(&self, q: &str, limit: usize) -> Vec<BookmarkDetail> {
        let limit = limit.min(100) as i64;
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        let pattern = format!("%{}%", escape_like(&q.to_lowercase()));
        conn.prepare(
            "SELECT id, title, url, hlc_timestamp FROM bookmarks WHERE is_deleted=0 AND (LOWER(title) LIKE ?1 ESCAPE '\\' OR LOWER(url) LIKE ?1 ESCAPE '\\') ORDER BY hlc_timestamp DESC LIMIT ?2",
        )
        .and_then(|mut s| {
            s.query_map(rusqlite::params![pattern, limit], |r| {
                let url: String = r.get(2)?;
                Ok(BookmarkDetail {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    is_folder: url.is_empty(),
                    url,
                    added: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
    }

    /// Groups of live bookmarks sharing one URL (case-folded).
    /// ponytail: case-only normalization, tracking-param variants group
    /// only when identical. Deeper normalization lives with history dedup
    /// if anyone asks for it.
    pub fn duplicate_bookmark_urls(&self) -> Vec<DuplicateGroup> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        let dupes: Vec<String> = conn
            .prepare(
                "SELECT LOWER(url) FROM bookmarks WHERE url<>'' AND is_deleted=0 GROUP BY LOWER(url) HAVING COUNT(*) > 1",
            )
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default();
        let mut out = Vec::new();
        for url in dupes {
            let items: Vec<DuplicateItem> = conn
                .prepare(
                    "SELECT id, title, parent_folder_id FROM bookmarks WHERE LOWER(url)=?1 AND is_deleted=0 ORDER BY rowid",
                )
                .and_then(|mut s| {
                    s.query_map([&url], |r| {
                        Ok(DuplicateItem {
                            id: r.get(0)?,
                            title: r.get(1)?,
                            parent: r.get(2)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()
                })
                .unwrap_or_default();
            if items.len() > 1 {
                out.push(DuplicateGroup { url, items });
            }
        }
        out
    }

    /// Parent folder id of a bookmarked URL, if any. `None` means not
    /// bookmarked, or a bar root (the star popover pairs this with the
    /// starred check to tell the two apart).
    pub fn bookmark_parent(&self, url: &str) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT parent_folder_id FROM bookmarks WHERE url=?1 AND is_deleted=0",
            [url],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    }

    /// Save a page to the reading list (idempotent per URL).
    pub fn reading_add(&self, url: &str, title: &str) -> VaultResult<String> {
        let url = url.trim();
        if url.is_empty() || url.starts_with("ferrite://") {
            return Err(VaultError::Storage("unreadable url".to_owned()));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT OR IGNORE INTO reading_list(id, url, title, added_at, is_read) VALUES(?1,?2,?3,?4,0)",
            rusqlite::params![Uuid::new_v4().to_string(), url, title, now],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.query_row("SELECT id FROM reading_list WHERE url=?1", [url], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|e| VaultError::Storage(e.to_string()))
    }

    /// Reading list, unread first, newest first within each group.
    pub fn reading_list(&self, limit: usize) -> Vec<ReadingItem> {
        let limit = limit.min(200) as i64;
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare(
            "SELECT id, url, title, added_at, is_read FROM reading_list ORDER BY is_read, added_at DESC LIMIT ?1",
        )
        .and_then(|mut s| {
            s.query_map([limit], |r| {
                Ok(ReadingItem {
                    id: r.get(0)?,
                    url: r.get(1)?,
                    title: r.get(2)?,
                    added_at: r.get(3)?,
                    is_read: r.get::<_, i64>(4).unwrap_or(0) != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
    }

    /// Mark read/unread. Read items recede (sort last) rather than vanish.
    pub fn reading_mark(&self, id: &str, read: bool) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "UPDATE reading_list SET is_read=?1 WHERE id=?2",
            rusqlite::params![if read { 1 } else { 0 }, id],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn reading_remove(&self, id: &str) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute("DELETE FROM reading_list WHERE id=?1", [id])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Re-insert one reading-list row verbatim (undo path). Keeps the
    /// original id, timestamp, and read state, [`Self::reading_add`]
    /// would mint a fresh id and reset `is_read`, rewriting history.
    pub fn restore_reading_row(&self, row: &ReadingItem) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO reading_list(id, url, title, added_at, is_read) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![
                row.id,
                row.url,
                row.title,
                row.added_at,
                if row.is_read { 1 } else { 0 },
            ],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// All new-tab overrides (pins first by position: the grid reads this
    /// order verbatim).
    pub fn ntp_overrides(&self) -> Vec<NtpTile> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        conn.prepare(
            "SELECT url, title, pinned, removed, position FROM ntp_tiles ORDER BY pinned DESC, position, url",
        )
        .and_then(|mut s| {
            s.query_map([], |r| {
                Ok(NtpTile {
                    url: r.get(0)?,
                    title: r.get(1)?,
                    pinned: r.get::<_, i64>(2).unwrap_or(0) != 0,
                    removed: r.get::<_, i64>(3).unwrap_or(0) != 0,
                    position: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
    }

    /// Pin (or add) a tile: new pins append at the end, re-pins keep their
    /// slot, and any removal is lifted. No drag-reorder yet (position is
    /// append-only: reorder wants a position swap, not a migration).
    pub fn ntp_pin(&self, url: &str, title: &str) -> VaultResult<()> {
        let url = url.trim();
        if url.is_empty() {
            return Err(VaultError::Storage("empty tile url".to_owned()));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO ntp_tiles(url, title, pinned, removed, position)
             VALUES(?1, ?2, 1, 0, (SELECT COALESCE(MAX(position), 0) + 1 FROM ntp_tiles))
             ON CONFLICT(url) DO UPDATE SET title=excluded.title, pinned=1, removed=0",
            rusqlite::params![url, title],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn ntp_unpin(&self, url: &str) -> VaultResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute("UPDATE ntp_tiles SET pinned=0 WHERE url=?1", [url])
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Hide a URL from tiles (history untouched; this is not a delete).
    pub fn ntp_remove(&self, url: &str) -> VaultResult<()> {
        let url = url.trim();
        if url.is_empty() {
            return Err(VaultError::Storage("empty tile url".to_owned()));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO ntp_tiles(url, title, pinned, removed, position) VALUES(?1, '', 0, 1, 0)
             ON CONFLICT(url) DO UPDATE SET pinned=0, removed=1",
            [url],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Add session block verdicts to today's counter (no-op on zero, so
    /// quiet days write no rows).
    pub fn record_blocked_day(&self, day: i64, n: u64) -> VaultResult<()> {
        if n == 0 {
            return Ok(());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| VaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO blocked_days(day, blocked) VALUES(?1, ?2)
             ON CONFLICT(day) DO UPDATE SET blocked=blocked+excluded.blocked",
            rusqlite::params![day, n as i64],
        )
        .map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(())
    }

    /// (today, trailing-7-days, all-time) block counts for the dashboard.
    /// Raw verdict sums only: no "time/data saved" estimates, ever.
    pub fn blocked_sums(&self, today: i64) -> (u64, u64, u64) {
        let Ok(conn) = self.conn.lock() else {
            return (0, 0, 0);
        };
        let nonneg = |v: Option<i64>| v.unwrap_or(0).max(0) as u64;
        let one = |sql: &str, arg: i64| -> u64 {
            nonneg(
                conn.query_row(sql, [arg], |r| r.get::<_, Option<i64>>(0))
                    .unwrap_or(None),
            )
        };
        let all = nonneg(
            conn.query_row("SELECT SUM(blocked) FROM blocked_days", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .unwrap_or(None),
        );
        (
            one("SELECT SUM(blocked) FROM blocked_days WHERE day=?1", today),
            one(
                "SELECT SUM(blocked) FROM blocked_days WHERE day>=?1",
                today - 6,
            ),
            all,
        )
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

    /// Re-encrypt this database under `new_key` (key rotation). The caller
    /// holds the open keyed connection; SQLCipher rewrites pages
    /// transactionally, so a crash lands on either the old or the new key
    /// and rotation resume converges by trying the new key first.
    /// Requires a keyed database and the `sqlcipher` feature: rekeying an
    /// unkeyed file would silently ENCRYPT it, so that path fails closed.
    /// Verifies under the new key before returning.
    pub fn rekey(&self, new_key: &[u8]) -> VaultResult<()> {
        if new_key.len() != 32 {
            return Err(VaultError::Crypto(
                "rotation key must be exactly 32 bytes".to_owned(),
            ));
        }
        let keyed = self
            .config
            .lock()
            .map(|c| c.as_ref().is_some_and(|v| !v.key.is_empty()))
            .unwrap_or(false);
        if !keyed {
            return Err(VaultError::Crypto(
                "rekey refused: database is not keyed (would encrypt plaintext)".to_owned(),
            ));
        }
        #[cfg(not(feature = "sqlcipher"))]
        {
            Err(VaultError::Crypto(
                "rekey requires the `sqlcipher` feature".to_owned(),
            ))
        }
        #[cfg(feature = "sqlcipher")]
        {
            let hex: String = new_key.iter().map(|b| format!("{b:02x}")).collect();
            let conn = self
                .conn
                .lock()
                .map_err(|e| VaultError::Storage(format!("vault lock poisoned: {e}")))?;
            conn.execute_batch(&format!("PRAGMA rekey = \"x'{hex}'\";"))
                .map_err(|e| VaultError::Storage(format!("sqlcipher rekey: {e}")))?;
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| VaultError::Storage(format!("rekey verify failed: {e}")))?;
            let ok: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .map_err(|e| VaultError::Storage(format!("integrity check failed: {e}")))?;
            if !ok.eq_ignore_ascii_case("ok") {
                return Err(VaultError::Storage(format!(
                    "database corrupt after rekey: {ok}"
                )));
            }
            // The file is under the new key now: refresh the retained copy
            // so later callers (and Drop) see coherent state. Old bytes are
            // zeroized, never merely dropped.
            if let Ok(mut guard) = self.config.lock()
                && let Some(cfg) = guard.as_mut()
            {
                cfg.key.zeroize();
                cfg.key = new_key.to_vec();
            }
            Ok(())
        }
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.config.lock()
            && let Some(cfg) = guard.as_mut()
        {
            // Checkpoint WAL so no plaintext journal lingers for backup tools.
            // No XOR re-encryption: SQLCipher handles at-rest encryption.
            if !cfg.key.is_empty()
                && let Ok(conn) = self.conn.lock()
            {
                let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
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
    fn device_identity_stable_then_fail_closed_on_corruption() {
        let v = Vault::open_in_memory().unwrap();
        let first = v.load_or_create_device_identity().unwrap();
        assert_eq!(first.seed.len(), 32);
        assert!(!first.device_id.is_empty());
        assert!(!first.device_name.is_empty());
        let second = v.load_or_create_device_identity().unwrap();
        assert_eq!(first.device_id, second.device_id);
        assert_eq!(first.seed, second.seed);
        // Corrupt seed length fails closed instead of minting a twin.
        {
            let conn = v.conn.lock().unwrap();
            conn.execute("UPDATE device_identity SET seed=x'00' WHERE id='self'", [])
                .unwrap();
        }
        assert!(v.load_or_create_device_identity().is_err());
    }

    #[test]
    fn default_device_name_sanitizes() {
        let name = default_device_name();
        assert!(!name.is_empty());
        assert!(name.chars().count() <= 64);
        assert!(!name.chars().any(|c| c.is_control()));
    }

    #[test]
    fn history_ingest_and_query_stay_interactive() {
        // Tripwire like `decide_throughput_stays_interactive`: 2000 visits
        // enqueue, flush, and re-query in seconds on any real hardware. The
        // bound is generous (batch writer idles 200ms between bursts); it
        // catches deadlocks and quadratic scans, never a 10% slowdown.
        let v = Vault::open_in_memory().unwrap();
        let start = std::time::SystemTime::now();
        for i in 0..2000 {
            v.insert_history(&format!("https://example.com/p{i}"), None, "link")
                .unwrap();
        }
        // Let the batch writer drain (50-row bursts, 200ms idle cadence).
        std::thread::sleep(std::time::Duration::from_secs(2));
        let rows = v.query_history("", 50);
        assert_eq!(rows.len(), 50);
        let elapsed = start.elapsed().unwrap();
        println!("history 2k ingest+query: {elapsed:?}");
        assert!(elapsed < std::time::Duration::from_secs(15), "{elapsed:?}");
    }

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
            v.insert_history("https://example.com", Some("Example"), "typed")
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
        let id1 = v
            .insert_history("https://a.com", Some("A"), "typed")
            .unwrap();
        let id2 = v
            .insert_history("https://b.com", Some("B"), "link")
            .unwrap();
        assert_ne!(id1, id2);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let hits = v.query_history("a.com", 5);
        assert!(!hits.is_empty());
    }

    #[test]
    fn history_snapshot_restore_roundtrip() {
        let v = Vault::open_in_memory().unwrap();
        v.insert_history("https://undo.test/p", Some("Undo Title"), "typed")
            .unwrap();
        let snap = v
            .history_row_by_url("https://undo.test/p")
            .expect("snapshot");
        assert_eq!(snap.title.as_deref(), Some("Undo Title"));
        assert_eq!(snap.transition, "typed");
        v.delete_history_url("https://undo.test/p").unwrap();
        assert!(v.history_row_by_url("https://undo.test/p").is_none());
        v.restore_history_rows(std::slice::from_ref(&snap)).unwrap();
        let back = v
            .history_row_by_url("https://undo.test/p")
            .expect("restored");
        // Verbatim restore: same id, same count, same timestamp.
        assert_eq!(back.id, snap.id);
        assert_eq!(back.visit_count, snap.visit_count);
        assert_eq!(back.last_visit, snap.last_visit);
        assert_eq!(back.title, snap.title);
    }

    #[test]
    fn history_site_snapshot_flags_overflow() {
        let v = Vault::open_in_memory().unwrap();
        v.insert_history("https://cap.test/a", Some("A"), "link")
            .unwrap();
        v.insert_history("https://cap.test/b", Some("B"), "link")
            .unwrap();
        // Cap 1 with 2 matching rows: overflow signal, never partial.
        assert_eq!(v.history_rows_for_site("cap.test", 1).len(), 2);
        let rows = v.history_rows_for_site("cap.test", 8);
        assert_eq!(rows.len(), 2);
        v.delete_history_site("cap.test").unwrap();
        assert!(v.history_rows_for_site("cap.test", 8).is_empty());
        v.restore_history_rows(&rows).unwrap();
        assert_eq!(v.history_rows_for_site("cap.test", 8).len(), 2);
    }

    #[test]
    fn bookmark_delete_restore_roundtrip() {
        let v = Vault::open_in_memory().unwrap();
        let id = v.insert_bookmark("https://bm.test/", "BM", None).unwrap();
        assert!(v.bookmark_cache.contains_key("https://bm.test/"));
        v.delete_bookmark(&id).unwrap();
        assert!(!v.bookmark_cache.contains_key("https://bm.test/"));
        v.restore_bookmark(&id).unwrap();
        let cached = v
            .bookmark_cache
            .get("https://bm.test/")
            .expect("cache refilled");
        assert_eq!(cached.value().0, id);
        assert_eq!(cached.value().1, "BM");
    }

    #[test]
    fn download_remove_restore_roundtrip() {
        let v = Vault::open_in_memory().unwrap();
        v.record_download(
            "dl-1",
            "https://f.test/a.zip",
            "a.zip",
            None,
            Some(9),
            None,
            "Completed",
            false,
        )
        .unwrap();
        let snap = v.download_by_id("dl-1").expect("snapshot");
        assert_eq!(snap.filename, "a.zip");
        v.remove_download("dl-1").unwrap();
        assert!(v.download_by_id("dl-1").is_none());
        v.restore_download_row(&snap).unwrap();
        let back = v.download_by_id("dl-1").expect("restored");
        assert_eq!(back.downloaded_at, snap.downloaded_at);
        assert_eq!(back.state, "Completed");
    }

    #[test]
    fn reading_remove_restore_roundtrip() {
        let v = Vault::open_in_memory().unwrap();
        let id = v.reading_add("https://r.test/essay", "Essay").unwrap();
        let snap = v
            .reading_list(10)
            .into_iter()
            .find(|r| r.id == id)
            .expect("snapshot");
        v.reading_remove(&id).unwrap();
        assert!(v.reading_list(10).iter().all(|r| r.id != id));
        v.restore_reading_row(&snap).unwrap();
        let back = v
            .reading_list(10)
            .into_iter()
            .find(|r| r.id == id)
            .expect("restored");
        assert_eq!(back.added_at, snap.added_at);
        assert!(!back.is_read);
    }

    #[test]
    fn history_revisit_upserts_instead_of_duplicating() {
        let v = Vault::open_in_memory().unwrap();
        let id1 = v
            .insert_history("https://a.com", Some("A"), "typed")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let id2 = v
            .insert_history("https://a.com", Some("A2"), "link")
            .unwrap();
        // Same URL keeps its identity; count bumps, title/transition refresh.
        assert_eq!(id1, id2);
        let hits = v.query_history("a.com", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].visit_count, 2);
        assert_eq!(hits[0].title.as_deref(), Some("A2"));
        assert_eq!(hits[0].transition, "link");
    }

    #[test]
    fn history_exclusion_and_site_delete() {
        let v = Vault::open_in_memory().unwrap();
        v.add_excluded_origin("bank.test").unwrap();
        assert!(v.history_excluded("https://bank.test/login"));
        assert!(v.history_excluded("https://sub.bank.test/x"));
        assert!(!v.history_excluded("https://bank.test.evil.com/"));
        assert!(!v.history_excluded("https://example.com"));
        assert_eq!(v.list_excluded_origins(), vec!["bank.test".to_owned()]);
        v.remove_excluded_origin("bank.test").unwrap();
        assert!(!v.history_excluded("https://bank.test/"));

        v.insert_history("https://a.test/1", None, "link").unwrap();
        v.insert_history("https://sub.a.test/2", None, "link")
            .unwrap();
        v.insert_history("https://b.test/3", None, "link").unwrap();
        // Deletes run against committed rows; let the batch writer flush.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(v.delete_history_site("a.test").unwrap(), 2);
        assert_eq!(v.query_history("a.test", 10).len(), 0);
        assert_eq!(v.query_history("b.test", 10).len(), 1);
    }

    #[test]
    fn history_range_clear_and_id_delete() {
        let v = Vault::open_in_memory().unwrap();
        v.insert_history("https://old.test", None, "link").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        v.insert_history("https://new.test", None, "link").unwrap();
        // Deletes run against committed rows; let the batch writer flush.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(v.delete_history_since(cutoff).unwrap(), 1);
        assert_eq!(v.query_history("", 10).len(), 1);
        let id = v.query_history("", 10)[0].id.clone();
        v.delete_history_id(&id).unwrap();
        assert!(v.query_history("", 10).is_empty());
    }

    #[test]
    fn history_url_delete() {
        let v = Vault::open_in_memory().unwrap();
        v.insert_history("https://gone.test/x", None, "link")
            .unwrap();
        v.insert_history("https://kept.test/y", None, "link")
            .unwrap();
        // Deletes run against committed rows; let the batch writer flush.
        std::thread::sleep(std::time::Duration::from_millis(500));
        v.delete_history_url("https://gone.test/x").unwrap();
        let rest = v.query_history("", 10);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].url, "https://kept.test/y");
        // Idempotent: unknown URL is a no-op, not an error.
        v.delete_history_url("https://gone.test/x").unwrap();
    }

    #[test]
    fn form_history_roundtrip_per_origin() {
        let v = Vault::open_in_memory().unwrap();
        v.record_form_value("https://a.test", "q", "ferrite", 1000)
            .unwrap();
        v.record_form_value("https://a.test", "q", "ferrite", 2000)
            .unwrap();
        v.record_form_value("https://a.test", "q", "wry", 3000)
            .unwrap();
        v.record_form_value("https://b.test", "q", "ferrite", 4000)
            .unwrap();
        // Most-used first; other origins never leak across.
        assert_eq!(
            v.form_values("https://a.test", "q", 10),
            vec!["ferrite".to_owned(), "wry".to_owned()]
        );
        assert_eq!(
            v.form_values("https://b.test", "q", 10),
            vec!["ferrite".to_owned()]
        );
        assert!(v.form_values("https://a.test", "other", 10).is_empty());
        assert_eq!(v.form_values("https://a.test", "q", 1).len(), 1);
        assert_eq!(v.clear_form_history().unwrap(), 3);
        assert!(v.form_values("https://a.test", "q", 10).is_empty());
    }

    #[test]
    fn form_history_origin_delete_scopes_to_one_origin() {
        let v = Vault::open_in_memory().unwrap();
        v.record_form_value("https://a.test", "q", "ferrite", 1000)
            .unwrap();
        v.record_form_value("https://a.test", "email", "a@x.test", 1000)
            .unwrap();
        v.record_form_value("https://b.test", "q", "ferrite", 1000)
            .unwrap();
        assert_eq!(v.count_form_history_origin("https://a.test"), 2);
        assert_eq!(v.count_form_history_origin("https://missing.test"), 0);
        assert_eq!(v.delete_form_history_origin("https://a.test").unwrap(), 2);
        assert_eq!(v.count_form_history_origin("https://a.test"), 0);
        assert_eq!(
            v.form_values("https://b.test", "q", 10),
            vec!["ferrite".to_owned()]
        );
    }

    #[test]
    fn zoom_levels_roundtrip_per_origin() {
        let v = Vault::open_in_memory().unwrap();
        assert_eq!(v.zoom_for("https://a.test"), None);
        v.set_zoom("https://a.test", 1.5).unwrap();
        assert_eq!(v.zoom_for("https://a.test"), Some(1.5));
        // 100% deletes the row (default needs no storage).
        v.set_zoom("https://a.test", 1.0).unwrap();
        assert_eq!(v.zoom_for("https://a.test"), None);
        // Out-of-range clamps into the preset table.
        v.set_zoom("https://b.test", 99.0).unwrap();
        assert_eq!(v.zoom_for("https://b.test"), Some(5.0));
        // Origins never leak across.
        assert_eq!(v.zoom_for("https://c.test"), None);
    }

    #[test]
    fn downloads_record_list_remove_roundtrip() {
        let v = Vault::open_in_memory().unwrap();
        assert!(v.list_downloads(50).is_empty());
        v.record_download(
            "dl-1",
            "https://example.com/f.zip",
            "f.zip",
            None,
            Some(1024),
            Some("ab12"),
            "Completed",
            false,
        )
        .unwrap();
        // Separate millis so newest-first ordering is deterministic.
        std::thread::sleep(std::time::Duration::from_millis(15));
        v.record_download(
            "dl-2",
            "https://example.com/g.exe",
            "g.exe",
            None,
            None,
            None,
            "Failed",
            true,
        )
        .unwrap();
        let rows = v.list_downloads(50);
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0].id, "dl-2");
        assert!(rows[0].is_dangerous);
        assert_eq!(rows[1].sha256_hex.as_deref(), Some("ab12"));
        // Terminal upsert overwrites state, never duplicates.
        v.record_download(
            "dl-1",
            "https://example.com/f.zip",
            "f.zip",
            None,
            None,
            None,
            "Failed",
            false,
        )
        .unwrap();
        let rows = v.list_downloads(50);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.id == "dl-1" && r.state == "Failed"));
        // Remove-from-list deletes the row, not the file (no fs touch here).
        v.remove_download("dl-2").unwrap();
        let rows = v.list_downloads(50);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "dl-1");
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
    fn bookmark_folders_tree_and_reorder() {
        let v = Vault::open_in_memory().unwrap();
        // Folders never touch the URL-keyed cache.
        let f = v.add_folder("Reading", None).unwrap();
        assert!(v.bookmark_cache.is_empty());
        assert!(v.add_folder("  ", None).is_err());
        let a = v.insert_bookmark("https://a.test", "A", None).unwrap();
        let b = v.insert_bookmark("https://b.test", "B", Some(&f)).unwrap();
        // Bar roots: pages and folders side by side.
        let bar: Vec<String> = v
            .list_bookmark_children(None)
            .iter()
            .map(|n| n.id.clone())
            .collect();
        assert!(bar.contains(&a));
        assert!(bar.contains(&f));
        assert!(!bar.contains(&b));
        let folders = v.list_folders();
        assert_eq!(folders.len(), 1);
        assert!(folders[0].is_folder);
        let kids = v.list_bookmark_children(Some(&f));
        assert_eq!(kids.len(), 1);
        assert!(!kids[0].is_folder);
        // Reorder swaps sibling positions (folder keeps its own slot).
        let c = v.insert_bookmark("https://c.test", "C", None).unwrap();
        v.reorder_bookmarks(None, &[c.clone(), a.clone()]).unwrap();
        let bar = v.list_bookmark_children(None);
        let pos = |id: &str| bar.iter().position(|n| n.id == id).unwrap();
        assert!(pos(&c) < pos(&a));
    }

    #[test]
    fn reading_list_add_read_remove() {
        let v = Vault::open_in_memory().unwrap();
        assert!(v.reading_list(50).is_empty());
        assert!(v.reading_add("ferrite://newtab", "x").is_err());
        assert!(v.reading_add("", "x").is_err());
        let id = v
            .reading_add("https://a.test/long-read", "Long Read")
            .unwrap();
        // Idempotent per URL: same id back, still one row.
        assert_eq!(
            v.reading_add("https://a.test/long-read", "Other Title")
                .unwrap(),
            id
        );
        assert_eq!(v.reading_list(50).len(), 1);
        v.reading_mark(&id, true).unwrap();
        let rows = v.reading_list(50);
        assert!(rows[0].is_read);
        v.reading_remove(&id).unwrap();
        assert!(v.reading_list(50).is_empty());
    }

    #[test]
    fn ntp_pin_remove_order() {
        let v = Vault::open_in_memory().unwrap();
        assert!(v.ntp_overrides().is_empty());
        assert!(v.ntp_pin("", "x").is_err());
        assert!(v.ntp_remove("").is_err());
        v.ntp_pin("https://b.test", "B").unwrap();
        v.ntp_pin("https://a.test", "A").unwrap();
        // Pins first, append order kept.
        let rows = v.ntp_overrides();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.pinned && !r.removed));
        assert_eq!(rows[0].url, "https://b.test");
        // Re-pin keeps slot, lifts removal, refreshes title.
        v.ntp_remove("https://b.test").unwrap();
        let rows = v.ntp_overrides();
        assert!(
            rows.iter()
                .find(|r| r.url == "https://b.test")
                .unwrap()
                .removed
        );
        v.ntp_pin("https://b.test", "B2").unwrap();
        let b = v
            .ntp_overrides()
            .into_iter()
            .find(|r| r.url == "https://b.test")
            .unwrap();
        assert!(b.pinned && !b.removed && b.title == "B2");
        // Unpin drops the pin but keeps the row (history untouched either way).
        v.ntp_unpin("https://a.test").unwrap();
        let a = v
            .ntp_overrides()
            .into_iter()
            .find(|r| r.url == "https://a.test")
            .unwrap();
        assert!(!a.pinned);
    }

    #[test]
    fn blocked_sums_day_week_all() {
        let v = Vault::open_in_memory().unwrap();
        assert_eq!(v.blocked_sums(100), (0, 0, 0));
        assert!(v.record_blocked_day(100, 0).is_ok());
        assert_eq!(v.blocked_sums(100), (0, 0, 0));
        v.record_blocked_day(100, 5).unwrap();
        v.record_blocked_day(100, 7).unwrap();
        v.record_blocked_day(99, 3).unwrap();
        v.record_blocked_day(90, 50).unwrap();
        // today=12, week(94..100)=12+3, all=65.
        assert_eq!(v.blocked_sums(100), (12, 15, 65));
        assert_eq!(v.blocked_sums(90), (50, 65, 65));
    }

    #[test]
    fn bookmark_move_rename_folders() {
        let v = Vault::open_in_memory().unwrap();
        let f1 = v.add_folder("Work", None).unwrap();
        let f2 = v.add_folder("Sub", Some(&f1)).unwrap();
        let b = v.insert_bookmark("https://x.test/a", "A", None).unwrap();
        // Move appends at the target's end.
        v.move_bookmark(&b, Some(&f1)).unwrap();
        let kids = v.list_bookmark_children(Some(&f1));
        assert!(kids.iter().any(|n| n.id == b));
        assert!(!v.list_bookmark_children(None).iter().any(|n| n.id == b));
        // Cycles refused: folder into its descendant, into itself.
        assert!(v.move_bookmark(&f1, Some(&f2)).is_err());
        assert!(v.move_bookmark(&f1, Some(&f1)).is_err());
        // Non-folder target and unknown id refused.
        assert!(v.move_bookmark(&f2, Some(&b)).is_err());
        assert!(v.move_bookmark("nope", None).is_err());
        // Rename ok; empty and unknown rejected.
        v.rename_bookmark(&b, "A2").unwrap();
        assert!(
            v.list_bookmark_children(Some(&f1))
                .iter()
                .any(|n| n.title == "A2")
        );
        assert!(v.rename_bookmark(&b, "  ").is_err());
        assert!(v.rename_bookmark("nope", "X").is_err());
        // Folders carry parents for tree nesting.
        let folders = v.bookmark_folders();
        let sub = folders.iter().find(|f| f.id == f2).unwrap();
        assert_eq!(sub.parent.as_deref(), Some(f1.as_str()));
        assert!(
            folders
                .iter()
                .find(|f| f.id == f1)
                .unwrap()
                .parent
                .is_none()
        );
    }

    #[test]
    fn bookmark_details_search_duplicates() {
        let v = Vault::open_in_memory().unwrap();
        v.insert_bookmark("https://d.test/1", "Dup One", None)
            .unwrap();
        v.insert_bookmark("https://D.test/1", "Dup Two", None)
            .unwrap();
        v.insert_bookmark("https://s.test/only", "Solo", None)
            .unwrap();
        let dups = v.duplicate_bookmark_urls();
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].items.len(), 2);
        let hits = v.search_bookmarks("dup", 10);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| !h.is_folder));
        assert_eq!(v.bookmark_details(None).len(), 3);
        // Folders never join dup groups, even title-matching ones.
        v.add_folder("Dup One", None).unwrap();
        assert_eq!(v.duplicate_bookmark_urls().len(), 1);
    }

    #[test]
    fn rekey_refuses_unkeyed_and_short_keys() {
        // In-memory vaults are never keyed: rekey would encrypt, so it
        // must refuse. Runs without the feature (refusal precedes it).
        let v = Vault::open_in_memory().unwrap();
        let err = v.rekey(&[0xAA; 32]).unwrap_err().to_string();
        assert!(err.contains("not keyed"), "got: {err}");
        // Short keys fail before anything is touched.
        let err = v.rekey(&[0xAA; 16]).unwrap_err().to_string();
        assert!(err.contains("32 bytes"), "got: {err}");
    }

    #[test]
    #[cfg(feature = "sqlcipher")]
    fn rekey_roundtrip_under_sqlcipher() {
        let dir = std::env::temp_dir().join(format!(
            "ferrite_test_{}_rekey",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path_str = dir.join("vault.db").to_string_lossy().to_string();
        let old = vec![0xAA; 32];
        let new = vec![0xBB; 32];
        let v = Vault::open(VaultConfig::new(path_str.clone(), old.clone())).unwrap();
        v.insert_history("https://example.com", Some("Example"), "typed")
            .unwrap();
        v.rekey(&new).unwrap();
        drop(v);
        // New key opens with history intact; old key fails closed.
        let v2 = Vault::open(VaultConfig::new(path_str.clone(), new)).unwrap();
        assert!(v2.history_row_by_url("https://example.com").is_some());
        drop(v2);
        assert!(Vault::open(VaultConfig::new(path_str, old)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
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
                transition: "link".to_owned(),
            };
            assert!(tx.try_send(entry).is_ok());
        }
        let mut buf = Vec::new();
        while let Ok(e) = rx.try_recv() {
            buf.push(e);
        }
        assert_eq!(buf.len(), 1000);
        for e in buf {
            let _ = v.insert_history(&e.url, e.title.as_deref(), "link");
        }
        assert!(start.elapsed().as_millis() < 2000);
    }

    #[test]
    fn import_netscape_bookmarks() {
        let html = "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<TITLE>Bookmarks</TITLE>\n<DL><p>\n<DT><A HREF=\"https://example.com\">Example</A>\n<DT><A HREF=\"https://rust-lang.org\">Rust</A>\n</DL><p>";
        let path = std::env::temp_dir().join("ferrite_test_bookmarks.html");
        std::fs::write(&path, html).unwrap();
        let v = Vault::open_in_memory().unwrap();
        let report = crate::import::import_netscape_bookmarks(path.to_str().unwrap(), &v).unwrap();
        assert_eq!(report.imported, 2);
        assert!(v.bookmark_cache.contains_key("https://example.com"));
        // Arrivals land in a dated folder parented at the bar root.
        let roots = v.list_bookmark_children(None);
        assert_eq!(roots.len(), 1);
        assert!(roots[0].is_folder);
        let folders = v.bookmark_folders();
        assert_eq!(folders.len(), 1);
        assert!(folders[0].title.starts_with("Imported "));
        assert_eq!(v.list_bookmark_children(Some(&folders[0].id)).len(), 2);
        // Repeat import reuses the same-day folder and skips duplicates.
        let report2 = crate::import::import_netscape_bookmarks(path.to_str().unwrap(), &v).unwrap();
        assert_eq!(report2.imported, 0);
        assert_eq!(report2.skipped_duplicate, 2);
        assert_eq!(v.bookmark_folders().len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn import_chromium_bookmarks_json() {
        // Chromium `Bookmarks` is JSON (not SQLite): roots with typed
        // nodes, folders nesting children, empty urls skipped.
        let json = r#"{"roots":{
            "bookmark_bar":{"type":"folder","children":[
                {"type":"url","name":"Example","url":"https://example.com"},
                {"type":"folder","name":"Sub","children":[
                    {"type":"url","name":"","url":"https://nested.test/x"}
                ]}
            ]},
            "other":{"type":"folder","children":[
                {"type":"url","name":"Nope","url":""}
            ]},
            "synced":{"type":"folder","children":[]}
        }}"#;
        let path = std::env::temp_dir().join("ferrite_test_chromium_bookmarks");
        std::fs::write(&path, json).unwrap();
        let v = Vault::open_in_memory().unwrap();
        let report = crate::import::import_chromium_bookmarks(path.to_str().unwrap(), &v).unwrap();
        assert_eq!(report.imported, 2);
        assert!(v.bookmark_cache.contains_key("https://example.com"));
        assert!(v.bookmark_cache.contains_key("https://nested.test/x"));
        // Dated folder holds a "Bookmarks bar" mirror with the nested "Sub".
        let folders = v.bookmark_folders();
        assert_eq!(folders.len(), 3);
        assert!(folders[0].title.starts_with("Imported "));
        let bar = v
            .child_folder(Some(&folders[0].id), "Bookmarks bar")
            .unwrap();
        let sub = v.child_folder(Some(&bar), "Sub").unwrap();
        assert!(
            v.list_bookmark_children(Some(&sub))
                .iter()
                .any(|n| n.url == "https://nested.test/x")
        );
        // Not JSON at all: honest error, never a panic.
        std::fs::write(&path, "<html>nope</html>").unwrap();
        assert!(crate::import::import_chromium_bookmarks(path.to_str().unwrap(), &v).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn source_db_copies_before_opening() {
        // A live-shaped SQLite file imports through a temp copy; the
        // copy is gone when the handle drops, the source untouched.
        let src = std::env::temp_dir().join("ferrite_test_source_history.db");
        {
            let conn = rusqlite::Connection::open(&src).unwrap();
            // WebKit microseconds for 2024-01-01T00:00Z with visit counts.
            conn.execute_batch(
                "CREATE TABLE urls(url TEXT, title TEXT, last_visit_time INTEGER, visit_count INTEGER);
                 INSERT INTO urls VALUES('https://a.test','A',13348540800000000,3),('https://b.test','B',13348540800000000,1);",
            )
            .unwrap();
        }
        let v = Vault::open_in_memory().unwrap();
        let report = crate::import::import_chrome_history(src.to_str().unwrap(), &v).unwrap();
        assert_eq!(report.imported, 2);
        // Original timestamps and counts survive, not "now".
        let row = v.history_row_by_url("https://a.test").unwrap();
        assert_eq!(row.last_visit, 1_704_067_200_000);
        assert_eq!(row.visit_count, 3);
        // Re-import merges instead of duplicating.
        let again = crate::import::import_chrome_history(src.to_str().unwrap(), &v).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.merged, 2);
        assert_eq!(
            v.history_row_by_url("https://a.test").unwrap().visit_count,
            6
        );
        assert!(src.exists(), "source file must survive the import");
        let leftover: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("ferrite-import-")
            })
            .collect();
        assert!(leftover.is_empty(), "temp copy must be removed");
        assert!(crate::import::open_source_db("/nonexistent/path.db").is_err());
        let _ = std::fs::remove_file(src);
    }

    #[test]
    fn fractional_insert_touches_only_the_new_row() {
        let v = Vault::open_in_memory().unwrap();
        let a = v.insert_bookmark("https://a.com", "A", None).unwrap();
        let b = v.insert_bookmark("https://b.com", "B", None).unwrap();
        let oa = v.bookmark_order(&a).unwrap();
        let ob = v.bookmark_order(&b).unwrap();
        assert!(oa < ob);
        // Drop between A and B: midpoint, neighbors untouched.
        let m = v
            .insert_bookmark_at("https://m.com", "M", None, Some(1))
            .unwrap();
        let om = v.bookmark_order(&m).unwrap();
        assert_eq!(om, (oa + ob) / 2.0);
        assert_eq!(v.bookmark_order(&a).unwrap(), oa);
        assert_eq!(v.bookmark_order(&b).unwrap(), ob);
        let kids = v.list_bookmark_children(None);
        assert_eq!(
            kids.iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
            vec![a, m, b]
        );
    }

    #[test]
    fn move_to_index_rewrites_one_row() {
        let v = Vault::open_in_memory().unwrap();
        let a = v.insert_bookmark("https://a.com", "A", None).unwrap();
        let b = v.insert_bookmark("https://b.com", "B", None).unwrap();
        let c = v.insert_bookmark("https://c.com", "C", None).unwrap();
        let oa = v.bookmark_order(&a).unwrap();
        let ob = v.bookmark_order(&b).unwrap();
        // C -> front: lands before A, A and B untouched.
        v.move_bookmark_to(&c, None, Some(0)).unwrap();
        assert!(v.bookmark_order(&c).unwrap() < oa);
        assert_eq!(v.bookmark_order(&a).unwrap(), oa);
        assert_eq!(v.bookmark_order(&b).unwrap(), ob);
        let kids = v.list_bookmark_children(None);
        assert_eq!(
            kids.iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
            vec![c.clone(), a.clone(), b.clone()]
        );
        // Cross-folder move lands at target end.
        let f = v.add_folder("F", None).unwrap();
        v.move_bookmark(&a, Some(&f)).unwrap();
        assert!(v.list_bookmark_children(Some(&f)).iter().any(|k| k.id == a));
        // Folder cycle refused.
        assert!(v.move_bookmark(&f, Some(&f)).is_err());
    }

    #[test]
    fn reorder_is_atomic_and_broadcasts() {
        let v = Vault::open_in_memory().unwrap();
        let rx = v.subscribe_bookmarks();
        let a = v.insert_bookmark("https://a.com", "A", None).unwrap();
        let b = v.insert_bookmark("https://b.com", "B", None).unwrap();
        let _ = rx.try_iter().collect::<Vec<_>>(); // drain inserts
        v.reorder_bookmarks(None, &[b.clone(), a.clone()]).unwrap();
        let kids = v.list_bookmark_children(None);
        assert_eq!(
            kids.iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
            vec![b.clone(), a.clone()]
        );
        let events: Vec<_> = rx.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|e| e.kind == crate::BookmarkEventKind::Reordered)
        );
        // Mutations fan out to every subscriber.
        let rx2 = v.subscribe_bookmarks();
        v.rename_bookmark(&a, "A2").unwrap();
        assert!(
            rx2.try_iter()
                .any(|e| e.kind == crate::BookmarkEventKind::Renamed && e.id == a)
        );
    }

    #[test]
    fn favicon_orphans_swept_not_grown() {
        let v = Vault::open_in_memory().unwrap();
        let id = v.insert_bookmark("https://a.com", "A", None).unwrap();
        v.upsert_favicon("https://a.com", &[1, 2, 3]).unwrap();
        assert_eq!(v.favicon_for("https://a.com").unwrap(), vec![1, 2, 3]);
        assert!(v.upsert_favicon("https://a.com", &[]).is_err());
        assert!(v.upsert_favicon("", &[1]).is_err());
        // Still referenced (bookmark live): nothing swept.
        assert_eq!(v.prune_orphan_favicons().unwrap(), 0);
        // Delete bookmark, no history row: orphan reclaimed.
        v.delete_bookmark(&id).unwrap();
        assert_eq!(v.prune_orphan_favicons().unwrap(), 1);
        assert!(v.favicon_for("https://a.com").is_none());
        // History references pin icons too.
        v.upsert_favicon("https://h.com", &[9]).unwrap();
        v.insert_history("https://h.com", Some("H"), "link")
            .unwrap();
        assert_eq!(v.prune_orphan_favicons().unwrap(), 0);
    }

    #[test]
    fn site_permissions_crud_and_replace() {
        let v = Vault::open_in_memory().unwrap();
        assert!(v.list_permission_grants().unwrap().is_empty());
        // Validation first: unknown sources, empty kinds, non-http origins.
        assert!(
            v.save_permission_grant("https://a.test", "camera", "allow", 1, None)
                .is_err()
        );
        assert!(
            v.save_permission_grant("https://a.test", "", "user", 1, None)
                .is_err()
        );
        assert!(
            v.save_permission_grant("ferrite://settings", "camera", "user", 1, None)
                .is_err()
        );
        v.save_permission_grant("https://a.test", "camera", "user", 1000, None)
            .unwrap();
        v.save_permission_grant(
            "https://spam.test",
            "microphone",
            "auto_cooldown",
            1000,
            Some(2000),
        )
        .unwrap();
        let rows = v.list_permission_grants().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&SitePermissionRow {
            origin: "https://a.test".to_owned(),
            kind: "camera".to_owned(),
            source: "user".to_owned(),
            granted_at: 1000,
            expires_at: None,
        }));
        // Upsert overwrites.
        v.save_permission_grant("https://a.test", "camera", "user", 3000, None)
            .unwrap();
        assert_eq!(
            v.list_permission_grants()
                .unwrap()
                .iter()
                .find(|r| r.origin == "https://a.test")
                .unwrap()
                .granted_at,
            3000
        );
        // Reset action.
        assert!(
            v.remove_permission_grant("https://a.test", "camera")
                .unwrap()
        );
        assert!(
            !v.remove_permission_grant("https://a.test", "camera")
                .unwrap()
        );
        // Whole-snapshot replace is atomic (bad row rejects everything).
        assert!(
            v.replace_permission_grants(&[SitePermissionRow {
                origin: "https://b.test".to_owned(),
                kind: "camera".to_owned(),
                source: "bogus".to_owned(),
                granted_at: 1,
                expires_at: None,
            }])
            .is_err()
        );
        assert!(
            v.list_permission_grants()
                .unwrap()
                .iter()
                .any(|r| r.origin == "https://spam.test")
        );
        v.replace_permission_grants(&[]).unwrap();
        assert!(v.list_permission_grants().unwrap().is_empty());
    }

    #[test]
    fn cert_exception_keys_and_match() {
        assert_eq!(
            cert_exception_host("https://Router.LAN:8443/admin?x=1"),
            Some("router.lan".to_owned())
        );
        assert_eq!(
            cert_exception_host("router.lan"),
            Some("router.lan".to_owned())
        );
        assert_eq!(cert_exception_host("https://"), None);
        assert_eq!(cert_exception_host("not a host"), None);
        assert_eq!(cert_exception_host(""), None);
        assert!(valid_cert_fingerprint(&"ab".repeat(32)));
        assert!(!valid_cert_fingerprint("ab"));
        assert!(!valid_cert_fingerprint(&"zz".repeat(32)));

        let v = Vault::open_in_memory().unwrap();
        let fp = "ab".repeat(32);
        // Validation first.
        assert!(
            v.save_cert_exception("not a host", &fp, 2000, 1000)
                .is_err()
        );
        assert!(v.save_cert_exception("h.test", "nope", 2000, 1000).is_err());
        assert!(v.save_cert_exception("h.test", &fp, 500, 1000).is_err());
        assert!(!v.cert_exception_match("h.test", &fp, 1000));
        v.save_cert_exception("https://h.test:8443/x", &fp, 2000, 1000)
            .unwrap();
        // Exact host, exact fingerprint, unexpired; nothing looser.
        assert!(v.cert_exception_match("h.test", &fp, 1000));
        assert!(v.cert_exception_match("https://h.test/", &fp, 1999));
        assert!(!v.cert_exception_match("h.test", &fp, 2000));
        assert!(!v.cert_exception_match("h.test", &"cd".repeat(32), 1000));
        assert!(!v.cert_exception_match("sub.h.test", &fp, 1000));
        assert!(!v.cert_exception_match("h.test.evil.com", &fp, 1000));
        // Refresh + list + revoke + prune.
        v.save_cert_exception("h.test", &fp, 5000, 1000).unwrap();
        assert!(v.cert_exception_match("h.test", &fp, 2000));
        assert_eq!(v.list_cert_exceptions().unwrap().len(), 1);
        assert!(!v.remove_cert_exception("h.test", &"cd".repeat(32)).unwrap());
        assert!(v.remove_cert_exception("h.test", &fp).unwrap());
        assert!(!v.cert_exception_match("h.test", &fp, 1000));
        v.save_cert_exception("old.test", &fp, 1500, 1000).unwrap();
        assert_eq!(v.prune_expired_cert_exceptions(1500).unwrap(), 1);
        assert!(v.list_cert_exceptions().unwrap().is_empty());
    }

    #[test]
    fn threat_overrides_reports_and_top_hosts() {
        let v = Vault::open_in_memory().unwrap();
        assert!(v.save_threat_exception("not a host", 1000).is_err());
        assert!(v.save_threat_exception("evil.test", 0).is_err());
        v.save_threat_exception("https://EVIL.test/x", 1000)
            .unwrap();
        assert_eq!(
            v.list_threat_exceptions().unwrap(),
            vec!["evil.test".to_owned()]
        );
        assert!(!v.remove_threat_exception("other.test").unwrap());
        assert!(v.remove_threat_exception("evil.test").unwrap());
        assert!(v.list_threat_exceptions().unwrap().is_empty());
        // Reports validate trigger + host, stay local and countable.
        assert!(v.save_threat_report("evil.test", "bogus", 1000).is_err());
        assert!(v.save_threat_report("not a host", "malware", 1000).is_err());
        v.save_threat_report("evil.test", "lookalike", 1000)
            .unwrap();
        v.save_threat_report("evil.test", "malware", 2000).unwrap();
        assert_eq!(v.threat_report_count("evil.test"), 2);
        assert_eq!(v.threat_report_count("other.test"), 0);
        // Top hosts come from visit counts, deduped, most-visited first.
        v.insert_history("https://bank.test/a", Some("A"), "typed")
            .unwrap();
        v.insert_history("https://bank.test/b", Some("B"), "link")
            .unwrap();
        v.insert_history("https://shop.test/", Some("S"), "link")
            .unwrap();
        let top = v.top_hosts(10);
        assert_eq!(top.first().map(String::as_str), Some("bank.test"));
        assert!(top.contains(&"shop.test".to_owned()));
        assert_eq!(v.top_hosts(1), vec!["bank.test".to_owned()]);
    }

    #[test]
    fn history_origin_stats_and_delete_match_exact_host() {
        let v = Vault::open_in_memory().unwrap();
        v.import_history_row(
            "https://a.test/x",
            Some("X"),
            "link",
            Some(1_700_000_000_000),
            3,
        )
        .unwrap();
        v.import_history_row(
            "https://a.test/y",
            Some("Y"),
            "link",
            Some(1_701_000_000_000),
            2,
        )
        .unwrap();
        v.import_history_row(
            "https://evil-a.test/",
            Some("E"),
            "link",
            Some(1_702_000_000_000),
            9,
        )
        .unwrap();
        v.import_history_row(
            "https://a.test:8443/p",
            Some("P"),
            "link",
            Some(1_703_000_000_000),
            1,
        )
        .unwrap();
        let (visits, last) = v.history_origin_stats("a.test").unwrap();
        assert_eq!(visits, 6);
        assert_eq!(last, 1_703_000_000_000);
        assert_eq!(v.history_origin_stats("missing.test").unwrap(), (0, 0));
        // Substring host survives; port variants count as the same host.
        assert_eq!(v.delete_history_origin("a.test").unwrap(), 3);
        assert!(v.history_row_by_url("https://evil-a.test/").is_some());
        assert!(v.history_row_by_url("https://a.test/x").is_none());
    }

    #[test]
    fn history_retention_prunes_by_age_mirrors_cache() {
        let v = Vault::open_in_memory().unwrap();
        let day = 86_400_000i64;
        let now = 1_750_000_000_000i64;
        v.import_history_row(
            "https://old.test/",
            Some("Old"),
            "link",
            Some(now - 100 * day),
            1,
        )
        .unwrap();
        v.import_history_row(
            "https://new.test/",
            Some("New"),
            "link",
            Some(now - 10 * day),
            1,
        )
        .unwrap();
        // Cutoff at 90 days: only the old row dies, cache mirrors it.
        assert_eq!(v.prune_history_older_than(now - 90 * day).unwrap(), 1);
        assert!(v.history_row_by_url("https://old.test/").is_none());
        assert!(v.history_row_by_url("https://new.test/").is_some());
        // Nothing left to prune: zero, not an error.
        assert_eq!(v.prune_history_older_than(now - 90 * day).unwrap(), 0);
    }

    #[test]
    fn reclaim_space_is_safe_and_bounded() {
        let v = Vault::open_in_memory().unwrap();
        // Fresh database: nothing to reclaim, never an error.
        assert_eq!(v.reclaim_space().unwrap(), 0);
        // Fragment, then reclaim: deleted rows free pages, the sweep
        // gives some back (exact count is SQLite's business).
        for i in 0..200 {
            v.import_history_row(
                &format!("https://frag.test/{i}"),
                None,
                "link",
                Some(1_750_000_000_000),
                1,
            )
            .unwrap();
        }
        for i in 0..200 {
            v.delete_history_url(&format!("https://frag.test/{i}"))
                .unwrap();
        }
        let _ = v.reclaim_space().unwrap();
    }

    #[test]
    fn history_cache_bounded_while_db_keeps_everything() {
        let v = Vault::open_in_memory().unwrap();
        for i in 0..25_000 {
            v.import_history_row(
                &format!("https://cap.test/{i}"),
                Some("T"),
                "link",
                Some(1_700_000_000_000 + i),
                1,
            )
            .unwrap();
        }
        assert!(
            v.history_cache.len() <= 20_000,
            "cache must stay bounded, len={}",
            v.history_cache.len()
        );
        let n: i64 = v
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 25_000, "eviction never drops DB rows");
        // An evicted URL still resolves through the DB fallback.
        assert!(v.history_row_by_url("https://cap.test/0").is_some());
        assert_eq!(v.query_history("cap.test/0", 5).len(), 1);
    }

    #[test]
    fn top_hosts_counts_cold_db_rows() {
        let v = Vault::open_in_memory().unwrap();
        // Bypass the hot cache with direct SQL: simulates rows that are
        // committed but cold (post-restart, pre-warmup, or cache-evicted).
        {
            let conn = v.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO history(id, url, title, visit_count, last_visit_timestamp, hlc_timestamp, transition) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    "https://cold.test/a",
                    "Cold",
                    9,
                    1_700_000_000_000i64,
                    1_700_000_000_000i64,
                    "link",
                ],
            )
            .unwrap();
        }
        assert!(v.history_cache.is_empty());
        // Cache-only code returns nothing here; the UNION must find it.
        let top = v.top_hosts(10);
        assert!(
            top.contains(&"cold.test".to_owned()),
            "cold rows must count, got: {top:?}"
        );
    }
}
