//! Trusted device table plus mesh propagation over CRDT.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::crdt::{LwwMap, SyncRecord};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustedDevice {
    pub device_id: String,
    pub device_name: String,
    pub public_key: Vec<u8>,
    pub paired_via_device_id: Option<String>,
    pub first_paired_at: i64,
    pub last_seen_at: Option<i64>,
    pub last_synced_at: Option<i64>,
    pub hlc_timestamp: i64,
    pub is_revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SyncAction {
    AddTrustedDevice(TrustedDevice),
    RevokeTrustedDevice { device_id: String, hlc: i64 },
}

impl TrustedDevice {
    pub fn to_sync_record(&self) -> SyncRecord {
        SyncRecord {
            id: format!("trusted:{}", self.device_id),
            value: serde_json::to_value(self).unwrap_or(serde_json::Value::Null),
            hlc: self.hlc_timestamp,
            deleted: self.is_revoked,
            device: String::new(),
        }
    }

    pub fn from_sync_record(rec: &SyncRecord) -> Option<Self> {
        if !rec.id.starts_with("trusted:") {
            return None;
        }
        serde_json::from_value(rec.value.clone()).ok()
    }
}

#[derive(Debug)]
pub struct TrustedStore {
    conn: Arc<Mutex<Connection>>,
}

impl TrustedStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    pub fn init_schema(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS trusted_devices (
                device_id TEXT PRIMARY KEY,
                device_name TEXT NOT NULL,
                public_key BLOB NOT NULL,
                paired_via_device_id TEXT,
                first_paired_at INTEGER NOT NULL,
                last_seen_at INTEGER,
                last_synced_at INTEGER,
                hlc_timestamp INTEGER NOT NULL,
                is_revoked INTEGER DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_trusted_hlc ON trusted_devices(hlc_timestamp);
            "#,
        )
        .map_err(|e| format!("schema: {e}"))?;
        Ok(())
    }

    pub fn add_trusted(&self, dev: &TrustedDevice) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        conn.execute(
            r#"INSERT OR REPLACE INTO trusted_devices
            (device_id, device_name, public_key, paired_via_device_id, first_paired_at, last_seen_at, last_synced_at, hlc_timestamp, is_revoked)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"#,
            rusqlite::params![
                dev.device_id,
                dev.device_name,
                dev.public_key,
                dev.paired_via_device_id,
                dev.first_paired_at,
                dev.last_seen_at,
                dev.last_synced_at,
                dev.hlc_timestamp,
                if dev.is_revoked { 1 } else { 0 }
            ],
        )
        .map_err(|e| format!("insert: {e}"))?;
        info!(device_id=%dev.device_id, "trusted device added");
        Ok(())
    }

    pub fn revoke(&self, device_id: &str, hlc: i64) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        conn.execute(
            "UPDATE trusted_devices SET is_revoked=1, hlc_timestamp=?1 WHERE device_id=?2",
            rusqlite::params![hlc, device_id],
        )
        .map_err(|e| format!("revoke: {e}"))?;
        info!(%device_id, "device revoked");
        Ok(())
    }

    pub fn is_trusted(&self, device_id: &str, public_key: &[u8]) -> bool {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return false,
        };
        let row: Option<(Vec<u8>, i64)> = conn
            .query_row(
                "SELECT public_key, is_revoked FROM trusted_devices WHERE device_id=?1",
                [device_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap_or(None);
        match row {
            Some((pk, revoked)) => revoked == 0 && pk == public_key,
            None => false,
        }
    }

    pub fn all(&self) -> Vec<TrustedDevice> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut stmt = match conn.prepare(
            "SELECT device_id, device_name, public_key, paired_via_device_id, first_paired_at, last_seen_at, last_synced_at, hlc_timestamp, is_revoked FROM trusted_devices",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| {
            Ok(TrustedDevice {
                device_id: r.get(0)?,
                device_name: r.get(1)?,
                public_key: r.get(2)?,
                paired_via_device_id: r.get(3)?,
                first_paired_at: r.get(4)?,
                last_seen_at: r.get(5)?,
                last_synced_at: r.get(6)?,
                hlc_timestamp: r.get(7)?,
                is_revoked: r.get::<_, i64>(8)? != 0,
            })
        });
        rows.map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Build LWW map for propagation, each device as one SyncRecord.
    pub fn to_lww(&self) -> LwwMap {
        let mut map = LwwMap::new();
        for dev in self.all() {
            map.insert(dev.to_sync_record());
        }
        map
    }

    /// Apply incoming LWW delta from peer sync (4.3 passive join)
    pub fn apply_lww_delta(&self, map: &LwwMap) -> Vec<String> {
        let mut notifications = Vec::new();
        for rec in map.to_vec() {
            if let Some(dev) = TrustedDevice::from_sync_record(&rec) {
                let exists = self.all().iter().any(|d| d.device_id == dev.device_id);
                let _ = self.add_trusted(&dev);
                if !exists && !dev.is_revoked {
                    let msg = format!(
                        "{} joined your sync mesh via {} on {}",
                        dev.device_name,
                        dev.paired_via_device_id
                            .clone()
                            .unwrap_or_else(|| "direct pairing".to_owned()),
                        dev.first_paired_at
                    );
                    debug!(%msg, "passive join notification");
                    notifications.push(msg);
                }
            }
        }
        notifications
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn store() -> TrustedStore {
        let conn = Connection::open_in_memory().unwrap();
        let s = TrustedStore::new(Arc::new(Mutex::new(conn)));
        s.init_schema().unwrap();
        s
    }

    #[test]
    fn add_and_is_trusted() {
        let s = store();
        let dev = TrustedDevice {
            device_id: "dev-a".to_owned(),
            device_name: "MacBook".to_owned(),
            public_key: vec![1, 2, 3],
            paired_via_device_id: None,
            first_paired_at: 100,
            last_seen_at: None,
            last_synced_at: None,
            hlc_timestamp: 100,
            is_revoked: false,
        };
        s.add_trusted(&dev).unwrap();
        assert!(s.is_trusted("dev-a", &[1, 2, 3]));
        assert!(!s.is_trusted("dev-a", &[9, 9, 9]));
    }

    #[test]
    fn revoke_propagation() {
        let s = store();
        let dev = TrustedDevice {
            device_id: "dev-b".to_owned(),
            device_name: "Phone".to_owned(),
            public_key: vec![4, 5],
            paired_via_device_id: Some("dev-a".to_owned()),
            first_paired_at: 200,
            last_seen_at: None,
            last_synced_at: None,
            hlc_timestamp: 200,
            is_revoked: false,
        };
        s.add_trusted(&dev).unwrap();
        s.revoke("dev-b", 300).unwrap();
        assert!(!s.is_trusted("dev-b", &[4, 5]));
        let all = s.all();
        assert!(
            all.iter()
                .find(|d| d.device_id == "dev-b")
                .unwrap()
                .is_revoked
        );
    }

    /// Lost-device loop closed: revoke cuts trust, and pairing the same
    /// device again (fresh ceremony, new approval) restores it. Revocation
    /// is not a tombstone on the identity; it is a state the next
    /// ceremony overwrites, so "lost phone, new phone, same laptop"
    /// always has a way back that does not involve the old key.
    #[test]
    fn revoke_then_repair_restores_trust() {
        let s = store();
        let dev = TrustedDevice {
            device_id: "dev-phone".to_owned(),
            device_name: "Phone".to_owned(),
            public_key: vec![4, 5],
            paired_via_device_id: None,
            first_paired_at: 200,
            last_seen_at: None,
            last_synced_at: None,
            hlc_timestamp: 200,
            is_revoked: false,
        };
        s.add_trusted(&dev).unwrap();
        assert!(s.is_trusted("dev-phone", &[4, 5]));
        s.revoke("dev-phone", 300).unwrap();
        assert!(!s.is_trusted("dev-phone", &[4, 5]));
        // Fresh ceremony for the same device id (new approval, new row):
        // trust is whole again, old revocation does not haunt the id.
        let mut repaired = dev.clone();
        repaired.hlc_timestamp = 400;
        repaired.is_revoked = false;
        s.add_trusted(&repaired).unwrap();
        assert!(s.is_trusted("dev-phone", &[4, 5]));
        // ...while a different key on the same id still fails: repair is
        // approval-shaped, never takeover-shaped.
        assert!(!s.is_trusted("dev-phone", &[9, 9]));
    }

    #[test]
    fn lww_passive_join() {
        let a = store();
        let b = store();
        let dev_c = TrustedDevice {
            device_id: "dev-c".to_owned(),
            device_name: "Tablet".to_owned(),
            public_key: vec![7, 8],
            paired_via_device_id: Some("dev-b".to_owned()),
            first_paired_at: 400,
            last_seen_at: None,
            last_synced_at: None,
            hlc_timestamp: 400,
            is_revoked: false,
        };
        b.add_trusted(&dev_c).unwrap();
        let lww = b.to_lww();
        let notes = a.apply_lww_delta(&lww);
        assert_eq!(notes.len(), 1);
        assert!(a.is_trusted("dev-c", &[7, 8]));
    }
}
