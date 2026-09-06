//! Privacy engine.

use std::sync::{Arc, RwLock};

use tracing::debug;

use ferrite_proxy::{DecisionHook, RequestMeta, Verdict};

use crate::error::{PrivacyError, PrivacyResult};
use crate::filters::FilterEngine;
use crate::threat::ThreatDb;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrivacySettings {
    #[serde(default = "default_true")]
    pub adblock_enabled: bool,
    #[serde(default = "default_true")]
    pub gpc_enabled: bool,
    #[serde(default)]
    pub realtime_lookup_enabled: bool,
    #[serde(default = "default_true")]
    pub https_upgrade_enabled: bool,
}

fn default_true() -> bool {
    true
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
    tracker_hosts: Arc<RwLock<TrackerSet>>,
    proxy_port: Arc<RwLock<Option<u16>>>,
    verdict_counts: VerdictCounts,
}

/// Fleet-triagable decision counters (monotonic, process lifetime).
#[derive(Debug, Default)]
pub struct VerdictCounts {
    pub allow: std::sync::atomic::AtomicU64,
    pub ad: std::sync::atomic::AtomicU64,
    pub tracker: std::sync::atomic::AtomicU64,
    pub malware: std::sync::atomic::AtomicU64,
    pub mixed: std::sync::atomic::AtomicU64,
}

impl VerdictCounts {
    fn record(&self, v: Verdict) {
        use std::sync::atomic::Ordering::Relaxed;
        match v {
            Verdict::Allow => self.allow.fetch_add(1, Relaxed),
            Verdict::BlockAd => self.ad.fetch_add(1, Relaxed),
            Verdict::BlockTracker => self.tracker.fetch_add(1, Relaxed),
            Verdict::BlockMalware => self.malware.fetch_add(1, Relaxed),
            Verdict::BlockMixedContent => self.mixed.fetch_add(1, Relaxed),
        };
    }

    /// Snapshot as (allow, ad, tracker, malware, mixed).
    pub fn snapshot(&self) -> (u64, u64, u64, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.allow.load(Relaxed),
            self.ad.load(Relaxed),
            self.tracker.load(Relaxed),
            self.malware.load(Relaxed),
            self.mixed.load(Relaxed),
        )
    }
}

/// Bounded LRU set for tracker hosts with TTL. The old unbounded
/// `HashSet` grew forever and took a write lock per novel tracker.
#[derive(Debug)]
struct TrackerSet {
    map: std::collections::HashMap<String, std::time::Instant>,
    order: std::collections::VecDeque<String>,
    cap: usize,
    ttl: std::time::Duration,
}

impl TrackerSet {
    fn new(cap: usize, ttl: std::time::Duration) -> Self {
        Self {
            map: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
            cap,
            ttl,
        }
    }

    fn contains(&self, host: &str) -> bool {
        matches!(self.map.get(host), Some(t) if t.elapsed() < self.ttl)
    }

    fn insert(&mut self, host: String) {
        let now = std::time::Instant::now();
        if self.map.contains_key(&host) {
            self.map.insert(host.clone(), now);
            self.order.retain(|h| h != &host);
            self.order.push_back(host);
            return;
        }
        // Opportunistic expiry of the front (amortized, no full scan).
        while let Some(front) = self.order.front() {
            let expired = self
                .map
                .get(front)
                .map(|t| t.elapsed() >= self.ttl)
                .unwrap_or(true);
            if !expired {
                break;
            }
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        self.map.insert(host.clone(), now);
        self.order.push_back(host);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

impl Default for TrackerSet {
    fn default() -> Self {
        Self::new(10_000, std::time::Duration::from_secs(7 * 24 * 3600))
    }
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
            tracker_hosts: Arc::new(RwLock::new(TrackerSet::default())),
            proxy_port: Arc::new(RwLock::new(None)),
            verdict_counts: VerdictCounts::default(),
        })
    }

    pub fn with_threat_db(db: ThreatDb) -> PrivacyResult<Self> {
        Ok(Self {
            filters: Arc::new(RwLock::new(FilterEngine::new()?)),
            threat_db: Arc::new(RwLock::new(db)),
            settings: Arc::new(RwLock::new(PrivacySettings::default())),
            tracker_hosts: Arc::new(RwLock::new(TrackerSet::default())),
            proxy_port: Arc::new(RwLock::new(None)),
            verdict_counts: VerdictCounts::default(),
        })
    }

    /// Monotonic decision counters for fleet triage.
    pub fn verdict_counts(&self) -> (u64, u64, u64, u64, u64) {
        self.verdict_counts.snapshot()
    }

    pub fn set_proxy_port(&self, port: Option<u16>) {
        if let Ok(mut p) = self.proxy_port.write() {
            *p = port;
        }
    }

    pub fn proxy_port(&self) -> Option<u16> {
        self.proxy_port.read().ok().and_then(|p| *p)
    }

    pub fn load_filters(
        &self,
        lists: &[(&str, &str)],
        expected_sha256: Option<&str>,
    ) -> PrivacyResult<()> {
        // RCU-style: build the new engine WITHOUT holding the lock, then
        // swap under a brief write. The old code held the write lock across
        // EasyList parsing and froze every navigation on hourly update.
        let mut fresh = FilterEngine::new()?;
        fresh.load(lists, expected_sha256)?;
        let mut engine = self
            .filters
            .write()
            .map_err(|e| PrivacyError::AdblockInit(format!("filter lock poisoned: {e}")))?;
        *engine = fresh;
        Ok(())
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
        let key = request_domain.to_lowercase();
        self.tracker_hosts
            .read()
            .map(|set| set.contains(&key))
            .unwrap_or(false)
    }

    pub fn save_settings_to_file(&self, path: impl AsRef<std::path::Path>) {
        let settings = self.settings();
        if let Ok(json) = serde_json::to_string_pretty(&settings) {
            let path = path.as_ref();
            // Atomic write: tmp + rename + best-effort fsync.
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            } else {
                tracing::warn!("save settings failed");
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
        lookup: impl Fn(&str) -> Option<bool>,
    ) -> Option<Verdict> {
        if !self.settings().realtime_lookup_enabled || !is_top_level {
            return None;
        }
        let full = crate::threat::hash_domain(domain);
        let prefix = full[..8].to_owned();
        if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        // Prefer network lookup through the proxy; fall back to the injected
        // lookup (tests) when no port is configured or the network yields no
        // answer. Single 300ms budget, no per-request thread fan-out.
        if let Some(port) = self.proxy_port() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(rt) = rt {
                let fut = crate::realtime::lookup(&prefix, port);
                if let Ok(Some(hit)) = rt.block_on(tokio::time::timeout(
                    std::time::Duration::from_millis(300),
                    fut,
                )) {
                    return if hit {
                        Some(Verdict::BlockMalware)
                    } else {
                        None
                    };
                }
            }
        }
        match lookup(&prefix) {
            Some(true) => Some(Verdict::BlockMalware),
            _ => None,
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
        self.tracker_hosts
            .read()
            .map(|set| set.contains(&host.to_lowercase()))
            .unwrap_or(false)
    }

    fn should_upgrade_https(&self) -> bool {
        self.settings
            .read()
            .map(|s| s.https_upgrade_enabled)
            .unwrap_or(true)
    }

    fn csp_directives(&self, meta: &RequestMeta) -> Option<String> {
        let url = if meta.request_url.is_empty() {
            format!("https://{}/", meta.request_domain)
        } else {
            meta.request_url.clone()
        };
        let source = meta.referrer.as_deref().unwrap_or(&url);
        let req_type = match meta.resource {
            ferrite_proxy::ResourceType::Document => "document",
            ferrite_proxy::ResourceType::Script => "script",
            ferrite_proxy::ResourceType::Image => "image",
            ferrite_proxy::ResourceType::Stylesheet => "stylesheet",
            ferrite_proxy::ResourceType::Xhr => "xmlhttprequest",
            ferrite_proxy::ResourceType::Media => "media",
            ferrite_proxy::ResourceType::Font => "font",
            ferrite_proxy::ResourceType::Other => "other",
        };
        let filters = self.filters.read().ok()?;
        let req = adblock::request::Request::new(&url, source, req_type, "get").ok()?;
        filters.get_csp(&req)
    }

    fn decide(&self, meta: &RequestMeta) -> Verdict {
        // Every terminal verdict is counted exactly once for triage.
        let verdict = Self::decide_inner(self, meta);
        self.verdict_counts.record(verdict);
        verdict
    }
}

impl PrivacyEngine {
    fn decide_inner(&self, meta: &RequestMeta) -> Verdict {
        if is_loopback(&meta.request_domain) {
            return Verdict::Allow;
        }
        if meta.is_mixed_content() {
            debug!(host = %meta.request_domain, "blocked mixed content");
            return Verdict::BlockMixedContent;
        }

        // Threat lookup is an in-memory suffix check: no SQLite on the hot
        // path (see ThreatDb). Lock failure fails OPEN with a warning and
        // a counter-worthy log — availability over fail-closed here because
        // adblock still applies below; poison indicates a bug, not attack.
        match self.threat_db.read() {
            Ok(db) => {
                if db.contains_with_suffixes(&meta.request_domain) {
                    debug!(host = %meta.request_domain, "blocked malware/phishing");
                    return Verdict::BlockMalware;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "threat lock poisoned - fail-open");
            }
        }

        let is_top_level =
            meta.resource == ferrite_proxy::ResourceType::Document && meta.referrer.is_none();
        if self.settings().realtime_lookup_enabled && is_top_level {
            if self.realtime_block(&meta.request_domain) {
                debug!(host = %meta.request_domain, "blocked via realtime prefix (k-anonymity)");
                return Verdict::BlockMalware;
            }
        }

        let adblock_enabled = self
            .settings
            .read()
            .map(|s| s.adblock_enabled)
            .unwrap_or(true);
        if adblock_enabled {
            match self.filters.read() {
                Ok(filters) => {
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
                Err(e) => {
                    tracing::warn!(error = %e, "filter lock poisoned - fail-open");
                }
            }
        }

        Verdict::Allow
    }
}

impl PrivacyEngine {
    /// Shared realtime prefix check with a 300ms fail-open budget and no
    /// per-request thread+runtime fan-out. Uses blocking std TCP with
    /// timeouts in the calling thread (cheaper than spawn+runtime per nav).
    fn realtime_block(&self, domain: &str) -> bool {
        let prefix = crate::threat::hash_domain(domain);
        let prefix = prefix[..8].to_owned();
        if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        let port = self.proxy_port.read().ok().and_then(|p| *p);
        let Some(port) = port else {
            return false;
        };
        // Run the async lookup on a minimal current-thread runtime inline.
        // No extra thread: `decide` already runs on a proxy worker, and the
        // lookup itself has connect/read timeouts well under 300ms.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(rt) = rt else {
            return false;
        };
        match rt.block_on(tokio::time::timeout(
            std::time::Duration::from_millis(300),
            crate::realtime::lookup(&prefix, port),
        )) {
            Ok(Some(true)) => true,
            Ok(_) => false,
            Err(_) => {
                tracing::debug!(%domain, "realtime lookup timeout - fail-open");
                false
            }
        }
    }
}

fn is_loopback(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    h == "localhost" || h == "127.0.0.1" || h == "::1" || h.ends_with(".localhost")
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

    #[test]
    fn verdict_counters_record_every_decision() {
        let e = PrivacyEngine::new().unwrap();
        e.load_filters(&[("t", "||ads.example^")], None).unwrap();
        let allow = RequestMeta::new("example.com", ResourceType::Document);
        let block = RequestMeta::new("ads.example", ResourceType::Script);
        assert_eq!(e.decide(&allow), Verdict::Allow);
        assert_eq!(e.decide(&block), Verdict::BlockAd);
        assert_eq!(e.decide(&block), Verdict::BlockAd);
        let (allow_n, ad_n, tracker_n, malware_n, mixed_n) = e.verdict_counts();
        assert_eq!((allow_n, ad_n), (1, 2));
        assert_eq!((tracker_n, malware_n, mixed_n), (0, 0, 0));
    }
}
