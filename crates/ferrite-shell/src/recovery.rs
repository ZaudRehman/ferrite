//! Recovery-phrase sidecar: the escape hatch for a lost device key.
//!
//! The wrapped master key lives in `ferrite-recovery.wrap` BESIDE the
//! databases, never inside them: a wrap inside the encrypted vault is
//! unreachable exactly when recovery is needed (key lost → vault won't
//! open → wrap unreadable). The sidecar is safe in plaintext because the
//! blob is AES-GCM under Argon2(passphrase) with a 128/256-bit mnemonic;
//! without the phrase it is opaque.
//!
//! Flows: settings enables (generates a fresh phrase, writes the sidecar,
//! shows the phrase once); the decision screen recovers (phrase → master
//! → keyring → relaunch into a verifying boot). Overwriting the sidecar
//! invalidates the previous phrase.

use std::path::{Path, PathBuf};

pub const RECOVERY_FILE_NAME: &str = "ferrite-recovery.wrap";

/// Data-root sidecar path (migrated from the launch directory on first boot
/// like every other persistent file; see `migrate_cwd_to_data_root`).
pub fn recovery_path() -> PathBuf {
    crate::profiles::data_root().join(RECOVERY_FILE_NAME)
}

fn sidecar_doc(salt: &[u8], wrap: &[u8]) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    serde_json::json!({
        "v": 1,
        "salt": b64.encode(salt),
        "wrap": b64.encode(wrap),
    })
    .to_string()
}

fn open_sidecar_doc(text: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    use base64::Engine;
    if text.len() > 16 * 1024 {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("v").and_then(|x| x.as_u64()) != Some(1) {
        return None;
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let salt = v
        .get("salt")
        .and_then(|x| x.as_str())
        .and_then(|s| b64.decode(s).ok())?;
    let wrap = v
        .get("wrap")
        .and_then(|x| x.as_str())
        .and_then(|s| b64.decode(s).ok())?;
    if salt.len() < 16 || wrap.is_empty() || wrap.len() > 1024 {
        return None;
    }
    Some((salt, wrap))
}

/// Enable recovery: mint a fresh 24-word phrase, wrap `master`, verify
/// the round-trip, atomically store the sidecar. Returns the phrase for
/// one-time display (the only copy, by design). Overwrites any previous
/// sidecar, invalidating its phrase.
pub fn enable_sidecar(master: &[u8]) -> Result<String, String> {
    enable_sidecar_to(&recovery_path(), master)
}

pub(crate) fn enable_sidecar_to(path: &Path, master: &[u8]) -> Result<String, String> {
    use ferrite_vault::recovery;
    if master.len() != 32 {
        return Err("master key must be exactly 32 bytes".to_owned());
    }
    let phrase = recovery::generate_mnemonic(24).map_err(|e| e.to_string())?;
    let mut salt = vec![0u8; 16];
    {
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut salt);
    }
    let wrapping = recovery::derive_wrapping_key(&phrase, &salt).map_err(|e| e.to_string())?;
    let wrapped = recovery::wrap_master_key(master, &wrapping).map_err(|e| e.to_string())?;
    // Verify before persisting (never store a bad wrap).
    let check = recovery::unwrap_master_key(&wrapped, &wrapping).map_err(|e| e.to_string())?;
    if check.len() != 32 {
        return Err("recovery wrap verification failed".to_owned());
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, sidecar_doc(&salt, &wrapped))
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    Ok(phrase)
}

/// Recover the master key from the sidecar under `phrase`. Wrong phrases
/// fail closed (auth error); corrupt sidecars read as failure, never half
/// keys. Callers store the result in the keyring and relaunch.
pub fn recover_from_sidecar(phrase: &str) -> Result<Vec<u8>, String> {
    recover_from_sidecar_at(&recovery_path(), phrase)
}

pub(crate) fn recover_from_sidecar_at(path: &Path, phrase: &str) -> Result<Vec<u8>, String> {
    use ferrite_vault::recovery;
    let words: Vec<&str> = phrase.split_whitespace().collect();
    if words.len() != 12 && words.len() != 24 {
        return Err("recovery phrase must be 12 or 24 words".to_owned());
    }
    let text =
        std::fs::read_to_string(path).map_err(|_| "no recovery phrase is set up".to_owned())?;
    let (salt, wrapped) =
        open_sidecar_doc(&text).ok_or_else(|| "recovery data unreadable".to_owned())?;
    let wrapping = recovery::derive_wrapping_key(phrase, &salt).map_err(|e| e.to_string())?;
    let master = recovery::unwrap_master_key(&wrapped, &wrapping).map_err(|e| e.to_string())?;
    if master.len() != 32 {
        return Err("recovered key invalid".to_owned());
    }
    Ok(master)
}

/// Delete the sidecar, invalidating its phrase.
pub fn disable_sidecar() -> bool {
    std::fs::remove_file(recovery_path()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("ferrite-recover-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("ferrite-recovery.wrap"))
    }

    const MASTER: [u8; 32] = [0x77; 32];

    #[test]
    fn enable_recover_round_trip() {
        let (dir, path) = case("roundtrip");
        let phrase = enable_sidecar_to(&path, &MASTER).unwrap();
        assert_eq!(phrase.split_whitespace().count(), 24);
        // Sidecar discloses nothing without the phrase.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains(&phrase.replace(' ', "")));
        let back = recover_from_sidecar_at(&path, &phrase).unwrap();
        assert_eq!(back, MASTER.to_vec());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_phrase_and_shapes_fail_closed() {
        let (dir, path) = case("wrong");
        let phrase = enable_sidecar_to(&path, &MASTER).unwrap();
        let mut wrong: Vec<&str> = phrase.split_whitespace().collect();
        wrong[0] = if wrong[0] == "abandon" {
            "ability"
        } else {
            "abandon"
        };
        assert!(recover_from_sidecar_at(&path, &wrong.join(" ")).is_err());
        assert!(recover_from_sidecar_at(&path, "too short").is_err());
        assert!(recover_from_sidecar_at(&path, "").is_err());
        let missing = dir.join("absent.wrap");
        assert!(recover_from_sidecar_at(&missing, &phrase).is_err());
        std::fs::write(&path, b"{nope}").unwrap();
        assert!(recover_from_sidecar_at(&path, &phrase).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overwrite_invalidates_previous_phrase() {
        let (dir, path) = case("overwrite");
        let first = enable_sidecar_to(&path, &MASTER).unwrap();
        let second = enable_sidecar_to(&path, &MASTER).unwrap();
        // Fresh salt per enable: phrases differ, only the latest works.
        assert_ne!(first, second);
        assert!(recover_from_sidecar_at(&path, &second).is_ok());
        assert!(recover_from_sidecar_at(&path, &first).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
