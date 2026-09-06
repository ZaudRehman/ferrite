//! Recovery phrase handling.

use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use bip39::{Language, Mnemonic};
use rand::RngCore;

use crate::error::{VaultError, VaultResult};

pub fn generate_mnemonic(words: usize) -> VaultResult<String> {
    let entropy_bits = match words {
        12 => 128,
        24 => 256,
        _ => {
            return Err(VaultError::Crypto(
                "mnemonic must be 12 or 24 words".to_owned(),
            ));
        }
    };
    let mut entropy = vec![0u8; entropy_bits / 8];
    rand::thread_rng().fill_bytes(&mut entropy);
    let m = Mnemonic::from_entropy_in(Language::English, &entropy)
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    Ok(m.to_string())
}

pub fn derive_wrapping_key(phrase: &str, salt: &[u8]) -> VaultResult<Vec<u8>> {
    if salt.len() < 16 {
        return Err(VaultError::Crypto(
            "kdf salt must be >= 16 bytes".to_owned(),
        ));
    }
    let salt_str = SaltString::encode_b64(salt).map_err(|e| VaultError::Crypto(e.to_string()))?;
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(phrase.as_bytes(), &salt_str)
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    let bytes = hash
        .hash
        .ok_or_else(|| VaultError::Crypto("argon2 hash missing".to_owned()))?
        .as_bytes()[..32]
        .to_vec();
    Ok(bytes)
}

pub fn wrap_master_key(master: &[u8], wrapping_key: &[u8]) -> VaultResult<Vec<u8>> {
    use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce, aead::Aead};
    if wrapping_key.len() != 32 {
        return Err(VaultError::Crypto(
            "wrapping key must be 32 bytes".to_owned(),
        ));
    }
    if master.is_empty() || master.len() > 64 {
        return Err(VaultError::Crypto("master key length invalid".to_owned()));
    }
    #[allow(deprecated)]
    let key = Key::<Aes256Gcm>::from_slice(&wrapping_key[..32]);
    let cipher = Aes256Gcm::new(key);
    let mut nonce_bytes = [0u8; 12];
    {
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
    }
    #[allow(deprecated)]
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, master)
        .map_err(|e| VaultError::Crypto(format!("wrap failed: {e}")))?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn unwrap_master_key(wrapped: &[u8], wrapping_key: &[u8]) -> VaultResult<Vec<u8>> {
    use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce, aead::Aead};
    if wrapping_key.len() != 32 {
        return Err(VaultError::Crypto(
            "wrapping key must be 32 bytes".to_owned(),
        ));
    }
    if wrapped.len() < 12 + 16 {
        return Err(VaultError::Crypto("wrapped key truncated".to_owned()));
    }
    #[allow(deprecated)]
    let key = Key::<Aes256Gcm>::from_slice(&wrapping_key[..32]);
    let cipher = Aes256Gcm::new(key);
    let (nonce_bytes, ct) = wrapped.split_at(12);
    #[allow(deprecated)]
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ct)
        .map_err(|_| VaultError::Crypto("wrapped key authentication failed".to_owned()))
}
