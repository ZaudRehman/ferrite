//! Permission broker. Fail-closed sync decisions plus a pending queue. The
//! wry handler carries no origin, so the factory mirrors latest origins.
//! Revocation applies to the next request, never live streams.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use uuid::Uuid;

/// Denies per (origin, kind) before silent auto block.
pub const QUIET_AFTER_DENIES: u32 = 3;
/// Auto block duration. Expiry lets the origin prompt again.
pub const COOLDOWN_MS: i64 = 24 * 3600 * 1000;
/// Pending queue cap. Oldest rows drop first.
// ponytail: raise only with a per tab cap alongside.
pub const PENDING_CAP: usize = 32;

/// Why a row exists. The native store holds allow or deny. The why lives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantSource {
    User,
    AutoCooldown,
}

impl GrantSource {
    pub fn as_str(self) -> &'static str {
        match self {
            GrantSource::User => "user",
            GrantSource::AutoCooldown => "auto_cooldown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(GrantSource::User),
            "auto_cooldown" => Some(GrantSource::AutoCooldown),
            _ => None,
        }
    }
}

/// One persisted row: a permanent user grant, or an active auto-block.
/// `expires_at_ms` is `None` for user grants (never expire) and `Some`
/// for cooldowns. One-time grants vanish after use and are never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGrant {
    pub origin: String,
    pub kind: String,
    pub source: GrantSource,
    pub granted_at_ms: i64,
    pub expires_at_ms: Option<i64>,
}

/// One denied request awaiting a user decision in the omnibox popover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRequest {
    pub tab_id: Uuid,
    pub origin: String,
    pub kind: String,
    pub at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AlwaysMeta {
    source: GrantSource,
    granted_at_ms: i64,
}

#[derive(Debug, Default)]
struct State {
    /// (origin, kind) granted for all future requests (persisted).
    always: HashMap<(String, String), AlwaysMeta>,
    /// Session-scoped always-grants from private tabs / incognito: enforced
    /// like `always` but never exported to the Vault snapshot. Dies with the
    /// process. This is what keeps "Always allow" inside a private tab from
    /// becoming a persistent row.
    ephemeral_always: HashMap<(String, String), AlwaysMeta>,
    /// (origin, kind) granted once (consumed by the next request).
    once: HashSet<(String, String)>,
    pending: Vec<PendingRequest>,
    /// Consecutive denied requests per (origin, kind) with no grant between.
    denied: HashMap<(String, String), u32>,
    /// (origin, kind) auto-blocked until `until_ms` (persisted while active).
    cooldown: HashMap<(String, String), i64>,
    /// Global notifications kill switch. When set, every `notifications`
    /// request denies silently, no queue, no backoff.
    notifications_muted: bool,
}

#[derive(Default)]
pub struct PermissionStore {
    state: Mutex<State>,
    /// Write-through hook the shell sets when a Vault is available: fired
    /// (outside the state lock) whenever `decide` trips or expires an
    /// auto-block, so automation state survives restarts. User-grant paths
    /// persist shell-side instead, same snapshot, one code path there.
    sink: Mutex<Option<SnapshotSink>>,
}

impl std::fmt::Debug for PermissionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionStore").finish_non_exhaustive()
    }
}

/// Write-through hook: receives the full post-mutation snapshot.
pub type SnapshotSink = std::sync::Arc<dyn Fn(Vec<StoredGrant>) + Send + Sync>;

impl PermissionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the persistence sink (shell boot, when a Vault exists).
    /// Replaces any previous sink. Never called on the hot path.
    pub fn set_snapshot_sink(&self, sink: SnapshotSink) {
        if let Ok(mut slot) = self.sink.lock() {
            *slot = Some(sink);
        }
    }

    /// Global notifications kill switch, persisted by the shell. `false`
    /// denies every future `notifications` request silently. In-flight
    /// grants are unaffected (revocation is next-request, never live).
    pub fn set_notifications_global(&self, on: bool) {
        if let Ok(mut st) = self.state.lock() {
            st.notifications_muted = !on;
        }
    }

    /// Current kill-switch state (`true` = notifications enabled).
    pub fn notifications_global(&self) -> bool {
        self.state
            .lock()
            .map(|st| !st.notifications_muted)
            .unwrap_or(true)
    }

    /// Drop expired cooldown rows. Maps stay tiny (bounded by distinct
    /// origins the user actually visits), so a full sweep per decision is
    /// cheaper than tracking dirty flags.
    fn purge_expired(st: &mut State, now_ms: i64) {
        st.cooldown.retain(|_, until| *until > now_ms);
    }

    /// Sync decision for one request. `true` = allow. Denials for
    /// http(s) origins queue a pending row and count backoff. A tripped
    /// origin auto-blocks silently until `COOLDOWN_MS` passes.
    pub fn decide(&self, tab_id: Uuid, origin: &str, kind: &str, now_ms: i64) -> bool {
        let key = (origin.to_owned(), kind.to_owned());
        // Snapshot to persist after the lock drops (the sink re-locks for
        // export, firing it under the guard would deadlock).
        let mut snapshot: Option<Vec<StoredGrant>> = None;
        let allowed = {
            let Ok(mut st) = self.state.lock() else {
                return false;
            };
            if kind == "notifications" && st.notifications_muted {
                return false;
            }
            let cooldown_before = st.cooldown.len();
            Self::purge_expired(&mut st, now_ms);
            let purged = st.cooldown.len() != cooldown_before;
            if st.cooldown.contains_key(&key) {
                if purged {
                    snapshot = Some(Self::snapshot_locked(&st, now_ms));
                }
                false
            } else if st.always.contains_key(&key) || st.ephemeral_always.contains_key(&key) {
                if purged {
                    snapshot = Some(Self::snapshot_locked(&st, now_ms));
                }
                true
            } else if st.once.remove(&key) {
                st.denied.remove(&key);
                if purged {
                    snapshot = Some(Self::snapshot_locked(&st, now_ms));
                }
                true
            } else if origin.starts_with("http://") || origin.starts_with("https://") {
                if !st
                    .pending
                    .iter()
                    .any(|p| p.tab_id == tab_id && p.origin == origin && p.kind == kind)
                {
                    if st.pending.len() >= PENDING_CAP {
                        st.pending.remove(0);
                    }
                    st.pending.push(PendingRequest {
                        tab_id,
                        origin: origin.to_owned(),
                        kind: kind.to_owned(),
                        at_ms: now_ms,
                    });
                }
                let n = st.denied.entry(key.clone()).or_insert(0);
                *n = n.saturating_add(1);
                // Persist only on change (trip or expiry), never per deny:
                // a chatty origin re-asking every navigation must not
                // rewrite the Vault on every single refusal.
                let mut changed = purged;
                if *n >= QUIET_AFTER_DENIES
                    && st
                        .cooldown
                        .insert(key, now_ms.saturating_add(COOLDOWN_MS))
                        .is_none()
                {
                    changed = true;
                }
                if changed {
                    snapshot = Some(Self::snapshot_locked(&st, now_ms));
                }
                false
            } else {
                if purged {
                    snapshot = Some(Self::snapshot_locked(&st, now_ms));
                }
                false
            }
        };
        if let Some(snap) = snapshot
            && let Ok(slot) = self.sink.lock()
            && let Some(sink) = slot.as_ref()
        {
            sink(snap);
        }
        allowed
    }

    /// Snapshot builder for a locked state (no purge, no lock, callers
    /// own both decisions already).
    fn snapshot_locked(st: &State, now_ms: i64) -> Vec<StoredGrant> {
        let mut out: Vec<StoredGrant> = st
            .always
            .iter()
            .map(|((o, k), m)| StoredGrant {
                origin: o.clone(),
                kind: k.clone(),
                source: m.source,
                granted_at_ms: m.granted_at_ms,
                expires_at_ms: None,
            })
            .collect();
        out.extend(
            st.cooldown
                .iter()
                .filter(|(_, until)| **until > now_ms)
                .map(|((o, k), until)| StoredGrant {
                    origin: o.clone(),
                    kind: k.clone(),
                    source: GrantSource::AutoCooldown,
                    granted_at_ms: now_ms,
                    expires_at_ms: Some(*until),
                }),
        );
        out.sort_by(|a, b| (&a.origin, &a.kind).cmp(&(&b.origin, &b.kind)));
        out
    }

    /// Allow the next request for (origin, kind). Clears backoff and any
    /// auto-block: an explicit user decision always wins over automation.
    pub fn grant_once(&self, origin: &str, kind: &str) {
        if let Ok(mut st) = self.state.lock() {
            let key = (origin.to_owned(), kind.to_owned());
            st.once.insert(key.clone());
            st.denied.remove(&key);
            st.cooldown.remove(&key);
            st.pending
                .retain(|p| !(p.origin == origin && p.kind == kind));
        }
    }

    /// Allow all future requests for (origin, kind). Clears backoff and
    /// auto-block. Caller persists [`Self::export_snapshot`] (memory alone
    /// would make "always" a lie across restarts).
    pub fn grant_always(&self, origin: &str, kind: &str, now_ms: i64) {
        if let Ok(mut st) = self.state.lock() {
            let key = (origin.to_owned(), kind.to_owned());
            st.always.insert(
                key.clone(),
                AlwaysMeta {
                    source: GrantSource::User,
                    granted_at_ms: now_ms,
                },
            );
            st.ephemeral_always.remove(&key);
            st.denied.remove(&key);
            st.cooldown.remove(&key);
            st.pending
                .retain(|p| !(p.origin == origin && p.kind == kind));
        }
    }

    /// Session-scoped "always" for private tabs / incognito: enforced for the
    /// process lifetime, never exported (see [`Self::export_snapshot`]).
    /// The shell calls this when the deciding tab `is_private` or the process
    /// session is ephemeral, so a private "always" can't become persistent.
    pub fn grant_always_ephemeral(&self, origin: &str, kind: &str, now_ms: i64) {
        if let Ok(mut st) = self.state.lock() {
            let key = (origin.to_owned(), kind.to_owned());
            // Never shadow a persistent grant with an ephemeral twin: the
            // persistent row already allows, adding a twin only complicates
            // revocation semantics.
            if !st.always.contains_key(&key) {
                st.ephemeral_always.insert(
                    key.clone(),
                    AlwaysMeta {
                        source: GrantSource::User,
                        granted_at_ms: now_ms,
                    },
                );
            }
            st.denied.remove(&key);
            st.cooldown.remove(&key);
            st.pending
                .retain(|p| !(p.origin == origin && p.kind == kind));
        }
    }

    /// Drop all session-scoped grants (last private tab closed / explicit
    /// reset). Persistent rows untouched.
    pub fn clear_ephemeral(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.ephemeral_always.clear();
        }
    }

    /// Pending rows for one tab (chip + popover source).
    pub fn pending_for(&self, tab_id: Uuid) -> Vec<PendingRequest> {
        self.state
            .lock()
            .map(|st| {
                st.pending
                    .iter()
                    .filter(|p| p.tab_id == tab_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the chip for `origin` stays quiet (backoff tripped for any
    /// of its kinds). Cooldown expiry is evaluated against `now_ms`.
    pub fn quiet(&self, origin: &str, now_ms: i64) -> bool {
        self.state
            .lock()
            .map(|mut st| {
                Self::purge_expired(&mut st, now_ms);
                st.cooldown.keys().any(|(o, _)| o == origin)
            })
            .unwrap_or(false)
    }

    /// Drop rows for tabs that no longer exist.
    pub fn retain_tabs(&self, live: &std::collections::HashSet<Uuid>) {
        if let Ok(mut st) = self.state.lock() {
            st.pending.retain(|p| live.contains(&p.tab_id));
        }
    }

    /// Always-grants for settings persistence (user rows only, cooldowns
    /// are automation state, persisted separately via the snapshot).
    pub fn always_grants(&self) -> Vec<(String, String)> {
        self.state
            .lock()
            .map(|st| {
                st.always
                    .iter()
                    .filter(|(_, m)| m.source == GrantSource::User)
                    .map(|((o, k), _)| (o.clone(), k.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Restore always-grants at boot (legacy plaintext pairs migrate in as
    /// user grants stamped at `now_ms`).
    pub fn load_always(&self, grants: Vec<(String, String)>, now_ms: i64) {
        if let Ok(mut st) = self.state.lock() {
            st.always = grants
                .into_iter()
                .map(|(o, k)| {
                    (
                        (o, k),
                        AlwaysMeta {
                            source: GrantSource::User,
                            granted_at_ms: now_ms,
                        },
                    )
                })
                .collect();
        }
    }

    /// Full persistence snapshot: persistent user grants plus active
    /// (unexpired) auto-blocks. Session-scoped (`grant_always_ephemeral`)
    /// rows are deliberately excluded; exporting them would persist a
    /// private-tab decision to disk. The shell writes this to Vault on every
    /// mutation.
    pub fn export_snapshot(&self, now_ms: i64) -> Vec<StoredGrant> {
        self.state
            .lock()
            .map(|mut st| {
                Self::purge_expired(&mut st, now_ms);
                Self::snapshot_locked(&st, now_ms)
            })
            .unwrap_or_default()
    }

    /// Restore a snapshot at boot. Expired cooldowns are dropped (their
    /// origin may prompt again). Unknown sources are rejected, never
    /// defaulted, a corrupt row must not become a grant.
    pub fn load_snapshot(&self, grants: Vec<StoredGrant>, now_ms: i64) {
        if let Ok(mut st) = self.state.lock() {
            st.always.clear();
            st.cooldown.clear();
            for g in grants {
                match g.source {
                    GrantSource::User => {
                        st.always.insert(
                            (g.origin, g.kind),
                            AlwaysMeta {
                                source: GrantSource::User,
                                granted_at_ms: g.granted_at_ms,
                            },
                        );
                    }
                    GrantSource::AutoCooldown => {
                        if let Some(until) = g.expires_at_ms
                            && until > now_ms
                        {
                            st.cooldown.insert((g.origin, g.kind), until);
                        }
                    }
                }
            }
        }
    }

    /// Revoke one always-grant (settings UI reset action). Returns whether
    /// it existed. Also clears backoff/cooldown for the pair. An explicit
    /// reset wins over automation either way.
    pub fn revoke_always(&self, origin: &str, kind: &str) -> bool {
        self.state
            .lock()
            .map(|mut st| {
                let key = (origin.to_owned(), kind.to_owned());
                st.denied.remove(&key);
                st.cooldown.remove(&key);
                let persistent = st.always.remove(&key).is_some();
                let ephemeral = st.ephemeral_always.remove(&key).is_some();
                persistent || ephemeral
            })
            .unwrap_or(false)
    }
}

/// `https://example.com/page` to `Some("https://example.com")`.
/// Anything else to `None` (never grantable).
pub fn origin_of(uri: &str) -> Option<String> {
    let lower = uri.trim().to_lowercase();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = lower.strip_prefix(scheme) {
            let host = rest.split(['/', '?', '#']).next().unwrap_or("");
            let host = host.rsplit('@').next().unwrap_or(host);
            // Default ports are the same origin (no re-prompt for :443).
            let host = match scheme {
                "https://" => host.strip_suffix(":443").unwrap_or(host),
                _ => host.strip_suffix(":80").unwrap_or(host),
            };
            if host.is_empty() {
                return None;
            }
            return Some(format!("{scheme}{host}"));
        }
    }
    None
}

/// Short UI label for a wry kind string (`PermissionKind::to_string`).
pub fn kind_label(kind: &str) -> &str {
    match kind {
        "camera" => "Camera",
        "microphone" => "Microphone",
        "geolocation" => "Location",
        "notifications" => "Notifications",
        "clipboard-read" => "Clipboard",
        _ => kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(store: &PermissionStore, origin: &str, kind: &str) -> bool {
        store.decide(Uuid::new_v4(), origin, kind, 1000)
    }

    #[test]
    fn default_is_deny_and_queue() {
        let s = PermissionStore::new();
        let tab = Uuid::new_v4();
        assert!(!s.decide(tab, "https://a.test", "camera", 1));
        let p = s.pending_for(tab);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].origin, "https://a.test");
        // Same request re-asked: no duplicate row.
        assert!(!s.decide(tab, "https://a.test", "camera", 2));
        assert_eq!(s.pending_for(tab).len(), 1);
    }

    #[test]
    fn once_grant_consumed() {
        let s = PermissionStore::new();
        s.grant_once("https://a.test", "camera");
        assert!(req(&s, "https://a.test", "camera"));
        assert!(!req(&s, "https://a.test", "camera"));
        assert!(s.pending_for(Uuid::new_v4()).is_empty());
    }

    #[test]
    fn always_grant_persists_in_memory() {
        let s = PermissionStore::new();
        s.grant_always("https://a.test", "microphone", 1000);
        assert!(req(&s, "https://a.test", "microphone"));
        assert!(req(&s, "https://a.test", "microphone"));
        assert_eq!(
            s.always_grants(),
            vec![("https://a.test".to_owned(), "microphone".to_owned())]
        );
        let r = PermissionStore::new();
        r.load_always(s.always_grants(), 1000);
        assert!(r.decide(Uuid::new_v4(), "https://a.test", "microphone", 5));
    }

    #[test]
    fn revoke_always_drops_grant() {
        let s = PermissionStore::new();
        s.grant_always("https://a.test", "camera", 1000);
        assert!(s.revoke_always("https://a.test", "camera"));
        assert!(!s.revoke_always("https://a.test", "camera"));
        assert!(!s.decide(Uuid::new_v4(), "https://a.test", "camera", 9));
    }

    #[test]
    fn backoff_quiets_after_three_denies() {
        let s = PermissionStore::new();
        assert!(!s.quiet("https://spam.test", 1000));
        for _ in 0..3 {
            assert!(!req(&s, "https://spam.test", "notifications"));
        }
        assert!(s.quiet("https://spam.test", 1000));
        s.grant_once("https://spam.test", "notifications");
        assert!(!s.quiet("https://spam.test", 1000));
    }

    #[test]
    fn tripped_origin_denies_silently_without_queue() {
        let s = PermissionStore::new();
        let tab = Uuid::new_v4();
        for i in 0..3 {
            assert!(!s.decide(tab, "https://spam.test", "camera", 1000 + i));
        }
        // Auto-blocked: further requests deny and queue nothing new.
        let before = s.pending_for(tab).len();
        assert!(!s.decide(tab, "https://spam.test", "camera", 2000));
        assert_eq!(s.pending_for(tab).len(), before);
        // Other kinds on the same origin still prompt (per-kind backoff).
        assert!(!s.decide(tab, "https://spam.test", "microphone", 2000));
        assert!(s.pending_for(tab).iter().any(|p| p.kind == "microphone"));
    }

    #[test]
    fn cooldown_expires_and_origin_may_prompt_again() {
        let s = PermissionStore::new();
        for _ in 0..3 {
            assert!(!req(&s, "https://spam.test", "camera"));
        }
        assert!(s.quiet("https://spam.test", 1000));
        // Past COOLDOWN_MS: quiet lifts, next request queues again.
        let later = 1000 + COOLDOWN_MS + 1;
        assert!(!s.quiet("https://spam.test", later));
        let tab = Uuid::new_v4();
        assert!(!s.decide(tab, "https://spam.test", "camera", later));
        assert_eq!(s.pending_for(tab).len(), 1);
    }

    #[test]
    fn user_grant_beats_auto_block() {
        let s = PermissionStore::new();
        for _ in 0..3 {
            assert!(!req(&s, "https://spam.test", "camera"));
        }
        assert!(s.quiet("https://spam.test", 1000));
        s.grant_always("https://spam.test", "camera", 2000);
        assert!(!s.quiet("https://spam.test", 2000));
        assert!(req(&s, "https://spam.test", "camera"));
    }

    #[test]
    fn snapshot_roundtrips_sources_and_drops_expired() {
        let s = PermissionStore::new();
        s.grant_always("https://a.test", "camera", 1000);
        for _ in 0..3 {
            assert!(!s.decide(Uuid::new_v4(), "https://spam.test", "microphone", 1000));
        }
        let snap = s.export_snapshot(1000);
        assert_eq!(snap.len(), 2);
        let user = snap.iter().find(|g| g.origin == "https://a.test").unwrap();
        assert_eq!(user.source, GrantSource::User);
        assert_eq!(user.expires_at_ms, None);
        let auto = snap
            .iter()
            .find(|g| g.origin == "https://spam.test")
            .unwrap();
        assert_eq!(auto.source, GrantSource::AutoCooldown);
        assert_eq!(auto.expires_at_ms, Some(1000 + COOLDOWN_MS));
        // Restore into a fresh store: both enforced.
        let r = PermissionStore::new();
        r.load_snapshot(snap, 1000);
        assert!(r.decide(Uuid::new_v4(), "https://a.test", "camera", 1000));
        assert!(r.quiet("https://spam.test", 1000));
        // Restored after expiry: cooldown gone, user grant intact.
        let r2 = PermissionStore::new();
        r2.load_snapshot(r.export_snapshot(1000), 1000 + COOLDOWN_MS + 1);
        assert!(!r2.quiet("https://spam.test", 1000 + COOLDOWN_MS + 1));
        assert!(r2.decide(
            Uuid::new_v4(),
            "https://a.test",
            "camera",
            1000 + COOLDOWN_MS + 1
        ));
    }

    #[test]
    fn sink_fires_on_trip_and_expiry_not_per_deny() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let s = PermissionStore::new();
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let h = std::sync::Arc::clone(&hits);
        s.set_snapshot_sink(std::sync::Arc::new(move |_| {
            h.fetch_add(1, Ordering::SeqCst);
        }));
        // Five denials, one trip: exactly one write-through.
        for _ in 0..5 {
            assert!(!req(&s, "https://spam.test", "camera"));
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // Expiry purge on the next decision: exactly one more.
        assert!(!s.decide(
            Uuid::new_v4(),
            "https://spam.test",
            "camera",
            1000 + COOLDOWN_MS + 1
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn grant_source_strings() {
        assert_eq!(GrantSource::User.as_str(), "user");
        assert_eq!(GrantSource::AutoCooldown.as_str(), "auto_cooldown");
        assert_eq!(GrantSource::parse("user"), Some(GrantSource::User));
        assert_eq!(
            GrantSource::parse("auto_cooldown"),
            Some(GrantSource::AutoCooldown)
        );
        assert_eq!(GrantSource::parse("allow"), None);
        assert_eq!(GrantSource::parse(""), None);
    }

    #[test]
    fn non_http_never_grants_nor_queues() {
        let s = PermissionStore::new();
        let tab = Uuid::new_v4();
        assert!(!s.decide(tab, "ferrite://settings", "camera", 1));
        assert!(s.pending_for(tab).is_empty());
    }

    #[test]
    fn retain_tabs_drops_dead_rows() {
        let s = PermissionStore::new();
        let live = Uuid::new_v4();
        let dead = Uuid::new_v4();
        s.decide(live, "https://a.test", "camera", 1);
        s.decide(dead, "https://b.test", "camera", 1);
        let mut set = std::collections::HashSet::new();
        set.insert(live);
        s.retain_tabs(&set);
        assert_eq!(s.pending_for(live).len(), 1);
        assert!(s.pending_for(dead).is_empty());
    }

    #[test]
    fn origin_parsing() {
        assert_eq!(
            origin_of("https://Example.COM:443/a?b#c"),
            Some("https://example.com".to_owned())
        );
        assert_eq!(
            origin_of("http://a.test/"),
            Some("http://a.test".to_owned())
        );
        assert_eq!(origin_of("ferrite://settings"), None);
        assert_eq!(origin_of("not a url"), None);
        assert_eq!(origin_of(""), None);
    }

    #[test]
    fn kind_labels() {
        assert_eq!(kind_label("camera"), "Camera");
        assert_eq!(kind_label("geolocation"), "Location");
        assert_eq!(kind_label("clipboard-read"), "Clipboard");
        assert_eq!(kind_label("midi"), "midi");
    }

    #[test]
    fn clipboard_read_uses_the_broker_like_camera() {
        // Contract: clipboard *reads* gate through decide/queue/grant
        // exactly like camera/mic; writes are never gated (no kind for
        // them exists, so nothing can queue one).
        let s = PermissionStore::new();
        let tab = Uuid::new_v4();
        assert!(!s.decide(tab, "https://a.test", "clipboard-read", 1));
        assert!(
            s.pending_for(tab)
                .iter()
                .any(|p| p.kind == "clipboard-read")
        );
        s.grant_once("https://a.test", "clipboard-read");
        assert!(req(&s, "https://a.test", "clipboard-read"));
        // Consumed: the next read prompts again.
        assert!(!req(&s, "https://a.test", "clipboard-read"));
    }

    #[test]
    fn notifications_kill_switch_denies_silently() {
        let s = PermissionStore::new();
        assert!(s.notifications_global());
        let tab = Uuid::new_v4();
        // Baseline: prompts like any other kind.
        assert!(!s.decide(tab, "https://a.test", "notifications", 1));
        assert_eq!(s.pending_for(tab).len(), 1);
        // Muted: silent deny, no queue, no backoff rows.
        s.set_notifications_global(false);
        assert!(!s.notifications_global());
        let tab2 = Uuid::new_v4();
        assert!(!s.decide(tab2, "https://b.test", "notifications", 2));
        assert!(s.pending_for(tab2).is_empty());
        // Prior always-grants do not punch through the mute.
        s.set_notifications_global(true);
        s.grant_always("https://c.test", "notifications", 3);
        s.set_notifications_global(false);
        assert!(!s.decide(Uuid::new_v4(), "https://c.test", "notifications", 4));
        // Unmuted: broker behavior resumes.
        s.set_notifications_global(true);
        assert!(s.decide(Uuid::new_v4(), "https://c.test", "notifications", 5));
    }

    #[test]
    fn ephemeral_grant_enforced_but_never_exported() {
        let s = PermissionStore::new();
        s.grant_always_ephemeral("https://priv.test", "camera", 1000);
        // Enforced like a persistent grant.
        assert!(req(&s, "https://priv.test", "camera"));
        // Never exported: a restart (snapshot round-trip) drops it.
        assert!(s.export_snapshot(1000).is_empty());
        assert!(s.always_grants().is_empty());
        let r = PermissionStore::new();
        r.load_snapshot(s.export_snapshot(1000), 1000);
        assert!(!r.decide(Uuid::new_v4(), "https://priv.test", "camera", 1000));
        // Revocation clears the ephemeral twin too.
        assert!(s.revoke_always("https://priv.test", "camera"));
        assert!(!req(&s, "https://priv.test", "camera"));
        // Persistent grant wins: ephemeral twin is not created beside it.
        s.grant_always("https://a.test", "camera", 1000);
        s.grant_always_ephemeral("https://a.test", "camera", 1000);
        assert_eq!(s.export_snapshot(1000).len(), 1);
        s.clear_ephemeral();
        assert!(req(&s, "https://a.test", "camera"));
    }
}
