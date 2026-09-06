//! LWW CRDT via HLC timestamps already in vault tables.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CrdtError {
    #[error("merge conflict: {0}")]
    Conflict(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncRecord {
    pub id: String,
    pub value: serde_json::Value,
    pub hlc: i64,
    pub deleted: bool,
    /// Origin device id. Decides totally-ordered tie-breaks on equal HLC
    /// (lexicographic). Defaults empty for records written before P2.
    #[serde(default)]
    pub device: String,
}

#[derive(Debug, Default)]
pub struct LwwMap {
    inner: std::collections::HashMap<String, SyncRecord>,
}

impl LwwMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, rec: SyncRecord) {
        if let Some(entry) = self.inner.get(&rec.id) {
            if rec.hlc < entry.hlc {
                return;
            }
            // Equal HLC: higher device id wins (total order, all replicas
            // agree). The old code compared serialized VALUES, which is
            // format-dependent and attacker-influenceable.
            if rec.hlc == entry.hlc && rec.device <= entry.device {
                return;
            }
        }
        self.inner.insert(rec.id.clone(), rec);
    }

    pub fn merge(&mut self, other: &LwwMap) {
        for rec in other.inner.values() {
            self.insert(rec.clone());
        }
    }

    pub fn to_vec(&self) -> Vec<SyncRecord> {
        self.inner.values().cloned().collect()
    }

    pub fn diff_since(&self, since_hlc: i64) -> Vec<SyncRecord> {
        self.inner
            .values()
            .filter(|r| r.hlc > since_hlc)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lww_last_write_wins() {
        let mut m = LwwMap::new();
        m.insert(SyncRecord {
            id: "a".into(),
            value: json!("v1"),
            hlc: 100,
            deleted: false,
            device: "dev-a".into(),
        });
        m.insert(SyncRecord {
            id: "a".into(),
            value: json!("v2"),
            hlc: 200,
            deleted: false,
            device: "dev-a".into(),
        });
        assert_eq!(m.inner["a"].value, json!("v2"));
    }

    #[test]
    fn lww_tie_breaks_on_device_id() {
        let mut m = LwwMap::new();
        m.insert(SyncRecord {
            id: "a".into(),
            value: json!("from-b"),
            hlc: 100,
            deleted: false,
            device: "dev-b".into(),
        });
        // Same HLC, lexicographically smaller device loses even with a
        // "larger" value payload.
        m.insert(SyncRecord {
            id: "a".into(),
            value: json!("zzzz-from-a"),
            hlc: 100,
            deleted: false,
            device: "dev-a".into(),
        });
        assert_eq!(m.inner["a"].value, json!("from-b"));
        // Higher device wins.
        m.insert(SyncRecord {
            id: "a".into(),
            value: json!("from-c"),
            hlc: 100,
            deleted: false,
            device: "dev-c".into(),
        });
        assert_eq!(m.inner["a"].value, json!("from-c"));
    }

    #[test]
    fn merge_keeps_newer() {
        let mut a = LwwMap::new();
        a.insert(SyncRecord {
            id: "x".into(),
            value: json!(1),
            hlc: 10,
            deleted: false,
            device: "a".into(),
        });
        let mut b = LwwMap::new();
        b.insert(SyncRecord {
            id: "x".into(),
            value: json!(2),
            hlc: 20,
            deleted: false,
            device: "b".into(),
        });
        a.merge(&b);
        assert_eq!(a.inner["x"].value, json!(2));
    }

    #[test]
    fn diff_since() {
        let mut m = LwwMap::new();
        m.insert(SyncRecord {
            id: "a".into(),
            value: json!(1),
            hlc: 5,
            deleted: false,
            device: String::new(),
        });
        m.insert(SyncRecord {
            id: "b".into(),
            value: json!(2),
            hlc: 15,
            deleted: false,
            device: String::new(),
        });
        assert_eq!(m.diff_since(10).len(), 1);
    }
}
