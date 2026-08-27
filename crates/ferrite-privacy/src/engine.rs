//! Privacy engine.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use tracing::debug;

use ferrite_proxy::{DecisionHook, RequestMeta, Verdict};

use crate::error::{PrivacyError, PrivacyResult};
use crate::filters::FilterEngine;
use crate::threat::ThreatDb;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrivacySettings {
    pub adblock_enabled: bool,
    pub gpc_enabled: bool,
    pub realtime_lookup_enabled: bool,
    pub https_upgrade_enabled: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            adblock_enabled: true,
            gpc_enabled: true,
            realtime_lookup_enabled: false,
            https_upgrade_enabled: true,
        }
    }
}

#[derive(Debug)]
pub struct PrivacyEngine {
    filters: Arc<RwLock<FilterEngine>>,
    threat_db: Arc<RwLock<ThreatDb>>,
    settings: Arc<RwLock<PrivacySettings>>,
    tracker_hosts: Arc<RwLock<HashSet<String>>>,
}

impl PrivacyEngine {
    pub fn new() -> PrivacyResult<Self> {
        let threat = ThreatDb::new_file("ferrite-threat.db")
            .or_else(|_| ThreatDb::new_in_memory())
            .map_err(|e| PrivacyError::ThreatDb(e.to_string()))?;
        Ok(Self {
            filters: Arc::new(RwLock::new(FilterEngine::new()?)),
            threat_db: Arc::new(RwLock::new(threat)),
            settings: Arc::new(RwLock::new(PrivacySettings::default())),
            tracker_hosts: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    pub fn with_threat_db(db: ThreatDb) -> PrivacyResult<Self> {
        Ok(Self {
            filters: Arc::new(RwLock::new(FilterEngine::new()?)),
            threat_db: Arc::new(RwLock::new(db)),
            settings: Arc::new(RwLock::new(PrivacySettings::default())),
            tracker_hosts: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    pub fn load_filters(
        &self,
        lists: &[(&str, &str)],
        expected_sha256: Option<&str>,
    ) -> PrivacyResult<()> {
        let mut engine = self
            .filters
            .write()
            .map_err(|e| PrivacyError::AdblockInit(format!("filter lock poisoned: {e}")))?;
        engine.load(lists, expected_sha256)
    }

    pub fn load_threat_snapshot(&self, urls: &[&str]) -> PrivacyResult<()> {
        let db = self
            .threat_db
            .write()
            .map_err(|e| PrivacyError::ThreatDb(format!("threat lock poisoned: {e}")))?;
        db.load_snapshot(urls).map_err(PrivacyError::ThreatDb)
    }

    pub fn update_settings(&self, f: impl FnOnce(&mut PrivacySettings)) {
        if let Ok(mut s) = self.settings.write() {
            f(&mut s);
        }
    }

    pub fn settings(&self) -> PrivacySettings {
        self.settings.read().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn should_send_gpc(&self) -> bool {
        self.settings.read().map(|s| s.gpc_enabled).unwrap_or(true)
    }

    pub fn should_strip_cookies(&self, request_domain: &str) -> bool {
        let hosts = self.tracker_hosts.read().ok();
        if let Some(set) = hosts {
            set.contains(&request_domain.to_lowercase())
        } else {
            false
        }
    }

    pub fn save_settings_to_file(&self, path: impl AsRef<std::path::Path>) {
        let settings = self.settings();
        if let Ok(json) = serde_json::to_string_pretty(&settings) {
            if let Err(e) = std::fs::write(path.as_ref(), json) {
                tracing::warn!(error = %e, "save settings failed");
            }
        }
    }

    pub fn load_settings_from_file(&self, path: impl AsRef<std::path::Path>) {
        if let Ok(data) = std::fs::read_to_string(path.as_ref()) {
            if let Ok(loaded) = serde_json::from_str::<PrivacySettings>(&data) {
                if let Ok(mut s) = self.settings.write() {
                    *s = loaded;
                }
            }
        }
    }

    pub fn spawn_hourly_updater(
        self: &std::sync::Arc<Self>,
        fetch_filter: impl Fn() -> Option<(String, Option<String>)> + Send + 'static,
        fetch_threat: impl Fn() -> Option<Vec<String>> + Send + 'static,
    ) {
        let engine = std::sync::Arc::clone(self);
        std::thread::Builder::new()
            .name("ferrite-privacy-updater".to_owned())
            .spawn(move || {
                let mut rng: u64 = 0x9E3779B97F4A7C15;
                loop {
                    // jitter 0..300s via xorshift
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    let jitter = rng % 300;
                    std::thread::sleep(std::time::Duration::from_secs(3600 + jitter));
                    // Filter update - best-effort, retain last_good on any failure.
                    if let Some((text, expected_sha)) = fetch_filter() {
                        let lists = [("easylist", text.as_str())];
                        if let Err(e) = engine.load_filters(&lists, expected_sha.as_deref()) {
                            tracing::warn!(error = %e, "hourly filter update failed - retaining last_known_good");
                        } else {
                            tracing::info!("hourly filter update applied");
                        }
                    }
                    // Threat snapshot update - same retain semantics via ThreatDb transaction.
                    if let Some(urls) = fetch_threat() {
                        let refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
                        if let Err(e) = engine.load_threat_snapshot(&refs) {
                            tracing::warn!(error = %e, "hourly threat update failed - retaining last_known_good");
                        } else {
                            tracing::info!(entries = urls.len(), "hourly threat update applied");
                        }
                    }
                }
            })
            .ok();
    }

    pub fn realtime_check(
        &self,
        domain: &str,
        is_top_level: bool,
        lookup: impl Fn(&str) -> Option<bool> + Send + 'static,
    ) -> Option<Verdict> {
        if !self.settings().realtime_lookup_enabled || !is_top_level {
            return None;
        }
        let prefix = crate::threat::hash_domain(domain)[..8].to_owned();
        // 300ms timeout via channel
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let res = lookup(&prefix);
            let _ = tx.send(res);
        });
        match rx.recv_timeout(std::time::Duration::from_millis(300)) {
            Ok(Some(true)) => Some(Verdict::BlockMalware),
            Ok(_) => None,
            Err(_) => {
                tracing::debug!(%domain, "realtime lookup timeout - fail-open");
                None
            }
        }
    }
}

impl Default for PrivacyEngine {
    fn default() -> Self {
        Self::new().expect("privacy engine default init")
    }
}

impl DecisionHook for PrivacyEngine {
    fn should_send_gpc(&self) -> bool {
        self.settings.read().map(|s| s.gpc_enabled).unwrap_or(true)
    }

    fn should_strip_cookies(&self, host: &str) -> bool {
        if let Ok(set) = self.tracker_hosts.read() {
            set.contains(&host.to_lowercase())
        } else {
            false
        }
    }

    fn should_upgrade_https(&self) -> bool {
        self.settings
            .read()
            .map(|s| s.https_upgrade_enabled)
            .unwrap_or(true)
    }

    fn decide(&self, meta: &RequestMeta) -> Verdict {
        if meta.is_mixed_content() {
            debug!(host = %meta.request_domain, "blocked mixed content");
            return Verdict::BlockMixedContent;
        }

        if let Ok(db) = self.threat_db.read() {
            if db.contains(&meta.request_domain) {
                debug!(host = %meta.request_domain, "blocked malware/phishing");
                return Verdict::BlockMalware;
            }
        }

        let is_top_level =
            meta.resource == ferrite_proxy::ResourceType::Document && meta.referrer.is_none();
        if self.settings().realtime_lookup_enabled && is_top_level {
            let prefix = crate::threat::hash_domain(&meta.request_domain)[..8].to_owned();
            let (tx, rx) = std::sync::mpsc::channel();
            let db_clone = std::sync::Arc::clone(&self.threat_db);
            std::thread::spawn(move || {
                let hit = db_clone
                    .read()
                    .map(|_db| {
                        let _ = &prefix;
                        false
                    })
                    .unwrap_or(false);
                let _ = tx.send(if hit { Some(true) } else { None });
            });
            if let Ok(Some(true)) = rx.recv_timeout(std::time::Duration::from_millis(300)) {
                debug!(host = %meta.request_domain, "blocked via realtime prefix");
                return Verdict::BlockMalware;
            }
        }

        let adblock_enabled = self
            .settings
            .read()
            .map(|s| s.adblock_enabled)
            .unwrap_or(true);
        if adblock_enabled {
            if let Ok(filters) = self.filters.read() {
                if let Some(v) = filters.check(meta) {
                    // Record tracker hosts for cookie stripping.
                    if v == Verdict::BlockTracker {
                        if let Ok(mut set) = self.tracker_hosts.write() {
                            set.insert(meta.request_domain.to_lowercase());
                        }
                    }
                    return v;
                }
            }
        }

        Verdict::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrite_proxy::ResourceType;

    fn engine_with_blocked(host: &str) -> PrivacyEngine {
        let e = PrivacyEngine::new().unwrap();
        e.load_filters(&[("test", &format!("||{host}^"))], None)
            .unwrap();
        e
    }

    #[test]
    fn allow_all_when_no_filters() {
        let e = PrivacyEngine::new().unwrap();
        let meta = RequestMeta::new("example.com", ResourceType::Document);
        assert_eq!(e.decide(&meta), Verdict::Allow);
    }

    #[test]
    fn blocks_ad_via_filter() {
        let e = engine_with_blocked("ads.example");
        let meta = RequestMeta::new("ads.example", ResourceType::Script);
        assert_eq!(e.decide(&meta), Verdict::BlockAd);
    }

    #[test]
    fn mixed_content_blocked() {
        let e = PrivacyEngine::new().unwrap();
        let mut meta = RequestMeta::new("http://cdn.com/img.png", ResourceType::Image);
        meta.is_secure = false;
        meta.referrer = Some("https://example.com".to_owned());
        assert!(meta.is_mixed_content());
        assert_eq!(e.decide(&meta), Verdict::BlockMixedContent);
    }

    #[test]
    fn threat_db_blocks_malware() {
        let e = PrivacyEngine::new().unwrap();
        e.load_threat_snapshot(&["phishing.test"]).unwrap();
        let meta = RequestMeta::new("phishing.test", ResourceType::Document);
        assert_eq!(e.decide(&meta), Verdict::BlockMalware);
    }

    #[test]
    fn gpc_toggle() {
        let e = PrivacyEngine::new().unwrap();
        assert!(e.should_send_gpc());
        e.update_settings(|s| s.gpc_enabled = false);
        assert!(!e.should_send_gpc());
    }

    #[test]
    fn realtime_default_off() {
        let e = PrivacyEngine::new().unwrap();
        assert!(!e.settings().realtime_lookup_enabled);
    }
}
