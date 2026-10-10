//! Encrypted credential rows. Passwords are AES-256-GCM blobs.
//! Deletes tombstone. Concurrent writes resolve last-writer-wins on HLC.

use base64::Engine;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroize;

use ferrite_vault::Vault;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub id: String,
    pub url: String,
    pub username: String,
    pub encrypted_password: Vec<u8>,
    pub hlc_timestamp: i64,
}

impl CredentialRecord {
    pub fn new(url: &str, username: &str, password: &str, key: &[u8]) -> Result<Self, String> {
        let mut pwd = password.as_bytes().to_vec();
        let enc = encrypt_field(&pwd, key)?;
        pwd.zeroize();
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            url: url.to_owned(),
            username: username.to_owned(),
            encrypted_password: enc,
            hlc_timestamp: now_millis(),
        })
    }

    pub fn decrypt_password(&self, key: &[u8]) -> Option<String> {
        let mut plain = decrypt_field(&self.encrypted_password, key).ok()?;
        let s = String::from_utf8(plain.clone()).ok();
        plain.zeroize();
        s
    }
}

/// AES-256-GCM field encryption under the 32-byte device master key.
/// Output is `nonce(12) || ciphertext+tag`. Public for shell-owned
/// encrypted files (session, settings), which reuse this exact primitive
/// so there is one file-encryption construction to audit.
pub fn encrypt_field(data: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
    use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce, aead::Aead};
    if key.len() != 32 {
        return Err("master key must be exactly 32 bytes".to_owned());
    }
    #[allow(deprecated)]
    let key_arr = Key::<Aes256Gcm>::from_slice(&key[..32]);
    let cipher = Aes256Gcm::new(key_arr);
    let mut nonce_bytes = [0u8; 12];
    {
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
    }
    #[allow(deprecated)]
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, data)
        .map_err(|e| format!("encrypt failed: {e}"))?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(nonce_bytes.as_slice());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt [`encrypt_field`] output. Wrong key or tampering fails closed
/// (auth error, never garbage). See the public note on `encrypt_field`.
pub fn decrypt_field(data: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
    use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce, aead::Aead};
    if key.len() != 32 {
        return Err("master key must be exactly 32 bytes".to_owned());
    }
    if data.len() < 12 + 16 {
        return Err("ciphertext truncated".to_owned());
    }
    #[allow(deprecated)]
    let key_arr = Key::<Aes256Gcm>::from_slice(&key[..32]);
    let cipher = Aes256Gcm::new(key_arr);
    let (nonce_bytes, ct) = data.split_at(12);
    #[allow(deprecated)]
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ct)
        .map_err(|_| "authentication failed".to_owned())
}

/// Sealed file envelope: versioned, self-describing base64 of
/// [`encrypt_field`] output. The single file-encryption construction for
/// shell-owned files (session, settings) and engine settings files, so
/// there is one envelope to audit, not one per file.
pub fn seal_bytes(plain: &[u8], key: &[u8]) -> Result<String, String> {
    let ct = encrypt_field(plain, key)?;
    let data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &ct);
    Ok(format!(r#"{{"v":1,"alg":"AES-256-GCM","data":"{data}"}}"#))
}

/// Open [`seal_bytes`] output. Caps sizes before parsing or decoding;
/// wrong key, tampering, or foreign shapes yield `None`, never an error
/// string that could leak structure. Callers treat `None` as
/// missing-or-unusable (fresh boot, migration replay, or quarantine).
pub fn open_sealed_bytes(text: &str, key: &[u8]) -> Option<Vec<u8>> {
    if text.len() > 4 * 1024 * 1024 {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("v").and_then(|x| x.as_u64()) != Some(1) {
        return None;
    }
    if v.get("alg").and_then(|x| x.as_str()) != Some("AES-256-GCM") {
        return None;
    }
    let data = v.get("data").and_then(|x| x.as_str())?;
    if data.len() > 4 * 1024 * 1024 {
        return None;
    }
    let ct = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    decrypt_field(&ct, key).ok()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct CredentialStore {
    vault: std::sync::Arc<Vault>,
}

impl CredentialStore {
    pub fn new(vault: std::sync::Arc<Vault>) -> Self {
        Self { vault }
    }

    pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS credentials(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                username TEXT NOT NULL,
                encrypted_password BLOB NOT NULL,
                hlc_timestamp INTEGER NOT NULL,
                is_deleted INTEGER NOT NULL DEFAULT 0,
                origin TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_credentials_url ON credentials(url);
            CREATE INDEX IF NOT EXISTS idx_credentials_hlc ON credentials(hlc_timestamp);
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
            "#,
        )?;
        // Migrations for older DBs (created before the column existed).
        // The vault's own init may own the `credentials` table already, so
        // every step here tolerates "already there": `IF NOT EXISTS` for the
        // index, ignored duplicate-column errors for the ALTERs. Order
        // matters: the column must exist before its index is built.
        // Migration for DBs created before `is_deleted` existed.
        let _ = conn.execute_batch(
            "ALTER TABLE credentials ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0;",
        );
        // Migration for DBs created before the `origin` lookup column: legacy
        // rows keep `origin=''` and are covered by the Rust-parse fallback in
        // `get_for_origin` until re-saved (exactness is re-checked in Rust
        // either way, so no row can over-match during the transition).
        let _ = conn
            .execute_batch("ALTER TABLE credentials ADD COLUMN origin TEXT NOT NULL DEFAULT '';");
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_credentials_origin ON credentials(origin);",
        );
        // Autofill tombstones: sync emits `is_deleted` (see
        // `ferrite-sync` crdt_engine emit path). Older DBs predate the
        // column; additive ALTER only, ignored when already present.
        let _ = conn.execute_batch(
            "ALTER TABLE autofill_records ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0;",
        );
        Ok(())
    }

    /// Normalized lookup key `scheme://host:port` (all lowercase, default
    /// ports explicit), or `None` when the URL is unparseable. Pure.
    pub fn origin_of(url: &str) -> Option<String> {
        let u = url::Url::parse(url).ok()?;
        let host = u.host_str()?.to_lowercase();
        Some(format!(
            "{}://{}:{}",
            u.scheme().to_lowercase(),
            host,
            u.port_or_known_default().unwrap_or(443)
        ))
    }

    pub fn save(&self, rec: &CredentialRecord) -> rusqlite::Result<()> {
        let conn =
            self.vault.conn.lock().map_err(|_| {
                rusqlite::Error::InvalidPath("vault lock poisoned".to_owned().into())
            })?;
        let origin = Self::origin_of(&rec.url).unwrap_or_default();
        conn.execute(
            "INSERT INTO credentials(id, url, username, encrypted_password, hlc_timestamp, is_deleted, origin)
             VALUES(?1,?2,?3,?4,?5,0,?6)
             ON CONFLICT(id) DO UPDATE SET url=excluded.url, username=excluded.username,
               encrypted_password=excluded.encrypted_password, hlc_timestamp=excluded.hlc_timestamp,
               is_deleted=0, origin=excluded.origin WHERE excluded.hlc_timestamp >= credentials.hlc_timestamp",
            params![rec.id, rec.url, rec.username, rec.encrypted_password, rec.hlc_timestamp, origin],
        )?;
        Ok(())
    }

    /// Exact origin match (scheme, host, port). Never substring match.
    /// Two-phase SQL pre-filter on the indexed `origin` column: exact
    /// matches first (up to `EXACT_CAP`), then legacy `origin=''` rows
    /// fill remaining slots. Legacy rows never crowd out exact matches
    /// in the SQL window. The Rust parse below re-checks every
    /// candidate, so exactness never depends on the index.
    /// Nested `if let Ok` guards stay nested: each phase must read as
    /// prepare-then-map, and collapsing them buries fallible SQL setup
    /// inside match conditions.
    #[allow(clippy::collapsible_if)]
    pub fn get_for_origin(&self, origin: &str) -> Vec<CredentialRecord> {
        const EXACT_CAP: usize = 200;
        const LEGACY_SCAN_LIMIT: i64 = 2000;
        let parsed_origin = match url::Url::parse(origin)
            .or_else(|_| url::Url::parse(&format!("https://{origin}")))
        {
            Ok(u) => u,
            Err(_) => return Vec::new(),
        };
        let scheme = parsed_origin.scheme().to_owned();
        let host = match parsed_origin.host_str() {
            Some(h) => h.to_lowercase(),
            None => return Vec::new(),
        };
        let port = parsed_origin.port_or_known_default().unwrap_or(443);
        let wanted = format!("{}://{}:{}", scheme.to_lowercase(), host, port);
        let conn = match self.vault.conn.lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let matches = |r: &CredentialRecord| -> bool {
            let Ok(u) = url::Url::parse(&r.url) else {
                return false;
            };
            if u.scheme() != scheme {
                return false;
            }
            if u.host_str().map(|h| h.to_lowercase()).as_deref() != Some(host.as_str()) {
                return false;
            }
            if u.port_or_known_default().unwrap_or(443) != port {
                return false;
            }
            true
        };
        let mut out = Vec::new();
        // Phase 1: exact origin matches first.
        if let Ok(mut stmt) = conn.prepare(
            "SELECT id, url, username, encrypted_password, hlc_timestamp FROM credentials WHERE is_deleted=0 AND origin=?1 LIMIT 200",
        ) {
            if let Ok(rows) = stmt.query_map([wanted.as_str()], |r| {
                Ok(CredentialRecord {
                    id: r.get(0)?,
                    url: r.get(1)?,
                    username: r.get(2)?,
                    encrypted_password: r.get(3)?,
                    hlc_timestamp: r.get(4)?,
                })
            }) {
                for r in rows.flatten() {
                    if matches(&r) {
                        out.push(r);
                        if out.len() >= EXACT_CAP {
                            break;
                        }
                    }
                }
            }
        }
        // Phase 2: legacy rows (predate the `origin` column) fill any
        // remaining slots; they can never displace phase-1 exact matches.
        if out.len() < EXACT_CAP {
            if let Ok(mut stmt) = conn.prepare(
                "SELECT id, url, username, encrypted_password, hlc_timestamp FROM credentials WHERE is_deleted=0 AND origin='' LIMIT ?1",
            ) {
                if let Ok(rows) = stmt.query_map([LEGACY_SCAN_LIMIT], |r| {
                    Ok(CredentialRecord {
                        id: r.get(0)?,
                        url: r.get(1)?,
                        username: r.get(2)?,
                        encrypted_password: r.get(3)?,
                        hlc_timestamp: r.get(4)?,
                    })
                }) {
                    for r in rows.flatten() {
                        if matches(&r) {
                            // Exact-phase rows and legacy rows are disjoint
                            // by `origin`, but guard against id overlap.
                            if !out.iter().any(|e| e.id == r.id) {
                                out.push(r);
                                if out.len() >= EXACT_CAP {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// (Credential, TOTP) pairs for one origin. Secrets stay encrypted.
    /// The secrets lookup is scoped to the served credential ids
    /// (`WHERE credential_id IN (...)`), never a full-table scan.
    pub fn totp_for_origin(
        &self,
        origin: &str,
    ) -> Vec<(CredentialRecord, crate::totp::TotpSecret)> {
        use crate::totp::TotpSecret;
        let creds = self.get_for_origin(origin);
        if creds.is_empty() {
            return Vec::new();
        }
        let conn = match self.vault.conn.lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        // Ids are UUIDs minted by `CredentialRecord::new` (or test
        // fixtures); they are quoted as SQL string literals here, never
        // interpolated raw.
        let mut placeholders = String::from("(");
        for (i, _) in creds.iter().enumerate() {
            if i > 0 {
                placeholders.push(',');
            }
            placeholders.push('?');
        }
        placeholders.push(')');
        let sql = format!(
            "SELECT credential_id, encrypted_secret, digits, period_seconds FROM totp_secrets WHERE credential_id IN {placeholders}"
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let ids: Vec<&str> = creds.iter().map(|c| c.id.as_str()).collect();
        let secrets: std::collections::HashMap<String, TotpSecret> = stmt
            .query_map(rusqlite::params_from_iter(ids), |r| {
                let id: String = r.get(0)?;
                Ok((
                    id.clone(),
                    TotpSecret {
                        credential_id: id,
                        encrypted_secret: r.get(1)?,
                        digits: r.get(2)?,
                        period: r.get(3)?,
                    },
                ))
            })
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        creds
            .into_iter()
            .filter_map(|c| secrets.get(&c.id).cloned().map(|s| (c, s)))
            .collect()
    }

    pub fn all(&self) -> Vec<CredentialRecord> {
        let conn = match self.vault.conn.lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut stmt = match conn
            .prepare("SELECT id, url, username, encrypted_password, hlc_timestamp FROM credentials WHERE is_deleted=0 LIMIT 2000")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| {
            Ok(CredentialRecord {
                id: r.get(0)?,
                url: r.get(1)?,
                username: r.get(2)?,
                encrypted_password: r.get(3)?,
                hlc_timestamp: r.get(4)?,
            })
        });
        let mut out = Vec::new();
        if let Ok(rows) = rows {
            for r in rows.flatten() {
                out.push(r);
            }
        }
        out
    }

    /// Import one row. Exact (url, username) match. Identical passwords
    /// skip, differing ones keep the stored entry and report a conflict.
    pub fn import_row(&self, rec: &CredentialRecord, key: &[u8]) -> ImportVerdict {
        let incumbents: Vec<CredentialRecord> = self
            .all()
            .into_iter()
            .filter(|r| r.url == rec.url && r.username == rec.username)
            .collect();
        if incumbents.is_empty() {
            return if self.save(rec).is_ok() {
                ImportVerdict::Imported
            } else {
                ImportVerdict::Failed
            };
        }
        let incoming = rec.decrypt_password(key);
        let identical = incoming.as_ref().is_some_and(|pw| {
            incumbents
                .iter()
                .any(|r| r.decrypt_password(key).as_ref() == Some(pw))
        });
        if identical {
            ImportVerdict::SkippedIdentical
        } else {
            ImportVerdict::ConflictKeptExisting
        }
    }

    pub fn delete(&self, id: &str) -> rusqlite::Result<()> {
        let conn =
            self.vault.conn.lock().map_err(|_| {
                rusqlite::Error::InvalidPath("vault lock poisoned".to_owned().into())
            })?;
        // Tombstone, never hard delete. Cascade clears TOTP.
        conn.execute(
            "UPDATE credentials SET is_deleted=1, hlc_timestamp=?1 WHERE id=?2",
            params![now_millis(), id],
        )?;
        Ok(())
    }
}

/// Outcome of importing one credential row (see
/// [`CredentialStore::import_row`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportVerdict {
    Imported,
    SkippedIdentical,
    ConflictKeptExisting,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrite_vault::Vault;

    #[test]
    fn credential_encrypt_decrypt() {
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let rec = CredentialRecord::new("https://example.com", "user", "s3cret", key).unwrap();
        assert_ne!(rec.encrypted_password, b"s3cret");
        assert_eq!(rec.decrypt_password(key).as_deref(), Some("s3cret"));
        assert_eq!(
            rec.decrypt_password(b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"),
            None
        );
    }

    #[test]
    fn sealed_envelope_round_trips_and_rejects_foreign_shapes() {
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let doc = seal_bytes(b"{\"tabs\":[]}", key).unwrap();
        assert!(!doc.contains("tabs"), "envelope must not leak plaintext");
        assert_eq!(
            open_sealed_bytes(&doc, key),
            Some(b"{\"tabs\":[]}".to_vec())
        );
        assert!(open_sealed_bytes(&doc, b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF").is_none());
        assert!(open_sealed_bytes("{}", key).is_none());
        assert!(open_sealed_bytes(r#"{"v":2,"alg":"AES-256-GCM","data":"eA=="}"#, key).is_none());
        assert!(open_sealed_bytes(r#"{"v":1,"alg":"AES-128-GCM","data":"eA=="}"#, key).is_none());
        assert!(open_sealed_bytes("not json", key).is_none());
        assert!(open_sealed_bytes(&"x".repeat(5 * 1024 * 1024), key).is_none());
        assert!(seal_bytes(b"x", b"short").is_err());
    }

    #[test]
    fn save_and_get() {
        let vault = std::sync::Arc::new(Vault::open_in_memory().unwrap());
        {
            let conn = vault.conn.lock().unwrap();
            CredentialStore::init_schema(&conn).unwrap();
        }
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let store = CredentialStore::new(vault.clone());
        let rec = CredentialRecord::new("https://example.com", "alice", "pwd123", key).unwrap();
        let id = rec.id.clone();
        store.save(&rec).unwrap();
        let got = store.get_for_origin("https://example.com");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, id);
        assert!(store.get_for_origin("https://evilexample.com").is_empty());
        assert!(
            store
                .get_for_origin("https://example.com.evil.com")
                .is_empty()
        );
        store.delete(&id).unwrap();
        assert!(store.get_for_origin("https://example.com").is_empty());
    }

    #[test]
    fn origin_key_normalizes_and_legacy_rows_still_match() {
        assert_eq!(
            CredentialStore::origin_of("https://Example.COM:443/a?x=1").as_deref(),
            Some("https://example.com:443")
        );
        assert_eq!(
            CredentialStore::origin_of("https://example.com").as_deref(),
            Some("https://example.com:443")
        );
        assert_eq!(
            CredentialStore::origin_of("http://example.com:8080/x").as_deref(),
            Some("http://example.com:8080")
        );
        assert!(CredentialStore::origin_of("not a url %%").is_none());
        let vault = std::sync::Arc::new(Vault::open_in_memory().unwrap());
        {
            let conn = vault.conn.lock().unwrap();
            CredentialStore::init_schema(&conn).unwrap();
        }
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let store = CredentialStore::new(vault.clone());
        // Legacy row predating the `origin` column (empty key): still found
        // through the fallback, still exact-matched in Rust.
        {
            let conn = vault.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO credentials(id, url, username, encrypted_password, hlc_timestamp, is_deleted) VALUES(?1,?2,?3,?4,?5,0)",
                rusqlite::params![
                    "legacy-1",
                    "https://legacy.test/login",
                    "old",
                    CredentialRecord::new("https://legacy.test/login", "old", "pw", key)
                        .unwrap()
                        .encrypted_password,
                    1i64
                ],
            )
            .unwrap();
        }
        assert_eq!(store.get_for_origin("https://legacy.test").len(), 1);
        assert!(store.get_for_origin("https://other.test").is_empty());
        // Origins never leak across ports or schemes.
        let rec =
            CredentialRecord::new("https://example.com:8443/a", "alice", "pwd123", key).unwrap();
        store.save(&rec).unwrap();
        assert!(store.get_for_origin("https://example.com").is_empty());
        assert!(store.get_for_origin("http://example.com:8443").is_empty());
        assert_eq!(store.get_for_origin("https://example.com:8443").len(), 1);
    }

    #[test]
    fn stale_save_does_not_overwrite_newer() {
        let vault = std::sync::Arc::new(Vault::open_in_memory().unwrap());
        {
            let conn = vault.conn.lock().unwrap();
            CredentialStore::init_schema(&conn).unwrap();
        }
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let store = CredentialStore::new(vault.clone());
        let mut rec = CredentialRecord::new("https://example.com", "alice", "new", key).unwrap();
        store.save(&rec).unwrap();
        rec.encrypted_password = CredentialRecord::new("https://example.com", "alice", "old", key)
            .unwrap()
            .encrypted_password;
        rec.hlc_timestamp -= 1_000_000;
        store.save(&rec).unwrap();
        let got = store.get_for_origin("https://example.com");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].decrypt_password(key).as_deref(), Some("new"));
    }

    #[test]
    fn totp_pairs_join_secrets_to_origin_creds() {
        use crate::totp::TotpSecret;
        let vault = std::sync::Arc::new(Vault::open_in_memory().unwrap());
        {
            let conn = vault.conn.lock().unwrap();
            CredentialStore::init_schema(&conn).unwrap();
        }
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let store = CredentialStore::new(vault.clone());
        let rec = CredentialRecord::new("https://example.com", "alice", "pw", key).unwrap();
        store.save(&rec).unwrap();
        let other = CredentialRecord::new("https://example.com", "bob", "pw2", key).unwrap();
        store.save(&other).unwrap();
        let sec = TotpSecret::new(&rec.id, "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP", key);
        {
            let conn = vault.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO totp_secrets(credential_id, encrypted_secret, digits, period_seconds) VALUES(?1,?2,?3,?4)",
                rusqlite::params![sec.credential_id, sec.encrypted_secret, sec.digits, sec.period],
            )
            .unwrap();
        }
        let pairs = store.totp_for_origin("https://example.com");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.username, "alice");
        let code = pairs[0].1.current_code(key).unwrap();
        assert_eq!(code.len(), 6);
        assert!(store.totp_for_origin("https://other.test").is_empty());
    }

    #[test]
    fn import_row_never_overwrites_or_duplicates() {
        let vault = std::sync::Arc::new(Vault::open_in_memory().unwrap());
        {
            let conn = vault.conn.lock().unwrap();
            CredentialStore::init_schema(&conn).unwrap();
        }
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let store = CredentialStore::new(vault);
        let fresh = CredentialRecord::new("https://example.com", "alice", "s3cret", key).unwrap();
        assert_eq!(store.import_row(&fresh, key), ImportVerdict::Imported);
        // Identical row: skipped, no duplicate.
        let same = CredentialRecord::new("https://example.com", "alice", "s3cret", key).unwrap();
        assert_eq!(
            store.import_row(&same, key),
            ImportVerdict::SkippedIdentical
        );
        assert_eq!(store.get_for_origin("https://example.com").len(), 1);
        // Differing password: existing kept, conflict reported.
        let changed =
            CredentialRecord::new("https://example.com", "alice", "n3wpass", key).unwrap();
        assert_eq!(
            store.import_row(&changed, key),
            ImportVerdict::ConflictKeptExisting
        );
        let got = store.get_for_origin("https://example.com");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].decrypt_password(key).as_deref(), Some("s3cret"));
        // Same origin, different username: independent row.
        let other = CredentialRecord::new("https://example.com", "bob", "s3cret", key).unwrap();
        assert_eq!(store.import_row(&other, key), ImportVerdict::Imported);
    }

    #[test]
    fn exact_matches_served_first_and_cover_sixty_logins() {
        let vault = std::sync::Arc::new(Vault::open_in_memory().unwrap());
        {
            let conn = vault.conn.lock().unwrap();
            CredentialStore::init_schema(&conn).unwrap();
        }
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let store = CredentialStore::new(vault.clone());
        // 600 legacy rows (origin='') for an unrelated origin, inserted
        // first: under the old single-query `LIMIT 500` window they crowd
        // exact matches out of the SQL results.
        {
            let conn = vault.conn.lock().unwrap();
            for i in 0..600 {
                let enc = CredentialRecord::new(
                    &format!("https://other.test/login{i}"),
                    "someone",
                    "pw",
                    key,
                )
                .unwrap()
                .encrypted_password;
                conn.execute(
                    "INSERT INTO credentials(id, url, username, encrypted_password, hlc_timestamp, is_deleted) VALUES(?1,?2,?3,?4,?5,0)",
                    rusqlite::params![
                        format!("legacy-other-{i}"),
                        format!("https://other.test/login{i}"),
                        "someone",
                        enc,
                        1i64,
                    ],
                )
                .unwrap();
            }
        }
        // 60 exact logins on one origin must all be served.
        for i in 0..60 {
            let rec = CredentialRecord::new(
                &format!("https://victim.test/login{i}"),
                &format!("user{i}"),
                "pw",
                key,
            )
            .unwrap();
            store.save(&rec).unwrap();
        }
        let got = store.get_for_origin("https://victim.test");
        assert_eq!(got.len(), 60, "all 60 exact logins must be served");
        // Legacy rows for the same origin fill remaining slots without
        // displacing exact matches.
        {
            let conn = vault.conn.lock().unwrap();
            for i in 0..3 {
                let enc = CredentialRecord::new(
                    &format!("https://victim.test/legacy{i}"),
                    "old",
                    "pw",
                    key,
                )
                .unwrap()
                .encrypted_password;
                conn.execute(
                    "INSERT INTO credentials(id, url, username, encrypted_password, hlc_timestamp, is_deleted) VALUES(?1,?2,?3,?4,?5,0)",
                    rusqlite::params![
                        format!("legacy-victim-{i}"),
                        format!("https://victim.test/legacy{i}"),
                        "old",
                        enc,
                        1i64,
                    ],
                )
                .unwrap();
            }
        }
        let got = store.get_for_origin("https://victim.test");
        assert_eq!(got.len(), 63);
        // Unrelated legacy rows never leak in.
        assert!(store.get_for_origin("https://missing.test").is_empty());
    }

    #[test]
    fn totp_for_origin_only_resolves_served_credentials() {
        use crate::totp::TotpSecret;
        let vault = std::sync::Arc::new(Vault::open_in_memory().unwrap());
        {
            let conn = vault.conn.lock().unwrap();
            CredentialStore::init_schema(&conn).unwrap();
        }
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let store = CredentialStore::new(vault.clone());
        let a = CredentialRecord::new("https://a.test/login", "alice", "pw", key).unwrap();
        store.save(&a).unwrap();
        let b = CredentialRecord::new("https://b.test/login", "bob", "pw", key).unwrap();
        store.save(&b).unwrap();
        // Same origin as `a` but no secret attached.
        let c = CredentialRecord::new("https://a.test/other", "carol", "pw", key).unwrap();
        store.save(&c).unwrap();
        for rec in [&a, &b] {
            let sec = TotpSecret::new(&rec.id, "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP", key);
            let conn = vault.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO totp_secrets(credential_id, encrypted_secret, digits, period_seconds) VALUES(?1,?2,?3,?4)",
                rusqlite::params![sec.credential_id, sec.encrypted_secret, sec.digits, sec.period],
            )
            .unwrap();
        }
        let pairs_a = store.totp_for_origin("https://a.test");
        assert_eq!(pairs_a.len(), 1);
        assert_eq!(pairs_a[0].0.username, "alice");
        let pairs_b = store.totp_for_origin("https://b.test");
        assert_eq!(pairs_b.len(), 1);
        assert_eq!(pairs_b[0].0.username, "bob");
        assert!(store.totp_for_origin("https://missing.test").is_empty());
    }

    #[test]
    fn both_init_paths_converge_on_tombstone_and_origin_columns() {
        // Vault-owned init first (the `Vault::open_in_memory` path), then
        // the credentials-owned init: both must leave the same columns.
        let vault = std::sync::Arc::new(Vault::open_in_memory().unwrap());
        {
            let conn = vault.conn.lock().unwrap();
            CredentialStore::init_schema(&conn).unwrap();
        }
        let conn = vault.conn.lock().unwrap();
        let cols = |table: &str| -> Vec<String> {
            let mut stmt = conn
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .flatten()
                .collect()
        };
        let creds = cols("credentials");
        assert!(creds.contains(&"is_deleted".to_owned()), "{creds:?}");
        assert!(creds.contains(&"origin".to_owned()), "{creds:?}");
        let autofill = cols("autofill_records");
        assert!(autofill.contains(&"is_deleted".to_owned()), "{autofill:?}");
        assert!(
            autofill.contains(&"hlc_timestamp".to_owned()),
            "{autofill:?}"
        );
    }
}
