//! OS keychain integration.

use keyring::Entry;
use rand::RngCore;

use crate::error::{VaultError, VaultResult};

const SERVICE: &str = "ferrite-browser";
const USER: &str = "vault-master-key";
/// Rotation journal: the pending next key, written before any store
/// re-encrypts and deleted after all stores verify under it. A crash
/// between the two leaves boot able to resume (new key known) instead of
/// bricked (neither key opening everything).
const NEXT_USER: &str = "vault-next-key";

fn decode_hex_key(hex: &str) -> VaultResult<Vec<u8>> {
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        // Corrupt entry must error, never mint a replacement key.
        return Err(VaultError::Keyring(
            "stored master key is corrupt (expected 64 hex chars)".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(32);
    for i in (0..hex.len()).step_by(2) {
        let b = u8::from_str_radix(&hex[i..i + 2], 16)
            .map_err(|_| VaultError::Keyring("stored key hex decode failed".to_owned()))?;
        bytes.push(b);
    }
    if bytes.len() == 32 {
        Ok(bytes)
    } else {
        Err(VaultError::Keyring("stored key length invalid".to_owned()))
    }
}

pub fn store_master_key(key: &[u8]) -> VaultResult<()> {
    if key.len() != 32 {
        return Err(VaultError::Keyring(
            "master key must be exactly 32 bytes".to_owned(),
        ));
    }
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let entry = Entry::new(SERVICE, USER).map_err(|e| VaultError::Keyring(e.to_string()))?;
    entry
        .set_password(&hex)
        .map_err(|e| VaultError::Keyring(e.to_string()))?;
    Ok(())
}

pub fn load_master_key() -> VaultResult<Option<Vec<u8>>> {
    let entry = Entry::new(SERVICE, USER).map_err(|e| VaultError::Keyring(e.to_string()))?;
    match entry.get_password() {
        Ok(hex) => decode_hex_key(&hex).map(Some),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(VaultError::Keyring(e.to_string())),
    }
}

/// Read the pending rotation key, if a rotation was interrupted. `None`
/// means no rotation is outstanding (or the journal entry is corrupt, in
/// which case it errors like any corrupt keyring entry).
pub fn load_next_key() -> VaultResult<Option<Vec<u8>>> {
    let entry = Entry::new(SERVICE, NEXT_USER).map_err(|e| VaultError::Keyring(e.to_string()))?;
    match entry.get_password() {
        Ok(hex) => decode_hex_key(&hex).map(Some),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(VaultError::Keyring(e.to_string())),
    }
}

/// Journal a rotation key before re-encrypting any store.
pub fn store_next_key(key: &[u8]) -> VaultResult<()> {
    if key.len() != 32 {
        return Err(VaultError::Keyring(
            "rotation key must be exactly 32 bytes".to_owned(),
        ));
    }
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let entry = Entry::new(SERVICE, NEXT_USER).map_err(|e| VaultError::Keyring(e.to_string()))?;
    entry
        .set_password(&hex)
        .map_err(|e| VaultError::Keyring(e.to_string()))?;
    Ok(())
}

/// Clear the rotation journal after every store verifies under the new key.
pub fn delete_next_key() -> VaultResult<()> {
    let entry = Entry::new(SERVICE, NEXT_USER).map_err(|e| VaultError::Keyring(e.to_string()))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(VaultError::Keyring(e.to_string())),
    }
}

pub fn generate_master_key() -> VaultResult<Vec<u8>> {
    // Same CSPRNG pattern as the recovery phrase (recovery.rs): the OS RNG
    // via rand, no direct getrandom dep (0.4 removed the free function, and
    // a second RNG API for one call site is pure surface).
    let mut key = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_master_key_rejects_non_32_bytes_before_touching_keychain() {
        for bad in [&[][..], &[0xAA; 16][..], &[0xAA; 31][..], &[0xAA; 33][..]] {
            let err = store_master_key(bad).unwrap_err().to_string();
            assert!(err.contains("32 bytes"), "got: {err}");
        }
    }

    #[test]
    fn store_master_key_accepts_32_bytes() {
        // Must not reject the length. No round-trip assertion: some
        // environments back the keyring with a non-persistent store
        // where a successful write still reads back as missing.
        assert!(store_master_key(&[0xAA; 32]).is_ok());
    }
}
