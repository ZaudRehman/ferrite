//! Encrypted cloud backup, OAuth token encryption, and pairing.

use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce, aead::Aead};
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Debug, Clone)]
pub struct BackupConfig {
    pub master_password: String,
    pub salt: Vec<u8>,
    pub remote_url: String,
    pub retain_versions: usize,
}

impl BackupConfig {
    pub fn new(master_password: String, remote_url: String) -> Self {
        let mut salt = vec![0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        Self {
            master_password,
            salt,
            remote_url,
            retain_versions: 5,
        }
    }
}

pub fn derive_backup_key(password: &str, salt: &[u8]) -> Result<Vec<u8>, String> {
    if salt.len() < 16 {
        return Err("backup salt must be >= 16 bytes".into());
    }
    let salt_str = SaltString::encode_b64(salt).map_err(|e| e.to_string())?;
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt_str)
        .map_err(|e| e.to_string())?;
    let bytes = hash.hash.ok_or("argon2 missing")?.as_bytes()[..32].to_vec();
    Ok(bytes)
}

pub fn encrypt_vault_snapshot(data: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
    if key.len() != 32 {
        return Err("backup key must be exactly 32 bytes".into());
    }
    #[allow(deprecated)]
    let k = Key::<Aes256Gcm>::from_slice(&key[..32]);
    let cipher = Aes256Gcm::new(k);
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    #[allow(deprecated)]
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, data)
        .map_err(|e| format!("encrypt: {e}"))?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt_vault_snapshot(blob: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
    if blob.len() < 12 + 16 {
        return Err("blob too short".into());
    }
    if key.len() != 32 {
        return Err("backup key must be exactly 32 bytes".into());
    }
    #[allow(deprecated)]
    let k = Key::<Aes256Gcm>::from_slice(&key[..32]);
    let cipher = Aes256Gcm::new(k);
    let (nb, ct) = blob.split_at(12);
    #[allow(deprecated)]
    let nonce = Nonce::from_slice(nb);
    cipher
        .decrypt(nonce, ct)
        .map_err(|e| format!("decrypt/auth fail: {e}"))
}

/// Serialize relevant tables for backup. Values keep their SQLite types
/// (INTEGER to number, BLOB to `{"$b64": ...}`, NULL to null) so restores
/// are lossless. Trust-exception tables never serialize (device-specific).
pub fn serialize_vault(conn: &rusqlite::Connection) -> Result<Vec<u8>, String> {
    let mut data = serde_json::Map::new();
    for table in [
        "bookmarks",
        "history",
        "credentials",
        "autofill_records",
        "totp_secrets",
        "trusted_devices",
        "sync_settings",
    ] {
        if let Ok(mut stmt) = conn.prepare(&format!("SELECT * FROM {table}")) {
            let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            let col_count = stmt.column_count();
            if let Ok(rows) = stmt.query_map([], |r| {
                let mut obj = serde_json::Map::new();
                for (i, name) in cols.iter().enumerate().take(col_count) {
                    let v: rusqlite::types::Value = r.get(i)?;
                    let json = match v {
                        rusqlite::types::Value::Null => serde_json::Value::Null,
                        rusqlite::types::Value::Integer(n) => serde_json::Value::from(n),
                        rusqlite::types::Value::Real(f) => serde_json::Number::from_f64(f)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null),
                        rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
                        rusqlite::types::Value::Blob(b) => {
                            use base64::Engine;
                            let mut m = serde_json::Map::new();
                            m.insert(
                                "$b64".to_owned(),
                                serde_json::Value::String(
                                    base64::engine::general_purpose::STANDARD.encode(&b),
                                ),
                            );
                            serde_json::Value::Object(m)
                        }
                    };
                    obj.insert(name.clone(), json);
                }
                Ok(obj)
            }) {
                let vals: Vec<_> = rows.flatten().collect();
                data.insert(
                    table.to_owned(),
                    serde_json::Value::Array(
                        vals.into_iter().map(serde_json::Value::Object).collect(),
                    ),
                );
            }
        }
    }
    serde_json::to_vec(&data).map_err(|e| e.to_string())
}

/// OAuth token encryption via the field-level pattern. Returns None on
/// failure so callers can never persist an empty token and silently
/// destroy the credential.
pub fn encrypt_oauth_token(token: &str, vault_key: &[u8]) -> Option<Vec<u8>> {
    encrypt_vault_snapshot(token.as_bytes(), vault_key).ok()
}
pub fn decrypt_oauth_token(blob: &[u8], vault_key: &[u8]) -> Option<String> {
    decrypt_vault_snapshot(blob, vault_key)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

/// Versioning: list/upload/prune (3.5)
pub fn backup_filename(ts: &str) -> String {
    format!("ferrite_vault_{ts}.enc")
}

/// Sealed backup file format for device migration (distinct from sync).
/// JSON wrapper with the Argon2id salt and the AES-GCM blob. Short
/// passphrases and oversize files are refused before any heavy parse.
pub const BACKUP_FILE_FORMAT: &str = "ferrite-backup-1";
pub const MIN_BACKUP_PASSPHRASE_CHARS: usize = 8;
pub const MAX_BACKUP_FILE_BYTES: usize = 64 * 1024 * 1024;

pub fn seal_backup_file(envelope_json: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    if passphrase.chars().count() < MIN_BACKUP_PASSPHRASE_CHARS {
        return Err("backup passphrase must be at least 8 characters".to_owned());
    }
    if envelope_json.is_empty() || envelope_json.len() > MAX_BACKUP_FILE_BYTES {
        return Err("backup envelope has an impossible size".to_owned());
    }
    let mut salt = vec![0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let key = derive_backup_key(passphrase, &salt)?;
    let blob = encrypt_vault_snapshot(envelope_json, &key)?;
    let wrapper = serde_json::json!({
        "format": BACKUP_FILE_FORMAT,
        "salt_b64": base64::engine::general_purpose::STANDARD.encode(&salt),
        "blob_b64": base64::engine::general_purpose::STANDARD.encode(&blob),
    });
    serde_json::to_vec(&wrapper).map_err(|e| e.to_string())
}

pub fn open_backup_file(raw: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    if raw.len() > MAX_BACKUP_FILE_BYTES {
        return Err("backup file too large".to_owned());
    }
    let wrapper: serde_json::Value =
        serde_json::from_slice(raw).map_err(|_| "not a Ferrite backup file".to_owned())?;
    if wrapper.get("format").and_then(|f| f.as_str()) != Some(BACKUP_FILE_FORMAT) {
        return Err("not a Ferrite backup file".to_owned());
    }
    use base64::Engine;
    let salt = wrapper
        .get("salt_b64")
        .and_then(|s| s.as_str())
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .filter(|s| s.len() >= 16)
        .ok_or("backup header is corrupt")?;
    let blob = wrapper
        .get("blob_b64")
        .and_then(|s| s.as_str())
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .ok_or("backup header is corrupt")?;
    let key = derive_backup_key(passphrase, &salt)?;
    decrypt_vault_snapshot(&blob, &key).map_err(|_| "wrong passphrase or corrupt backup".to_owned())
}

pub fn prune_old_versions(existing: &mut Vec<String>, retain: usize) -> Vec<String> {
    // Only manage our own `ferrite_vault_*.enc` files: unknown names are
    // never deleted (the old code pruned ANY strings past `retain`).
    let mut managed: Vec<usize> = existing
        .iter()
        .enumerate()
        .filter(|(_, n)| n.starts_with("ferrite_vault_") && n.ends_with(".enc"))
        .map(|(i, _)| i)
        .collect();
    managed.sort_by(|&a, &b| existing[a].cmp(&existing[b]));
    if managed.len() <= retain {
        return Vec::new();
    }
    let drop_idx: std::collections::HashSet<usize> =
        managed[..managed.len() - retain].iter().copied().collect();
    let mut removed = Vec::new();
    let mut kept = Vec::with_capacity(existing.len());
    for (i, name) in existing.drain(..).enumerate() {
        if drop_idx.contains(&i) {
            removed.push(name);
        } else {
            kept.push(name);
        }
    }
    *existing = kept;
    removed
}

// X25519 pairing verification code (4.4) - 6-digit from sha2 of shared secret
pub struct PairingChannel {
    pub secret: StaticSecret,
    pub public: PublicKey,
}

impl PairingChannel {
    pub fn new() -> Self {
        let mut rng = rand::thread_rng();
        let secret = StaticSecret::random_from_rng(&mut rng);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    pub fn public_bytes(&self) -> [u8; 32] {
        *self.public.as_bytes()
    }

    pub fn diffie_hellman(&self, peer_pub: &[u8; 32]) -> x25519_dalek::SharedSecret {
        let peer = PublicKey::from(*peer_pub);
        self.secret.diffie_hellman(&peer)
    }

    pub fn verification_code(shared: &x25519_dalek::SharedSecret) -> String {
        let hash = Sha256::digest(shared.as_bytes());
        let num = u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) % 1_000_000;
        format!("{num:06}")
    }
}

impl Default for PairingChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Multi-relay failover list (5.3)
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://relay.ferrite.browser/sync",
    "wss://relay2.ferrite.browser/sync",
    "wss://relay3.ferrite.browser/sync",
];

/// Real failover: TCP reachability probe (3s timeout, exponential backoff
/// between candidates) against each relay host and port. Returns the first
/// reachable URL. Probes transport only, not WS auth.
pub async fn connect_with_failover(urls: &[String]) -> Result<String, String> {
    let mut last_err = String::from("no relays configured");
    for (i, url) in urls.iter().enumerate() {
        let backoff = std::time::Duration::from_millis(100 * 2u64.pow(i.min(10) as u32));
        tokio::time::sleep(backoff).await;
        match relay_host_port(url) {
            Some((host, port)) => {
                let dial = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    tokio::net::TcpStream::connect((host.as_str(), port)),
                )
                .await;
                match dial {
                    Ok(Ok(_)) => return Ok(url.clone()),
                    Ok(Err(e)) => last_err = format!("{url}: tcp: {e}"),
                    Err(_) => last_err = format!("{url}: tcp timeout"),
                }
            }
            None => last_err = format!("{url}: unparseable relay url"),
        }
    }
    Err(last_err)
}

fn relay_host_port(url: &str) -> Option<(String, u16)> {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let authority = without_scheme.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }
    if let Some((h, p)) = authority.rsplit_once(':')
        && let Ok(port) = p.parse::<u16>()
    {
        return Some((h.to_owned(), port));
    }
    let default_port = if url.starts_with("wss://") || url.starts_with("https://") {
        443
    } else {
        80
    };
    Some((authority.to_owned(), default_port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e2e_encrypt_decrypt() {
        let key = derive_backup_key("master123", b"saltsaltsaltsalt").unwrap();
        let data = b"vault snapshot json";
        let enc = encrypt_vault_snapshot(data, &key).unwrap();
        assert_ne!(enc, data);
        let dec = decrypt_vault_snapshot(&enc, &key).unwrap();
        assert_eq!(dec, data);
        // Wrong password fails GCM
        let wrong = derive_backup_key("wrong", b"saltsaltsaltsalt").unwrap();
        assert!(decrypt_vault_snapshot(&enc, &wrong).is_err());
    }

    #[test]
    fn versioning_prune() {
        let mut files = vec![
            "ferrite_vault_2026-08-28T14-30.enc".to_owned(),
            "ferrite_vault_2026-08-29T14-30.enc".to_owned(),
            "ferrite_vault_2026-08-30T14-30.enc".to_owned(),
        ];
        let pruned = prune_old_versions(&mut files, 2);
        assert_eq!(pruned.len(), 1);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn oauth_token_encrypted() {
        let vault_key = b"0123456789ABCDEF0123456789ABCDEF";
        let token = "ya29.a0AfH6SMB-example";
        let enc = encrypt_oauth_token(token, vault_key).expect("encrypt must succeed");
        assert_ne!(enc, token.as_bytes());
        let dec = decrypt_oauth_token(&enc, vault_key).unwrap();
        assert_eq!(dec, token);
        // Short key never yields a storable token.
        assert!(encrypt_oauth_token(token, b"short").is_none());
    }

    #[test]
    fn x25519_verification_code_match() {
        let a = PairingChannel::new();
        let b = PairingChannel::new();
        let a_pub = a.public_bytes();
        let b_pub = b.public_bytes();
        let shared_a = a.diffie_hellman(&b_pub);
        let shared_b = b.diffie_hellman(&a_pub);
        assert_eq!(shared_a.as_bytes(), shared_b.as_bytes());
        let code_a = PairingChannel::verification_code(&shared_a);
        let code_b = PairingChannel::verification_code(&shared_b);
        assert_eq!(code_a, code_b);
        assert_eq!(code_a.len(), 6);
        // MITM would have different shared
        let mitm = PairingChannel::new();
        let mitm_shared = a.diffie_hellman(&mitm.public_bytes());
        let mitm_code = PairingChannel::verification_code(&mitm_shared);
        assert_ne!(code_a, mitm_code);
    }

    #[test]
    fn prune_leaves_unknown_files_alone() {
        let mut files = vec![
            "ferrite_vault_2026-08-28T14-30.enc".to_owned(),
            "ferrite_vault_2026-08-29T14-30.enc".to_owned(),
            "ferrite_vault_2026-08-30T14-30.enc".to_owned(),
            "user-notes.txt".to_owned(),
        ];
        let pruned = prune_old_versions(&mut files, 2);
        assert_eq!(pruned.len(), 1);
        assert!(pruned[0].starts_with("ferrite_vault_2026-08-28"));
        assert!(files.contains(&"user-notes.txt".to_owned()));
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn serialize_keeps_typed_columns() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE bookmarks(id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL,
             parent_folder_id TEXT, sort_order REAL NOT NULL DEFAULT 0,
             hlc_timestamp INTEGER NOT NULL, is_deleted INTEGER DEFAULT 0);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bookmarks VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                "id-1",
                "https://example.com",
                "Example",
                Option::<String>::None,
                1536.5,
                123456789i64,
                0
            ],
        )
        .unwrap();
        let bytes = serialize_vault(&conn).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let row = &v["bookmarks"][0];
        // Fractional positions survive as numbers (old code dropped them).
        assert_eq!(row["sort_order"], 1536.5);
        assert_eq!(row["url"], "https://example.com");
    }

    #[test]
    fn snapshot_never_sees_credential_plaintext() {
        // The pipeline takes only `&Connection`, so no key path exists, and
        // decryption is structurally impossible. This test pins it: a
        // field-encrypted credential round-trips through serialize +
        // backup-wrap with the password appearing nowhere.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE credentials(id TEXT PRIMARY KEY, url TEXT NOT NULL, username TEXT NOT NULL,
             encrypted_password BLOB NOT NULL, hlc_timestamp INTEGER NOT NULL);",
        )
        .unwrap();
        let field_key = b"0123456789ABCDEF0123456789ABCDEF";
        let secret = "s3cret-p@ssw0rd-no-backup-sees";
        let enc = encrypt_vault_snapshot(secret.as_bytes(), field_key).unwrap();
        conn.execute(
            "INSERT INTO credentials VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params!["c1", "https://bank.test", "user", enc, 1i64],
        )
        .unwrap();
        let snap = serialize_vault(&conn).unwrap();
        assert!(!snap.windows(secret.len()).any(|w| w == secret.as_bytes()));
        let backup_key = derive_backup_key("master123", b"saltsaltsaltsalt").unwrap();
        let blob = encrypt_vault_snapshot(&snap, &backup_key).unwrap();
        assert!(!blob.windows(secret.len()).any(|w| w == secret.as_bytes()));
    }

    #[test]
    fn device_local_tables_never_serialize() {
        // Trust exceptions and extension state must not cross devices:
        // present in the DB, absent from every snapshot, even though the
        // serializer iterates tables generically.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE bookmarks(id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL,
             parent_folder_id TEXT, sort_order REAL NOT NULL DEFAULT 0,
             hlc_timestamp INTEGER NOT NULL, is_deleted INTEGER DEFAULT 0);
             CREATE TABLE site_permissions(origin TEXT, kind TEXT);
             CREATE TABLE cert_exceptions(host TEXT, sha256 TEXT);
             CREATE TABLE threat_exceptions(host TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO site_permissions VALUES (?1,?2)",
            ["https://bank.test", "camera"],
        )
        .unwrap();
        let bytes = serialize_vault(&conn).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.get("bookmarks").is_some());
        for excluded in ["site_permissions", "cert_exceptions", "threat_exceptions"] {
            assert!(v.get(excluded).is_none(), "{excluded} must never sync");
        }
    }

    #[test]
    fn failover_probes_real_connectivity() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // Reachable: local listener.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let _hold = listener;
            let good = format!("wss://127.0.0.1:{port}/sync");
            let bad = "wss://127.0.0.1:1/sync".to_owned();
            // Bad first: failover must skip it and reach the good one.
            let picked = connect_with_failover(&[bad, good.clone()]).await.unwrap();
            assert_eq!(picked, good);
            // Nothing reachable: error, not a fake success.
            assert!(
                connect_with_failover(&["wss://127.0.0.1:1/sync".to_owned()])
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn hex_inspection_is_ciphertext() {
        let key = derive_backup_key("master", b"salt123456789012").unwrap();
        let enc = encrypt_vault_snapshot(b"bookmark data", &key).unwrap();
        // Hex editor would see no plaintext bookmark
        assert!(!enc.windows(8).any(|w| w == b"bookmark"));
    }

    #[test]
    fn sealed_file_round_trips_and_rejects() {
        let envelope = br#"{"format":"ferrite-backup-1","bookmarks":[]}"#;
        let raw = seal_backup_file(envelope, "correct horse 123").unwrap();
        let back = open_backup_file(&raw, "correct horse 123").unwrap();
        assert_eq!(back, envelope);
        // Wrapper carries no plaintext.
        assert!(!raw.windows(8).any(|w| w == b"bookmark"));
        // Wrong passphrase, wrong shape, short passphrase all fail closed.
        assert!(open_backup_file(&raw, "wrong passphrase").is_err());
        assert!(open_backup_file(b"{}", "correct horse 123").is_err());
        assert!(open_backup_file(b"not json", "correct horse 123").is_err());
        assert!(seal_backup_file(envelope, "short").is_err());
        assert!(seal_backup_file(b"", "correct horse 123").is_err());
    }
}
