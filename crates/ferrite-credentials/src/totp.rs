//! TOTP handling.

use totp_rs::{Algorithm, Secret, TOTP};
use zeroize::Zeroize;

#[derive(Debug, Clone)]
pub struct TotpSecret {
    pub credential_id: String,
    pub encrypted_secret: Vec<u8>,
    pub digits: u8,
    pub period: u32,
}

impl TotpSecret {
    pub fn new(credential_id: &str, secret: &str, key: &[u8]) -> Self {
        assert_eq!(key.len(), 32, "master key must be exactly 32 bytes");
        let mut s = secret.as_bytes().to_vec();
        let enc = {
            use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce, aead::Aead};
            #[allow(deprecated)]
            let k = Key::<Aes256Gcm>::from_slice(&key[..32]);
            let cipher = Aes256Gcm::new(k);
            let mut nonce_bytes = [0u8; 12];
            {
                use rand::RngCore;
                rand::thread_rng().fill_bytes(&mut nonce_bytes);
            }
            #[allow(deprecated)]
            let nonce = Nonce::from_slice(&nonce_bytes);
            let ct = cipher
                .encrypt(nonce, s.as_slice())
                .expect("totp encrypt must succeed");
            let mut out = Vec::with_capacity(12 + ct.len());
            out.extend_from_slice(&nonce_bytes);
            out.extend_from_slice(&ct);
            out
        };
        s.zeroize();
        Self {
            credential_id: credential_id.to_owned(),
            encrypted_secret: enc,
            digits: 6,
            period: 30,
        }
    }

    pub fn current_code(&self, key: &[u8]) -> Option<String> {
        use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce, aead::Aead};
        if key.len() != 32 || self.encrypted_secret.len() < 12 + 16 {
            return None;
        }
        let mut plain = {
            #[allow(deprecated)]
            let k = Key::<Aes256Gcm>::from_slice(&key[..32]);
            let cipher = Aes256Gcm::new(k);
            let (nb, ct) = self.encrypted_secret.split_at(12);
            #[allow(deprecated)]
            let nonce = Nonce::from_slice(nb);
            cipher.decrypt(nonce, ct).ok()?
        };
        let secret_str = String::from_utf8_lossy(&plain).into_owned();
        plain.zeroize();
        let secret_bytes = Secret::Encoded(secret_str.clone()).to_bytes().ok()?;
        let totp = TOTP::new(
            Algorithm::SHA1,
            self.digits as usize,
            1,
            self.period as u64,
            secret_bytes,
            Some("Ferrite".to_string()),
            "user".to_string(),
        )
        .ok()?;
        totp.generate_current().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_code() {
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let s = TotpSecret::new("id", "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP", key);
        let code = s.current_code(key).unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }
}
