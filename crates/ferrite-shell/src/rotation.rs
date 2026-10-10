//! Key rotation: re-encrypt every store under a fresh device key,
//! crash-safe. The keyring next-entry is the journal (written before any
//! store changes, deleted after all stores verify under the new key).
//! Resume converges per store by trying the new key first (already
//! rotated) then the old (pending). Fresh rotation (live handles, worker
//! thread) and boot resume (fresh opens) share `rotate_stores`, so resume
//! is tested by construction, not by disaster.
//!
//! Rotation covers the default profile stores (the only ones the shell
//! boots). File steps reuse the boot loaders, so quarantine and migration
//! semantics match startup exactly; the new-key probe short-circuits
//! before any loader runs.

use std::path::Path;

/// Re-seal the session file from `old_key` to `new_key`. Returns whether
/// bytes changed. Already-under-new short-circuits on a side-effect-free
/// probe; otherwise the boot loader runs (decrypt-or-legacy with
/// quarantine, same semantics as startup), and its document reseals under
/// the new key. Missing or foreign files are not an error.
pub(crate) fn reseal_session_file(
    enc: &Path,
    legacy: &Path,
    old_key: &[u8],
    new_key: &[u8],
) -> Result<bool, String> {
    if crate::session::read_sealed_from(enc, new_key).is_some() {
        return Ok(false);
    }
    let loaded = crate::session::load_session_from(enc, legacy, Some(old_key));
    let Some(json) = loaded.json else {
        return Ok(false);
    };
    crate::session::save_session_to(enc, legacy, &json, new_key)?;
    Ok(true)
}

/// Re-seal the settings file from `old_key` to `new_key`. Same
/// probe-then-loader shape as the session (boot semantics reused, not
/// reimplemented).
pub(crate) fn reseal_settings_file(
    enc: &Path,
    legacy: &Path,
    old_key: &[u8],
    new_key: &[u8],
) -> Result<bool, String> {
    // Probe with the raw envelope: a decoded-Value read cannot tell an
    // empty document from an unreadable one, and rewriting identical
    // bytes buys nothing either way.
    let already = std::fs::read_to_string(enc)
        .ok()
        .and_then(|text| ferrite_credentials::open_sealed_bytes(&text, new_key))
        .is_some();
    if already {
        return Ok(false);
    }
    let current = crate::settings_file::read_settings_from(enc, legacy, Some(old_key));
    if current.as_object().is_none() {
        return Ok(false);
    }
    if crate::settings_file::write_settings_to(enc, legacy, &current, new_key) {
        Ok(true)
    } else {
        Err("settings reseal write failed".to_owned())
    }
}

/// Full rotation under already-resolved keys: journal the next key,
/// rotate every store, promote, delete the journal. Returns the
/// user-facing report line (success or remedy). Callers zeroize their key
/// copies after return.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_rotation(
    vault: &ferrite_vault::Vault,
    archive: Option<&ferrite_archive::Archive>,
    session_enc: &Path,
    session_legacy: &Path,
    settings_enc: &Path,
    settings_legacy: &Path,
    old_key: &[u8],
    new_key: &[u8],
) -> String {
    use ferrite_vault::keyring;
    if keyring::store_next_key(new_key).is_err() {
        return "Key rotation failed: couldn't journal the new key.".to_owned();
    }
    if let Err(e) = rotate_stores(
        vault,
        archive,
        session_enc,
        session_legacy,
        settings_enc,
        settings_legacy,
        old_key,
        new_key,
    ) {
        return format!(
            "Key rotation failed ({e}). Your old key still works; restart to resume rotation."
        );
    }
    if keyring::store_master_key(new_key).is_err() {
        return "Key rotation stalled: stores are re-encrypted but the new key couldn't be stored. Restart to complete.".to_owned();
    }
    if keyring::delete_next_key().is_err() {
        return "Key rotation done, but the journal entry lingers; it clears on next boot."
            .to_owned();
    }
    "Encryption key rotated.".to_owned()
}

/// Re-encrypt the live stores under `new_key`: vault rekey, archive rekey
/// when persistent, both files re-sealed. Works on live handles (worker)
/// and fresh opens (boot resume) alike. Any error aborts with the old key
/// still live; split-key states converge on the next resume because every
/// step below is idempotent.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rotate_stores(
    vault: &ferrite_vault::Vault,
    archive: Option<&ferrite_archive::Archive>,
    session_enc: &Path,
    session_legacy: &Path,
    settings_enc: &Path,
    settings_legacy: &Path,
    old_key: &[u8],
    new_key: &[u8],
) -> Result<(), String> {
    vault.rekey(new_key).map_err(|e| format!("vault: {e}"))?;
    if let Some(store) = archive
        && store.is_persistent()
    {
        store.rekey(new_key).map_err(|e| format!("archive: {e}"))?;
    }
    reseal_session_file(session_enc, session_legacy, old_key, new_key)?;
    reseal_settings_file(settings_enc, settings_legacy, old_key, new_key)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("ferrite-rotate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("s.enc"), dir.join("s.json"))
    }

    const OLD: [u8; 32] = [0x11; 32];
    const NEW: [u8; 32] = [0x22; 32];
    const DOC: &str = r#"{"version":1,"active":0,"tabs":[]}"#;

    #[test]
    fn session_reseal_converges_and_idempotent() {
        let (dir, enc, legacy) = case("session");
        crate::session::save_session_to(&enc, &legacy, DOC, &OLD).unwrap();
        assert!(reseal_session_file(&enc, &legacy, &OLD, &NEW).unwrap());
        assert_eq!(
            crate::session::read_sealed_from(&enc, &NEW).as_deref(),
            Some(DOC)
        );
        assert!(crate::session::read_sealed_from(&enc, &OLD).is_none());
        // Second run converges without rewriting.
        assert!(!reseal_session_file(&enc, &legacy, &OLD, &NEW).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_reseal_missing_is_fine() {
        let (dir, enc, legacy) = case("missing");
        assert!(!reseal_session_file(&enc, &legacy, &OLD, &NEW).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_reseal_converges() {
        let (dir, enc, legacy) = case("settings");
        let v = serde_json::json!({"theme": "dark"});
        assert!(crate::settings_file::write_settings_to(
            &enc, &legacy, &v, &OLD
        ));
        assert!(reseal_settings_file(&enc, &legacy, &OLD, &NEW).unwrap());
        assert_eq!(
            crate::settings_file::read_settings_from(&enc, &legacy, Some(&NEW)),
            v
        );
        assert!(!reseal_settings_file(&enc, &legacy, &OLD, &NEW).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
