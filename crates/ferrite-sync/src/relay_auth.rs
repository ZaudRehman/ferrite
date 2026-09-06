//! Relay session-auth protocol (must match `relay/main.go`).
//!
//! `auth` payload = `{"pubkey": base64(32B ed25519), "ts": unix_sec,
//! "sig": base64(64B ed25519 over "ferrite-relay-auth-v1|<device_id>|<ts>")}`.
//! The relay pins device_id -> pubkey and rejects spoofed `from`.

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::crypto::DeviceIdentity;

pub const AUTH_DOMAIN: &str = "ferrite-relay-auth-v1";
pub const MAX_AUTH_SKEW_SECS: i64 = 60;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct RelayAuthPayload {
    pub pubkey: String,
    pub ts: i64,
    pub sig: String,
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// Build the signed auth claim for `identity` at timestamp `now_secs`.
pub fn build_auth(identity: &DeviceIdentity, device_id: &str, now_secs: i64) -> RelayAuthPayload {
    // SigningKey has no Clone bound issue here: sign borrows.
    let key = SigningKey::from_bytes(&identity.signing_key.to_bytes());
    let msg = format!("{AUTH_DOMAIN}|{device_id}|{now_secs}");
    let sig: Signature = key.sign(msg.as_bytes());
    RelayAuthPayload {
        pubkey: b64(&identity.public_key_bytes()),
        ts: now_secs,
        sig: b64(&sig.to_bytes()),
    }
}

/// Verify an auth claim for `device_id` at `now_secs`. Returns the verified
/// public key. Mirrors the relay's `verifyAuth` exactly.
pub fn verify_auth(
    device_id: &str,
    payload: &RelayAuthPayload,
    now_secs: i64,
) -> Option<VerifyingKey> {
    if device_id.is_empty() {
        return None;
    }
    let pub_raw = unb64(&payload.pubkey)?;
    if pub_raw.len() != 32 {
        return None;
    }
    let sig_raw = unb64(&payload.sig)?;
    if sig_raw.len() != 64 {
        return None;
    }
    if payload.ts < now_secs - MAX_AUTH_SKEW_SECS || payload.ts > now_secs + MAX_AUTH_SKEW_SECS {
        return None;
    }
    let vk_bytes: [u8; 32] = pub_raw.as_slice().try_into().ok()?;
    let vk = VerifyingKey::from_bytes(&vk_bytes).ok()?;
    let sig_bytes: [u8; 64] = sig_raw.as_slice().try_into().ok()?;
    let msg = format!("{AUTH_DOMAIN}|{device_id}|{}", payload.ts);
    vk.verify_strict(msg.as_bytes(), &Signature::from_bytes(&sig_bytes))
        .ok()?;
    Some(vk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_accepts() {
        let id = DeviceIdentity::generate().unwrap();
        let name = id.device_id.to_string();
        let p = build_auth(&id, &name, 1_700_000_000);
        let vk = verify_auth(&name, &p, 1_700_000_000).expect("valid claim");
        assert_eq!(vk.as_bytes(), id.verifying_key.as_bytes());
    }

    #[test]
    fn rejects_wrong_device_stale_and_malformed() {
        let id = DeviceIdentity::generate().unwrap();
        let name = id.device_id.to_string();
        let p = build_auth(&id, &name, 1_700_000_000);
        assert!(verify_auth("other", &p, 1_700_000_000).is_none());
        assert!(verify_auth(&name, &p, 1_700_000_000 + MAX_AUTH_SKEW_SECS + 1).is_none());
        assert!(verify_auth(&name, &p, 1_700_000_000 - MAX_AUTH_SKEW_SECS - 1).is_none());
        let mut bad = p.clone();
        bad.pubkey = "!!!".to_owned();
        assert!(verify_auth(&name, &bad, 1_700_000_000).is_none());
        assert!(verify_auth("", &p, 1_700_000_000).is_none());
    }

    #[test]
    fn cross_key_claim_rejected() {
        // Signature from key A presented with key B: must fail.
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let name = a.device_id.to_string();
        let mut p = build_auth(&a, &name, 1_700_000_000);
        p.pubkey = b64(&b.public_key_bytes());
        assert!(verify_auth(&name, &p, 1_700_000_000).is_none());
    }
}
