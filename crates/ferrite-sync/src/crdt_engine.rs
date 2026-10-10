//! Delta state CRDT engine. Deltas filter in SQL, apply in one transaction
//! with LWW guards. Live tabs are a TTL inbox, not consensus.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension};

use crate::sync_actions::{CrdtMessage, LiveTabAction, SyncAction, SyncCategory};
use crate::vector_clock::VectorClock;

/// Per category delta cap. Bounds one sync round.
pub const DELTA_LIMIT_PER_CATEGORY: usize = 500;
/// Total cap per `deltas_for_peer` call.
pub const DELTA_TOTAL_CAP: usize = 3000;
/// Live tab presence TTL and cap.
pub const LIVE_TAB_TTL: Duration = Duration::from_secs(5 * 60);
pub const LIVE_TAB_CAP: usize = 100;
/// Wire-timestamp skew bound for live-tab pushes. The wire `timestamp`
/// is accepted only when within +/- this of local receipt time;
/// otherwise receipt time is stamped (clamps replay/future ghosts).
pub const LIVE_TAB_SKEW_MS: i64 = 10 * 60 * 1000;

pub struct CrdtEngine {
    vault: Arc<Mutex<Connection>>,
    clock: VectorClock,
    device_id: String,
    live_tabs: HashMap<(String, String), (LiveTabAction, Instant)>,
}

impl CrdtEngine {
    pub fn new(vault: Arc<Mutex<Connection>>, device_id: String) -> Self {
        let mut engine = Self {
            vault: Arc::clone(&vault),
            clock: VectorClock::new(),
            device_id: device_id.clone(),
            live_tabs: HashMap::new(),
        };
        engine.ensure_sync_indexes();
        engine.load_clock();
        engine
    }

    /// Sync owned indexes. Best effort, older vaults still sync, just slower.
    fn ensure_sync_indexes(&self) {
        if let Ok(conn) = self.vault.lock() {
            let _ = conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_bookmarks_hlc ON bookmarks(hlc_timestamp);
                 CREATE INDEX IF NOT EXISTS idx_history_hlc ON history(hlc_timestamp);
                 CREATE INDEX IF NOT EXISTS idx_credentials_hlc ON credentials(hlc_timestamp);
                 CREATE INDEX IF NOT EXISTS idx_autofill_hlc ON autofill_records(hlc_timestamp);
                 CREATE INDEX IF NOT EXISTS idx_totp_cred ON totp_secrets(credential_id);",
            );
            let _ = conn.execute_batch(
                "ALTER TABLE autofill_records ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0;",
            );
        }
    }

    /// Copy of our vector clock for SyncRequest watermarks. Peers compute
    /// their deltas against it (high-water approximation, see
    /// `peer_watermark`).
    pub fn clock_snapshot(&self) -> HashMap<String, u64> {
        self.clock.0.clone()
    }

    pub fn persist_clock(&self) {
        if let Ok(conn) = self.vault.lock() {
            for (did, cnt) in &self.clock.0 {
                let _ = conn.execute(
                    "INSERT OR REPLACE INTO sync_vector_clock(device_id, counter) VALUES(?1,?2)",
                    rusqlite::params![did, *cnt as i64],
                );
            }
        }
    }

    fn load_clock(&mut self) {
        if let Ok(conn) = self.vault.lock()
            && let Ok(mut stmt) = conn.prepare("SELECT device_id, counter FROM sync_vector_clock")
            && let Ok(rows) =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        {
            for row in rows.flatten() {
                let (did, cnt) = row;
                self.clock.0.insert(did, cnt as u64);
            }
        }
    }

    /// High-water mark from the peer clock. Rows store HLC timestamps, so
    /// per-device causality is approximated by the maximum. True per-device
    /// causality needs origin-device columns (deferred).
    fn peer_watermark(peer_clock: &HashMap<String, u64>) -> i64 {
        peer_clock.values().copied().max().unwrap_or(0) as i64
    }

    /// Generate deltas since the peer high-water mark, filtered by
    /// enabled categories. TrustedDevices always included even if
    /// disabled (mandatory). SQL-filtered and bounded, no full-table clones.
    pub fn deltas_for_peer(
        &self,
        peer_clock: &HashMap<String, u64>,
        enabled: &HashSet<SyncCategory>,
    ) -> Vec<SyncAction> {
        let mut deltas = Vec::new();
        let Ok(conn) = self.vault.lock() else {
            return deltas;
        };
        let watermark = Self::peer_watermark(peer_clock);
        let limit = DELTA_LIMIT_PER_CATEGORY as i64;
        // Helper to check if category should be sent
        let should_send =
            |cat: SyncCategory| cat == SyncCategory::TrustedDevices || enabled.contains(&cat);
        let push = |deltas: &mut Vec<SyncAction>, a: SyncAction| {
            if deltas.len() < DELTA_TOTAL_CAP {
                deltas.push(a);
            }
        };

        if should_send(SyncCategory::Bookmarks)
            && let Ok(mut stmt) = conn.prepare(
                "SELECT id, url, title, parent_folder_id, sort_order, hlc_timestamp, is_deleted FROM bookmarks WHERE hlc_timestamp > ?1 ORDER BY hlc_timestamp ASC LIMIT ?2",
            )
            && let Ok(rows) = stmt.query_map(rusqlite::params![watermark, limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            })
        {
            for row in rows.flatten() {
                let (id, url, title, parent, sort_order, hlc, deleted) = row;
                let hlc_u = hlc as u64;
                if deleted != 0 {
                    push(
                        &mut deltas,
                        SyncAction::DeleteBookmark {
                            id,
                            hlc_timestamp: hlc_u,
                        },
                    );
                } else {
                    push(
                        &mut deltas,
                        SyncAction::AddBookmark {
                            id,
                            url,
                            title,
                            parent_folder_id: parent,
                            sort_order,
                            hlc_timestamp: hlc_u,
                        },
                    );
                }
            }
        }

        if should_send(SyncCategory::History)
            && let Ok(mut stmt) = conn.prepare(
                "SELECT id, url, title, visit_count, last_visit_timestamp, hlc_timestamp FROM history WHERE hlc_timestamp > ?1 ORDER BY hlc_timestamp ASC LIMIT ?2",
            )
            && let Ok(rows) = stmt.query_map(rusqlite::params![watermark, limit], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                })
        {
            for row in rows.flatten() {
                let (id, url, title, visit_count, last_visit, hlc) = row;
                push(
                    &mut deltas,
                    SyncAction::UpsertHistoryEntry {
                        id,
                        url,
                        title: title.unwrap_or_default(),
                        visit_count: visit_count as u32,
                        last_visit_timestamp: last_visit,
                        hlc_timestamp: hlc as u64,
                    },
                );
            }
        }

        // Archive metadata only (lazy) - compressed_html not sent unless FetchArchiveBody
        if should_send(SyncCategory::Archive)
            && let Ok(mut stmt) = conn.prepare(
                "SELECT id, url, capture_timestamp, is_pinned, hlc_timestamp FROM web_snapshots WHERE hlc_timestamp > ?1 ORDER BY hlc_timestamp ASC LIMIT ?2",
            )
            && let Ok(rows) = stmt.query_map(rusqlite::params![watermark, limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
        {
            for row in rows.flatten() {
                let (id, url, cap, pinned, hlc) = row;
                // Lazy: only metadata, body via FetchArchiveBody on demand
                push(
                    &mut deltas,
                    SyncAction::AddWebSnapshotMetadata {
                        id,
                        url,
                        capture_timestamp: cap,
                        is_pinned: pinned != 0,
                        hlc_timestamp: hlc as u64,
                    },
                );
            }
        }

        for cat in [
            SyncCategory::Credentials,
            SyncCategory::Autofill,
            SyncCategory::Totp,
            SyncCategory::TrustedDevices,
        ] {
            if !should_send(cat) && cat != SyncCategory::TrustedDevices {
                continue;
            }
            match cat {
                SyncCategory::Credentials => {
                    if let Ok(mut stmt) = conn.prepare(
                        "SELECT id, encrypted_password, hlc_timestamp, is_deleted FROM credentials WHERE hlc_timestamp > ?1 ORDER BY hlc_timestamp ASC LIMIT ?2",
                    )
                        && let Ok(rows) = stmt.query_map(rusqlite::params![watermark, limit], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, Vec<u8>>(1)?,
                                r.get::<_, i64>(2)?,
                                r.get::<_, i64>(3)?,
                            ))
                        })
                    {
                        for row in rows.flatten() {
                            let (id, enc, hlc, deleted) = row;
                            let hlc_u = hlc as u64;
                            if deleted != 0 {
                                push(
                                    &mut deltas,
                                    SyncAction::DeleteCredential {
                                        id,
                                        hlc_timestamp: hlc_u,
                                    },
                                );
                            } else {
                                push(
                                    &mut deltas,
                                    SyncAction::UpdateCredential {
                                        id,
                                        encrypted_password: enc,
                                        hlc_timestamp: hlc_u,
                                    },
                                );
                            }
                        }
                    }
                }
                SyncCategory::Autofill => {
                    if let Ok(mut stmt) = conn.prepare(
                        "SELECT id, encrypted_fields, hlc_timestamp, is_deleted FROM autofill_records WHERE hlc_timestamp > ?1 ORDER BY hlc_timestamp ASC LIMIT ?2",
                    )
                        && let Ok(rows) = stmt.query_map(rusqlite::params![watermark, limit], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, Vec<u8>>(1)?,
                                r.get::<_, i64>(2)?,
                                r.get::<_, i64>(3)?,
                            ))
                        })
                    {
                        for row in rows.flatten() {
                            let (id, enc, hlc, deleted) = row;
                            let hlc_u = hlc as u64;
                            if deleted != 0 {
                                push(
                                    &mut deltas,
                                    SyncAction::DeleteAutofillRecord {
                                        id,
                                        hlc_timestamp: hlc_u,
                                    },
                                );
                            } else {
                                push(
                                    &mut deltas,
                                    SyncAction::UpdateAutofillRecord {
                                        id,
                                        encrypted_fields: enc,
                                        hlc_timestamp: hlc_u,
                                    },
                                );
                            }
                        }
                    }
                }
                SyncCategory::Totp => {
                    // Stable HLC via the credential row: the old code minted
                    // `now()` per row per sync, so peers never converged and
                    // re-sent forever.
                    if let Ok(mut stmt) = conn.prepare(
                        "SELECT t.credential_id, t.encrypted_secret, t.digits, t.period_seconds, c.hlc_timestamp FROM totp_secrets t JOIN credentials c ON c.id = t.credential_id WHERE c.hlc_timestamp > ?1 ORDER BY c.hlc_timestamp ASC LIMIT ?2",
                    )
                        && let Ok(rows) = stmt.query_map(rusqlite::params![watermark, limit], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, Vec<u8>>(1)?,
                                r.get::<_, i64>(2)?,
                                r.get::<_, i64>(3)?,
                                r.get::<_, i64>(4)?,
                            ))
                        })
                    {
                        for row in rows.flatten() {
                            let (cid, enc, digits, period, hlc) = row;
                            push(
                                &mut deltas,
                                SyncAction::UpsertTotpSecret {
                                    credential_id: cid,
                                    encrypted_secret: enc,
                                    digits: digits as u8,
                                    period_seconds: period as u32,
                                    hlc_timestamp: hlc as u64,
                                },
                            );
                        }
                    }
                }
                SyncCategory::TrustedDevices => {
                    if let Ok(mut stmt) = conn.prepare(
                        "SELECT device_id, device_name, public_key, paired_via_device_id, hlc_timestamp, is_revoked FROM trusted_devices WHERE hlc_timestamp > ?1 ORDER BY hlc_timestamp ASC LIMIT ?2",
                    )
                        && let Ok(rows) = stmt.query_map(rusqlite::params![watermark, limit], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, Vec<u8>>(2)?,
                                r.get::<_, Option<String>>(3)?,
                                r.get::<_, i64>(4)?,
                                r.get::<_, i64>(5)?,
                            ))
                        })
                    {
                        for row in rows.flatten() {
                            let (did, name, pk, via, hlc, revoked) = row;
                            let hlc_u = hlc as u64;
                            if revoked != 0 {
                                push(
                                    &mut deltas,
                                    SyncAction::RevokeTrustedDevice {
                                        device_id: did,
                                        hlc_timestamp: hlc_u,
                                    },
                                );
                            } else {
                                push(
                                    &mut deltas,
                                    SyncAction::AddTrustedDevice {
                                        device_id: did,
                                        device_name: name,
                                        public_key: pk,
                                        paired_via_device_id: via,
                                        hlc_timestamp: hlc_u,
                                    },
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if deltas.len() >= DELTA_TOTAL_CAP {
            tracing::debug!(
                capped = DELTA_TOTAL_CAP,
                "delta batch truncated; peer should re-request"
            );
        }
        deltas
    }

    /// Stored HLC for a row by primary key (O(1) LWW guard). Missing rows
    /// yield None (insert path).
    fn stored_hlc(
        conn: &rusqlite::Connection,
        table: &str,
        id_col: &str,
        id: &str,
    ) -> Result<Option<i64>, String> {
        // Table/column names are internal constants, never peer input.
        let sql = format!("SELECT hlc_timestamp FROM {table} WHERE {id_col}=?1");
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("lww prepare: {e}"))?;
        stmt.query_row([id], |r| r.get(0))
            .optional()
            .map_err(|e| format!("lww read: {e}"))
    }

    /// Apply incoming deltas with LWW on every type, in ONE transaction:
    /// a crash or error mid-batch rolls back instead of half-applying.
    pub fn apply_deltas(&mut self, deltas: Vec<SyncAction>) -> Result<(), String> {
        // Scope the connection guard: `persist_clock` re-locks below, and
        // std Mutex is not reentrant (holding it across caused a deadlock).
        let (result, max_hlc) = {
            let conn = self.vault.lock().map_err(|e| format!("lock: {e}"))?;
            Self::apply_in_txn(&conn, &deltas)?
        };
        match result {
            Ok(()) => {
                // Track the batch high-water mark durably (the old code
                // incremented per action and never persisted, so restarts
                // resurrected deleted rows). Stored on our own clock entry
                // as an HLC-scale watermark that peers use for filtering.
                self.clock.increment(&self.device_id);
                if max_hlc > 0 {
                    let cur = self.clock.0.get(&self.device_id).copied().unwrap_or(0);
                    self.clock
                        .0
                        .insert(self.device_id.clone(), cur.max(max_hlc));
                }
                self.persist_clock();
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn apply_in_txn(
        conn: &rusqlite::Connection,
        deltas: &[SyncAction],
    ) -> Result<(Result<(), String>, u64), String> {
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| format!("begin: {e}"))?;
        fn apply_one(conn: &rusqlite::Connection, action: &SyncAction) -> Result<(), String> {
            match action {
                SyncAction::AddBookmark {
                    id,
                    url,
                    title,
                    parent_folder_id,
                    sort_order,
                    hlc_timestamp,
                } => {
                    if let Some(s) = CrdtEngine::stored_hlc(conn, "bookmarks", "id", id)?
                        && (*hlc_timestamp as i64) <= s
                    {
                        return Ok(());
                    }
                    conn.execute(
                            "INSERT OR REPLACE INTO bookmarks(id, url, title, parent_folder_id, sort_order, hlc_timestamp, is_deleted) VALUES(?1,?2,?3,?4,?5,?6,0)",
                            rusqlite::params![id, url, title, parent_folder_id, sort_order, *hlc_timestamp as i64],
                        )
                        .map_err(|e| format!("bookmark: {e}"))?;
                }
                SyncAction::DeleteBookmark { id, hlc_timestamp } => {
                    let n = conn
                            .execute(
                                "UPDATE bookmarks SET is_deleted=1, hlc_timestamp=?1 WHERE id=?2 AND hlc_timestamp < ?1",
                                rusqlite::params![*hlc_timestamp as i64, id],
                            )
                            .map_err(|e| format!("del bookmark: {e}"))?;
                    if n == 0 && CrdtEngine::stored_hlc(conn, "bookmarks", "id", id)?.is_none() {
                        // Tombstone for unknown rows so the delete wins
                        // over concurrent re-adds with older HLC.
                        conn.execute(
                                "INSERT INTO bookmarks(id, url, title, parent_folder_id, sort_order, hlc_timestamp, is_deleted) VALUES(?1,'','',NULL,0,?2,1)",
                                rusqlite::params![id, *hlc_timestamp as i64],
                            )
                            .map_err(|e| format!("del bookmark tombstone: {e}"))?;
                    }
                }
                SyncAction::UpsertHistoryEntry {
                    id,
                    url,
                    title,
                    visit_count,
                    last_visit_timestamp,
                    hlc_timestamp,
                } => {
                    if let Some(s) = CrdtEngine::stored_hlc(conn, "history", "id", id)?
                        && (*hlc_timestamp as i64) <= s
                    {
                        return Ok(());
                    }
                    conn.execute(
                            "INSERT OR REPLACE INTO history(id, url, title, visit_count, last_visit_timestamp, hlc_timestamp) VALUES(?1,?2,?3,?4,?5,?6)",
                            rusqlite::params![id, url, title, *visit_count as i64, last_visit_timestamp, *hlc_timestamp as i64],
                        )
                        .map_err(|e| format!("history: {e}"))?;
                }
                SyncAction::AddWebSnapshotMetadata {
                    id,
                    url,
                    capture_timestamp,
                    is_pinned,
                    hlc_timestamp,
                } => {
                    // Lazy: metadata only; body arrives via FetchArchiveBody.
                    conn.execute(
                            "INSERT OR IGNORE INTO web_snapshots(id, url, capture_timestamp, compressed_html, is_pinned, hlc_timestamp) VALUES(?1,?2,?3,?4,?5,?6)",
                            rusqlite::params![id, url, capture_timestamp, Vec::<u8>::new(), if *is_pinned {1} else {0}, *hlc_timestamp as i64],
                        )
                        .map_err(|e| format!("archive meta: {e}"))?;
                    conn.execute(
                            "UPDATE web_snapshots SET is_pinned=?1, hlc_timestamp=?2 WHERE id=?3 AND hlc_timestamp < ?2",
                            rusqlite::params![if *is_pinned {1} else {0}, *hlc_timestamp as i64, id],
                        )
                        .map_err(|e| format!("archive meta pin: {e}"))?;
                }
                SyncAction::AddTrustedDevice {
                    device_id,
                    device_name,
                    public_key,
                    paired_via_device_id,
                    hlc_timestamp,
                } => {
                    if let Some(s) =
                        CrdtEngine::stored_hlc(conn, "trusted_devices", "device_id", device_id)?
                        && (*hlc_timestamp as i64) <= s
                    {
                        return Ok(());
                    }
                    conn.execute(
                            "INSERT OR REPLACE INTO trusted_devices(device_id, device_name, public_key, paired_via_device_id, first_paired_at, hlc_timestamp, is_revoked) VALUES(?1,?2,?3,?4,?5,?6,0)",
                            rusqlite::params![device_id, device_name, public_key, paired_via_device_id, *hlc_timestamp as i64, *hlc_timestamp as i64],
                        )
                        .map_err(|e| format!("trusted add: {e}"))?;
                }
                SyncAction::RevokeTrustedDevice {
                    device_id,
                    hlc_timestamp,
                } => {
                    let n = conn
                            .execute(
                                "UPDATE trusted_devices SET is_revoked=1, hlc_timestamp=?1 WHERE device_id=?2 AND hlc_timestamp < ?1",
                                rusqlite::params![*hlc_timestamp as i64, device_id],
                            )
                            .map_err(|e| format!("revoke: {e}"))?;
                    if n == 0
                        && CrdtEngine::stored_hlc(conn, "trusted_devices", "device_id", device_id)?
                            .is_none()
                    {
                        conn.execute(
                                "INSERT INTO trusted_devices(device_id, device_name, public_key, paired_via_device_id, first_paired_at, hlc_timestamp, is_revoked) VALUES(?1,'',x'',NULL,?2,?2,1)",
                                rusqlite::params![device_id, *hlc_timestamp as i64],
                            )
                            .map_err(|e| format!("revoke tombstone: {e}"))?;
                    }
                }
                SyncAction::UpdateCredential {
                    id,
                    encrypted_password,
                    hlc_timestamp,
                } => {
                    // Deltas carry only the secret: preserve url/username
                    // from the local row, or store placeholders for fresh
                    // rows (subselects yield NULL and violate NOT NULL).
                    let existing: Option<(String, String, i64)> = conn
                        .query_row(
                            "SELECT url, username, hlc_timestamp FROM credentials WHERE id=?1",
                            [id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .optional()
                        .map_err(|e| format!("credential read: {e}"))?;
                    if let Some((_, _, s)) = &existing
                        && (*hlc_timestamp as i64) <= *s
                    {
                        return Ok(());
                    }
                    let (url, username) = existing.map(|(u, n, _)| (u, n)).unwrap_or_default();
                    conn.execute(
                            "INSERT OR REPLACE INTO credentials(id, url, username, encrypted_password, hlc_timestamp, is_deleted) VALUES(?1,?2,?3,?4,?5,0)",
                            rusqlite::params![id, url, username, encrypted_password, *hlc_timestamp as i64],
                        )
                        .map_err(|e| format!("credential: {e}"))?;
                }
                SyncAction::DeleteCredential { id, hlc_timestamp } => {
                    let n = conn
                            .execute(
                                "UPDATE credentials SET is_deleted=1, hlc_timestamp=?1 WHERE id=?2 AND hlc_timestamp < ?1",
                                rusqlite::params![*hlc_timestamp as i64, id],
                            )
                            .map_err(|e| format!("del credential: {e}"))?;
                    if n == 0 && CrdtEngine::stored_hlc(conn, "credentials", "id", id)?.is_none() {
                        conn.execute(
                                "INSERT INTO credentials(id, url, username, encrypted_password, hlc_timestamp, is_deleted) VALUES(?1,'','',x'',?2,1)",
                                rusqlite::params![id, *hlc_timestamp as i64],
                            )
                            .map_err(|e| format!("del credential tombstone: {e}"))?;
                    }
                }
                SyncAction::UpdateAutofillRecord {
                    id,
                    encrypted_fields,
                    hlc_timestamp,
                } => {
                    // Preserve record_type/label from the local row;
                    // fresh rows get neutral placeholders.
                    let existing: Option<(String, String, i64)> = conn
                            .query_row(
                                "SELECT record_type, label, hlc_timestamp FROM autofill_records WHERE id=?1",
                                [id],
                                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                            )
                            .optional()
                            .map_err(|e| format!("autofill read: {e}"))?;
                    if let Some((_, _, s)) = &existing
                        && (*hlc_timestamp as i64) <= *s
                    {
                        return Ok(());
                    }
                    let (rtype, label) = existing
                        .map(|(t, l, _)| (t, l))
                        .unwrap_or_else(|| ("address".to_owned(), String::new()));
                    conn.execute(
                            "INSERT OR REPLACE INTO autofill_records(id, record_type, label, encrypted_fields, hlc_timestamp, is_deleted) VALUES(?1,?2,?3,?4,?5,0)",
                            rusqlite::params![id, rtype, label, encrypted_fields, *hlc_timestamp as i64],
                        )
                        .map_err(|e| format!("autofill: {e}"))?;
                }
                SyncAction::DeleteAutofillRecord { id, hlc_timestamp } => {
                    let n = conn
                            .execute(
                                "UPDATE autofill_records SET is_deleted=1, hlc_timestamp=?1 WHERE id=?2 AND hlc_timestamp < ?1",
                                rusqlite::params![*hlc_timestamp as i64, id],
                            )
                            .map_err(|e| format!("del autofill: {e}"))?;
                    if n == 0
                        && CrdtEngine::stored_hlc(conn, "autofill_records", "id", id)?.is_none()
                    {
                        conn.execute(
                                "INSERT INTO autofill_records(id, record_type, label, encrypted_fields, hlc_timestamp, is_deleted) VALUES(?1,'address','',x'',?2,1)",
                                rusqlite::params![id, *hlc_timestamp as i64],
                            )
                            .map_err(|e| format!("del autofill tombstone: {e}"))?;
                    }
                }
                SyncAction::UpsertTotpSecret {
                    credential_id,
                    encrypted_secret,
                    digits,
                    period_seconds,
                    hlc_timestamp,
                } => {
                    // Guard against the credential's HLC: TOTP rows carry
                    // no HLC of their own, so the parent row decides.
                    let stored_cred =
                        CrdtEngine::stored_hlc(conn, "credentials", "id", credential_id)?;
                    if let Some(s) = stored_cred
                        && (*hlc_timestamp as i64) < s
                    {
                        return Ok(());
                    }
                    conn.execute(
                            "INSERT OR REPLACE INTO totp_secrets(credential_id, encrypted_secret, digits, period_seconds) VALUES(?1,?2,?3,?4)",
                            rusqlite::params![credential_id, encrypted_secret, *digits as i64, *period_seconds as i64],
                        )
                        .map_err(|e| format!("totp: {e}"))?;
                }
            }
            Ok(())
        }
        let mut max_hlc: u64 = 0;
        let mut apply_err: Option<String> = None;
        for action in deltas {
            max_hlc = max_hlc.max(action.hlc());
            if let Err(e) = apply_one(conn, action) {
                apply_err = Some(e);
                break;
            }
        }
        match apply_err {
            None => {
                conn.execute_batch("COMMIT;")
                    .map_err(|e| format!("commit: {e}"))?;
                Ok((Ok(()), max_hlc))
            }
            Some(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Ok((Err(e), max_hlc))
            }
        }
    }

    /// Ephemeral live-tab presence push (no consensus, no persistence).
    /// Only actions addressed to this device are stored. The wire
    /// timestamp is accepted only within +/- [`LIVE_TAB_SKEW_MS`] of
    /// local receipt time, else receipt time is stamped (clamps
    /// replay/future ghosts). Entries expire [`LIVE_TAB_TTL`] after the
    /// effective stamp and the inbox is capped at [`LIVE_TAB_CAP`].
    pub fn push_live_tab(&mut self, action: LiveTabAction) -> bool {
        let (url, source, target, wire_ts) = match &action {
            LiveTabAction::PushTab {
                url,
                source_device,
                target_device,
                timestamp,
            } => (
                url.clone(),
                source_device.clone(),
                target_device.clone(),
                *timestamp,
            ),
        };
        if target != self.device_id || url.len() > 2048 {
            return false;
        }
        let receipt_ms = now_millis();
        let effective = if (wire_ts - receipt_ms).abs() <= LIVE_TAB_SKEW_MS {
            wire_ts
        } else {
            receipt_ms
        };
        let clamped = LiveTabAction::PushTab {
            url: url.clone(),
            source_device: source.clone(),
            target_device: target.clone(),
            timestamp: effective,
        };
        let now = Instant::now();
        // Expire by effective stamp (wall clock) and receipt instant;
        // either bound evicts. Wall-clock expiry kills replays whose
        // wire time is old-but-within-skew; instant expiry bounds
        // future-dated (within-skew) entries to 5min from receipt.
        self.prune_live_tabs(receipt_ms, now);
        if self.live_tabs.len() >= LIVE_TAB_CAP
            && let Some(oldest) = self
                .live_tabs
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
        {
            self.live_tabs.remove(&oldest);
        }
        self.live_tabs.insert((source, url), (clamped, now));
        true
    }

    fn prune_live_tabs(&mut self, now_ms: i64, now: Instant) {
        let ttl_ms = LIVE_TAB_TTL.as_millis() as i64;
        self.live_tabs.retain(|_, (a, at)| {
            if now.duration_since(*at) >= LIVE_TAB_TTL {
                return false;
            }
            let ts = match a {
                LiveTabAction::PushTab { timestamp, .. } => *timestamp,
            };
            now_ms - ts < ttl_ms
        });
    }

    /// Snapshot of unexpired live-tab pushes.
    pub fn live_tabs(&mut self) -> Vec<LiveTabAction> {
        let now = Instant::now();
        self.prune_live_tabs(now_millis(), now);
        self.live_tabs.values().map(|(a, _)| a.clone()).collect()
    }

    pub fn handle_crdt_message(&mut self, msg: CrdtMessage) -> Option<CrdtMessage> {
        match msg {
            CrdtMessage::SyncRequest {
                since_vector_clock,
                enabled_categories,
            } => {
                let deltas = self.deltas_for_peer(&since_vector_clock, &enabled_categories);
                Some(CrdtMessage::SyncResponse { deltas })
            }
            CrdtMessage::SyncResponse { deltas } => {
                if let Err(e) = self.apply_deltas(deltas) {
                    tracing::warn!(error = %e, "sync apply failed (rolled back)");
                }
                None
            }
            CrdtMessage::PushLiveTab { action } => {
                self.push_live_tab(action);
                None
            }
            CrdtMessage::FetchArchiveBody { snapshot_id } => {
                let conn = self.vault.lock().ok()?;
                let mut stmt = conn
                    .prepare("SELECT compressed_html, hlc_timestamp FROM web_snapshots WHERE id=?1")
                    .ok()?;
                let (blob, hlc): (Vec<u8>, i64) = stmt
                    .query_row([snapshot_id.clone()], |r| Ok((r.get(0)?, r.get(1)?)))
                    .ok()?;
                Some(CrdtMessage::FetchArchiveBodyResponse {
                    snapshot_id,
                    compressed_html: blob,
                    hlc_timestamp: hlc as u64,
                })
            }
            CrdtMessage::FetchArchiveBodyResponse {
                snapshot_id,
                compressed_html,
                hlc_timestamp,
            } => {
                // LWW guard mirroring the metadata arm: drop when incoming
                // hlc <= stored. Empty stored bodies still fill (lazy
                // metadata-only rows carry the same HLC as the body, and
                // old peers send hlc 0): any bytes beat empty, but a
                // filled body never regresses to stale bytes.
                if let Ok(conn) = self.vault.lock() {
                    let stored: Option<(i64, Vec<u8>)> = conn
                        .query_row(
                            "SELECT hlc_timestamp, compressed_html FROM web_snapshots WHERE id=?1",
                            [&snapshot_id],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()
                        .unwrap_or(None);
                    let (stored_hlc, stored_body) = stored?;
                    let incoming = hlc_timestamp as i64;
                    if stored_body.is_empty() {
                        let _ = conn.execute(
                            "UPDATE web_snapshots SET compressed_html=?1 WHERE id=?2 AND hlc_timestamp <= ?3",
                            rusqlite::params![compressed_html, snapshot_id, incoming],
                        );
                    } else {
                        if incoming <= stored_hlc {
                            return None;
                        }
                        let _ = conn.execute(
                            "UPDATE web_snapshots SET compressed_html=?1 WHERE id=?2 AND hlc_timestamp < ?3",
                            rusqlite::params![compressed_html, snapshot_id, incoming],
                        );
                    }
                }
                None
            }
        }
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_actions::LiveTabAction;

    fn test_db() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE bookmarks(id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL,
              parent_folder_id TEXT, sort_order REAL DEFAULT 0, hlc_timestamp INTEGER NOT NULL, is_deleted INTEGER DEFAULT 0);
             CREATE TABLE history(id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT,
              visit_count INTEGER DEFAULT 1, last_visit_timestamp INTEGER NOT NULL, hlc_timestamp INTEGER NOT NULL);
             CREATE TABLE web_snapshots(id TEXT PRIMARY KEY, url TEXT NOT NULL, capture_timestamp INTEGER NOT NULL,
              compressed_html BLOB NOT NULL, is_pinned INTEGER DEFAULT 0, hlc_timestamp INTEGER NOT NULL);
             CREATE TABLE credentials(id TEXT PRIMARY KEY, url TEXT NOT NULL, username TEXT NOT NULL,
              encrypted_password BLOB NOT NULL, hlc_timestamp INTEGER NOT NULL, is_deleted INTEGER DEFAULT 0);
             CREATE TABLE autofill_records(id TEXT PRIMARY KEY, record_type TEXT NOT NULL, label TEXT NOT NULL,
              encrypted_fields BLOB NOT NULL, hlc_timestamp INTEGER NOT NULL, is_deleted INTEGER DEFAULT 0);
             CREATE TABLE totp_secrets(credential_id TEXT PRIMARY KEY, encrypted_secret BLOB NOT NULL,
              digits INTEGER NOT NULL, period_seconds INTEGER NOT NULL);
             CREATE TABLE trusted_devices(device_id TEXT PRIMARY KEY, device_name TEXT NOT NULL,
              public_key BLOB NOT NULL, paired_via_device_id TEXT, first_paired_at INTEGER NOT NULL,
              last_seen_at INTEGER, last_synced_at INTEGER, hlc_timestamp INTEGER NOT NULL, is_revoked INTEGER DEFAULT 0);
             CREATE TABLE sync_vector_clock(device_id TEXT PRIMARY KEY, counter INTEGER NOT NULL);",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn all_enabled() -> HashSet<SyncCategory> {
        SyncCategory::all().iter().copied().collect()
    }

    fn bookmark_title(conn: &Connection, id: &str) -> (String, i64, i64) {
        conn.query_row(
            "SELECT title, hlc_timestamp, is_deleted FROM bookmarks WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    #[test]
    fn mesh_converges_both_ways() {
        let mut a = CrdtEngine::new(test_db(), "dev-a".to_owned());
        let mut b = CrdtEngine::new(test_db(), "dev-b".to_owned());
        a.apply_deltas(vec![SyncAction::AddBookmark {
            id: "b1".into(),
            url: "https://example.com".into(),
            title: "A-title".into(),
            parent_folder_id: None,
            sort_order: 1024.0,
            hlc_timestamp: 100,
        }])
        .unwrap();
        // A -> B.
        let d = a.deltas_for_peer(&HashMap::new(), &all_enabled());
        assert!(
            d.iter()
                .any(|x| matches!(x, SyncAction::AddBookmark { id, .. } if id == "b1"))
        );
        b.apply_deltas(d).unwrap();
        assert_eq!(bookmark_title(&b.vault.lock().unwrap(), "b1").0, "A-title");
        // Fractional order survives the round trip (not reset to 0).
        let order: f64 = b
            .vault
            .lock()
            .unwrap()
            .query_row("SELECT sort_order FROM bookmarks WHERE id='b1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(order, 1024.0);
        // B -> A carries nothing new (converged).
        let back = b.deltas_for_peer(&a.clock.0, &all_enabled());
        assert!(
            !back
                .iter()
                .any(|x| matches!(x, SyncAction::AddBookmark { id, .. } if id == "b1")),
            "converged rows must not re-send"
        );
        // Concurrent edit: higher HLC wins on both sides.
        b.apply_deltas(vec![SyncAction::AddBookmark {
            id: "b1".into(),
            url: "https://example.com".into(),
            title: "B-title".into(),
            parent_folder_id: None,
            sort_order: 1024.0,
            hlc_timestamp: 200,
        }])
        .unwrap();
        let d2 = b.deltas_for_peer(&a.clock.0, &all_enabled());
        a.apply_deltas(d2).unwrap();
        assert_eq!(bookmark_title(&a.vault.lock().unwrap(), "b1").0, "B-title");
    }

    #[test]
    fn stale_write_dropped() {
        let mut a = CrdtEngine::new(test_db(), "dev-a".to_owned());
        a.apply_deltas(vec![SyncAction::AddBookmark {
            id: "b1".into(),
            url: "https://example.com".into(),
            title: "new".into(),
            parent_folder_id: None,
            sort_order: 1024.0,
            hlc_timestamp: 200,
        }])
        .unwrap();
        a.apply_deltas(vec![SyncAction::AddBookmark {
            id: "b1".into(),
            url: "https://example.com".into(),
            title: "stale".into(),
            parent_folder_id: None,
            sort_order: 1024.0,
            hlc_timestamp: 100,
        }])
        .unwrap();
        assert_eq!(bookmark_title(&a.vault.lock().unwrap(), "b1").0, "new");
    }

    #[test]
    fn delete_newer_wins_then_newer_add_revives() {
        let mut a = CrdtEngine::new(test_db(), "dev-a".to_owned());
        a.apply_deltas(vec![SyncAction::AddBookmark {
            id: "b1".into(),
            url: "https://example.com".into(),
            title: "t".into(),
            parent_folder_id: None,
            sort_order: 1024.0,
            hlc_timestamp: 100,
        }])
        .unwrap();
        a.apply_deltas(vec![SyncAction::DeleteBookmark {
            id: "b1".into(),
            hlc_timestamp: 300,
        }])
        .unwrap();
        assert_eq!(bookmark_title(&a.vault.lock().unwrap(), "b1").2, 1);
        // Older re-add loses to the tombstone.
        a.apply_deltas(vec![SyncAction::AddBookmark {
            id: "b1".into(),
            url: "https://example.com".into(),
            title: "resurrected".into(),
            parent_folder_id: None,
            sort_order: 1024.0,
            hlc_timestamp: 200,
        }])
        .unwrap();
        let (title, _, deleted) = bookmark_title(&a.vault.lock().unwrap(), "b1");
        assert_eq!(deleted, 1);
        assert_eq!(title, "t");
        // Newer re-add revives.
        a.apply_deltas(vec![SyncAction::AddBookmark {
            id: "b1".into(),
            url: "https://example.com".into(),
            title: "revived".into(),
            parent_folder_id: None,
            sort_order: 1024.0,
            hlc_timestamp: 400,
        }])
        .unwrap();
        let (title, _, deleted) = bookmark_title(&a.vault.lock().unwrap(), "b1");
        assert_eq!(deleted, 0);
        assert_eq!(title, "revived");
    }

    #[test]
    fn totp_does_not_resend_after_apply() {
        let a = CrdtEngine::new(test_db(), "dev-a".to_owned());
        let mut b = CrdtEngine::new(test_db(), "dev-b".to_owned());
        {
            let conn = a.vault.lock().unwrap();
            conn.execute(
                "INSERT INTO credentials(id, url, username, encrypted_password, hlc_timestamp, is_deleted) VALUES('c1','https://x.com','u',x'00',100,0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO totp_secrets(credential_id, encrypted_secret, digits, period_seconds) VALUES('c1',x'01',6,30)",
                [],
            )
            .unwrap();
        }
        let d = a.deltas_for_peer(&HashMap::new(), &all_enabled());
        assert_eq!(
            d.iter()
                .filter(|x| matches!(x, SyncAction::UpsertTotpSecret { .. }))
                .count(),
            1
        );
        b.apply_deltas(d).unwrap();
        // Second round with B's watermark: converged, nothing re-sends
        // (the old code minted fresh HLC per row and looped forever).
        let d2 = a.deltas_for_peer(&b.clock.0, &all_enabled());
        assert!(
            !d2.iter()
                .any(|x| matches!(x, SyncAction::UpsertTotpSecret { .. })),
            "totp must converge, got {d2:?}"
        );
    }

    #[test]
    fn clock_persisted_across_restart() {
        let db = test_db();
        {
            let mut a = CrdtEngine::new(Arc::clone(&db), "dev-a".to_owned());
            a.apply_deltas(vec![SyncAction::AddBookmark {
                id: "b1".into(),
                url: "https://example.com".into(),
                title: "t".into(),
                parent_folder_id: None,
                sort_order: 1024.0,
                hlc_timestamp: 100,
            }])
            .unwrap();
        }
        // Fresh engine on the same DB reloads the watermark.
        let b = CrdtEngine::new(db, "dev-a".to_owned());
        assert!(
            b.clock.0.get("dev-a").copied().unwrap_or(0) >= 100,
            "clock must survive restart"
        );
    }

    #[test]
    fn live_tab_push_accepts_targeted_and_prunes() {
        let mut a = CrdtEngine::new(test_db(), "dev-a".to_owned());
        let mk = |tgt: &str| LiveTabAction::PushTab {
            url: "https://example.com".into(),
            source_device: "dev-b".into(),
            target_device: tgt.into(),
            timestamp: 1,
        };
        assert!(!a.push_live_tab(mk("dev-other")));
        assert!(a.live_tabs().is_empty());
        assert!(a.push_live_tab(mk("dev-a")));
        assert_eq!(a.live_tabs().len(), 1);
    }

    #[test]
    fn live_tab_ancient_clamps_to_receipt_fresh_kept() {
        let mut a = CrdtEngine::new(test_db(), "dev-a".to_owned());
        let receipt = now_millis();
        // Ancient (1h old, beyond 10min skew) clamps to receipt.
        let ancient = receipt - 3_600_000;
        assert!(a.push_live_tab(LiveTabAction::PushTab {
            url: "https://example.com/old".into(),
            source_device: "dev-b".into(),
            target_device: "dev-a".into(),
            timestamp: ancient,
        }));
        let tabs = a.live_tabs();
        let ts_old = match tabs
            .iter()
            .find(|t| matches!(t, LiveTabAction::PushTab { url, .. } if url == "https://example.com/old"))
            .expect("old tab stored")
        {
            LiveTabAction::PushTab { timestamp, .. } => *timestamp,
        };
        assert!(
            (ts_old - receipt).abs() < 5_000,
            "ancient must clamp to receipt, got {ts_old} vs {receipt}"
        );
        // Fresh (receipt) is kept verbatim.
        assert!(a.push_live_tab(LiveTabAction::PushTab {
            url: "https://example.com/fresh".into(),
            source_device: "dev-b".into(),
            target_device: "dev-a".into(),
            timestamp: receipt,
        }));
        let tabs = a.live_tabs();
        let ts_fresh = match tabs
            .iter()
            .find(|t| matches!(t, LiveTabAction::PushTab { url, .. } if url == "https://example.com/fresh"))
            .expect("fresh tab stored")
        {
            LiveTabAction::PushTab { timestamp, .. } => *timestamp,
        };
        assert_eq!(ts_fresh, receipt, "fresh timestamp must be kept");
        // Far-future (1h ahead) also clamps.
        assert!(a.push_live_tab(LiveTabAction::PushTab {
            url: "https://example.com/future".into(),
            source_device: "dev-b".into(),
            target_device: "dev-a".into(),
            timestamp: receipt + 3_600_000,
        }));
        let tabs = a.live_tabs();
        let ts_future = match tabs
            .iter()
            .find(|t| matches!(t, LiveTabAction::PushTab { url, .. } if url == "https://example.com/future"))
            .expect("future tab stored")
        {
            LiveTabAction::PushTab { timestamp, .. } => *timestamp,
        };
        assert!(
            (ts_future - now_millis()).abs() < 5_000,
            "future must clamp to receipt, got {ts_future}"
        );
    }

    #[test]
    fn stale_archive_body_does_not_overwrite_newer() {
        let mut a = CrdtEngine::new(test_db(), "dev-a".to_owned());
        {
            let conn = a.vault.lock().unwrap();
            conn.execute(
                "INSERT INTO web_snapshots(id, url, capture_timestamp, compressed_html, is_pinned, hlc_timestamp) VALUES('s1','https://example.com',1,?1,0,200)",
                rusqlite::params![b"new-body".to_vec()],
            )
            .unwrap();
        }
        // Stale (hlc 100 < stored 200) must not overwrite.
        a.handle_crdt_message(CrdtMessage::FetchArchiveBodyResponse {
            snapshot_id: "s1".into(),
            compressed_html: b"stale-body".to_vec(),
            hlc_timestamp: 100,
        });
        let body: Vec<u8> = a
            .vault
            .lock()
            .unwrap()
            .query_row(
                "SELECT compressed_html FROM web_snapshots WHERE id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(body, b"new-body", "stale body must not overwrite newer");
        // Newer (hlc 300 > stored 200) overwrites.
        a.handle_crdt_message(CrdtMessage::FetchArchiveBodyResponse {
            snapshot_id: "s1".into(),
            compressed_html: b"newer-body".to_vec(),
            hlc_timestamp: 300,
        });
        let body: Vec<u8> = a
            .vault
            .lock()
            .unwrap()
            .query_row(
                "SELECT compressed_html FROM web_snapshots WHERE id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(body, b"newer-body", "newer body must win");
    }
}
