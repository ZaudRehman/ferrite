//! Credential store.

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

fn encrypt_field(data: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
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

fn decrypt_field(data: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
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
                is_deleted INTEGER NOT NULL DEFAULT 0
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
        // Migration for DBs created before `is_deleted` existed.
        let _ = conn.execute_batch(
            "ALTER TABLE credentials ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0;",
        );
        Ok(())
    }

    pub fn save(&self, rec: &CredentialRecord) -> rusqlite::Result<()> {
        let conn =
            self.vault.conn.lock().map_err(|_| {
                rusqlite::Error::InvalidPath("vault lock poisoned".to_owned().into())
            })?;
        // Last-writer-wins on HLC: never overwrite a newer row with stale data.
        conn.execute(
            "INSERT INTO credentials(id, url, username, encrypted_password, hlc_timestamp, is_deleted)
             VALUES(?1,?2,?3,?4,?5,0)
             ON CONFLICT(id) DO UPDATE SET url=excluded.url, username=excluded.username,
               encrypted_password=excluded.encrypted_password, hlc_timestamp=excluded.hlc_timestamp,
               is_deleted=0 WHERE excluded.hlc_timestamp >= credentials.hlc_timestamp",
            params![rec.id, rec.url, rec.username, rec.encrypted_password, rec.hlc_timestamp],
        )?;
        Ok(())
    }

    /// Exact-origin match (scheme + host + port). Never substring `LIKE`.
    pub fn get_for_origin(&self, origin: &str) -> Vec<CredentialRecord> {
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
        let conn = match self.vault.conn.lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut stmt = match conn.prepare(
            "SELECT id, url, username, encrypted_password, hlc_timestamp FROM credentials WHERE is_deleted=0 LIMIT 500",
        ) {
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
                let Ok(u) = url::Url::parse(&r.url) else {
                    continue;
                };
                if u.scheme() != scheme {
                    continue;
                }
                if u.host_str().map(|h| h.to_lowercase()).as_deref() != Some(host.as_str()) {
                    continue;
                }
                if u.port_or_known_default().unwrap_or(443) != port {
                    continue;
                }
                out.push(r);
                if out.len() >= 50 {
                    break;
                }
            }
        }
        out
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

    pub fn delete(&self, id: &str) -> rusqlite::Result<()> {
        let conn =
            self.vault.conn.lock().map_err(|_| {
                rusqlite::Error::InvalidPath("vault lock poisoned".to_owned().into())
            })?;
        // Tombstone (sync-safe): never hard-delete; cascade removes TOTP.
        conn.execute(
            "UPDATE credentials SET is_deleted=1, hlc_timestamp=?1 WHERE id=?2",
            params![now_millis(), id],
        )?;
        Ok(())
    }
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
        // Wrong key authenticates to None, never garbage.
        assert_eq!(
            rec.decrypt_password(b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"),
            None
        );
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
        // Cross-origin leak check: substring must not match.
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
}
