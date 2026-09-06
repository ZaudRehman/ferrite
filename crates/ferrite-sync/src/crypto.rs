//! Part 10 cryptography: Ed25519 identity + self-signed X.509 wrapping.

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("key generation failed: {0}")]
    KeyGen(String),
    #[error("cert generation failed: {0}")]
    Cert(String),
    #[error("pem error: {0}")]
    Pem(String),
}

pub type CryptoResult<T> = Result<T, CryptoError>;

/// Permanent device identity — Ed25519 keypair generated once on first launch.
#[derive(Debug)]
pub struct DeviceIdentity {
    pub device_id: uuid::Uuid,
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl DeviceIdentity {
    /// Generate new identity with fresh Ed25519 keypair via OsRng.
    pub fn generate() -> CryptoResult<Self> {
        let mut rng = OsRng;
        let signing_key = SigningKey::generate(&mut rng);
        let verifying_key = signing_key_verifying(&signing_key);
        Ok(Self {
            device_id: uuid::Uuid::new_v4(),
            signing_key,
            verifying_key,
        })
    }

    /// Generate with deterministic device_id (for tests / recovery).
    pub fn generate_with_id(device_id: uuid::Uuid) -> CryptoResult<Self> {
        let mut rng = OsRng;
        let signing_key = SigningKey::generate(&mut rng);
        let verifying_key = signing_key_verifying(&signing_key);
        Ok(Self {
            device_id,
            signing_key,
            verifying_key,
        })
    }

    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.verifying_key.as_bytes().to_vec()
    }

    pub fn fingerprint(&self) -> String {
        let hash = Sha256::digest(self.public_key_bytes());
        crate::crypto::hex::encode(&hash[..4])
    }

    pub fn public_key_pem(&self) -> CryptoResult<String> {
        let pkcs8 = self.verifying_key.to_bytes();
        // Encode raw 32B as PEM "PUBLIC KEY" for QR transport - use base64
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pkcs8);
        Ok(format!(
            "-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----"
        ))
    }

    /// Create ephemeral self-signed X.509 wrapping the permanent Ed25519 public key.
    /// Called at process startup, cert lives only in memory. The TLS keypair
    /// IS the device identity key (PKCS#8 of the signing seed), so the
    /// handshake signature provably binds the peer to the pinned verifying
    /// key. The verifying key is additionally embedded as custom extension
    /// OID 1.3.6.1.4.1.54392.1 so verifiers can extract it without PKCS#8
    /// re-encoding.
    pub fn self_signed_cert_pem(&self) -> CryptoResult<(String, String)> {
        let (cert_der, key_der) = self.self_signed_cert_der()?;
        let cert_pem = pem_encode("CERTIFICATE", &cert_der);
        let key_pem = pem_encode("PRIVATE KEY", &key_der);
        Ok((cert_pem, key_pem))
    }

    /// DER form of the bound identity certificate + PKCS#8 private key.
    /// This is the form rustls consumes; PEM above is derived from it.
    pub fn self_signed_cert_der(&self) -> CryptoResult<(Vec<u8>, Vec<u8>)> {
        let pkcs8 = ed25519_pkcs8_der(&self.signing_key);
        let key_pair = rcgen::KeyPair::from_der_and_sign_algo(
            &rustls_pki_types::PrivateKeyDer::Pkcs8(pkcs8.clone().into()),
            &rcgen::PKCS_ED25519,
        )
        .map_err(|e| CryptoError::Cert(e.to_string()))?;
        let mut params = rcgen::CertificateParams::new(vec![self.device_id.to_string()])
            .map_err(|e| CryptoError::Cert(e.to_string()))?;
        // Embed permanent Ed25519 verifying key (32B) as custom extension for pinned verification
        let ext_content = self.verifying_key.as_bytes().to_vec();
        let custom_ext =
            rcgen::CustomExtension::from_oid_content(&[1, 3, 6, 1, 4, 1, 54392, 1], ext_content);
        params.custom_extensions.push(custom_ext);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, self.device_id.to_string());
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| CryptoError::Cert(e.to_string()))?;
        Ok((cert.der().to_vec(), pkcs8))
    }
}

/// PKCS#8 DER encoding of an Ed25519 seed key (RFC 8410 §7:
/// `SEQUENCE { INTEGER 0, SEQUENCE { OID 1.3.101.112 }, OCTET STRING seed }`).
/// Fixed 16-byte header + 32-byte seed; no external pkcs8 dependency needed.
fn ed25519_pkcs8_der(signing_key: &SigningKey) -> Vec<u8> {
    const HEADER: &[u8; 16] = &[
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];
    let mut out = Vec::with_capacity(48);
    out.extend_from_slice(HEADER);
    out.extend_from_slice(&signing_key.to_bytes());
    out
}

fn pem_encode(label: &str, der: &[u8]) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

fn signing_key_verifying(sk: &SigningKey) -> VerifyingKey {
    sk.verifying_key()
}

// Minimal hex helper without extra dep
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generate_unique() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        assert_ne!(a.public_key_bytes(), b.public_key_bytes());
        assert_ne!(a.device_id, b.device_id);
        assert_eq!(a.fingerprint().len(), 8);
    }

    #[test]
    fn cert_generation() {
        let id = DeviceIdentity::generate().unwrap();
        let (cert, key) = id.self_signed_cert_pem().unwrap();
        assert!(cert.contains("BEGIN CERTIFICATE"));
        assert!(key.contains("PRIVATE KEY"));
    }

    #[test]
    fn ed25519_bound() {
        use x509_parser::prelude::*;
        // The TLS private key in the cert MUST be the identity key: parse
        // the emitted key PEM back and compare against the signing seed.
        let id = DeviceIdentity::generate().unwrap();
        let (cert_der, key_der) = id.self_signed_cert_der().unwrap();
        assert_eq!(&key_der[16..], &id.signing_key.to_bytes());
        // The custom extension MUST carry the verifying key.
        let (_, cert) = X509Certificate::from_der(&cert_der).unwrap();
        let mut found = false;
        for ext in cert.extensions() {
            if ext.oid.to_string() == "1.3.6.1.4.1.54392.1" {
                let v = ext.value;
                let key_bytes = if v.len() == 32 {
                    v.to_vec()
                } else if v.len() >= 34 && v[0] == 0x04 && v[1] == 0x20 {
                    v[2..34].to_vec()
                } else {
                    v.to_vec()
                };
                assert_eq!(&key_bytes, &id.public_key_bytes());
                found = true;
            }
        }
        assert!(found, "identity extension missing");
        // CN must be the device_id for SNI/device binding.
        let cn = cert
            .subject()
            .iter_common_name()
            .next()
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(cn, id.device_id.to_string());
    }
}
