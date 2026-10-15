//! Per-request verdicts for the proxy hook. Order: loopback, mixed
//! content, overrides, threat intel, realtime, filters, heuristics.

use std::sync::{Arc, RwLock};

use tracing::debug;

use ferrite_proxy::{DecisionHook, RequestMeta, Verdict};

use crate::error::{PrivacyError, PrivacyResult};
use crate::filters::{FilterEngine, FilterList, ListCategory};
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
    /// Loopback master switch. False goes direct with a visible error state.
    /// Fixed at boot, needs a restart to flip.
    #[serde(default = "default_true")]
    pub proxy_enabled: bool,
    /// DoH endpoint for upstream resolution at the proxy. None uses the OS.
    #[serde(default)]
    pub doh_endpoint: Option<String>,
    /// SafeBrowsing API key for realtime k-anonymity lookups. None disables
    /// network egress (honest no-op). Serde-default so old files parse.
    #[serde(default)]
    pub realtime_api_key: Option<String>,
    /// Origins with blocking paused by the user (exact lowercase hosts).
    /// A paused page loads unfiltered: pausing is an explicit action,
    /// persisted like any other setting (Incognito keeps it memory-only).
    /// Malware blocks pause too: pausing is consent to the risk, stated
    /// in the shield copy.
    #[serde(default)]
    pub paused_hosts: Vec<String>,
    /// Native spellcheck toggle. Off forces `spellcheck="false"`.
    /// Dictionary fetches stay blocked either way.
    #[serde(default = "default_true")]
    pub spellcheck_enabled: bool,
    /// Active languages, canonicalized on write. Detection stays native.
    #[serde(default = "crate::spellcheck::default_langs")]
    pub spellcheck_langs: Vec<String>,
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
            proxy_enabled: true,
            doh_endpoint: None,
            realtime_api_key: None,
            paused_hosts: Vec::new(),
            spellcheck_enabled: true,
            spellcheck_langs: crate::spellcheck::default_langs(),
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
    /// Frequent domains for lookalike checks. Memory only.
    frequent_hosts: Arc<RwLock<Vec<String>>>,
    /// User threat overrides. Exact host match bypasses blocks (loud by design).
    threat_exceptions: Arc<RwLock<std::collections::HashSet<String>>>,
    /// Cross domain password posts with report time. 5 minute TTL.
    form_risks: Arc<std::sync::Mutex<std::collections::HashMap<(String, String), i64>>>,
    /// Heuristic block evidence for the interstitial. 1 hour TTL, display only.
    block_details: Arc<std::sync::Mutex<std::collections::HashMap<String, (BlockWhy, i64)>>>,
    /// Per-page verdict tallies for the shield dropdown (session totals stay
    /// in `verdict_counts`; these attribute to the top page). Bounded, memory
    /// only: evicted platforms never see it, restarts reset it.
    origin_counts: Arc<RwLock<std::collections::HashMap<String, OriginCounts>>>,
}

/// Why a block fired, for interstitial copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockWhy {
    ListMatch,
    Lookalike { matched: String },
    FormRisk { action_host: String },
}

/// Pair and detail lifetimes plus map caps. Caps bound adversarial growth.
pub const FORM_RISK_TTL_MS: i64 = 5 * 60 * 1000;
pub const BLOCK_DETAIL_TTL_MS: i64 = 60 * 60 * 1000;
pub const FORM_RISKS_CAP: usize = 512;
pub const BLOCK_DETAILS_CAP: usize = 256;
pub const THREAT_EXCEPTIONS_CAP: usize = 4096;
/// Distinct origins tracked for per-site shield counts. Past this, new
/// origins still count globally but get no per-site row (memory bound).
pub const ORIGIN_COUNTS_CAP: usize = 4096;
/// Paused-host list cap (persisted; bounds the settings file entry).
pub const PAUSED_HOSTS_CAP: usize = 512;

/// Per-page verdict tally (session totals stay global; these attribute to
/// the top page for the shield dropdown). Plain integers under the engine
/// lock: updated once per verdict, read per chrome push.
#[derive(Debug, Clone, Copy, Default)]
pub struct OriginCounts {
    pub allow: u64,
    pub ad: u64,
    pub tracker: u64,
    pub fingerprint: u64,
    pub malware: u64,
    pub mixed: u64,
}

impl OriginCounts {
    fn record(&mut self, verdict: Verdict) {
        match verdict {
            Verdict::Allow => self.allow += 1,
            Verdict::BlockAd => self.ad += 1,
            Verdict::BlockTracker => self.tracker += 1,
            Verdict::BlockMalware => self.malware += 1,
            Verdict::BlockMixedContent => self.mixed += 1,
        }
    }

    /// Blocked total across categories (the shield's per-site headline).
    pub fn blocked(&self) -> u64 {
        self.ad + self.tracker + self.malware + self.mixed
    }
}

/// Monotonic counters, process lifetime.
#[derive(Debug, Default)]
pub struct VerdictCounts {
    pub allow: std::sync::atomic::AtomicU64,
    pub ad: std::sync::atomic::AtomicU64,
    pub tracker: std::sync::atomic::AtomicU64,
    /// Fingerprinting-list hits. Enforced as tracker blocks (same engine
    /// action) but counted apart so the shield can promise the breakdown.
    pub fingerprint: std::sync::atomic::AtomicU64,
    pub malware: std::sync::atomic::AtomicU64,
    pub mixed: std::sync::atomic::AtomicU64,
    /// Nonzero means protection was degraded.
    pub fail_open: std::sync::atomic::AtomicU64,
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

    /// Order is (allow, ad, tracker, fingerprint, malware, mixed).
    pub fn snapshot(&self) -> (u64, u64, u64, u64, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.allow.load(Relaxed),
            self.ad.load(Relaxed),
            self.tracker.load(Relaxed),
            self.fingerprint.load(Relaxed),
            self.malware.load(Relaxed),
            self.mixed.load(Relaxed),
        )
    }

    /// Shield tag. Enforcement is already counted under tracker.
    pub fn record_fingerprint(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.fingerprint.fetch_add(1, Relaxed);
    }

    /// Record one check that could not run.
    pub fn record_fail_open(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.fail_open.fetch_add(1, Relaxed);
    }

    /// Fail open count, process lifetime.
    pub fn fail_open_count(&self) -> u64 {
        use std::sync::atomic::Ordering::Relaxed;
        self.fail_open.load(Relaxed)
    }
}

// Bounded LRU set with TTL. Expiry is amortized on insert, no background thread.
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
    fn recent(&self, limit: usize) -> Vec<String> {
        let limit = limit.min(50);
        self.order
            .iter()
            .rev()
            .filter(|h| self.contains(h))
            .take(limit)
            .cloned()
            .collect()
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
    /// Safe default: every store in memory, zero filesystem touch. In
    /// particular this NEVER creates `ferrite-threat.db` in the current
    /// directory (a past version did, littering launch dirs and the repo).
    /// Durable threat caching is explicit via `with_threat_db` with a
    /// data-root path owned by the caller.
    pub fn new() -> PrivacyResult<Self> {
        let threat =
            ThreatDb::new_in_memory().map_err(|e| PrivacyError::ThreatDb(e.to_string()))?;
        Ok(Self {
            filters: Arc::new(RwLock::new(FilterEngine::new()?)),
            threat_db: Arc::new(RwLock::new(threat)),
            settings: Arc::new(RwLock::new(PrivacySettings::default())),
            tracker_hosts: Arc::new(RwLock::new(TrackerSet::default())),
            proxy_port: Arc::new(RwLock::new(None)),
            verdict_counts: VerdictCounts::default(),
            frequent_hosts: Arc::new(RwLock::new(Vec::new())),
            threat_exceptions: Arc::new(RwLock::new(std::collections::HashSet::new())),
            form_risks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            block_details: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            origin_counts: Arc::new(RwLock::new(std::collections::HashMap::new())),
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
            frequent_hosts: Arc::new(RwLock::new(Vec::new())),
            threat_exceptions: Arc::new(RwLock::new(std::collections::HashSet::new())),
            form_risks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            block_details: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            origin_counts: Arc::new(RwLock::new(std::collections::HashMap::new())),
        })
    }

    /// Counts as (allow, ad, tracker, fingerprint, malware, mixed).
    pub fn verdict_counts(&self) -> (u64, u64, u64, u64, u64, u64) {
        self.verdict_counts.snapshot()
    }

    /// Per-list (name, source bytes, rule lines) for the task manager.
    /// Empty when no lists loaded or the lock is poisoned.
    pub fn filter_list_stats(&self) -> Vec<(String, usize, usize)> {
        self.filters
            .read()
            .map(|f| f.list_stats())
            .unwrap_or_default()
    }

    /// Fail open count. Powers the degraded badge.
    pub fn fail_open_count(&self) -> u64 {
        self.verdict_counts.fail_open_count()
    }

    /// Top-page host a request attributes to: subresources bill to the
    /// page that caused them (referrer), top-level documents to themselves,
    /// CONNECT tunnels to their target. Lowercase; empty only for garbage.
    /// The shield dropdown and the pause switch key on exactly this.
    pub fn page_host(meta: &RequestMeta) -> String {
        if let Some(ref r) = meta.referrer
            && let Ok(u) = url::Url::parse(r)
            && let Some(h) = u.host_str()
            && !h.is_empty()
        {
            return h.to_lowercase();
        }
        meta.request_domain.to_lowercase()
    }

    /// Whether blocking is paused for this page (exact host match on the
    /// attributed page, never on the tracker host: pausing example.com
    /// must not pause evil-tracker.com everywhere else).
    pub fn is_paused(&self, page: &str) -> bool {
        let page = normalize_host(page);
        self.settings
            .read()
            .map(|s| s.paused_hosts.iter().any(|h| h == &page))
            .unwrap_or(false)
    }

    /// Pause or resume blocking for one page host. Normalizes, caps, and
    /// dedupes; returns the stored (normalized) host or None when empty.
    /// Persistence is the shell's job (sealed settings file); this only
    /// flips the live engine.
    pub fn set_paused(&self, page: &str, paused: bool) -> Option<String> {
        let page = normalize_host(page);
        if page.is_empty() {
            return None;
        }
        if let Ok(mut s) = self.settings.write() {
            if paused {
                if !s.paused_hosts.iter().any(|h| h == &page)
                    && s.paused_hosts.len() < PAUSED_HOSTS_CAP
                {
                    s.paused_hosts.push(page.clone());
                }
            } else {
                s.paused_hosts.retain(|h| h != &page);
            }
            Some(page)
        } else {
            None
        }
    }

    /// Per-page tally snapshot (allow, ad, tracker, fingerprint, malware,
    /// mixed). Unknown pages read all-zero, never an error.
    pub fn origin_breakdown(&self, page: &str) -> (u64, u64, u64, u64, u64, u64) {
        let page = normalize_host(page);
        self.origin_counts
            .read()
            .ok()
            .and_then(|m| m.get(&page).copied())
            .map(|c| (c.allow, c.ad, c.tracker, c.fingerprint, c.malware, c.mixed))
            .unwrap_or_default()
    }

    fn record_origin(&self, page: &str, verdict: Verdict) {
        if page.is_empty() {
            return;
        }
        if let Ok(mut m) = self.origin_counts.write() {
            // New rows stop past the cap (global counts still move in
            // `decide`); existing rows always update. One map, no LRU.
            if !m.contains_key(page) && m.len() >= ORIGIN_COUNTS_CAP {
                return;
            }
            m.entry(page.to_owned()).or_default().record(verdict);
        }
    }

    /// Fingerprint-tag bump with the same create-or-count rule as
    /// `record_origin` (called earlier in the verdict path, so the row may
    /// not exist yet; create it rather than dropping the tag).
    fn bump_origin_fp(&self, page: &str) {
        if page.is_empty() {
            return;
        }
        if let Ok(mut m) = self.origin_counts.write() {
            if !m.contains_key(page) && m.len() >= ORIGIN_COUNTS_CAP {
                return;
            }
            m.entry(page.to_owned()).or_default().fingerprint += 1;
        }
    }

    /// Newest first. Poisoned lock reads as empty, never panics.
    pub fn recent_tracker_hosts(&self, limit: usize) -> Vec<String> {
        self.tracker_hosts
            .read()
            .map(|t| t.recent(limit))
            .unwrap_or_default()
    }

    pub fn set_proxy_port(&self, port: Option<u16>) {
        if let Ok(mut p) = self.proxy_port.write() {
            *p = port;
        }
    }

    pub fn proxy_port(&self) -> Option<u16> {
        self.proxy_port.read().ok().and_then(|p| *p)
    }

    /// User DoH endpoint when set, else the OS resolver. Read live per spawn.
    /// Invalid endpoints are refused here (loud warn, OS fallback): a bad
    /// value in the settings file must never silently become plaintext DNS,
    /// and must never spam one warning per dial (spawn-time is once).
    pub fn dns_config(&self) -> ferrite_proxy::DnsConfig {
        let endpoint = self
            .settings
            .read()
            .ok()
            .and_then(|s| s.doh_endpoint.clone())
            .filter(|e| !e.trim().is_empty());
        let cfg = ferrite_proxy::DnsConfig {
            doh_endpoint: endpoint,
        };
        if let Some(problem) = cfg.endpoint_problem() {
            tracing::warn!(problem, "doh endpoint invalid: os fallback");
            return ferrite_proxy::DnsConfig::system();
        }
        cfg
    }

    pub fn load_filters(&self, lists: &[FilterList<'_>]) -> PrivacyResult<()> {
        let mut fresh = FilterEngine::new()?;
        fresh.load(lists)?;
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

    /// Cosmetic selectors for one URL. Honors the adblock toggle.
    pub fn cosmetic_selectors(&self, url: &str) -> Vec<String> {
        if !self.settings().adblock_enabled {
            return Vec::new();
        }
        self.filters
            .read()
            .map(|f| f.selectors_for_url(url))
            .unwrap_or_default()
    }

    /// Replace the lookalike set. Truncated, lowercased, empties dropped.
    pub fn set_frequent_hosts(&self, hosts: Vec<String>) {
        if let Ok(mut set) = self.frequent_hosts.write() {
            *set = hosts
                .into_iter()
                .map(|h| h.trim().to_lowercase())
                .filter(|h| !h.is_empty())
                .take(crate::heuristics::FREQUENT_HOSTS_CAP)
                .collect();
        }
    }

    /// Replace the threat override set. Exact host match bypasses blocks.
    pub fn set_threat_exceptions(&self, hosts: Vec<String>) {
        if let Ok(mut set) = self.threat_exceptions.write() {
            *set = hosts
                .into_iter()
                .map(|h| h.trim().to_lowercase())
                .filter(|h| !h.is_empty())
                .take(THREAT_EXCEPTIONS_CAP)
                .collect();
        }
    }

    fn is_threat_excepted(&self, host: &str) -> bool {
        self.threat_exceptions
            .read()
            .map(|set| set.contains(host.trim().to_lowercase().as_str()))
            .unwrap_or(false)
    }

    /// Record a cross domain password post. Same registrable pairs and
    /// unparseable URLs are not signals and return false.
    pub fn note_form_risk(&self, page_url: &str, action_url: &str, now_ms: i64) -> bool {
        let hosts = [page_url, action_url].map(|u| {
            url::Url::parse(u)
                .ok()
                .filter(|p| p.scheme() == "http" || p.scheme() == "https")
                .and_then(|p| p.host_str().map(str::to_owned))
        });
        let [Some(page_host), Some(action_host)] = hosts else {
            return false;
        };
        let (Some(page_reg), Some(action_reg)) = (
            crate::heuristics::registrable(&page_host),
            crate::heuristics::registrable(&action_host),
        ) else {
            return false;
        };
        if page_reg == action_reg {
            return false;
        }
        if let Ok(mut risks) = self.form_risks.lock() {
            risks.retain(|_, at| now_ms.saturating_sub(*at) < FORM_RISK_TTL_MS);
            if risks.len() >= FORM_RISKS_CAP {
                return false;
            }
            risks.insert((page_reg, action_reg), now_ms);
            true
        } else {
            false
        }
    }

    /// Why a heuristic fired for `host`. Expired reads as None.
    /// Lock-then-lookup stays nested: the expiry prune below borrows
    /// what the guard acquired, which a chain obscures.
    #[allow(clippy::collapsible_if)]
    pub fn block_detail(&self, host: &str, now_ms: i64) -> Option<BlockWhy> {
        let key = host.trim().to_lowercase();
        if let Ok(mut details) = self.block_details.lock() {
            if let Some((why, at)) = details.get(&key).cloned() {
                if now_ms.saturating_sub(at) < BLOCK_DETAIL_TTL_MS {
                    return Some(why);
                }
                details.remove(&key);
            }
        }
        None
    }

    fn note_block_detail(&self, host: &str, why: BlockWhy, now_ms: i64) {
        if let Ok(mut details) = self.block_details.lock() {
            details.retain(|_, (_, at)| now_ms.saturating_sub(*at) < BLOCK_DETAIL_TTL_MS);
            if details.len() >= BLOCK_DETAILS_CAP {
                return;
            }
            details.insert(host.trim().to_lowercase(), (why, now_ms));
        }
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// True when a media URL would block as ad or tracker. Fail open.
    pub fn is_blocked_media(&self, url: &str, referrer: Option<&str>) -> bool {
        if !self.settings().adblock_enabled {
            return false;
        }
        let domain = referrer
            .or(Some(url))
            .and_then(|u| {
                url::Url::parse(u)
                    .ok()
                    .and_then(|p| p.host_str().map(|h| h.to_owned()))
            })
            .unwrap_or_default();
        if domain.is_empty() {
            return false;
        }
        let meta = ferrite_proxy::RequestMeta::new(&domain, ferrite_proxy::ResourceType::Media)
            .with_url(url);
        let mut meta = meta;
        if let Some(r) = referrer {
            meta.referrer = Some(r.to_owned());
        }
        self.filters
            .read()
            .map(|f| f.check(&meta).is_some())
            .unwrap_or(false)
    }

    /// Serialize current settings for the shell-owned settings file.
    /// The shell merges this into its sealed document (its own keys
    /// survive: no clobber), so the engine never touches settings bytes.
    pub fn export_settings_json(&self) -> String {
        serde_json::to_string_pretty(&self.settings()).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Install settings from the shell-owned settings file. Unknown
    /// (shell-owned) keys are ignored by the struct shape; corrupt input
    /// keeps current settings instead of resetting them. An invalid DoH
    /// endpoint is dropped with a warning (fail-closed to OS DNS at resolve
    /// time would otherwise log once per spawn with no provenance).
    pub fn import_settings_json(&self, json: &str) {
        if let Ok(mut loaded) = serde_json::from_str::<PrivacySettings>(json)
            && let Ok(mut s) = self.settings.write()
        {
            if let Some(ref endpoint) = loaded.doh_endpoint
                && (ferrite_proxy::DnsConfig {
                    doh_endpoint: Some(endpoint.clone()),
                })
                .endpoint_problem()
                .is_some()
            {
                tracing::warn!("settings doh endpoint invalid: ignoring");
                loaded.doh_endpoint = None;
            }
            // Empty-means-absent: no key, no egress. Normalized on import
            // so old/blank files behave like missing (fail-closed to no-op).
            if loaded
                .realtime_api_key
                .as_deref()
                .is_some_and(|k| k.trim().is_empty())
            {
                loaded.realtime_api_key = None;
            }
            *s = loaded;
        }
    }

    /// One update pass: same fail-soft, retain-last-good semantics for the
    /// immediate first fetch and every hourly tick. Empty filter fetch
    /// retains last good, never swaps empty; `None` threat fetch retains.
    fn apply_update_once(&self, fetched: Vec<FilterFetch>, threat: Option<Vec<String>>) {
        // Empty fetch retains last good, never swaps empty.
        if !fetched.is_empty() {
            let lists: Vec<FilterList<'_>> = fetched
                .iter()
                .map(|f| FilterList {
                    name: &f.name,
                    category: f.category,
                    text: &f.text,
                    expected_sha256: f.sha256.as_deref(),
                })
                .collect();
            if let Err(e) = self.load_filters(&lists) {
                tracing::warn!(error = %e, "hourly filter update failed - retaining last_known_good");
            } else {
                tracing::info!("hourly filter update applied");
            }
        }
        // Threat update, same retain semantics.
        if let Some(urls) = threat {
            let refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
            if let Err(e) = self.load_threat_snapshot(&refs) {
                tracing::warn!(error = %e, "hourly threat update failed - retaining last_known_good");
            } else {
                tracing::info!(entries = urls.len(), "hourly threat update applied");
            }
        }
    }

    pub fn spawn_hourly_updater(
        self: &std::sync::Arc<Self>,
        fetch_filter: impl Fn() -> Vec<FilterFetch> + Send + 'static,
        fetch_threat: impl Fn() -> Option<Vec<String>> + Send + 'static,
    ) {
        let engine = std::sync::Arc::clone(self);
        std::thread::Builder::new()
            .name("ferrite-privacy-updater".to_owned())
            .spawn(move || {
                // Immediate first fetch: fresh installs otherwise sit on stub
                // rules for an hour+. Same fail-soft, caps, and
                // retain-last-good as the hourly tick.
                engine.apply_update_once(fetch_filter(), fetch_threat());
                let mut rng: u64 = 0x9E3779B97F4A7C15;
                loop {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    let jitter = rng % 300;
                    std::thread::sleep(std::time::Duration::from_secs(3600 + jitter));
                    engine.apply_update_once(fetch_filter(), fetch_threat());
                }
            })
            .ok();
    }

    /// Opt-in top-level k-anonymity lookup. `lookup` performs the prefix
    /// query and is the only I/O seam: the hot path passes the network
    /// lookup, tests pass a stub, and verdict logic stays unit-testable.
    /// `None` means allow (fail-open by contract, counted by the caller).
    pub fn realtime_check(
        &self,
        domain: &str,
        is_top_level: bool,
        lookup: impl Fn(&str) -> Option<bool>,
    ) -> Option<Verdict> {
        if !self.settings().realtime_lookup_enabled || !is_top_level {
            return None;
        }
        let prefix = Self::realtime_prefix(domain)?;
        match lookup(&prefix) {
            Some(true) => Some(Verdict::BlockMalware),
            _ => None,
        }
    }

    /// Validated 8-hex-char k-anonymity prefix for `domain`, if well-formed.
    /// Shared by the hot path and the injectable check above.
    fn realtime_prefix(domain: &str) -> Option<String> {
        let full = crate::threat::hash_domain(domain);
        let prefix = full[..8].to_owned();
        prefix
            .chars()
            .all(|c| c.is_ascii_hexdigit())
            .then_some(prefix)
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
        let verdict = Self::decide_inner(self, meta);
        self.verdict_counts.record(verdict);
        self.record_origin(&Self::page_host(meta), verdict);
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
        // User override wins over lists and heuristics. Transport checks stay.
        if self.is_threat_excepted(&meta.request_domain) {
            return Verdict::Allow;
        }
        // Paused page loads unfiltered: explicit per-site consent, checked on
        // the attributed page (never the tracker host). Loopback and
        // internal pages never reach this branch meaningfully; pausing
        // them is a no-op row, refused at the shell instead.
        if self.is_paused(&Self::page_host(meta)) {
            return Verdict::Allow;
        }

        // Dictionary delivery is always a tracker. Toggle governs in page
        // checking only. CONNECT hides paths, so delivery hosts match whole.
        // ponytail: engine traffic bypassing the per webview proxy (if any
        // future platform has it) is outside this choke. Bundled dicts
        // remove the need regardless.
        if crate::spellcheck::is_dict_host(&meta.request_domain)
            || (!meta.request_url.is_empty() && crate::spellcheck::is_dict_url(&meta.request_url))
        {
            debug!(host = %meta.request_domain, "blocked spellcheck dictionary fetch");
            return Verdict::BlockTracker;
        }

        // Threat check avoids SQLite on the hot path. Lock failure fails
        // open, since adblock still applies below.
        match self.threat_db.read() {
            Ok(db) => {
                if db.contains_with_suffixes(&meta.request_domain) {
                    debug!(host = %meta.request_domain, "blocked malware/phishing");
                    return Verdict::BlockMalware;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "threat lock poisoned - fail-open");
                self.verdict_counts.record_fail_open();
            }
        }

        let is_top_level =
            meta.resource == ferrite_proxy::ResourceType::Document && meta.referrer.is_none();
        if self.settings().realtime_lookup_enabled
            && is_top_level
            && self.realtime_block(&meta.request_domain)
        {
            debug!(host = %meta.request_domain, "blocked via realtime prefix (k-anonymity)");
            return Verdict::BlockMalware;
        }

        let adblock_enabled = self
            .settings
            .read()
            .map(|s| s.adblock_enabled)
            .unwrap_or(true);
        if adblock_enabled {
            match self.filters.read() {
                Ok(filters) => {
                    if let Some(tag) = filters.check_tagged(meta) {
                        if tag.verdict == Verdict::BlockTracker {
                            if tag.category == ListCategory::Fingerprinting {
                                self.verdict_counts.record_fingerprint();
                                // Mirror the global fp counter per page (the
                                // verdict itself bills as tracker when
                                // `decide` records it below, same as global).
                                self.bump_origin_fp(&Self::page_host(meta));
                            }
                            if let Ok(mut set) = self.tracker_hosts.write() {
                                set.insert(meta.request_domain.to_lowercase());
                            }
                        }
                        return tag.verdict;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "filter lock poisoned - fail-open");
                    self.verdict_counts.record_fail_open();
                }
            }
        }

        // Heuristics last: confirmed rules first, suspicion only on miss.
        // Documents only. Subresources inherit the top page judgment.
        if meta.resource == ferrite_proxy::ResourceType::Document {
            let now = Self::now_ms();
            let req_reg = crate::heuristics::registrable(&meta.request_domain);
            if let Some(referrer) = meta.referrer.as_deref()
                && let Some(page_host) = url::Url::parse(referrer)
                    .ok()
                    .and_then(|p| p.host_str().map(str::to_owned))
                && let (Some(page_reg), Some(req_r)) =
                    (crate::heuristics::registrable(&page_host), req_reg.clone())
                && page_reg != req_r
                && let Ok(risks) = self.form_risks.lock()
                && let Some(at) = risks.get(&(page_reg.clone(), req_r.clone()))
                && now.saturating_sub(*at) < FORM_RISK_TTL_MS
            {
                debug!(host = %meta.request_domain, page = %page_reg, "blocked form-target mismatch");
                self.note_block_detail(
                    &meta.request_domain,
                    BlockWhy::FormRisk {
                        action_host: meta.request_domain.clone(),
                    },
                    now,
                );
                return Verdict::BlockMalware;
            }
            if let Some(req_r) = req_reg
                && let Ok(frequent) = self.frequent_hosts.read()
                && !frequent.is_empty()
                && let Some(matched) = crate::heuristics::lookalike_match(&req_r, &frequent)
            {
                debug!(host = %meta.request_domain, matched = %matched, "blocked lookalike domain");
                self.note_block_detail(&meta.request_domain, BlockWhy::Lookalike { matched }, now);
                return Verdict::BlockMalware;
            }
        }

        Verdict::Allow
    }
}

impl PrivacyEngine {
    // Hot-path realtime check: 300ms fail-open budget, no extra threads.
    // Same verdict core as `realtime_check` (prefix + hit-means-malware);
    // the runtime/timeout wrapper and fail-open counting live here because
    // they are hot-path policy, not verdict logic.
    fn realtime_block(&self, domain: &str) -> bool {
        let Some(prefix) = Self::realtime_prefix(domain) else {
            return false;
        };
        // Absent key is an honest no-op: no runtime, no egress, fail-open.
        let api_key = self
            .settings
            .read()
            .ok()
            .and_then(|s| s.realtime_api_key.clone());
        let Some(key) = api_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) else {
            return false;
        };
        // Own the keyed copy so the async call holds no lock.
        let key = key.to_owned();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(rt) = rt else {
            self.verdict_counts.record_fail_open();
            return false;
        };
        match rt.block_on(tokio::time::timeout(
            std::time::Duration::from_millis(300),
            crate::realtime::lookup_direct(&prefix, Some(key.as_str())),
        )) {
            Ok(Some(true)) => true,
            Ok(_) => false,
            Err(_) => {
                tracing::debug!(%domain, "realtime lookup timeout - fail-open");
                self.verdict_counts.record_fail_open();
                false
            }
        }
    }
}

/// Owned buffers for the hourly updater. `sha256` is the transmission hash.
/// None skips verification for file fallbacks.
pub struct FilterFetch {
    pub name: String,
    pub category: ListCategory,
    pub text: String,
    pub sha256: Option<String>,
}

fn is_loopback(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    h == "localhost" || h == "127.0.0.1" || h == "::1" || h.ends_with(".localhost")
}

/// Bare-host normalization for pause/breakdown keys: trim, lowercase,
/// strip one trailing dot. Pause targets arrive server-derived (never
/// user-typed), so unlike the threat matcher this never guesses schemes;
/// garbage stays garbage and matches nothing.
fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrite_proxy::ResourceType;

    fn engine_with_blocked(host: &str) -> PrivacyEngine {
        let e = PrivacyEngine::new().unwrap();
        e.load_filters(&[FilterList {
            name: "test",
            category: ListCategory::Ads,
            text: &format!("||{host}^"),
            expected_sha256: None,
        }])
        .unwrap();
        e
    }

    /// Working-directory file names belonging to the threat cache (main db
    /// plus SQLite sidecars). `new()` must never create any of these: a past
    /// version wrote them to CWD, littering launch dirs and the repo itself.
    fn cwd_threat_files() -> Vec<std::path::PathBuf> {
        std::fs::read_dir(".")
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name())
                    .filter(|n| n.to_string_lossy().starts_with("ferrite-threat.db"))
                    .map(std::path::PathBuf::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn new_never_writes_threat_db_to_cwd() {
        let before = cwd_threat_files();
        let _a = PrivacyEngine::new().expect("in-memory default");
        let _b = PrivacyEngine::new().expect("second instance");
        assert_eq!(
            cwd_threat_files(),
            before,
            "PrivacyEngine::new() must not litter the working directory"
        );
    }

    #[test]
    fn allow_all_when_no_filters() {
        let e = PrivacyEngine::new().unwrap();
        let meta = RequestMeta::new("example.com", ResourceType::Document);
        assert_eq!(e.decide(&meta), Verdict::Allow);
    }

    #[test]
    fn decide_throughput_stays_interactive() {
        // Regression tripwire, not a benchmark: 20k mixed verdicts must
        // finish in seconds on any real hardware. The bound is deliberately
        // generous (CI runners wobble); it catches lock contention,
        // accidental sleeps, and quadratic blowups, never a 10% slowdown.
        // For real numbers run with --nocapture and read the printed p50/p99.
        let e = engine_with_blocked("ads.example");
        let metas: Vec<RequestMeta> = (0..20_000)
            .map(|i| {
                let host = match i % 4 {
                    0 => "ads.example".to_owned(),
                    1 => format!("sub{i}.example.com"),
                    2 => "tracker.test".to_owned(),
                    _ => "example.com".to_owned(),
                };
                RequestMeta::new(host, ResourceType::Document)
            })
            .collect();
        // Warmup (locks, caches) outside the clock.
        for m in metas.iter().take(100) {
            let _ = e.decide(m);
        }
        let start = std::time::Instant::now();
        let mut nanos: Vec<u128> = Vec::with_capacity(metas.len());
        for m in &metas {
            let t = std::time::Instant::now();
            let _ = e.decide(m);
            nanos.push(t.elapsed().as_nanos());
        }
        let total = start.elapsed();
        nanos.sort_unstable();
        let p50 = nanos[nanos.len() / 2];
        let p99 = nanos[nanos.len() * 99 / 100];
        println!("decide: total={total:?} p50={p50}ns p99={p99}ns");
        assert!(
            total < std::time::Duration::from_secs(5),
            "20k decides took {total:?}"
        );
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
    fn spellcheck_dict_fetch_blocked_as_tracker() {
        let e = PrivacyEngine::new().unwrap();
        // Delivery host: blocked on domain alone (CONNECT hides the path).
        let meta = RequestMeta::new("redirector.gvt1.com", ResourceType::Document);
        assert_eq!(e.decide(&meta), Verdict::BlockTracker);
        // Mixed-use host with a dictionary path and full URL carried.
        let meta = RequestMeta::new("www.gstatic.com", ResourceType::Other)
            .with_url("https://www.gstatic.com/chrome/dict/en-US.bdic");
        assert_eq!(e.decide(&meta), Verdict::BlockTracker);
        // Same host, ordinary subresource: untouched.
        let meta = RequestMeta::new("www.gstatic.com", ResourceType::Font)
            .with_url("https://www.gstatic.com/fonts/roboto.woff2");
        assert_eq!(e.decide(&meta), Verdict::Allow);
        // Toggle governs in-page checking only. The block holds either way.
        e.update_settings(|s| s.spellcheck_enabled = false);
        let meta = RequestMeta::new("redirector.gvt1.com", ResourceType::Document);
        assert_eq!(e.decide(&meta), Verdict::BlockTracker);
    }

    #[test]
    fn settings_export_import_round_trips_and_ignores_shell_keys() {
        let e = PrivacyEngine::new().unwrap();
        e.update_settings(|s| {
            s.realtime_lookup_enabled = true;
            s.spellcheck_langs = vec!["de".to_owned()];
        });
        let json = e.export_settings_json();
        // Shell-owned keys ride in the same document and must not disturb
        // the import (struct shape ignores unknown fields).
        let mut doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        doc["chrome_pack"] = serde_json::Value::String("tenebra".into());
        doc["home_url"] = serde_json::Value::String("https://example.com".into());
        let e2 = PrivacyEngine::new().unwrap();
        e2.import_settings_json(&doc.to_string());
        let s = e2.settings();
        assert!(s.realtime_lookup_enabled);
        assert_eq!(s.spellcheck_langs, vec!["de".to_owned()]);
        assert!(s.adblock_enabled, "defaults fill what import omits");
        // Corrupt input keeps current settings instead of resetting them.
        e2.import_settings_json("{not json");
        assert!(e2.settings().realtime_lookup_enabled);
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
    fn doh_off_by_default_invalid_never_sticks() {
        // Pinned default: no endpoint, OS resolver. Encrypted DNS stays an
        // explicit opt-in until the bootstrap story is solved (the DoH
        // server's own hostname still resolves in the clear).
        let e = PrivacyEngine::new().unwrap();
        assert!(e.settings().doh_endpoint.is_none());
        assert!(e.dns_config().doh_endpoint.is_none());
        // Garbage in the settings file is dropped at import, never stored.
        e.import_settings_json(r#"{"doh_endpoint":"http://evil.test/"}"#);
        assert!(e.settings().doh_endpoint.is_none());
        assert!(e.dns_config().doh_endpoint.is_none());
        // A valid endpoint survives import and reaches the resolver config.
        e.import_settings_json(r#"{"doh_endpoint":"https://dns.example/dns-query"}"#);
        assert_eq!(
            e.settings().doh_endpoint.as_deref(),
            Some("https://dns.example/dns-query")
        );
        assert_eq!(
            e.dns_config().doh_endpoint.as_deref(),
            Some("https://dns.example/dns-query")
        );
    }

    #[test]
    fn networked_lookups_stay_off_without_explicit_opt_in() {
        // A minimal/legacy config must not enable anything networked: the
        // missing key decodes to false, never true.
        let s: PrivacySettings = serde_json::from_str("{}").unwrap();
        assert!(!s.realtime_lookup_enabled);
        assert!(!PrivacySettings::default().realtime_lookup_enabled);
    }

    #[test]
    fn realtime_check_stub_lookup_never_touches_network() {
        let e = PrivacyEngine::new().unwrap();
        // Disabled by default: even a hit-stub cannot block.
        assert_eq!(e.realtime_check("evil.test", true, |_| Some(true)), None);
        e.update_settings(|s| s.realtime_lookup_enabled = true);
        // Enabled + top-level: stub hit blocks, stub miss allows, network
        // timeout (None) allows. No socket is opened in any arm.
        assert_eq!(
            e.realtime_check("evil.test", true, |_| Some(true)),
            Some(Verdict::BlockMalware)
        );
        assert_eq!(e.realtime_check("evil.test", true, |_| Some(false)), None);
        assert_eq!(e.realtime_check("evil.test", true, |_| None), None);
        // Subresource lookups never block regardless of the stub.
        assert_eq!(e.realtime_check("evil.test", false, |_| Some(true)), None);
    }

    #[test]
    fn pause_allows_listed_hosts_and_breakdown_attributes_pages() {
        let e = engine_with_blocked("ads.example");
        // Baseline: the ad host blocks, attributed to the page that caused it.
        // (`is_secure` mirrors what the proxy sets from the scheme; without
        // it the mixed-content rule, correctly, fires first.)
        let mut sub = RequestMeta::new("ads.example", ResourceType::Script)
            .with_url("https://ads.example/a.js");
        sub.is_secure = true;
        sub.referrer = Some("https://example.com/article".to_owned());
        assert_eq!(e.decide(&sub), Verdict::BlockAd);
        let (_, ad, _, _, _, _) = e.origin_breakdown("example.com");
        assert_eq!(ad, 1);
        let (_, ad_other, _, _, _, _) = e.origin_breakdown("other.test");
        assert_eq!(ad_other, 0);
        // Pause is exact-host on the page, never the tracker host.
        assert!(!e.is_paused("example.com"));
        assert_eq!(
            e.set_paused("Example.COM.", true).as_deref(),
            Some("example.com")
        );
        assert!(e.is_paused("example.com"));
        assert!(!e.is_paused("ads.example"));
        assert_eq!(e.decide(&sub), Verdict::Allow);
        // Resume restores blocking.
        e.set_paused("example.com", false);
        assert!(!e.is_paused("example.com"));
        assert_eq!(e.decide(&sub), Verdict::BlockAd);
        // Empty and garbage pause targets refuse without storing.
        assert_eq!(e.set_paused("  ", true), None);
        // Threat-listed hosts pause too (explicit consent), then resume.
        let evil = RequestMeta::new("malware.test", ResourceType::Document)
            .with_url("https://malware.test/");
        e.load_threat_snapshot(&["malware.test"]).unwrap();
        assert_eq!(e.decide(&evil), Verdict::BlockMalware);
        e.set_paused("malware.test", true);
        assert_eq!(e.decide(&evil), Verdict::Allow);
        e.set_paused("malware.test", false);
    }

    #[test]
    fn page_host_attributes_subresources_to_referrer() {
        let doc = RequestMeta::new("example.com", ResourceType::Document)
            .with_url("https://example.com/");
        assert_eq!(PrivacyEngine::page_host(&doc), "example.com");
        let mut sub = RequestMeta::new("cdn.other", ResourceType::Script);
        sub.referrer = Some("https://example.com/a".to_owned());
        assert_eq!(PrivacyEngine::page_host(&sub), "example.com");
        // Unparseable referrer falls back to the request host, lowercased.
        let mut weird = RequestMeta::new("CDN.Other", ResourceType::Script);
        weird.referrer = Some(":::".to_owned());
        assert_eq!(PrivacyEngine::page_host(&weird), "cdn.other");
    }

    #[test]
    fn blocked_media_flags_ad_hosted_video() {
        let e = engine_with_blocked("ads.example");
        assert!(e.is_blocked_media(
            "https://ads.example/preroll.mp4",
            Some("https://example.com/watch")
        ));
        assert!(!e.is_blocked_media(
            "https://cdn.example/film.mp4",
            Some("https://example.com/watch")
        ));
        // Disabled adblock fails open: nothing is ever ad-flagged.
        e.update_settings(|s| s.adblock_enabled = false);
        assert!(!e.is_blocked_media(
            "https://ads.example/preroll.mp4",
            Some("https://example.com/watch")
        ));
    }

    #[test]
    fn verdict_counters_record_every_decision() {
        let e = PrivacyEngine::new().unwrap();
        e.load_filters(&[FilterList {
            name: "t",
            category: ListCategory::Ads,
            text: "||ads.example^",
            expected_sha256: None,
        }])
        .unwrap();
        let allow = RequestMeta::new("example.com", ResourceType::Document);
        let block = RequestMeta::new("ads.example", ResourceType::Script);
        assert_eq!(e.decide(&allow), Verdict::Allow);
        assert_eq!(e.decide(&block), Verdict::BlockAd);
        assert_eq!(e.decide(&block), Verdict::BlockAd);
        let (allow_n, ad_n, tracker_n, fp_n, malware_n, mixed_n) = e.verdict_counts();
        assert_eq!((allow_n, ad_n), (1, 2));
        assert_eq!((tracker_n, fp_n, malware_n, mixed_n), (0, 0, 0, 0));
    }

    #[test]
    fn fingerprinting_blocks_as_tracker_and_counts_apart() {
        let e = PrivacyEngine::new().unwrap();
        e.load_filters(&[FilterList {
            name: "fp",
            category: ListCategory::Fingerprinting,
            text: "||fp.example^\n",
            expected_sha256: None,
        }])
        .unwrap();
        let meta = RequestMeta::new("fp.example", ResourceType::Script);
        // Same engine action as a tracker (proxy path unchanged)...
        assert_eq!(e.decide(&meta), Verdict::BlockTracker);
        // ...but the shield tag survives separately, and the host still
        // lands in the cookie-stripping set.
        let (_, _, tracker_n, fp_n, _, _) = e.verdict_counts();
        assert_eq!((tracker_n, fp_n), (1, 1));
        assert!(e.should_strip_cookies("fp.example"));
    }

    #[test]
    fn cosmetic_selectors_union_and_toggle_gate() {
        let e = PrivacyEngine::new().unwrap();
        e.load_filters(&[
            FilterList {
                name: "a",
                category: ListCategory::Ads,
                text: "example.com##.ad-banner\n",
                expected_sha256: None,
            },
            FilterList {
                name: "b",
                category: ListCategory::Tracking,
                text: "example.com##.sponsor\n",
                expected_sha256: None,
            },
        ])
        .unwrap();
        let mut sels = e.cosmetic_selectors("https://example.com/");
        sels.sort();
        assert_eq!(sels, vec![".ad-banner".to_owned(), ".sponsor".to_owned()]);
        // Toggle off means no hiding anywhere on the answer path.
        e.update_settings(|s| s.adblock_enabled = false);
        assert!(e.cosmetic_selectors("https://example.com/").is_empty());
    }

    #[test]
    fn lookalike_blocks_top_level_with_detail() {
        fn now() -> i64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        }
        let e = PrivacyEngine::new().unwrap();
        e.set_frequent_hosts(vec!["paypal.com".to_owned()]);
        let meta = RequestMeta::new("paypa1.com", ResourceType::Document)
            .with_url("https://paypa1.com/login");
        assert_eq!(e.decide(&meta), Verdict::BlockMalware);
        // Detail carries the evidence for the interstitial copy.
        assert_eq!(
            e.block_detail("paypa1.com", now() + 1000),
            Some(BlockWhy::Lookalike {
                matched: "paypal.com".to_owned()
            })
        );
        // Self is not its own lookalike; subresources inherit the top
        // page (never judged alone).
        let meta = RequestMeta::new("paypal.com", ResourceType::Document);
        assert_eq!(e.decide(&meta), Verdict::Allow);
        let sub = RequestMeta::new("paypa1.com", ResourceType::Script)
            .with_url("https://paypa1.com/x.js");
        assert_eq!(e.decide(&sub), Verdict::Allow);
    }

    #[test]
    fn form_risk_blocks_credential_post_with_detail() {
        fn now() -> i64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        }
        let e = PrivacyEngine::new().unwrap();
        // Same-registrable pairs are not signals.
        assert!(!e.note_form_risk(
            "https://a.bank.com/login",
            "https://sso.bank.com/auth",
            now()
        ));
        // Garbage in, nothing stored.
        assert!(!e.note_form_risk("not a url", "https://x.test/", now()));
        let at = now();
        assert!(e.note_form_risk(
            "https://shop.test/checkout",
            "https://steal.test/collect",
            at
        ));
        let mut meta = RequestMeta::new("steal.test", ResourceType::Document)
            .with_url("https://steal.test/collect");
        meta.referrer = Some("https://shop.test/checkout".to_owned());
        meta.is_secure = true;
        assert_eq!(e.decide(&meta), Verdict::BlockMalware);
        assert_eq!(
            e.block_detail("steal.test", at + 1000),
            Some(BlockWhy::FormRisk {
                action_host: "steal.test".to_owned()
            })
        );
        // Stale pair (recorded past its TTL): re-prompts normally.
        let e2 = PrivacyEngine::new().unwrap();
        assert!(e2.note_form_risk(
            "https://shop.test/checkout",
            "https://old.test/collect",
            at - FORM_RISK_TTL_MS - 1
        ));
        let mut meta = RequestMeta::new("old.test", ResourceType::Document)
            .with_url("https://old.test/collect");
        meta.referrer = Some("https://shop.test/checkout".to_owned());
        meta.is_secure = true;
        assert_eq!(e2.decide(&meta), Verdict::Allow);
        assert_eq!(e2.block_detail("old.test", at + 1000), None);
    }

    #[test]
    fn threat_exception_overrides_lists_and_heuristics() {
        let e = PrivacyEngine::new().unwrap();
        e.load_threat_snapshot(&["evil.test"]).unwrap();
        e.set_frequent_hosts(vec!["paypal.com".to_owned()]);
        let meta = RequestMeta::new("evil.test", ResourceType::Document);
        assert_eq!(e.decide(&meta), Verdict::BlockMalware);
        e.set_threat_exceptions(vec!["EVIL.test".to_owned()]);
        // Exact host wins over a confirmed-list verdict (loud by design).
        assert_eq!(e.decide(&meta), Verdict::Allow);
        // ...but not over transport safety, and not for subdomains.
        let mut mixed = RequestMeta::new("evil.test", ResourceType::Image);
        mixed.is_secure = false;
        mixed.referrer = Some("https://example.com".to_owned());
        assert!(mixed.is_mixed_content());
        assert_eq!(e.decide(&mixed), Verdict::BlockMixedContent);
        let sub = RequestMeta::new("sub.evil.test", ResourceType::Document);
        assert_eq!(e.decide(&sub), Verdict::BlockMalware);
    }

    #[test]
    fn fail_open_counter_starts_at_zero_and_records() {
        let e = PrivacyEngine::new().unwrap();
        assert_eq!(e.fail_open_count(), 0);
        e.verdict_counts.record_fail_open();
        e.verdict_counts.record_fail_open();
        assert_eq!(e.fail_open_count(), 2);
        // Terminal verdict counters are unaffected by fail-open records.
        let meta = RequestMeta::new("example.com", ResourceType::Document);
        assert_eq!(e.decide(&meta), Verdict::Allow);
        assert_eq!(e.fail_open_count(), 2);
        assert_eq!(e.verdict_counts().0, 1);
    }

    #[test]
    fn recent_tracker_hosts_returns_newest_first() {
        let mut set = TrackerSet::new(10, std::time::Duration::from_secs(3600));
        assert!(set.recent(5).is_empty());
        set.insert("a.test".to_owned());
        set.insert("b.test".to_owned());
        assert_eq!(
            set.recent(5),
            vec!["b.test".to_owned(), "a.test".to_owned()]
        );
        assert_eq!(set.recent(1), vec!["b.test".to_owned()]);
    }

    #[test]
    fn updater_applies_fetched_once_and_retains_last_good() {
        let e = PrivacyEngine::new().unwrap();
        // Empty fetch on a fresh engine retains (still allow, no crash).
        e.apply_update_once(vec![], None);
        assert_eq!(
            e.decide(&RequestMeta::new("ads.example", ResourceType::Script)),
            Verdict::Allow
        );
        // Real rules load through the once-helper.
        e.apply_update_once(
            vec![FilterFetch {
                name: "t".to_owned(),
                category: ListCategory::Ads,
                text: "||ads.example^\n".to_owned(),
                sha256: None,
            }],
            None,
        );
        assert_eq!(
            e.decide(&RequestMeta::new("ads.example", ResourceType::Script)),
            Verdict::BlockAd
        );
        // Empty retains last good.
        e.apply_update_once(vec![], None);
        assert_eq!(
            e.decide(&RequestMeta::new("ads.example", ResourceType::Script)),
            Verdict::BlockAd
        );
        // Threat snapshot loads; None retains.
        e.apply_update_once(vec![], Some(vec!["evil.test".to_owned()]));
        assert_eq!(
            e.decide(&RequestMeta::new("evil.test", ResourceType::Document)),
            Verdict::BlockMalware
        );
        e.apply_update_once(vec![], None);
        assert_eq!(
            e.decide(&RequestMeta::new("evil.test", ResourceType::Document)),
            Verdict::BlockMalware
        );
    }

    #[test]
    fn hourly_updater_fetches_immediately_on_spawn() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = "||ads.example^\n";
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut acc = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match s.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            acc.extend_from_slice(&buf[..n]);
                            if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        let url = format!("http://127.0.0.1:{port}/list.txt");
        let engine = std::sync::Arc::new(PrivacyEngine::new().unwrap());
        assert_eq!(
            engine.decide(&RequestMeta::new("ads.example", ResourceType::Script)),
            Verdict::Allow
        );
        engine.spawn_hourly_updater(
            {
                let url = url.clone();
                move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    match rt.block_on(crate::updater::fetch_direct(&url)) {
                        Some((text, sha)) => vec![FilterFetch {
                            name: "test".to_owned(),
                            category: ListCategory::Ads,
                            text,
                            sha256: sha,
                        }],
                        None => vec![],
                    }
                }
            },
            || None,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if engine.decide(&RequestMeta::new("ads.example", ResourceType::Script))
                == Verdict::BlockAd
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "immediate fetch did not load rules"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn realtime_api_key_defaults_none_and_roundtrips() {
        let s: PrivacySettings = serde_json::from_str("{}").unwrap();
        assert!(s.realtime_api_key.is_none());
        assert!(PrivacySettings::default().realtime_api_key.is_none());
        let e = PrivacyEngine::new().unwrap();
        e.update_settings(|s| s.realtime_api_key = Some("k123".to_owned()));
        let json = e.export_settings_json();
        let e2 = PrivacyEngine::new().unwrap();
        e2.import_settings_json(&json);
        assert_eq!(e2.settings().realtime_api_key.as_deref(), Some("k123"));
        // Empty-means-absent: blank normalizes to None (no egress).
        e2.import_settings_json(r#"{"realtime_api_key":""}"#);
        assert!(e2.settings().realtime_api_key.is_none());
        e2.import_settings_json(r#"{"realtime_api_key":"   "}"#);
        assert!(e2.settings().realtime_api_key.is_none());
        // Old files without the key still parse.
        let e3 = PrivacyEngine::new().unwrap();
        e3.import_settings_json(r#"{"adblock_enabled":true}"#);
        assert!(e3.settings().realtime_api_key.is_none());
    }

    #[test]
    fn realtime_block_without_key_is_noop_without_egress() {
        let e = PrivacyEngine::new().unwrap();
        e.update_settings(|s| {
            s.realtime_lookup_enabled = true;
            s.realtime_api_key = None;
        });
        let start = std::time::Instant::now();
        assert!(!e.realtime_block("example.com"));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "absent key must not do network I/O"
        );
        e.update_settings(|s| s.realtime_api_key = Some("".to_owned()));
        assert!(!e.realtime_block("example.com"));
        assert_eq!(e.fail_open_count(), 0, "no-op is not degradation");
    }
}
