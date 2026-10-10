//! QR pairing, verification flow, rate limits, and mesh propagation.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::trusted::{TrustedDevice, TrustedStore};

/// QR pairing payload, versioned. v0 is the legacy shape (no version,
/// no timestamp: accepted without expiry). v1 stamps both: receivers
/// reject future versions outright (a v2 code must never misparse as
/// v1) and expired v1 codes at the door. The version only ever gates
/// *freshness*, never trust: approval still binds the key.
pub const PAIRING_VERSION: u32 = 1;
/// A shown code stays usable long enough to walk across a room and scan
/// it, then dies. Stale screenshots and shoulder-surfed copies rot fast.
pub const PAIRING_CODE_TTL_MS: i64 = 15 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingPayload {
    pub ip: String,
    pub port: u16,
    pub device_id: String,
    pub public_key: Vec<u8>,
    pub device_name: String,
    /// 0 when absent (legacy codes predate the field).
    #[serde(default)]
    pub version: u32,
    /// Milliseconds since epoch at show time (v1 only).
    #[serde(default)]
    pub created_at_ms: Option<i64>,
}

impl PairingPayload {
    pub fn encode(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        BASE64.encode(json.as_bytes())
    }

    pub fn decode(s: &str) -> Option<Self> {
        let bytes = BASE64.decode(s.trim()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Freshness gate for an approve/deny decision at `now_ms`. v0 has
    /// no timestamp and stays accepted (legacy interop); v1 must carry
    /// a timestamp within TTL (clock skew included both ways); anything
    /// from the future beyond skew, or any version we don't know, is
    /// refused; a newer client must upgrade this side, not seamlessly
    /// pair with semantics it can't verify.
    pub fn is_fresh(&self, now_ms: i64) -> bool {
        match self.version {
            0 => true,
            PAIRING_VERSION => match self.created_at_ms {
                Some(t) => t <= now_ms + 60_000 && now_ms - t <= PAIRING_CODE_TTL_MS,
                None => false,
            },
            _ => false,
        }
    }
}

/// One pending request per IP per window.
pub struct PairingRateLimiter {
    window_ms: i64,
    last: Arc<Mutex<HashMap<IpAddr, i64>>>,
}

impl PairingRateLimiter {
    pub fn new(window_ms: i64) -> Self {
        Self {
            window_ms,
            last: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn check(&self, ip: IpAddr) -> bool {
        let now = now_millis();
        if let Ok(mut m) = self.last.lock() {
            if let Some(prev) = m.get(&ip)
                && now.wrapping_sub(*prev) < self.window_ms
            {
                // No address in the log line: pairing attempts are sensitive
                // (a LAN IP identifies a device), counters suffice here.
                warn!("pairing rate limited");
                return false;
            }
            m.insert(ip, now);
        }
        true
    }
}

impl Default for PairingRateLimiter {
    fn default() -> Self {
        Self::new(10_000)
    }
}

pub struct PairingService {
    store: TrustedStore,
    limiter: PairingRateLimiter,
    strict_mode: bool,
}

impl PairingService {
    pub fn new(store: TrustedStore) -> Self {
        Self {
            store,
            limiter: PairingRateLimiter::default(),
            strict_mode: false,
        }
    }

    pub fn set_strict_mode(&mut self, strict: bool) {
        self.strict_mode = strict;
    }

    /// Generate QR payload for host device (4.1)
    pub fn generate_payload(
        &self,
        ip: IpAddr,
        port: u16,
        device_id: String,
        public_key: Vec<u8>,
        device_name: String,
    ) -> PairingPayload {
        PairingPayload {
            ip: ip.to_string(),
            port,
            device_id,
            public_key,
            device_name,
            version: PAIRING_VERSION,
            created_at_ms: Some(now_millis()),
        }
    }

    /// Handle incoming pairing request from scanning device (4.2 steps 5-7).
    /// Returns true if approved and stored. Caller shows a UI prompt first.
    pub fn handle_pairing_request(
        &self,
        payload: &PairingPayload,
        remote_ip: IpAddr,
        remote_device_id: String,
        remote_public_key: Vec<u8>,
        approved: bool,
    ) -> bool {
        if !self.limiter.check(remote_ip) {
            return false;
        }
        if !approved {
            debug!("pairing denied by user");
            return false;
        }
        let now = now_millis();
        let dev = TrustedDevice {
            device_id: remote_device_id.clone(),
            device_name: payload.device_name.clone(),
            public_key: remote_public_key.clone(),
            paired_via_device_id: None,
            first_paired_at: now,
            last_seen_at: Some(now),
            last_synced_at: None,
            hlc_timestamp: now,
            is_revoked: false,
        };
        if self.store.add_trusted(&dev).is_ok() {
            debug!(%remote_device_id, "pairing approved and pinned");
            true
        } else {
            false
        }
    }

    /// Apply mesh-propagated trust entry (4.3). In strict mode requires explicit approval.
    pub fn apply_propagated(&self, dev: TrustedDevice, explicit_approval: bool) -> bool {
        if self.strict_mode && !explicit_approval {
            debug!(device_id=%dev.device_id, "strict mode: propagated trust requires explicit approval");
            return false;
        }
        self.store.add_trusted(&dev).is_ok()
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::{Arc, Mutex};

    fn test_store() -> TrustedStore {
        let conn = Connection::open_in_memory().unwrap();
        let s = crate::trusted::TrustedStore::new(Arc::new(Mutex::new(conn)));
        s.init_schema().unwrap();
        s
    }

    #[test]
    fn payload_roundtrip() {
        let p = PairingPayload {
            ip: "192.168.1.10".to_owned(),
            port: 8443,
            device_id: "dev-123".to_owned(),
            public_key: vec![1, 2, 3, 4],
            device_name: "Desktop".to_owned(),
            version: PAIRING_VERSION,
            created_at_ms: Some(1_700_000_000_000),
        };
        let enc = p.encode();
        let dec = PairingPayload::decode(&enc).unwrap();
        assert_eq!(dec.device_id, "dev-123");
        assert_eq!(dec.port, 8443);
        assert_eq!(dec.version, PAIRING_VERSION);
    }

    #[test]
    fn payload_version_and_expiry() {
        let now = 1_700_000_000_000i64;
        // Legacy v0 (no fields on the wire): accepted, no expiry.
        let legacy = PairingPayload {
            ip: "192.168.1.10".to_owned(),
            port: 8443,
            device_id: "dev-123".to_owned(),
            public_key: vec![1, 2, 3, 4],
            device_name: "Desktop".to_owned(),
            version: 0,
            created_at_ms: None,
        };
        let back = PairingPayload::decode(&legacy.encode()).expect("legacy decodes");
        assert_eq!(back.version, 0);
        assert!(back.is_fresh(now));
        assert!(back.is_fresh(now + 365 * 24 * 60 * 60 * 1000));
        // Fresh v1 round-trips with stamp.
        let fresh = PairingPayload {
            version: PAIRING_VERSION,
            created_at_ms: Some(now),
            ..legacy.clone()
        };
        let back = PairingPayload::decode(&fresh.encode()).expect("v1 decodes");
        assert!(back.is_fresh(now));
        assert!(back.is_fresh(now + PAIRING_CODE_TTL_MS));
        // Expired, future (beyond skew), missing stamp, future version.
        let old = PairingPayload {
            version: PAIRING_VERSION,
            created_at_ms: Some(now - PAIRING_CODE_TTL_MS - 1),
            ..legacy.clone()
        };
        assert!(!old.is_fresh(now));
        let future = PairingPayload {
            version: PAIRING_VERSION,
            created_at_ms: Some(now + 60_001),
            ..legacy.clone()
        };
        assert!(!future.is_fresh(now));
        let nostamp = PairingPayload {
            version: PAIRING_VERSION,
            created_at_ms: None,
            ..legacy.clone()
        };
        assert!(!nostamp.is_fresh(now));
        let v2 = PairingPayload {
            version: PAIRING_VERSION + 1,
            created_at_ms: Some(now),
            ..legacy.clone()
        };
        assert!(!v2.is_fresh(now));
    }

    #[test]
    fn rate_limiting() {
        let s = test_store();
        let svc = PairingService::new(s);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let pl = PairingPayload {
            ip: ip.to_string(),
            port: 8443,
            device_id: "host".to_owned(),
            public_key: vec![9],
            device_name: "Host".to_owned(),
            version: 0,
            created_at_ms: None,
        };
        assert!(svc.handle_pairing_request(&pl, ip, "dev-b".to_owned(), vec![1, 2], true));
        // Immediate second from same IP should be rate limited
        assert!(!svc.handle_pairing_request(&pl, ip, "dev-c".to_owned(), vec![3, 4], true));
    }

    #[test]
    fn strict_mode_blocks_propagated() {
        let s = test_store();
        let mut svc = PairingService::new(s);
        svc.set_strict_mode(true);
        let dev = TrustedDevice {
            device_id: "dev-c".to_owned(),
            device_name: "Tablet".to_owned(),
            public_key: vec![5, 6],
            paired_via_device_id: Some("dev-b".to_owned()),
            first_paired_at: 100,
            last_seen_at: None,
            last_synced_at: None,
            hlc_timestamp: 100,
            is_revoked: false,
        };
        assert!(!svc.apply_propagated(dev.clone(), false));
        assert!(svc.apply_propagated(dev, true));
    }
}
