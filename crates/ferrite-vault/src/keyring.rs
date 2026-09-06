//! OS keychain integration.

use keyring::Entry;

use crate::error::{VaultError, VaultResult};

const SERVICE: &str = "ferrite-browser";
const USER: &str = "vault-master-key";

pub fn store_master_key(key: &[u8]) -> VaultResult<()> {
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
        Ok(hex) => {
            if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                // Corrupt entry: do NOT mint a new key (would orphan data).
                // Surface as an error so the caller preserves the vault.
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
                Ok(Some(bytes))
            } else {
                Err(VaultError::Keyring("stored key length invalid".to_owned()))
            }
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(VaultError::Keyring(e.to_string())),
    }
}

pub fn generate_master_key() -> VaultResult<Vec<u8>> {
    let mut key = vec![0u8; 32];
    getrandom::getrandom(&mut key).map_err(|e| {
        VaultError::Crypto(format!("CSPRNG unavailable, refusing weak fallback: {e}"))
    })?;
    Ok(key)
}
