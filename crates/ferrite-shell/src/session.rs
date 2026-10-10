//! Encrypted session file. The tab manager serializes documents
//! (`save_json`/`load_json`); this module owns bytes: an AES-256-GCM
//! envelope under the device master key, atomic tmp+rename writes, a
//! one-way migration from the legacy plaintext file, and self-healing
//! quarantine of undecryptable files. A corrupt session regenerates from
//! history; it must never wedge boot or destroy evidence, so quarantined
//! files are renamed aside, never deleted and never loaded.

use std::path::{Path, PathBuf};

pub const SESSION_FILE_NAME: &str = "ferrite-session.enc";
pub(crate) const SESSION_LEGACY_FILE_NAME: &str = "ferrite-session.json";

/// Data-root paths for the encrypted session file and its legacy plaintext
/// original (migration source, never written). Scoped like settings:
/// default keeps legacy names, named profiles stay in their own dir.
pub fn session_paths() -> (PathBuf, PathBuf) {
    session_paths_for(crate::profiles::process_profile())
}

/// Per-profile session paths. Default keeps the legacy bare names;
/// named profiles scope inside their own dir, so two live processes
/// never share (or race) one session file.
pub fn session_paths_for(profile_id: &str) -> (PathBuf, PathBuf) {
    if profile_id.eq_ignore_ascii_case(crate::profiles::DEFAULT_ID) {
        let root = crate::profiles::data_root();
        (
            root.join(SESSION_FILE_NAME),
            root.join(SESSION_LEGACY_FILE_NAME),
        )
    } else {
        let dir = crate::profiles::profile_dir(profile_id);
        (dir.join("session.enc"), dir.join("session.json"))
    }
}

pub struct SessionLoad {
    pub json: Option<String>,
    /// True when a corrupt/foreign file was quarantined to unblock boot.
    /// Caller toasts; tabs regenerate from history.
    pub quarantined: bool,
}

/// Encrypt `json` under `key` and atomically replace the session file.
/// Completes the legacy migration by removing the plaintext original once
/// the encrypted write has landed (a leftover replays migration next boot
/// but is never written again).
pub fn save_session(json: &str, key: &[u8]) -> Result<(), String> {
    save_session_for(json, key, crate::profiles::DEFAULT_ID)
}

/// Profile-scoped save: named profiles keep their session inside their
/// own dir (two live profiles must never race one file).
pub fn save_session_for(json: &str, key: &[u8], profile_id: &str) -> Result<(), String> {
    let (path, legacy) = session_paths_for(profile_id);
    save_session_to(&path, &legacy, json, key)
}

pub(crate) fn save_session_to(
    path: &Path,
    legacy: &Path,
    json: &str,
    key: &[u8],
) -> Result<(), String> {
    let doc = ferrite_credentials::seal_bytes(json.as_bytes(), key)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, doc).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    let _ = std::fs::remove_file(legacy);
    Ok(())
}

/// Load the session document. Encrypted file first (needs `key`); legacy
/// plaintext once (migration read, keyless); corrupt or foreign files are
/// quarantined aside so a bad file cannot wedge boot. `None` without any
/// usable document means fresh boot, never an error.
pub fn load_session(key: Option<&[u8]>) -> SessionLoad {
    load_session_for(key, crate::profiles::DEFAULT_ID)
}

/// Profile-scoped load (mirrors `save_session_for`).
pub fn load_session_for(key: Option<&[u8]>, profile_id: &str) -> SessionLoad {
    let (path, legacy) = session_paths_for(profile_id);
    load_session_from(&path, &legacy, key)
}

pub(crate) fn load_session_from(path: &Path, legacy: &Path, key: Option<&[u8]>) -> SessionLoad {
    if path.exists() {
        if let Some(k) = key
            && let Ok(text) = std::fs::read_to_string(path)
            && let Some(plain) = ferrite_credentials::open_sealed_bytes(&text, k)
            && let Ok(json) = String::from_utf8(plain)
        {
            return SessionLoad {
                json: Some(json),
                quarantined: false,
            };
        }
        // Keyless boots cannot judge the file: leave it for a keyed boot.
        if key.is_none() {
            return SessionLoad {
                json: legacy_load_from(legacy),
                quarantined: false,
            };
        }
        quarantine_to(path);
        return SessionLoad {
            json: legacy_load_from(legacy),
            quarantined: true,
        };
    }
    SessionLoad {
        json: legacy_load_from(legacy),
        quarantined: false,
    }
}

/// Side-effect-free sealed read for rotation probes: decrypts under
/// `key`, never quarantines, never touches the legacy file. `None` on
/// any miss (missing, foreign, or other-key bytes).
pub(crate) fn read_sealed_from(path: &Path, key: &[u8]) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let plain = ferrite_credentials::open_sealed_bytes(&text, key)?;
    String::from_utf8(plain).ok()
}

/// Side-effect-free legacy read for rotation probes: content only when it
/// parses as a v1 session document. Never deletes (deletion happens on
/// sealed write).
fn legacy_load_from(legacy: &Path) -> Option<String> {
    let text = std::fs::read_to_string(legacy).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    if v.get("version").and_then(|x| x.as_u64()) != Some(1) {
        return None;
    }
    v.get("tabs")
        .and_then(|x| x.as_array())
        .is_some()
        .then_some(text)
}

fn quarantine_to(path: &Path) {
    let stamped = path.with_extension(format!(
        "corrupt-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let _ = std::fs::rename(path, stamped);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("ferrite-session-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let enc = dir.join("ferrite-session.enc");
        let legacy = dir.join("ferrite-session.json");
        (dir, enc, legacy)
    }

    const KEY: [u8; 32] = [0xA5; 32];
    const WRONG: [u8; 32] = [0x5A; 32];
    const DOC: &str = r#"{"version":1,"active":0,"tabs":[]}"#;

    #[test]
    fn save_load_round_trip_never_plaintext() {
        let (dir, enc, legacy) = case("roundtrip");
        save_session_to(&enc, &legacy, DOC, &KEY).unwrap();
        let raw = std::fs::read_to_string(&enc).unwrap();
        assert!(!raw.contains("tabs"), "ciphertext must not leak plaintext");
        let loaded = load_session_from(&enc, &legacy, Some(&KEY));
        assert_eq!(loaded.json.as_deref(), Some(DOC));
        assert!(!loaded.quarantined);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_key_quarantines_and_keeps_evidence() {
        let (dir, enc, legacy) = case("wrongkey");
        save_session_to(&enc, &legacy, DOC, &KEY).unwrap();
        let loaded = load_session_from(&enc, &legacy, Some(&WRONG));
        assert!(loaded.json.is_none());
        assert!(loaded.quarantined);
        assert!(!enc.exists(), "bad file renamed aside");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_plaintext_migrates_once_then_deletes() {
        let (dir, enc, legacy) = case("legacy");
        std::fs::write(&legacy, DOC).unwrap();
        // Migration read needs no key.
        let loaded = load_session_from(&enc, &legacy, None);
        assert_eq!(loaded.json.as_deref(), Some(DOC));
        assert!(!loaded.quarantined);
        assert!(
            legacy.exists(),
            "legacy survives until encrypted write lands"
        );
        // First encrypted save completes the migration.
        save_session_to(&enc, &legacy, DOC, &KEY).unwrap();
        assert!(!legacy.exists());
        let loaded = load_session_from(&enc, &legacy, Some(&KEY));
        assert_eq!(loaded.json.as_deref(), Some(DOC));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_and_foreign_files_quarantine_fresh() {
        let (dir, enc, legacy) = case("corrupt");
        std::fs::write(&enc, b"{not an envelope}").unwrap();
        let loaded = load_session_from(&enc, &legacy, Some(&KEY));
        assert!(loaded.json.is_none());
        assert!(loaded.quarantined);
        assert!(!enc.exists());
        // Missing everything is a fresh boot, not an error.
        let (dir2, enc2, legacy2) = case("fresh");
        let loaded = load_session_from(&enc2, &legacy2, Some(&KEY));
        assert!(loaded.json.is_none());
        assert!(!loaded.quarantined);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
