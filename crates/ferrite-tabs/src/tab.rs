//! Per-tab state: navigation stacks, lifecycle, backend handles.

use std::rc::Rc;
use std::time::Instant;

use tracing::debug;
use uuid::Uuid;

use crate::{error::TabResult, navigation::NavigationHistory, webview::WebViewHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TabLifecycle {
    Active,
    Suspended,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBackend {
    Live,
    Loading,
    Discarded,
    Crashed,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TabState {
    pub id: Uuid,
    pub url: String,
    pub title: String,
    #[serde(skip, default)]
    pub favicon: Option<Vec<u8>>,
    /// Engine-reported absolute icon URL (link rel=icon, resolved in-page).
    /// Bytes are fetched Rust-side through the proxy; the page never ships
    /// bitmaps over IPC. Skipped in session files, refetched on restore.
    #[serde(skip, default)]
    pub favicon_url: Option<String>,
    /// Ready-to-render `data:image/...;base64,...` (raster only, capped).
    /// Pushed to chrome as `TabRow.favicon`. Skipped in session files.
    #[serde(skip, default)]
    pub favicon_data: Option<String>,
    pub is_loading: bool,
    pub is_archive_view: bool,
    pub is_pdf_view: bool,
    pub is_passwords_view: bool,
    pub is_playing_audio: bool,
    pub is_muted: bool,
    pub is_pinned: bool,
    pub is_crashed: bool,
    /// Owning tab group. Pinned tabs never carry one (they live outside
    /// groups, left of the strip); the manager enforces that on assign.
    /// Persisted: groups restore with their members.
    #[serde(default)]
    pub group_id: Option<Uuid>,
    #[serde(skip, default)]
    pub is_private: bool,
    #[serde(skip, default)]
    pub has_unsaved_form: bool,
    #[serde(skip, default)]
    pub has_live_call: bool,
    #[serde(skip, default)]
    pub is_pip_active: bool,
    #[serde(skip, default)]
    pub pip_eligible: bool,
    /// Page-reported `lang` attribute (validated tag, e.g. `de`). Cleared
    /// on navigation; drives the translate offer. Never persisted (stale
    /// langs across restarts would offer wrongly).
    #[serde(skip, default)]
    pub page_lang: Option<String>,
    /// Active download owned by this tab (engine or dialog worker).
    /// Guards auto-sleep AND manual sleep: unloading mid-transfer loses
    /// data. Session-only (never persisted, restored tabs resume with no
    /// transfer in flight). Set/cleared by the shell from download events.
    #[serde(skip, default)]
    pub has_active_download: bool,
    /// Times this tab unloaded this session. Task-manager diagnostics
    /// (why-tabs-reload); resets on restore and never persisted.
    #[serde(skip, default)]
    pub discard_count: u32,
    #[serde(skip, default)]
    pub console_errors: u32,
    pub lifecycle: TabLifecycle,
    #[serde(skip, default)]
    pub suspended_since: Option<Instant>,
    #[serde(skip, default)]
    pub webview: Option<Rc<dyn WebViewHandle>>,
    pub history: NavigationHistory,
    pub scroll_position: (f32, f32),
    #[serde(skip, default)]
    pub suppress_next_history: bool,
    #[serde(skip, default)]
    pub last_nav_at_ms: i64,
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl TabState {
    pub fn new(id: Uuid, url: String, webview: Rc<dyn WebViewHandle>) -> Self {
        let is_archive_view = url.starts_with("ferrite://archive/");
        let is_pdf_view = url.ends_with(".pdf") || url.contains("ferrite://pdf/");
        let is_passwords_view = url.starts_with("ferrite://passwords");
        let title = if url.is_empty() {
            "New Tab".to_owned()
        } else {
            url.clone()
        };
        Self {
            id,
            url,
            title,
            favicon: None,
            favicon_url: None,
            favicon_data: None,
            is_loading: true,
            is_archive_view,
            is_pdf_view,
            is_passwords_view,
            is_playing_audio: false,
            is_muted: false,
            is_pinned: false,
            is_crashed: false,
            group_id: None,
            is_private: false,
            has_unsaved_form: false,
            has_live_call: false,
            is_pip_active: false,
            pip_eligible: false,
            page_lang: None,
            has_active_download: false,
            discard_count: 0,
            console_errors: 0,
            lifecycle: TabLifecycle::Active,
            suspended_since: None,
            webview: Some(webview),
            history: NavigationHistory::new(),
            scroll_position: (0.0, 0.0),
            suppress_next_history: false,
            last_nav_at_ms: now_millis(),
        }
    }

    /// Duplicate: same document lineage as the source: URL, title, icon,
    /// mute flag, back/forward stacks, scroll position. Fresh identity,
    /// fresh load, live signals reset (audio/PiP/console belong to the old
    /// page's elements, not the URL). Never pinned: the duplicate lands at
    /// the strip end, and a trailing pinned tab would break the pinned-left
    /// invariant every bulk action relies on. The initial load is
    /// history-suppressed: the visit is already recorded under the source.
    /// Private sources stay private: the flag pairs with the factory's
    /// private webview path, and callers must route them there (see
    /// `TabManager::duplicate_tab`, which enforces the pairing).
    pub fn duplicate_from(source: &TabState, id: Uuid, webview: Rc<dyn WebViewHandle>) -> Self {
        Self {
            id,
            url: source.url.clone(),
            title: source.title.clone(),
            favicon: source.favicon.clone(),
            favicon_url: source.favicon_url.clone(),
            favicon_data: source.favicon_data.clone(),
            is_loading: true,
            is_archive_view: source.is_archive_view,
            is_pdf_view: source.is_pdf_view,
            is_passwords_view: source.is_passwords_view,
            is_playing_audio: false,
            is_muted: source.is_muted,
            is_pinned: false,
            is_crashed: false,
            // Duplicates join the source's group (Chrome parity). Pinned
            // sources carry no group by invariant, so this stays clean.
            group_id: source.group_id,
            is_private: source.is_private,
            has_unsaved_form: false,
            has_live_call: false,
            is_pip_active: false,
            pip_eligible: false,
            // Same document language at copy time; the reporter re-posts
            // on the fresh load either way.
            page_lang: source.page_lang.clone(),
            // Fresh tab, no transfer: duplicates never inherit the flag
            // (the source keeps downloading undisturbed).
            has_active_download: false,
            discard_count: 0,
            console_errors: 0,
            lifecycle: TabLifecycle::Active,
            suspended_since: None,
            webview: Some(webview),
            history: source.history.clone(),
            scroll_position: source.scroll_position,
            suppress_next_history: true,
            last_nav_at_ms: now_millis(),
        }
    }

    /// Session-park form: document lineage kept (URL, title, icon URL,
    /// mute, pin, group, history, scroll), runtime signals cleared
    /// (audio, PiP, calls, console, loading, crash). Lifecycle forced
    /// Discarded: webviews never serialize (skipped), so a restore can
    /// never flip a blank tab Active. Used for workspace persistence and
    /// file loads; in-memory parks keep the live structs untouched.
    pub fn parked(&self) -> Self {
        let mut t = self.clone();
        t.favicon = None;
        t.favicon_data = None;
        t.is_loading = false;
        t.is_crashed = false;
        t.is_playing_audio = false;
        t.has_unsaved_form = false;
        t.has_live_call = false;
        t.is_pip_active = false;
        t.pip_eligible = false;
        // No transfer survives a park: the flag is runtime-only, and a
        // restored tab must never claim protection it no longer has
        // (which would pin it awake forever).
        t.has_active_download = false;
        t.console_errors = 0;
        t.lifecycle = TabLifecycle::Discarded;
        t.suspended_since = None;
        t.webview = None;
        t.suppress_next_history = false;
        t
    }

    /// Private tab: same as `new` but flagged incognito. The flag lives
    /// here, the non-persistent store lives in the factory. Callers must
    /// pair them, a private flag on a persistent store would lie.
    pub fn new_private(id: Uuid, url: String, webview: Rc<dyn WebViewHandle>) -> Self {
        let mut tab = Self::new(id, url, webview);
        tab.is_private = true;
        tab
    }

    pub fn new_discarded(id: Uuid, url: String, title: String) -> Self {
        let is_archive_view = url.starts_with("ferrite://archive/");
        let is_pdf_view = url.ends_with(".pdf") || url.contains("ferrite://pdf/");
        let is_passwords_view = url.starts_with("ferrite://passwords");
        let display_title = if title.trim().is_empty() {
            if url.is_empty() {
                "New Tab".to_owned()
            } else {
                url.clone()
            }
        } else {
            title
        };
        Self {
            id,
            url,
            title: display_title,
            favicon: None,
            favicon_url: None,
            favicon_data: None,
            is_loading: false,
            is_archive_view,
            is_pdf_view,
            is_passwords_view,
            is_playing_audio: false,
            is_muted: false,
            is_pinned: false,
            is_crashed: false,
            group_id: None,
            is_private: false,
            has_unsaved_form: false,
            has_live_call: false,
            is_pip_active: false,
            pip_eligible: false,
            page_lang: None,
            has_active_download: false,
            discard_count: 0,
            console_errors: 0,
            lifecycle: TabLifecycle::Discarded,
            suspended_since: None,
            webview: None,
            history: NavigationHistory::new(),
            scroll_position: (0.0, 0.0),
            suppress_next_history: false,
            // No load follows a discard: restore() stamps when it does.
            last_nav_at_ms: 0,
        }
    }

    pub fn navigate(&mut self, new_url: String) {
        let current = self.url.clone();
        self.history.push(&current, &new_url);
        self.url = new_url.clone();
        self.title = new_url.clone();
        // Explicit user navigation: any pending restore-suppression dies.
        // The visit this triggers is real.
        self.suppress_next_history = false;
        self.last_nav_at_ms = now_millis();
        // New document starts at the top: stale scroll must not follow.
        self.scroll_position = (0.0, 0.0);
        // Fresh document: engine console clears, so does our count.
        self.console_errors = 0;
        // New document kills the reporters' context: form/call flags die
        // with the old page (they re-report if still true).
        self.has_unsaved_form = false;
        self.has_live_call = false;
        // A new document destroys the old video element, so any held PiP
        // window dies with it and eligibility must be re-probed.
        self.is_pip_active = false;
        self.pip_eligible = false;
        self.is_archive_view = self.url.starts_with("ferrite://archive/");
        self.is_pdf_view = self.url.ends_with(".pdf") || self.url.contains("ferrite://pdf/");
        self.is_passwords_view = self.url.starts_with("ferrite://passwords");
        self.is_loading = true;
        self.is_crashed = false;
        // New document: the old page's icon must not linger. The reporter
        // re-posts when the new document's head settles.
        self.clear_favicon();
        // Same for the page language: the offer must re-probe per
        // document, never inherit the previous page's tongue.
        self.page_lang = None;
    }

    pub fn set_loaded(&mut self) {
        self.is_loading = false;
    }

    pub fn mark_crashed(&mut self) {
        self.is_crashed = true;
        self.is_loading = false;
        self.clear_pip();
    }

    pub fn set_title(&mut self, title: String) {
        if !title.trim().is_empty() {
            self.title = title;
        }
    }

    pub fn set_favicon(&mut self, bytes: Vec<u8>) {
        self.favicon = Some(bytes);
    }

    /// Record an engine-reported icon URL (length-capped, http(s) checked
    /// by the caller). Resets the rendered bitmap until the fetch lands.
    pub fn set_favicon_url(&mut self, url: String) {
        if self.favicon_url.as_deref() != Some(url.as_str()) {
            self.favicon_url = Some(url);
            self.favicon_data = None;
        }
    }

    /// Store a validated data URL (`data:image/...;base64,...`, capped by
    /// the caller). Ignored when empty.
    pub fn set_favicon_data(&mut self, data_url: String) {
        if !data_url.is_empty() {
            self.favicon_data = Some(data_url);
        }
    }

    /// Drop icon state on document change so a stale mark never outlives
    /// its page. Called by every navigation path.
    pub fn clear_favicon(&mut self) {
        self.favicon = None;
        self.favicon_url = None;
        self.favicon_data = None;
    }

    pub fn set_audio_playing(&mut self, playing: bool) {
        self.is_playing_audio = playing;
    }

    pub fn set_pip_report(&mut self, eligible: bool, active: bool) {
        self.pip_eligible = eligible;
        self.is_pip_active = active;
    }

    /// Store a validated page language tag (`Some`) or clear it (`None`
    /// for missing/invalid reports). Drives the translate offer.
    pub fn set_page_lang(&mut self, lang: Option<String>) {
        self.page_lang = lang;
    }

    pub fn clear_pip(&mut self) {
        self.is_pip_active = false;
        self.pip_eligible = false;
    }

    pub fn toggle_mute(&mut self) -> bool {
        self.is_muted = !self.is_muted;
        self.is_muted
    }

    pub fn toggle_pinned(&mut self) -> bool {
        self.is_pinned = !self.is_pinned;
        self.is_pinned
    }

    pub fn is_discarded(&self) -> bool {
        self.lifecycle == TabLifecycle::Discarded
    }

    /// Authoritative backend state for every reader (tab strip, task
    /// manager, restore page): one mapping, no per-surface re-derivation
    /// that can drift.
    pub fn backend(&self) -> TabBackend {
        if self.is_crashed {
            TabBackend::Crashed
        } else if self.is_discarded() {
            TabBackend::Discarded
        } else if self.is_loading {
            TabBackend::Loading
        } else {
            TabBackend::Live
        }
    }

    pub fn is_discard_exempt(&self) -> bool {
        self.is_playing_audio || self.has_unsaved_form || self.has_live_call
    }

    pub fn suspend(&mut self) {
        if self.lifecycle == TabLifecycle::Active {
            if let Some(handle) = self.webview.as_ref()
                && let Err(e) = handle.set_visible(false)
            {
                debug!(error = %e, "suspend hide failed");
            }
            self.lifecycle = TabLifecycle::Suspended;
            self.suspended_since = Some(Instant::now());
        }
    }

    pub fn discard(&mut self) {
        if self.lifecycle == TabLifecycle::Discarded {
            return;
        }
        // Dropping the webview drops its renderer process, which drops that
        // process's in-memory resource cache with it, expected, not a bug.
        // The persistent disk cache still holds a copy, so reactivation
        // reloads faster than cold. Memory cache needs no separate handling.
        self.webview = None;
        self.lifecycle = TabLifecycle::Discarded;
        self.suspended_since = None;
        self.is_loading = false;
        // Session-only counter for the task manager (resets on restore:
        // a reloaded tab starts a new counting life).
        self.discard_count = self.discard_count.saturating_add(1);
        self.clear_pip();
    }

    pub fn restore(&mut self, webview: Rc<dyn WebViewHandle>) {
        self.webview = Some(webview);
        self.lifecycle = TabLifecycle::Active;
        self.suspended_since = None;
        self.is_loading = true;
        self.is_crashed = false;
        self.last_nav_at_ms = now_millis();
        // Reactivation re-navigates to this tab's own URL: the commit is
        // Ferrite's doing, not a visit. The next engine commit is skipped.
        self.suppress_next_history = true;
        // is_muted is reapplied after navigation completes (caller does evaluate_script)
    }

    pub fn can_go_back(&self) -> bool {
        self.history.can_go_back()
    }

    pub fn can_go_forward(&self) -> bool {
        self.history.can_go_forward()
    }

    pub fn go_back(&mut self) -> Option<String> {
        // Engine navigation (load_url on the live webview by the caller):
        // back/forward cache is the engine's own, Chromium/WebKit restore
        // eligible pages instantly with zero Ferrite-side work. Nothing to
        // build, nothing to hook; this only tracks Rust-side state.
        let target = self.history.go_back(&self.url)?;
        self.url.clone_from(&target);
        self.is_archive_view = self.url.starts_with("ferrite://archive/");
        self.is_pdf_view = self.url.ends_with(".pdf") || self.url.contains("ferrite://pdf/");
        self.is_passwords_view = self.url.starts_with("ferrite://passwords");
        self.is_loading = true;
        self.last_nav_at_ms = now_millis();
        self.clear_pip();
        self.clear_favicon();
        Some(target)
    }

    pub fn go_forward(&mut self) -> Option<String> {
        let target = self.history.go_forward(&self.url)?;
        self.url.clone_from(&target);
        self.is_archive_view = self.url.starts_with("ferrite://archive/");
        self.is_pdf_view = self.url.ends_with(".pdf") || self.url.contains("ferrite://pdf/");
        self.is_passwords_view = self.url.starts_with("ferrite://passwords");
        self.is_loading = true;
        self.last_nav_at_ms = now_millis();
        self.clear_pip();
        self.clear_favicon();
        Some(target)
    }

    pub fn set_visible(&self, visible: bool) -> TabResult<()> {
        if let Some(handle) = self.webview.as_ref() {
            handle.set_visible(visible)?;
        }
        Ok(())
    }

    pub fn set_bounds(&self, bounds: wry::Rect) -> TabResult<()> {
        if let Some(handle) = self.webview.as_ref() {
            handle.set_bounds(bounds)?;
        }
        Ok(())
    }

    pub fn load_url(&mut self, url: String) -> TabResult<()> {
        self.navigate(url.clone());
        if let Some(handle) = self.webview.as_ref() {
            handle.load_url(&url)?;
        }
        Ok(())
    }

    /// Fold an engine-committed top-level navigation into tab state.
    /// Returns true when the caller should record a `"link"` visit. No
    /// write on restore-suppressed, same-URL, or traversal commits.
    pub fn commit_engine_navigation(&mut self, url: String) -> bool {
        if self.suppress_next_history {
            self.suppress_next_history = false;
            if self.url != url {
                self.url = url.clone();
                self.title = url;
            }
            return false;
        }
        if url == self.url {
            return false;
        }
        if self.history.peek_back() == Some(url.as_str()) {
            let _ = self.go_back();
            return false;
        }
        if self.history.peek_forward() == Some(url.as_str()) {
            let _ = self.go_forward();
            return false;
        }
        self.navigate(url);
        true
    }

    pub fn load_traversal(&mut self, url: String) -> TabResult<()> {
        self.url.clone_from(&url);
        self.title.clone_from(&url);
        self.is_archive_view = self.url.starts_with("ferrite://archive/");
        self.is_pdf_view = self.url.ends_with(".pdf") || self.url.contains("ferrite://pdf/");
        self.is_passwords_view = self.url.starts_with("ferrite://passwords");
        self.is_loading = true;
        self.is_crashed = false;
        self.last_nav_at_ms = now_millis();
        self.clear_pip();
        self.clear_favicon();
        if let Some(handle) = self.webview.as_ref() {
            handle.load_url(&url)?;
        }
        Ok(())
    }

    pub fn reload(&mut self) -> TabResult<()> {
        self.is_loading = true;
        self.is_crashed = false;
        self.last_nav_at_ms = now_millis();
        if let Some(handle) = self.webview.as_ref() {
            handle.reload()?;
        }
        Ok(())
    }

    pub fn stop(&mut self) -> TabResult<()> {
        if let Some(handle) = self.webview.as_ref() {
            // Best-effort: a gone renderer must not fail the stop.
            if let Err(e) = handle.evaluate_script("window.stop()") {
                debug!(error = %e, "stop script failed");
            }
        }
        self.is_loading = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webview::MockHandle;
    use std::rc::Rc;
    use uuid::Uuid;

    fn mock_tab(url: &str) -> TabState {
        let handle = Rc::new(MockHandle::default()) as Rc<dyn WebViewHandle>;
        TabState::new(Uuid::new_v4(), url.to_owned(), handle)
    }

    fn mock_discarded(url: &str) -> TabState {
        TabState::new_discarded(Uuid::new_v4(), url.to_owned(), "Title".to_owned())
    }

    #[test]
    fn new_tab_uses_url_as_title_and_active() {
        let tab = mock_tab("https://example.com");
        assert_eq!(tab.title, "https://example.com");
        assert!(tab.is_loading);
        assert!(!tab.is_crashed);
        assert_eq!(tab.lifecycle, TabLifecycle::Active);
        assert!(tab.webview.is_some());
        assert!(!tab.is_discarded());
    }

    #[test]
    fn new_discarded_has_no_webview() {
        let tab = mock_discarded("https://example.com");
        assert_eq!(tab.lifecycle, TabLifecycle::Discarded);
        assert!(tab.webview.is_none());
        assert!(tab.is_discarded());
        assert!(!tab.is_loading);
    }

    #[test]
    fn new_tab_empty_uses_new_tab_title() {
        let tab = mock_tab("");
        assert_eq!(tab.title, "New Tab");
    }

    #[test]
    fn navigate_resets_console_errors() {
        let mut tab = mock_tab("https://example.com");
        tab.console_errors = 7;
        tab.navigate("https://other.test".to_owned());
        assert_eq!(tab.console_errors, 0);
    }

    #[test]
    fn nav_timestamp_stamps_starts_not_discards() {
        let tab = mock_tab("https://example.com");
        assert!(tab.last_nav_at_ms > 0, "creation is a nav start");
        let mut tab = tab;
        let stamped = tab.last_nav_at_ms;
        tab.navigate("https://other.test".to_owned());
        assert!(tab.last_nav_at_ms >= stamped, "navigate re-stamps");
        let discarded = mock_discarded("https://example.com");
        assert_eq!(discarded.last_nav_at_ms, 0, "discard loads nothing");
    }

    #[test]
    fn is_archive_view_computed() {
        let tab = mock_tab("ferrite://archive/view?url=https://example.com");
        assert!(tab.is_archive_view);
        let tab2 = mock_tab("https://example.com");
        assert!(!tab2.is_archive_view);
    }

    #[test]
    fn navigate_updates_archive_flag() {
        let mut tab = mock_tab("https://a.com");
        tab.navigate("ferrite://archive/view?x".to_string());
        assert!(tab.is_archive_view);
        tab.navigate("https://b.com".to_string());
        assert!(!tab.is_archive_view);
    }

    #[test]
    fn suspend_transitions_to_suspended() {
        let mut tab = mock_tab("https://a.com");
        tab.suspend();
        assert_eq!(tab.lifecycle, TabLifecycle::Suspended);
        assert!(tab.suspended_since.is_some());
        assert!(tab.webview.is_some());
    }

    #[test]
    fn discard_drops_webview() {
        let mut tab = mock_tab("https://a.com");
        tab.suspend();
        tab.discard();
        assert_eq!(tab.lifecycle, TabLifecycle::Discarded);
        assert!(tab.webview.is_none());
        assert!(tab.suspended_since.is_none());
    }

    #[test]
    fn duplicate_keeps_lineage_drops_liveness() {
        let mut source = mock_tab("https://example.com/a");
        source.navigate("https://example.com/b".to_owned());
        source.toggle_mute();
        source.toggle_pinned();
        source.favicon_data = Some("data:image/png;base64,xx".to_owned());
        source.scroll_position = (0.0, 128.0);
        let handle = Rc::new(MockHandle::default()) as Rc<dyn WebViewHandle>;
        let dup = TabState::duplicate_from(&source, Uuid::new_v4(), handle);
        assert_ne!(dup.id, source.id);
        assert_eq!(dup.url, source.url);
        assert_eq!(dup.title, source.title);
        assert_eq!(dup.favicon_data, source.favicon_data);
        assert!(dup.is_muted);
        assert!(
            !dup.is_pinned,
            "duplicates land unpinned (pinned-left invariant)"
        );
        assert_eq!(dup.history, source.history);
        assert!(dup.can_go_back(), "back stack survives duplication");
        assert_eq!(dup.scroll_position, (0.0, 128.0));
        assert!(dup.webview.is_some());
        assert!(dup.suppress_next_history, "initial load must not re-record");
        // Liveness resets: new load, no crash, no audio, no PiP.
        assert!(dup.is_loading);
        assert!(!dup.is_crashed);
        assert!(!dup.is_playing_audio);
        assert!(!dup.is_pip_active);
        assert_eq!(dup.console_errors, 0);
        // Privacy flag round-trips (callers pair it with the factory path).
        let mut priv_src = mock_tab("https://x.test");
        priv_src.is_private = true;
        let handle = Rc::new(MockHandle::default()) as Rc<dyn WebViewHandle>;
        let priv_dup = TabState::duplicate_from(&priv_src, Uuid::new_v4(), handle);
        assert!(priv_dup.is_private);
    }

    #[test]
    fn restore_reactivates() {
        let mut tab = mock_discarded("https://a.com");
        let handle = Rc::new(MockHandle::default()) as Rc<dyn WebViewHandle>;
        tab.restore(handle);
        assert_eq!(tab.lifecycle, TabLifecycle::Active);
        assert!(tab.webview.is_some());
        assert!(tab.is_loading);
    }

    #[test]
    fn restore_commit_is_suppressed_not_a_visit() {
        let mut tab = mock_discarded("https://a.com");
        let handle = Rc::new(MockHandle::default()) as Rc<dyn WebViewHandle>;
        tab.restore(handle);
        // Reactivation re-commits the tab's own URL: no history write.
        assert!(!tab.commit_engine_navigation("https://a.com".to_owned()));
        assert!(!tab.can_go_back(), "suppressed commit pushes nothing");
        // Flag is one-shot: the next real link records.
        assert!(tab.commit_engine_navigation("https://b.com".to_owned()));
        assert_eq!(tab.url, "https://b.com");
        assert!(tab.can_go_back());
    }

    #[test]
    fn explicit_navigate_clears_restore_suppression() {
        let mut tab = mock_discarded("https://a.com");
        let handle = Rc::new(MockHandle::default()) as Rc<dyn WebViewHandle>;
        tab.restore(handle);
        tab.navigate("https://typed.test".to_owned());
        assert!(!tab.suppress_next_history);
        assert_eq!(tab.url, "https://typed.test");
    }

    #[test]
    fn engine_commit_same_url_and_traversal_record_nothing() {
        let mut tab = mock_tab("https://a.com");
        tab.navigate("https://b.com".to_owned());
        tab.navigate("https://c.com".to_owned());
        // Reload / redirect-to-self.
        assert!(!tab.commit_engine_navigation("https://c.com".to_owned()));
        assert_eq!(tab.history.back_len(), 2);
        // Page-side back/forward: stack bookkeeping, no new visit.
        assert!(!tab.commit_engine_navigation("https://b.com".to_owned()));
        assert_eq!(tab.url, "https://b.com");
        assert!(tab.can_go_forward());
        assert!(!tab.commit_engine_navigation("https://c.com".to_owned()));
        assert_eq!(tab.url, "https://c.com");
        // Fresh link: push + record.
        assert!(tab.commit_engine_navigation("https://d.com".to_owned()));
        assert_eq!(tab.url, "https://d.com");
    }

    #[test]
    fn pip_report_and_navigation_clear() {
        let mut tab = mock_tab("https://a.com");
        assert!(!tab.pip_eligible && !tab.is_pip_active);
        tab.set_pip_report(true, true);
        assert!(tab.pip_eligible && tab.is_pip_active);
        tab.navigate("https://b.com".to_string());
        assert!(!tab.pip_eligible && !tab.is_pip_active);
        tab.set_pip_report(true, true);
        tab.go_back();
        assert!(!tab.pip_eligible && !tab.is_pip_active);
        tab.set_pip_report(true, false);
        tab.mark_crashed();
        assert!(!tab.pip_eligible && !tab.is_pip_active);
    }

    #[test]
    fn audio_and_mute() {
        let mut tab = mock_tab("https://a.com");
        assert!(!tab.is_playing_audio);
        tab.set_audio_playing(true);
        assert!(tab.is_playing_audio);
        assert!(!tab.is_muted);
        assert!(tab.toggle_mute());
        assert!(tab.is_muted);
        assert!(!tab.toggle_mute());
    }

    #[test]
    fn pinned_toggle() {
        let mut tab = mock_tab("https://a.com");
        assert!(!tab.is_pinned);
        assert!(tab.toggle_pinned());
        assert!(tab.is_pinned);
    }

    #[test]
    fn back_forward_cycle() {
        let mut tab = mock_tab("https://a.com");
        tab.navigate("https://b.com".to_string());
        tab.navigate("https://c.com".to_string());
        assert!(tab.can_go_back());
        let back = tab.go_back().unwrap();
        assert_eq!(back, "https://b.com");
        assert_eq!(tab.url, "https://b.com");
        let fwd = tab.go_forward().unwrap();
        assert_eq!(fwd, "https://c.com");
    }

    #[test]
    fn set_visible_noop_when_discarded() {
        let tab = mock_discarded("https://a.com");
        assert!(tab.set_visible(false).is_ok());
        assert!(
            tab.set_bounds(wry::Rect {
                position: wry::dpi::Position::Logical(wry::dpi::LogicalPosition::new(0.0, 0.0)),
                size: wry::dpi::Size::Logical(wry::dpi::LogicalSize::new(100.0, 100.0)),
            })
            .is_ok()
        );
    }
}
