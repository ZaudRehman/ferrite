//! E2EE cloud sync via Argon2 + AES-GCM.

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, KeyInit},
};
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use rand::RngCore;

pub fn derive_key(password: &str, salt: &[u8]) -> Vec<u8> {
    assert!(salt.len() >= 16, "salt must be >= 16 bytes");
    let salt_str = SaltString::encode_b64(salt).expect("salt must be valid b64 input");
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt_str)
        .expect("argon2 must succeed");
    hash.hash.expect("argon2 hash missing").as_bytes()[..32].to_vec()
}

pub fn encrypt_vault(data: &[u8], key: &[u8]) -> Vec<u8> {
    assert_eq!(key.len(), 32, "vault key must be exactly 32 bytes");
    let key = Key::<Aes256Gcm>::try_from(&key[..32]).expect("32B key");
    let cipher = Aes256Gcm::new(&key);
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);
    let mut out = nonce_bytes.to_vec();
    out.extend(cipher.encrypt(&nonce, data).expect("encrypt must succeed"));
    out
}

pub fn decrypt_vault(data: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 12 + 16 || key.len() != 32 {
        return None;
    }
    let key = Key::<Aes256Gcm>::try_from(&key[..32]).ok()?;
    let cipher = Aes256Gcm::new(&key);
    let nonce = Nonce::try_from(&data[..12]).ok()?;
    cipher.decrypt(&nonce, &data[12..]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn e2ee_roundtrip() {
        let pwd = "correct horse battery staple";
        let salt = b"saltsaltsaltsalt";
        let key = derive_key(pwd, salt);
        let data = b"vault data";
        let enc = encrypt_vault(data, &key);
        assert_ne!(enc, data);
        let dec = decrypt_vault(&enc, &key).unwrap();
        assert_eq!(dec, data);
        assert!(decrypt_vault(&enc, &derive_key("wrong", salt)).is_none());
    }
}
