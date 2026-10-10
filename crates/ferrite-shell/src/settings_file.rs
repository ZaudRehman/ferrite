//! Shell-owned settings file. One sealed document holds both the privacy
//! engine's fields and the shell's own keys (the engine serializes its
//! half via `export_settings_json`; merges happen on JSON values, so a
//! privacy toggle can no longer clobber UI prefs by rewriting the file).
//! AES-256-GCM under the device master key, atomic tmp+rename writes, and
//! a one-way migration from the legacy plaintext file. Corrupt sealed
//! files quarantine aside with defaults (preferences regenerate; they must
//! never wedge boot).

use std::path::{Path, PathBuf};

pub const SETTINGS_FILE_NAME: &str = "ferrite-settings.enc";
pub(crate) const SETTINGS_LEGACY_FILE_NAME: &str = "ferrite-settings.json";

/// Data-root paths for the sealed settings file and its legacy plaintext
/// original (migration source, never written). Scoped to the process
/// profile: default keeps legacy bare names, named profiles live inside
/// their own dir (two live profiles must never race one file).
pub fn settings_paths() -> (PathBuf, PathBuf) {
    let id = crate::profiles::process_profile();
    if id.eq_ignore_ascii_case(crate::profiles::DEFAULT_ID) {
        let root = crate::profiles::data_root();
        (
            root.join(SETTINGS_FILE_NAME),
            root.join(SETTINGS_LEGACY_FILE_NAME),
        )
    } else {
        let dir = crate::profiles::profile_dir(id);
        (dir.join("settings.enc"), dir.join("settings.json"))
    }
}

fn empty_doc() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Read the settings document: sealed file first (needs `key`), legacy
/// plaintext once (migration read, keyless). Missing or unusable yields
/// an empty document, never an error.
pub fn read_settings_value(key: Option<&[u8]>) -> serde_json::Value {
    let (path, legacy) = settings_paths();
    read_settings_from(&path, &legacy, key)
}

pub(crate) fn read_settings_from(
    path: &Path,
    legacy: &Path,
    key: Option<&[u8]>,
) -> serde_json::Value {
    if path.exists() {
        if let Some(k) = key
            && let Ok(text) = std::fs::read_to_string(path)
            && let Some(plain) = ferrite_credentials::open_sealed_bytes(&text, k)
            && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&plain)
            && v.is_object()
        {
            return v;
        }
        // Keyless boots cannot judge the file: leave it for a keyed boot.
        if key.is_none() {
            return legacy_value(legacy);
        }
        quarantine_to(path);
        return legacy_value(legacy);
    }
    legacy_value(legacy)
}

/// Write the settings document sealed under `key` (atomic replace).
/// Completes the legacy migration by removing the plaintext original.
/// `false` without a key or on any failure: callers keep running on the
/// in-memory values and retry next persist.
pub fn write_settings_value(v: &serde_json::Value, key: &[u8]) -> bool {
    let (path, legacy) = settings_paths();
    // The data root may not exist on first boot (fresh install): create it
    // here so a missing dir can never wedge settings persistence.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    write_settings_to(&path, &legacy, v, key)
}

pub(crate) fn write_settings_to(
    path: &Path,
    legacy: &Path,
    v: &serde_json::Value,
    key: &[u8],
) -> bool {
    let Ok(doc) = ferrite_credentials::seal_bytes(
        serde_json::to_string(v).unwrap_or_default().as_bytes(),
        key,
    ) else {
        return false;
    };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, doc).is_err() {
        return false;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    let _ = std::fs::remove_file(legacy);
    true
}

/// Remove both the sealed file and any legacy leftover (settings reset).
pub fn delete_settings_files() {
    let (path, legacy) = settings_paths();
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(legacy);
}

/// One-time legacy read: the document only when it parses as a JSON
/// object, so a stray non-settings file can never seed preferences.
fn legacy_value(legacy: &Path) -> serde_json::Value {
    std::fs::read_to_string(legacy)
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
        .filter(|v: &serde_json::Value| v.is_object())
        .unwrap_or_else(empty_doc)
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
            std::env::temp_dir().join(format!("ferrite-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (
            dir.clone(),
            dir.join("ferrite-settings.enc"),
            dir.join("ferrite-settings.json"),
        )
    }

    const KEY: [u8; 32] = [0x3C; 32];
    const WRONG: [u8; 32] = [0xC3; 32];

    fn doc() -> serde_json::Value {
        serde_json::json!({"adblock_enabled": true, "chrome_pack": "tenebra"})
    }

    #[test]
    fn round_trip_never_plaintext() {
        let (dir, enc, legacy) = case("roundtrip");
        assert!(write_settings_to(&enc, &legacy, &doc(), &KEY));
        let raw = std::fs::read_to_string(&enc).unwrap();
        assert!(!raw.contains("chrome_pack"));
        assert_eq!(read_settings_from(&enc, &legacy, Some(&KEY)), doc());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_quarantines_and_yields_empty() {
        let (dir, enc, legacy) = case("corrupt");
        std::fs::write(&enc, b"{nope}").unwrap();
        let v = read_settings_from(&enc, &legacy, Some(&KEY));
        assert_eq!(v, empty_doc());
        assert!(!enc.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_key_quarantines() {
        let (dir, enc, legacy) = case("wrongkey");
        assert!(write_settings_to(&enc, &legacy, &doc(), &KEY));
        let v = read_settings_from(&enc, &legacy, Some(&WRONG));
        assert_eq!(v, empty_doc());
        assert!(!enc.exists(), "unauthenticated file renamed aside");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_migrates_once() {
        let (dir, enc, legacy) = case("legacy");
        std::fs::write(&legacy, serde_json::to_string(&doc()).unwrap()).unwrap();
        // Keyless boots still read (migration replay, no deletion).
        assert_eq!(read_settings_from(&enc, &legacy, None), doc());
        assert!(legacy.exists());
        assert!(write_settings_to(&enc, &legacy, &doc(), &KEY));
        assert!(!legacy.exists());
        assert_eq!(read_settings_from(&enc, &legacy, Some(&KEY)), doc());
        // Non-object legacy files never seed preferences.
        std::fs::write(&legacy, b"[1,2]").unwrap();
        let _ = std::fs::remove_file(&enc);
        assert_eq!(read_settings_from(&enc, &legacy, Some(&KEY)), empty_doc());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_is_empty_not_error() {
        let (dir, enc, legacy) = case("missing");
        assert_eq!(read_settings_from(&enc, &legacy, Some(&KEY)), empty_doc());
        assert!(!write_settings_to(&enc, &legacy, &doc(), b"short"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
