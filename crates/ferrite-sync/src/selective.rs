//! Selective sync per 3.4 - per-device category enable set, TrustedDevices mandatory.

use std::collections::HashSet;

use crate::sync_actions::SyncCategory;

#[derive(Debug, Clone)]
pub struct SyncSettings {
    enabled: HashSet<SyncCategory>,
    pub pinned_eager: bool,
}

impl Default for SyncSettings {
    fn default() -> Self {
        let mut s = HashSet::new();
        for cat in SyncCategory::all() {
            s.insert(*cat);
        }
        Self {
            enabled: s,
            pinned_eager: true,
        }
    }
}

impl SyncSettings {
    pub fn all_enabled() -> Self {
        Self::default()
    }

    pub fn disabled(mut self, cat: SyncCategory) -> Self {
        if cat == SyncCategory::TrustedDevices {
            return self;
        }
        self.enabled.remove(&cat);
        self
    }

    pub fn with_pinned_eager(mut self, eager: bool) -> Self {
        self.pinned_eager = eager;
        self
    }

    pub fn is_enabled(&self, cat: SyncCategory) -> bool {
        if cat == SyncCategory::TrustedDevices {
            return true;
        }
        self.enabled.contains(&cat)
    }

    pub fn enabled_set(&self) -> HashSet<SyncCategory> {
        self.enabled.clone()
    }

    pub fn set_enabled(&mut self, cat: SyncCategory, enabled: bool) {
        if cat == SyncCategory::TrustedDevices {
            return;
        }
        if enabled {
            self.enabled.insert(cat);
        } else {
            self.enabled.remove(&cat);
        }
    }

    pub fn save_to_file(&self, path: &str) {
        let ids: Vec<String> = self.enabled.iter().map(|c| format!("{c:?}")).collect();
        let doc = serde_json::json!({"enabled": ids, "pinned_eager": self.pinned_eager});
        if let Ok(json) = serde_json::to_string(&doc) {
            // Atomic write; failures only log (settings must never crash sync).
            let tmp = format!("{path}.tmp");
            if std::fs::write(&tmp, json).is_ok()
                && let Err(e) = std::fs::rename(&tmp, path)
            {
                tracing::warn!(error = %e, "sync settings persist failed");
            }
        }
    }

    pub fn load_from_file(path: &str) -> Self {
        // Accepts both the current {"enabled","pinned_eager"} document and
        // legacy bare-array files.
        if let Ok(data) = std::fs::read_to_string(path)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&data)
        {
            let (ids, pinned_eager) = if let Some(arr) = v.as_array() {
                (arr.clone(), true)
            } else {
                let ids = v
                    .get("enabled")
                    .and_then(|e| e.as_array())
                    .cloned()
                    .unwrap_or_default();
                let eager = v
                    .get("pinned_eager")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(true);
                (ids, eager)
            };
            let mut set = HashSet::new();
            for id in ids {
                if let Some(s) = id.as_str()
                    && let Some(cat) = parse_category(s)
                {
                    set.insert(cat);
                }
            }
            // Always ensure TrustedDevices
            set.insert(SyncCategory::TrustedDevices);
            return Self {
                enabled: set,
                pinned_eager,
            };
        }
        Self::default()
    }
}

fn parse_category(s: &str) -> Option<SyncCategory> {
    match s {
        "Bookmarks" => Some(SyncCategory::Bookmarks),
        "History" => Some(SyncCategory::History),
        "Credentials" => Some(SyncCategory::Credentials),
        "Autofill" => Some(SyncCategory::Autofill),
        "Totp" => Some(SyncCategory::Totp),
        "Archive" => Some(SyncCategory::Archive),
        "TrustedDevices" => Some(SyncCategory::TrustedDevices),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mandatory_trusted() {
        let mut s = SyncSettings::default();
        s.set_enabled(SyncCategory::TrustedDevices, false);
        assert!(s.is_enabled(SyncCategory::TrustedDevices));
    }
    #[test]
    fn selective_disable() {
        let mut s = SyncSettings::default();
        s.set_enabled(SyncCategory::Credentials, false);
        assert!(!s.is_enabled(SyncCategory::Credentials));
        assert!(s.is_enabled(SyncCategory::Bookmarks));
    }

    #[test]
    fn save_load_roundtrips_pinned_eager() {
        let dir = std::env::temp_dir().join(format!(
            "ferrite-sel-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let ps = path.to_string_lossy().to_string();
        let s = SyncSettings::default()
            .disabled(SyncCategory::History)
            .with_pinned_eager(false);
        s.save_to_file(&ps);
        let back = SyncSettings::load_from_file(&ps);
        assert!(!back.is_enabled(SyncCategory::History));
        assert!(back.is_enabled(SyncCategory::Bookmarks));
        assert!(!back.pinned_eager, "opt-out must survive restart");
        assert!(back.is_enabled(SyncCategory::TrustedDevices));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_legacy_array_format() {
        let dir = std::env::temp_dir().join(format!(
            "ferrite-sel-legacy-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"["Bookmarks","History"]"#).unwrap();
        let back = SyncSettings::load_from_file(path.to_str().unwrap());
        assert!(back.is_enabled(SyncCategory::Bookmarks));
        assert!(!back.is_enabled(SyncCategory::Credentials));
        assert!(back.pinned_eager);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
