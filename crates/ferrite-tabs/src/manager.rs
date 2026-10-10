use std::rc::Rc;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use uuid::Uuid;
use winit::window::Window;

use crate::{
    error::{TabError, TabResult},
    navigation::NavigationHistory,
    tab::{TabLifecycle, TabState},
    webview::WebViewFactory,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SessionTab {
    url: String,
    title: String,
    is_pinned: bool,
    /// Owning group by id. Restored only when the group itself restores;
    /// unknown ids (hand-edited files) drop the membership, never the tab.
    #[serde(default)]
    group_id: Option<Uuid>,
    /// Back/forward stacks (newest 50 per side). Absent pre-restore:
    /// old sessions load with dead Back, never an error.
    #[serde(default)]
    history: NavigationHistory,
    /// Last scroll offset, re-applied once on restore.
    #[serde(default)]
    scroll: (f32, f32),
}

/// Max history entries persisted per stack per tab (memory stacks are
/// unbounded; the file is not).
pub const SESSION_HISTORY_LIMIT: usize = 50;

/// Named tab group. Members reference it by id from `TabState.group_id`;
/// the strip renders headers inline where membership changes. Empty groups
/// are pruned (Chrome parity): a group with no tabs does not exist.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TabGroup {
    pub id: Uuid,
    pub name: String,
    /// Index into [`GROUP_COLORS`]. Validated on set and on session load.
    pub color: u8,
    pub collapsed: bool,
}

/// Group color palette: (name, hex). Fixed so sessions stay comparable
/// across versions; append only, never reorder.
pub const GROUP_COLORS: &[(&str, &str)] = &[
    ("Grey", "#9AA0A6"),
    ("Blue", "#4F86F7"),
    ("Red", "#E57373"),
    ("Yellow", "#F6BF26"),
    ("Green", "#33A852"),
    ("Pink", "#F194B4"),
    ("Purple", "#A074E8"),
    ("Orange", "#F2994A"),
    ("Cyan", "#4FC3F7"),
];

/// Max group name length, in chars. The strip chip truncates visually;
/// this bound keeps session files and IPC honest.
pub const MAX_GROUP_NAME_CHARS: usize = 64;

/// Parked default session: the live default tree while a named workspace
/// is active. Memory-only; `save_json` persists it into the session file
/// (private tabs filtered, runtime signals normalized).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ParkedDefault {
    tabs: Vec<TabState>,
    groups: Vec<TabGroup>,
    active_id: Option<Uuid>,
}

/// Named workspace: a full tab tree parked under a name. The ACTIVE
/// workspace's tabs live in the manager's own `tabs`/`groups` (moved, not
/// copied); parked ones rest here with their webviews alive for instant
/// switches. Persistence stores normalized copies (see
/// [`TabState::parked`]); private tabs never reach disk.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub tabs: Vec<TabState>,
    #[serde(default)]
    pub groups: Vec<TabGroup>,
    #[serde(default)]
    pub active_id: Option<Uuid>,
}

/// Max named workspaces per session file. Live trees are already capped
/// by `max_tabs`; this bounds hostile files, not users.
pub const MAX_WORKSPACES: usize = 50;

/// Max tab name length for workspaces (same discipline as groups).
pub const MAX_WORKSPACE_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SessionFile {
    version: u32,
    active: usize,
    tabs: Vec<SessionTab>,
    /// Groups restore before tabs so membership can resolve. Absent in
    /// pre-group files (serde default): old sessions load ungrouped.
    #[serde(default)]
    groups: Vec<TabGroup>,
    /// Named workspaces (full trees, normalized). Absent pre-workspace.
    #[serde(default)]
    spaces: Vec<Workspace>,
    /// Active workspace id. None = default session owns `tabs`.
    /// `tabs`/`groups`/`active` below ALWAYS carry the default tree
    /// (live when default is active, parked content otherwise); a named
    /// workspace's live tree rides its own `spaces` entry. Single source
    /// each, no divergence.
    #[serde(default)]
    active_space: Option<Uuid>,
    /// Wall-clock millis of the write. Old files without it load as 0.
    #[serde(default)]
    last_saved_at: i64,
}

/// Wall-clock millis for `last_saved_at` (session recency, not ordering).
fn wall_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Lowercase host of an http(s) URL. None for internal pages and
/// unparseable input (sleep exemptions only ever match web hosts).
fn tab_host(url: &str) -> Option<String> {
    let lower = url.trim().to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    url::Url::parse(url.trim())
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
        .filter(|h| !h.is_empty())
}

/// Normalize one tab for persistence inside a group set: runtime signals
/// cleared (`parked`), membership kept only when the group travels too.
/// A group id without its group is a dangling pointer (hand-edited
/// files), so it drops while the tab survives.
fn parked_normalized(t: &TabState, groups: &[TabGroup]) -> TabState {
    let mut p = t.parked();
    if let Some(gid) = p.group_id
        && !groups.iter().any(|g| g.id == gid)
    {
        p.group_id = None;
    }
    p
}

/// Validate persisted group rows (session file or workspace). Invalid
/// rows drop, never the session. Cap 100: hostile files, not users.
fn clean_session_groups(src: &[TabGroup]) -> Vec<TabGroup> {
    let mut out = Vec::new();
    for g in src.iter().take(100) {
        if (g.color as usize) >= GROUP_COLORS.len() {
            continue;
        }
        let name = g.name.trim();
        if name.is_empty() || name.chars().count() > MAX_GROUP_NAME_CHARS {
            continue;
        }
        if out.iter().any(|e: &TabGroup| e.id == g.id) {
            continue;
        }
        out.push(TabGroup {
            id: g.id,
            name: name.to_owned(),
            color: g.color,
            collapsed: g.collapsed,
        });
    }
    out
}

/// Normalize persisted full tabs (workspace trees): overlong URLs and
/// private tabs drop, runtime clears, pinned never grouped. Cap 200 per
/// tree, same as the default session path.
fn clean_space_tabs(src: &[TabState], groups: &[TabGroup]) -> Vec<TabState> {
    src.iter()
        .take(200)
        .filter(|t| !t.is_private && t.url.len() <= 2048)
        .map(|t| {
            let mut p = parked_normalized(t, groups);
            p.history = p.history.cleaned(SESSION_HISTORY_LIMIT);
            if p.is_pinned {
                p.group_id = None;
            }
            let (sx, sy) = p.scroll_position;
            p.scroll_position = if sx.is_finite() && sy.is_finite() {
                (sx.clamp(-1e7, 1e7), sy.clamp(-1e7, 1e7))
            } else {
                (0.0, 0.0)
            };
            p
        })
        .collect()
}

#[derive(Debug)]
pub struct TabManager {
    tabs: Vec<TabState>,
    active_index: Option<usize>,
    max_tabs: Option<usize>,
    factory: Rc<dyn WebViewFactory>,
    idle_timeout: Duration,
    /// Set on structural change. Cleared when the shell persists.
    dirty: bool,
    /// Last memory pressure 0.0 to 1.0. Weights the discard timeout.
    mem_pressure: f32,
    /// Sleep exception hosts (lowercase, no leading dot): suffix-matched,
    /// so `example.com` covers `mail.example.com`. Auto-discard only;
    /// explicit Sleep-tab gestures bypass the list.
    sleep_exceptions: Vec<String>,
    /// Named groups in creation order. Tabs point at these by id; empty
    /// groups are pruned after every membership change.
    groups: Vec<TabGroup>,
    /// Named workspaces (parked trees). The active workspace's tabs live
    /// in `tabs`/`groups`; its entry keeps only the name.
    spaces: Vec<Workspace>,
    /// Active workspace id. None = the default session owns `tabs`.
    active_space: Option<Uuid>,
    /// Default tree parked while a named workspace is active. Some
    /// exactly when `active_space` is Some (invariant, enforced by the
    /// switch paths, never assumed by save/load).
    default_park: Option<ParkedDefault>,
}

/// Per window tab cap. Override with `with_max_tabs`.
pub const DEFAULT_MAX_TABS: usize = 100;

/// Single window registry. Tab strip, task manager, and session all read
/// this, no second mapping. A second native window earns its own tree.
impl TabManager {
    pub fn new(factory: Rc<dyn WebViewFactory>) -> Self {
        Self::with_max_tabs(factory, Some(DEFAULT_MAX_TABS))
    }
    pub fn with_max_tabs(factory: Rc<dyn WebViewFactory>, max_tabs: Option<usize>) -> Self {
        Self {
            tabs: Vec::new(),
            active_index: None,
            max_tabs,
            factory,
            idle_timeout: Duration::from_secs(30 * 60),
            dirty: false,
            mem_pressure: 0.5,
            sleep_exceptions: Vec::new(),
            groups: Vec::new(),
            spaces: Vec::new(),
            active_space: None,
            default_park: None,
        }
    }
    /// Mark the session dirty after a navigation commit (URL or title change,
    /// the one structural change inside `TabState`, out of this manager
    /// sight). Everything else marks itself.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Take the dirty flag for the debounced autosave: `true` means "persist
    /// now". Clears on read, so a quiet browser stops writing.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Feed last observed memory pressure 0.0 to 1.0 (clamped). The shell
    /// owns the sampler, this owns the policy.
    pub fn set_memory_pressure(&mut self, pressure: f32) {
        self.mem_pressure = pressure.clamp(0.0, 1.0);
    }

    /// Idle timeout weighted by memory pressure: aggressive under real
    /// pressure, conservative when memory is plentiful, never one fixed
    /// value regardless of system state.
    pub fn effective_idle_timeout(&self) -> Duration {
        // ponytail: 4-step table, not a curve, steps are reviewable, a
        // curve would pretend to precision the sampler doesn't have.
        if self.mem_pressure > 0.9 {
            Duration::from_secs(5 * 60)
        } else if self.mem_pressure > 0.6 {
            Duration::from_secs(15 * 60)
        } else if self.mem_pressure > 0.3 {
            self.idle_timeout
        } else {
            Duration::from_secs(60 * 60)
        }
    }
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }
    /// Base auto-sleep timeout (pressure-weighted by
    /// `effective_idle_timeout`). Zero disables auto-sleep in the shell
    /// tick; the manager keeps the value for display.
    pub fn set_idle_timeout(&mut self, timeout: Duration) {
        self.idle_timeout = timeout;
        self.dirty = true;
    }
    /// Replace the sleep-exception host list (already normalized
    /// shell-side; re-normalized here defensively).
    pub fn set_sleep_exceptions(&mut self, hosts: Vec<String>) {
        self.sleep_exceptions = hosts
            .into_iter()
            .map(|h| h.trim().trim_start_matches('.').to_lowercase())
            .filter(|h| !h.is_empty() && h.len() <= 253)
            .collect();
        self.sleep_exceptions.sort();
        self.sleep_exceptions.dedup();
        self.dirty = true;
    }
    pub fn sleep_exceptions(&self) -> &[String] {
        &self.sleep_exceptions
    }
    /// Why auto-sleep must skip this tab, if anything: exempt signals
    /// (audio, unsaved form, live call), an in-progress download
    /// (unloading mid-transfer loses data), pinned under real pressure,
    /// crashed, or a listed host. None means discardable. Task-manager
    /// diagnostics read this; `sleep_exempt` is the boolean face.
    /// Behavior-identical to the old inline rule, reason by reason.
    pub fn sleep_exempt_reason(&self, tab: &TabState) -> Option<&'static str> {
        if tab.is_crashed {
            return Some("crashed");
        }
        if tab.is_playing_audio {
            return Some("playing audio");
        }
        if tab.has_unsaved_form {
            return Some("unsaved form");
        }
        if tab.has_live_call {
            return Some("live call");
        }
        if tab.has_active_download {
            return Some("downloading");
        }
        if tab.is_pinned && self.mem_pressure <= 0.9 {
            return Some("pinned");
        }
        if let Some(host) = tab_host(&tab.url) {
            let host = host.to_lowercase();
            if self
                .sleep_exceptions
                .iter()
                .any(|ex| host == *ex || host.ends_with(&format!(".{ex}")))
            {
                return Some("listed site");
            }
        }
        None
    }

    /// Whether auto-sleep must skip this tab. Explicit sleep gestures
    /// bypass hosts but NEVER downloads or signals (the shell refuses
    /// those with a toast).
    pub fn sleep_exempt(&self, tab: &TabState) -> bool {
        self.sleep_exempt_reason(tab).is_some()
    }
    /// Factory that created this manager's webviews. The shell reuses it
    /// for non-tab surfaces (e.g. the HTML tab-search overlay) so chrome
    /// and content share protocol assets, proxy config, and IPC plumbing.
    pub fn factory(&self) -> &Rc<dyn WebViewFactory> {
        &self.factory
    }
    pub fn len(&self) -> usize {
        self.tabs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
    pub fn active_index(&self) -> Option<usize> {
        self.active_index
    }
    pub fn active_id(&self) -> Option<Uuid> {
        self.active_index
            .and_then(|idx| self.tabs.get(idx).map(|t| t.id))
    }
    pub fn active_tab(&self) -> Option<&TabState> {
        self.active_index.and_then(|idx| self.tabs.get(idx))
    }
    pub fn active_tab_mut(&mut self) -> Option<&mut TabState> {
        if let Some(idx) = self.active_index {
            self.tabs.get_mut(idx)
        } else {
            None
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = &TabState> {
        self.tabs.iter()
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut TabState> {
        self.tabs.iter_mut()
    }
    pub fn active_is_archive_view(&self) -> bool {
        self.active_tab()
            .map(|t| t.is_archive_view)
            .unwrap_or(false)
    }
    pub fn new_tab(
        &mut self,
        window: Option<&Window>,
        url: Option<String>,
        bounds: Option<wry::Rect>,
    ) -> TabResult<Uuid> {
        if let Some(limit) = self.max_tabs
            && self.tabs.len() >= limit
        {
            return Err(TabError::TabLimitExceeded {
                current: self.tabs.len(),
                limit,
            });
        }
        let raw_url = url.unwrap_or_else(|| "ferrite://newtab".to_owned());
        let url = if raw_url.trim().is_empty() {
            "ferrite://newtab".to_owned()
        } else {
            raw_url
        };
        if !url.starts_with("ferrite://") {
            url::Url::parse(&url).map_err(|source| TabError::InvalidUrl {
                url: url.clone(),
                source,
            })?;
        }
        let id = Uuid::new_v4();
        let window_ref = window.ok_or(TabError::WebViewCreationFailed {
            source: wry::Error::InitScriptError,
        })?;
        let webview = self.factory.create_with_id(window_ref, &url, bounds, id)?;
        if let Some(prev_idx) = self.active_index
            && let Some(prev) = self.tabs.get_mut(prev_idx)
        {
            prev.suspend();
        }
        let tab = TabState::new(id, url, webview);
        self.tabs.push(tab);
        self.active_index = Some(self.tabs.len() - 1);
        self.dirty = true;
        info!(%id, active_index = self.active_index.unwrap_or(0), "new tab");
        Ok(id)
    }
    /// Private tab: incognito engine store via the factory private path
    /// (never the profile disk cache), flagged so session and history skip it.
    /// Same cap and URL validation as [`Self::new_tab`].
    pub fn new_private_tab(
        &mut self,
        window: Option<&Window>,
        url: Option<String>,
        bounds: Option<wry::Rect>,
    ) -> TabResult<Uuid> {
        if let Some(limit) = self.max_tabs
            && self.tabs.len() >= limit
        {
            return Err(TabError::TabLimitExceeded {
                current: self.tabs.len(),
                limit,
            });
        }
        let raw_url = url.unwrap_or_else(|| "ferrite://newtab".to_owned());
        let url = if raw_url.trim().is_empty() {
            "ferrite://newtab".to_owned()
        } else {
            raw_url
        };
        if !url.starts_with("ferrite://") {
            url::Url::parse(&url).map_err(|source| TabError::InvalidUrl {
                url: url.clone(),
                source,
            })?;
        }
        let id = Uuid::new_v4();
        let window_ref = window.ok_or(TabError::WebViewCreationFailed {
            source: wry::Error::InitScriptError,
        })?;
        let webview = self
            .factory
            .create_private_with_id(window_ref, &url, bounds, id)?;
        if let Some(prev_idx) = self.active_index
            && let Some(prev) = self.tabs.get_mut(prev_idx)
        {
            prev.suspend();
        }
        let tab = TabState::new_private(id, url, webview);
        self.tabs.push(tab);
        self.active_index = Some(self.tabs.len() - 1);
        // Private tabs never reach the session file (filtered at save), but
        // the dirty flag still tracks manager mutation: autosave debounces
        // on it, and a dirty manager with only private tabs simply writes
        // an unchanged-equivalent document. Same invariant as `new_tab`.
        self.dirty = true;
        info!(%id, active_index = self.active_index.unwrap_or(0), "new private tab");
        Ok(id)
    }
    /// Duplicate a live tab: same URL/title/icon/flags/history/scroll, fresh
    /// load. Private sources route through the private factory path (a
    /// private flag on a persistent store would lie); everything else takes
    /// the normal path. Same cap and suspension rules as [`Self::new_tab`].
    pub fn duplicate_tab(
        &mut self,
        window: Option<&Window>,
        source_id: Uuid,
        bounds: Option<wry::Rect>,
    ) -> TabResult<Uuid> {
        if let Some(limit) = self.max_tabs
            && self.tabs.len() >= limit
        {
            return Err(TabError::TabLimitExceeded {
                current: self.tabs.len(),
                limit,
            });
        }
        let source = self
            .tabs
            .iter()
            .find(|t| t.id == source_id)
            .ok_or(TabError::TabNotFound { id: source_id })?;
        // Discarded sources duplicate fine: the metadata (URL, title,
        // history) is intact; only the live webview is gone, and a fresh
        // one is built below either way.
        let (url, private) = (source.url.clone(), source.is_private);
        let window_ref = window.ok_or(TabError::WebViewCreationFailed {
            source: wry::Error::InitScriptError,
        })?;
        let id = Uuid::new_v4();
        let webview = if private {
            self.factory
                .create_private_with_id(window_ref, &url, bounds, id)?
        } else {
            self.factory.create_with_id(window_ref, &url, bounds, id)?
        };
        if let Some(prev_idx) = self.active_index
            && let Some(prev) = self.tabs.get_mut(prev_idx)
        {
            prev.suspend();
        }
        let source = self
            .tabs
            .iter()
            .find(|t| t.id == source_id)
            .ok_or(TabError::TabNotFound { id: source_id })?;
        let tab = TabState::duplicate_from(source, id, webview);
        self.tabs.push(tab);
        self.active_index = Some(self.tabs.len() - 1);
        self.dirty = true;
        info!(%id, "tab duplicated");
        Ok(id)
    }
    pub fn new_discarded_tab(
        &mut self,
        url: String,
        title: String,
        is_pinned: bool,
    ) -> TabResult<Uuid> {
        if let Some(limit) = self.max_tabs
            && self.tabs.len() >= limit
        {
            return Err(TabError::TabLimitExceeded {
                current: self.tabs.len(),
                limit,
            });
        }
        let id = Uuid::new_v4();
        let mut tab = TabState::new_discarded(id, url, title);
        tab.is_pinned = is_pinned;
        self.tabs.push(tab);
        if self.active_index.is_none() {
            self.active_index = Some(0);
        }
        self.dirty = true;
        Ok(id)
    }
    pub fn new_tab_mock(&mut self, url: Option<String>) -> TabResult<Uuid> {
        use crate::webview::MockHandle;
        if let Some(limit) = self.max_tabs
            && self.tabs.len() >= limit
        {
            return Err(TabError::TabLimitExceeded {
                current: self.tabs.len(),
                limit,
            });
        }
        let raw_url = url.unwrap_or_else(|| "ferrite://newtab".to_owned());
        let url = if raw_url.trim().is_empty() {
            "ferrite://newtab".to_owned()
        } else {
            raw_url
        };
        if !url.starts_with("ferrite://") {
            url::Url::parse(&url).map_err(|source| TabError::InvalidUrl {
                url: url.clone(),
                source,
            })?;
        }
        let id = Uuid::new_v4();
        let handle = Rc::new(MockHandle::default()) as Rc<dyn crate::webview::WebViewHandle>;
        let _ = handle.load_url(&url);
        if let Some(prev_idx) = self.active_index
            && let Some(prev) = self.tabs.get_mut(prev_idx)
        {
            prev.suspend();
        }
        let tab = TabState::new(id, url, handle);
        self.tabs.push(tab);
        self.active_index = Some(self.tabs.len() - 1);
        self.dirty = true;
        Ok(id)
    }
    /// Headless twin of [`Self::duplicate_tab`] for tests: same state
    /// cloning (history, flags, scroll) over a mock handle, no window.
    /// Private sources stay flagged (callers pair flags with stores).
    pub fn duplicate_tab_mock(&mut self, source_id: Uuid) -> TabResult<Uuid> {
        use crate::webview::MockHandle;
        if let Some(limit) = self.max_tabs
            && self.tabs.len() >= limit
        {
            return Err(TabError::TabLimitExceeded {
                current: self.tabs.len(),
                limit,
            });
        }
        let src_idx = self
            .tabs
            .iter()
            .position(|t| t.id == source_id)
            .ok_or(TabError::TabNotFound { id: source_id })?;
        let id = Uuid::new_v4();
        let handle = Rc::new(MockHandle::default()) as Rc<dyn crate::webview::WebViewHandle>;
        if let Some(prev_idx) = self.active_index
            && let Some(prev) = self.tabs.get_mut(prev_idx)
        {
            prev.suspend();
        }
        // Re-fetch after the mutable borrow above ends: suspending the
        // previous tab must not invalidate the source snapshot.
        let source = self
            .tabs
            .get(src_idx)
            .ok_or(TabError::TabNotFound { id: source_id })?;
        let tab = TabState::duplicate_from(source, id, handle);
        self.tabs.push(tab);
        self.active_index = Some(self.tabs.len() - 1);
        self.dirty = true;
        Ok(id)
    }
    pub fn close_tab(&mut self, id: Uuid) -> TabResult<()> {
        let idx = self
            .tabs
            .iter()
            .position(|t| t.id == id)
            .ok_or(TabError::TabNotFound { id })?;
        self.close_tab_by_index(idx)
    }
    pub fn close_tab_by_index(&mut self, index: usize) -> TabResult<()> {
        if index >= self.tabs.len() {
            return Err(TabError::IndexOutOfBounds {
                index,
                len: self.tabs.len(),
            });
        }
        let removed_id = self.tabs.get(index).map(|t| t.id).unwrap_or_else(Uuid::nil);
        if self.tabs.get(index).map(|t| t.is_pinned).unwrap_or(false) {
            warn!(%removed_id, "close blocked for pinned tab: unpin or force close");
            return Err(TabError::PinnedTab { id: removed_id });
        }
        self.tabs.remove(index);
        self.dirty = true;
        // A closed last member dissolves its group (no empty groups).
        self.prune_empty_groups();
        info!(%removed_id, "tab closed");
        match self.active_index {
            None => {}
            Some(active) if active == index => {
                if self.tabs.is_empty() {
                    self.active_index = None;
                } else {
                    let new_active = index.min(self.tabs.len() - 1);
                    self.active_index = Some(new_active);
                    if let Some(tab) = self.tabs.get_mut(new_active) {
                        // Never flip a discarded tab to Active without a
                        // webview (that yields blank tabs): it stays
                        // Discarded until switch_tab_with_window restores it.
                        if !tab.is_discarded() {
                            tab.lifecycle = TabLifecycle::Active;
                            tab.suspended_since = None;
                            if let Err(e) = tab.set_visible(true) {
                                debug!(error = %e, "post-close show failed");
                            }
                        }
                    }
                }
            }
            Some(active) if active > index => {
                self.active_index = Some(active - 1);
            }
            _ => {}
        }
        Ok(())
    }
    pub fn close_all_tabs(&mut self, force_pinned: bool) -> usize {
        let before = self.tabs.len();
        self.tabs.retain(|t| !force_pinned && t.is_pinned);
        let closed = before - self.tabs.len();
        if self.tabs.is_empty() {
            self.active_index = None;
        } else {
            let new_active = self.active_index.map(|i| i.min(self.tabs.len() - 1));
            self.active_index = new_active;
            if let Some(idx) = self.active_index
                && let Some(tab) = self.tabs.get_mut(idx)
                && !tab.is_discarded()
            {
                tab.lifecycle = TabLifecycle::Active;
                tab.suspended_since = None;
                if let Err(e) = tab.set_visible(true) {
                    debug!(error = %e, "bulk-close show failed");
                }
            }
        }
        info!(closed, "bulk close");
        if closed > 0 {
            self.dirty = true;
            self.prune_empty_groups();
        }
        closed
    }
    pub fn discard_tab(&mut self, index: usize) -> TabResult<()> {
        let len = self.tabs.len();
        if index >= len {
            return Err(TabError::IndexOutOfBounds { index, len });
        }
        let tab = self
            .tabs
            .get_mut(index)
            .ok_or(TabError::IndexOutOfBounds { index, len })?;
        if tab.is_pinned || tab.is_playing_audio {
            debug!(tab_id = %tab.id, pinned = tab.is_pinned, audio = tab.is_playing_audio, "discard skipped (exempt)");
            return Ok(());
        }
        tab.discard();
        self.dirty = true;
        info!(tab_id = %tab.id, "tab discarded");
        Ok(())
    }
    pub fn restore_tab(
        &mut self,
        window: &Window,
        index: usize,
        bounds: Option<wry::Rect>,
    ) -> TabResult<()> {
        let len = self.tabs.len();
        if index >= len {
            return Err(TabError::IndexOutOfBounds { index, len });
        }
        let (url, is_muted, id, private) = match self.tabs.get(index) {
            Some(t) => (t.url.clone(), t.is_muted, t.id, t.is_private),
            None => return Err(TabError::IndexOutOfBounds { index, len }),
        };
        // A discarded private tab restores into the incognito store, never
        // the profile cache, same factory path as creation.
        let webview = if private {
            self.factory
                .create_private_with_id(window, &url, bounds, id)?
        } else {
            self.factory.create_with_id(window, &url, bounds, id)?
        };
        let tab = self
            .tabs
            .get_mut(index)
            .ok_or(TabError::IndexOutOfBounds { index, len })?;
        tab.restore(webview);
        self.dirty = true;
        if is_muted && let Some(handle) = tab.webview.as_ref() {
            handle.evaluate_script(
                "Array.from(document.querySelectorAll('audio,video')).forEach(m=>m.muted=true)",
            )?;
        }
        Ok(())
    }
    pub fn switch_tab(&mut self, id: Uuid) -> TabResult<()> {
        let idx = self
            .tabs
            .iter()
            .position(|t| t.id == id)
            .ok_or(TabError::TabNotFound { id })?;
        self.switch_tab_by_index(idx)
    }
    pub fn switch_tab_by_index(&mut self, index: usize) -> TabResult<()> {
        if index >= self.tabs.len() {
            return Err(TabError::IndexOutOfBounds {
                index,
                len: self.tabs.len(),
            });
        }
        let already_active = self.active_index == Some(index);
        let target_discarded = self.tabs.get(index).is_some_and(|t| t.is_discarded());
        if already_active && !target_discarded {
            return Ok(());
        }
        if let Some(prev_idx) = self.active_index
            && prev_idx != index
            && let Some(prev) = self.tabs.get_mut(prev_idx)
        {
            prev.suspend();
        }
        if target_discarded {
            // Mock/headless reactivation: no window exists to rebuild the
            // webview, so the lifecycle flips here and the (mock) handle
            // stays as-is. Production always routes through
            // `switch_tab_with_window`, which restores a real webview first;
            // that path, not this one, owns webview fidelity.
            if let Some(tab) = self.tabs.get_mut(index) {
                tab.lifecycle = TabLifecycle::Active;
                tab.suspended_since = None;
            }
        } else {
            if let Some(next) = self.tabs.get_mut(index) {
                next.lifecycle = TabLifecycle::Active;
                next.suspended_since = None;
                if let Err(e) = next.set_visible(true) {
                    debug!(error = %e, "tab show failed");
                }
            }
        }
        self.active_index = Some(index);
        self.dirty = true;
        debug!(%index, "switched tab");
        Ok(())
    }
    pub fn switch_tab_with_window(
        &mut self,
        window: &Window,
        index: usize,
        bounds: Option<wry::Rect>,
    ) -> TabResult<()> {
        if index >= self.tabs.len() {
            return Err(TabError::IndexOutOfBounds {
                index,
                len: self.tabs.len(),
            });
        }
        if self
            .tabs
            .get(index)
            .map(|t| t.is_discarded())
            .unwrap_or(false)
        {
            self.restore_tab(window, index, bounds)?;
        }
        self.switch_tab_by_index(index)
    }
    pub fn toggle_mute(&mut self, index: usize) -> TabResult<bool> {
        let len = self.tabs.len();
        if index >= len {
            return Err(TabError::IndexOutOfBounds { index, len });
        }
        let tab = self
            .tabs
            .get_mut(index)
            .ok_or(TabError::IndexOutOfBounds { index, len })?;
        let muted = tab.toggle_mute();
        if let Some(handle) = tab.webview.as_ref() {
            let js = if muted {
                "Array.from(document.querySelectorAll('audio,video')).forEach(m=>m.muted=true)"
            } else {
                "Array.from(document.querySelectorAll('audio,video')).forEach(m=>m.muted=false)"
            };
            if let Err(e) = handle.evaluate_script(js) {
                debug!(error = %e, "mute script failed");
            }
        }
        Ok(muted)
    }
    pub fn toggle_pin(&mut self, index: usize) -> TabResult<bool> {
        let len = self.tabs.len();
        if index >= len {
            return Err(TabError::IndexOutOfBounds { index, len });
        }
        let tab = self
            .tabs
            .get_mut(index)
            .ok_or(TabError::IndexOutOfBounds { index, len })?;
        let pinned = tab.toggle_pinned();
        self.dirty = true;
        Ok(pinned)
    }
    /// Groups in creation order. The strip renders headers inline where
    /// membership changes; collapse only hides members, never unloads them.
    pub fn groups(&self) -> &[TabGroup] {
        &self.groups
    }

    pub fn find_group(&self, id: Uuid) -> Option<&TabGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// Drop groups with no member tabs. Called after every membership
    /// change: an empty group does not exist (Chrome parity).
    fn prune_empty_groups(&mut self) {
        let membered: std::collections::HashSet<Uuid> =
            self.tabs.iter().filter_map(|t| t.group_id).collect();
        let before = self.groups.len();
        self.groups.retain(|g| membered.contains(&g.id));
        if self.groups.len() != before {
            self.dirty = true;
        }
    }

    fn clean_group_name(name: &str) -> TabResult<String> {
        let trimmed = name.trim();
        let count = trimmed.chars().count();
        if trimmed.is_empty() || count > MAX_GROUP_NAME_CHARS {
            return Err(TabError::InvalidGroupName);
        }
        Ok(trimmed.to_owned())
    }

    /// Create a group (grey, expanded) and return its id. Members join via
    /// [`Self::set_tab_group`] or [`Self::group_tabs`]; a group with no
    /// members is pruned on the next membership change.
    pub fn create_group(&mut self, name: &str) -> TabResult<Uuid> {
        let name = Self::clean_group_name(name)?;
        let id = Uuid::new_v4();
        self.groups.push(TabGroup {
            id,
            name,
            color: 0,
            collapsed: false,
        });
        self.dirty = true;
        info!(%id, "tab group created");
        Ok(id)
    }

    /// Delete a group: members stay open, ungrouped. Unknown id errors.
    pub fn delete_group(&mut self, id: Uuid) -> TabResult<()> {
        let pos = self
            .groups
            .iter()
            .position(|g| g.id == id)
            .ok_or(TabError::GroupNotFound { id })?;
        self.groups.remove(pos);
        for t in self.tabs.iter_mut().filter(|t| t.group_id == Some(id)) {
            t.group_id = None;
        }
        self.dirty = true;
        info!(%id, "tab group deleted");
        Ok(())
    }

    pub fn rename_group(&mut self, id: Uuid, name: &str) -> TabResult<()> {
        let name = Self::clean_group_name(name)?;
        let group = self
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or(TabError::GroupNotFound { id })?;
        group.name = name;
        self.dirty = true;
        Ok(())
    }

    /// Recolor by palette index. Out-of-range errors (no wrapping: a stale
    /// client must fail loudly, not paint a surprise color).
    pub fn set_group_color(&mut self, id: Uuid, color: u8) -> TabResult<()> {
        if (color as usize) >= GROUP_COLORS.len() {
            return Err(TabError::InvalidGroupColor { color });
        }
        let group = self
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or(TabError::GroupNotFound { id })?;
        group.color = color;
        self.dirty = true;
        Ok(())
    }

    pub fn set_group_collapsed(&mut self, id: Uuid, collapsed: bool) -> TabResult<()> {
        let group = self
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or(TabError::GroupNotFound { id })?;
        group.collapsed = collapsed;
        self.dirty = true;
        Ok(())
    }

    /// Move one tab into a group (`None` ungroups). Pinned tabs are
    /// rejected with [`TabError::PinnedTab`]: they live left of the strip,
    /// outside groups, and mixing the two breaks the pinned-first invariant.
    pub fn set_tab_group(&mut self, tab_id: Uuid, group: Option<Uuid>) -> TabResult<()> {
        if let Some(gid) = group
            && self.find_group(gid).is_none()
        {
            return Err(TabError::GroupNotFound { id: gid });
        }
        let tab = self
            .tabs
            .iter_mut()
            .find(|t| t.id == tab_id)
            .ok_or(TabError::TabNotFound { id: tab_id })?;
        if tab.is_pinned && group.is_some() {
            return Err(TabError::PinnedTab { id: tab_id });
        }
        tab.group_id = group;
        self.dirty = true;
        self.prune_empty_groups();
        Ok(())
    }

    /// Create a group and move the given tabs into it. Unknown tab ids and
    /// pinned tabs are skipped (reported via the returned count, never an
    /// error: bulk gestures must not die on one stale row). Returns
    /// (group id, members assigned).
    pub fn group_tabs(
        &mut self,
        ids: &[Uuid],
        name: &str,
        color: u8,
        collapsed: bool,
    ) -> TabResult<(Uuid, usize)> {
        if (color as usize) >= GROUP_COLORS.len() {
            return Err(TabError::InvalidGroupColor { color });
        }
        let gid = self.create_group(name)?;
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == gid) {
            g.color = color;
            g.collapsed = collapsed;
        }
        let mut assigned = 0;
        for id in ids {
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == *id)
                && !tab.is_pinned
            {
                tab.group_id = Some(gid);
                assigned += 1;
            }
        }
        self.dirty = true;
        self.prune_empty_groups();
        // All-skipped means the group is already pruned: report it.
        if assigned == 0 {
            return Err(TabError::GroupNotFound { id: gid });
        }
        Ok((gid, assigned))
    }

    /// Member tab ids in strip order. Empty (unknown group prunes to this).
    pub fn tabs_in_group(&self, id: Uuid) -> Vec<Uuid> {
        self.tabs
            .iter()
            .filter(|t| t.group_id == Some(id))
            .map(|t| t.id)
            .collect()
    }

    /// Close every member tab. Pinned members cannot exist (assign rejects
    /// them); if one is found anyway it is skipped and counted as kept.
    /// Returns (closed, kept). The emptied group prunes itself.
    pub fn close_group(&mut self, id: Uuid) -> TabResult<(usize, usize)> {
        if self.find_group(id).is_none() {
            return Err(TabError::GroupNotFound { id });
        }
        let members: Vec<Uuid> = self.tabs_in_group(id);
        let (mut closed, mut kept) = (0, 0);
        for mid in members {
            match self.close_tab(mid) {
                Ok(()) => closed += 1,
                Err(_) => kept += 1,
            }
        }
        self.prune_empty_groups();
        Ok((closed, kept))
    }

    /// Move a group's whole run as one unit so `to` lands the run's first
    /// member. The run cannot enter the leading pinned run (same clamp rule
    /// as [`Self::reorder`]); the active tab tracks by id, not index.
    pub fn move_group(&mut self, id: Uuid, to: usize) -> TabResult<()> {
        if self.find_group(id).is_none() {
            return Err(TabError::GroupNotFound { id });
        }
        let active_id = self.active_id();
        let pinned = self.tabs.iter().take_while(|t| t.is_pinned).count();
        let mut run: Vec<TabState> = Vec::new();
        let mut rest: Vec<TabState> = Vec::new();
        for tab in self.tabs.drain(..) {
            if tab.group_id == Some(id) {
                run.push(tab);
            } else {
                rest.push(tab);
            }
        }
        if run.is_empty() {
            // No members (should have pruned): restore and report.
            self.tabs = rest;
            return Err(TabError::GroupNotFound { id });
        }
        let to = to.max(pinned).min(rest.len());
        for (i, tab) in run.into_iter().enumerate() {
            rest.insert(to + i, tab);
        }
        self.tabs = rest;
        if let Some(aid) = active_id {
            self.active_index = self.tabs.iter().position(|t| t.id == aid);
        }
        self.dirty = true;
        Ok(())
    }

    /// Move several tabs as one block (multi-select drag), preserving
    /// their current relative order. Pinned ids are skipped (the pinned
    /// run moves only via single-tab clamp in [`Self::reorder`]); unknown
    /// ids are skipped. Returns (moved, kept). `to` is a manager index
    /// into the list *after* removing the block, clamped into the
    /// unpinned zone like [`Self::move_group`].
    pub fn move_tabs(&mut self, ids: &[Uuid], to: usize) -> (usize, usize) {
        let wanted: std::collections::HashSet<Uuid> = ids.iter().copied().collect();
        let active_id = self.active_id();
        let pinned = self.tabs.iter().take_while(|t| t.is_pinned).count();
        let mut block: Vec<TabState> = Vec::new();
        let mut rest: Vec<TabState> = Vec::new();
        let mut kept = 0;
        for tab in self.tabs.drain(..) {
            if wanted.contains(&tab.id) {
                if tab.is_pinned {
                    kept += 1;
                    rest.push(tab);
                } else {
                    block.push(tab);
                }
            } else {
                rest.push(tab);
            }
        }
        let moved = block.len();
        if moved == 0 {
            self.tabs = rest;
            return (0, kept);
        }
        // `to` counts in the post-removal list; clamp past the pinned run.
        // Pinned ids stayed in `rest`, so `pinned` still bounds correctly.
        let to = to.max(pinned).min(rest.len());
        for (i, tab) in block.into_iter().enumerate() {
            rest.insert(to + i, tab);
        }
        self.tabs = rest;
        if let Some(aid) = active_id {
            self.active_index = self.tabs.iter().position(|t| t.id == aid);
        }
        self.dirty = true;
        (moved, kept)
    }

    // ── Workspaces ──

    fn clean_workspace_name(name: &str) -> TabResult<String> {
        let trimmed = name.trim();
        let count = trimmed.chars().count();
        if trimmed.is_empty() || count > MAX_WORKSPACE_NAME_CHARS {
            return Err(TabError::InvalidGroupName);
        }
        Ok(trimmed.to_owned())
    }

    /// (id, name, tab count) for every workspace, creation order. The
    /// active workspace counts its live tabs; parked ones count stored.
    pub fn workspace_list(&self) -> Vec<(Uuid, String, usize)> {
        self.spaces
            .iter()
            .map(|s| {
                let n = if Some(s.id) == self.active_space {
                    self.tabs.len()
                } else {
                    s.tabs.len()
                };
                (s.id, s.name.clone(), n)
            })
            .collect()
    }

    pub fn active_space_name(&self) -> Option<String> {
        match self.active_space {
            None => None,
            Some(id) => self
                .spaces
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.name.clone()),
        }
    }

    /// Active workspace id. None = default session.
    pub fn active_space_id(&self) -> Option<Uuid> {
        self.active_space
    }
    /// Create a workspace and switch into it (empty tree, Chrome parity:
    /// a new workspace starts blank). The previous tree parks (default
    /// into `default_park`, named into its entry).
    pub fn create_workspace(&mut self, name: &str) -> TabResult<Uuid> {
        let name = Self::clean_workspace_name(name)?;
        let id = Uuid::new_v4();
        self.spaces.push(Workspace {
            id,
            name,
            tabs: Vec::new(),
            groups: Vec::new(),
            active_id: None,
        });
        self.switch_workspace(id)?;
        info!(%id, "workspace created");
        Ok(id)
    }

    pub fn rename_workspace(&mut self, id: Uuid, name: &str) -> TabResult<()> {
        let name = Self::clean_workspace_name(name)?;
        let space = self
            .spaces
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(TabError::WorkspaceNotFound { id })?;
        space.name = name;
        self.dirty = true;
        Ok(())
    }

    /// Delete a workspace. Returns (url, title) of dropped non-private
    /// tabs for the closed-tab stack (the stack caps itself at push
    /// time). Deleting the ACTIVE workspace returns to default first, so
    /// its tabs park and join the dropped list like any parked tree:
    /// nothing vanishes without a trace.
    pub fn delete_workspace(&mut self, id: Uuid) -> TabResult<Vec<(String, String)>> {
        if self.spaces.iter().all(|s| s.id != id) {
            return Err(TabError::WorkspaceNotFound { id });
        }
        if Some(id) == self.active_space {
            self.switch_default()?;
        }
        let pos = self
            .spaces
            .iter()
            .position(|s| s.id == id)
            .ok_or(TabError::WorkspaceNotFound { id })?;
        let space = self.spaces.remove(pos);
        self.dirty = true;
        info!(%id, "workspace deleted");
        Ok(space
            .tabs
            .iter()
            .filter(|t| !t.is_private)
            .map(|t| (t.url.clone(), t.title.clone()))
            .collect())
    }

    /// Park the live tree into its slot: the active workspace's entry, or
    /// `default_park` for the default session. Webviews travel with the
    /// structs (kept alive for instant return); persistence normalizes
    /// copies at save time, never here.
    fn park_live(&mut self) {
        let tabs = std::mem::take(&mut self.tabs);
        let groups = std::mem::take(&mut self.groups);
        let active_id = self.active_id();
        match self.active_space {
            Some(id) => {
                if let Some(space) = self.spaces.iter_mut().find(|s| s.id == id) {
                    space.tabs = tabs;
                    space.groups = groups;
                    space.active_id = active_id;
                } else {
                    // Entry vanished mid-flight (should not happen): the
                    // tree would otherwise be lost, so repark as default.
                    self.default_park = Some(ParkedDefault {
                        tabs,
                        groups,
                        active_id,
                    });
                    self.active_space = None;
                }
            }
            None => {
                self.default_park = Some(ParkedDefault {
                    tabs,
                    groups,
                    active_id,
                });
            }
        }
        self.active_index = None;
    }

    /// Take a parked tree live and settle the active tab (stored id wins,
    /// else first tab, else none). Only the active tab's webview shows;
    /// the rest hide (same discipline as `show_only_active`). Discarded
    /// tabs carry no webview: the caller restores the active one through
    /// the window path.
    fn unpark_into_live(
        &mut self,
        tabs: Vec<TabState>,
        groups: Vec<TabGroup>,
        active_id: Option<Uuid>,
    ) {
        self.tabs = tabs;
        self.groups = groups;
        self.prune_empty_groups();
        self.active_index = active_id
            .and_then(|aid| self.tabs.iter().position(|t| t.id == aid))
            .or(if self.tabs.is_empty() { None } else { Some(0) });
        let active = self.active_id();
        for t in &self.tabs {
            if let Some(h) = t.webview.as_ref()
                && h.set_visible(Some(t.id) == active).is_err()
            {
                debug!(tab_id = %t.id, "workspace unpark show failed");
            }
        }
        self.dirty = true;
    }

    /// Switch into a named workspace, parking the current tree. No-op
    /// when already there.
    pub fn switch_workspace(&mut self, id: Uuid) -> TabResult<()> {
        if self.spaces.iter().all(|s| s.id != id) {
            return Err(TabError::WorkspaceNotFound { id });
        }
        if Some(id) == self.active_space {
            return Ok(());
        }
        self.park_live();
        let space = self
            .spaces
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(TabError::WorkspaceNotFound { id })?;
        let tabs = std::mem::take(&mut space.tabs);
        let groups = std::mem::take(&mut space.groups);
        let active_id = space.active_id;
        self.active_space = Some(id);
        self.unpark_into_live(tabs, groups, active_id);
        info!(%id, "workspace switched");
        Ok(())
    }

    /// Return to the default session, parking the current workspace.
    /// No-op when already default.
    pub fn switch_default(&mut self) -> TabResult<()> {
        if self.active_space.is_none() {
            return Ok(());
        }
        self.park_live();
        let park = std::mem::take(&mut self.default_park).unwrap_or_default();
        self.active_space = None;
        self.unpark_into_live(park.tabs, park.groups, park.active_id);
        info!("workspace switched to default");
        Ok(())
    }

    pub fn reorder(&mut self, from: usize, to: usize) -> TabResult<()> {
        if from >= self.tabs.len() || to >= self.tabs.len() {
            return Err(TabError::IndexOutOfBounds {
                index: from.max(to),
                len: self.tabs.len(),
            });
        }
        if from == to {
            return Ok(());
        }
        // Pinned block stays left: a pinned tab cannot leave the leading
        // pinned run, and an unpinned tab cannot enter it. This keeps the
        // `close-right` leftmost invariant (and every other pinned-first
        // assumption) true no matter what the strip drag reports. Stray
        // unpinned rows inside the run (pre-clamp sessions) self-heal: any
        // move lands them in the unpinned zone.
        let pinned = self.tabs.iter().take_while(|t| t.is_pinned).count();
        let moving_pinned = self.tabs.get(from).is_some_and(|t| t.is_pinned);
        let to = if moving_pinned {
            to.min(pinned.saturating_sub(1))
        } else {
            to.max(pinned).min(self.tabs.len() - 1)
        };
        if from == to {
            return Ok(());
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        // Adjust active_index
        if let Some(active) = self.active_index {
            if active == from {
                self.active_index = Some(to);
            } else if from < active && to >= active {
                self.active_index = Some(active - 1);
            } else if from > active && to <= active {
                self.active_index = Some(active + 1);
            }
        }
        self.dirty = true;
        Ok(())
    }

    pub fn update_active_bounds(&self, rect: wry::Rect) -> TabResult<()> {
        let tab = self.active_tab().ok_or(TabError::NoActiveTab)?;
        if tab.is_discarded() {
            return Ok(());
        }
        tab.set_bounds(rect)
    }

    /// Hide every webview except the active tab. Repairs visible and active
    /// drift. Idempotent and cheap, called on every chrome push. Returns
    /// (shown, hidden).
    pub fn show_only_active(&self) -> (usize, usize) {
        let active = self.active_id();
        let (mut shown, mut hidden) = (0, 0);
        for t in &self.tabs {
            if let Some(h) = t.webview.as_ref() {
                let want = Some(t.id) == active;
                if h.set_visible(want).is_ok() {
                    if want {
                        shown += 1;
                    } else {
                        hidden += 1;
                    }
                }
            }
        }
        (shown, hidden)
    }
    pub fn mark_crashed(&mut self, id: Uuid) -> TabResult<()> {
        let tab = self
            .tabs
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(TabError::TabNotFound { id })?;
        tab.mark_crashed();
        warn!(%id, "tab crashed");
        Ok(())
    }
    pub fn get_tab(&self, id: Uuid) -> Option<&TabState> {
        self.tabs.iter().find(|t| t.id == id)
    }
    pub fn get_tab_mut(&mut self, id: Uuid) -> Option<&mut TabState> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }
    /// Persisted tab metadata for crash recovery. Webviews are never
    /// serialized: loaded tabs start Discarded and rebuild on selection.
    /// Private tabs are never persisted at all (not write-then-delete).
    /// Returns the JSON document; the shell owns bytes (encryption, atomic
    /// file write) so plaintext never touches this layer's callers.
    pub fn save_json(&self) -> TabResult<String> {
        // Default tree: live when default is active, parked otherwise.
        // Either way it normalizes (runtime signals cleared) and drops
        // private tabs (never persisted, not write-then-delete).
        let (def_tabs, def_groups, def_active_id): (Vec<TabState>, Vec<TabGroup>, Option<Uuid>) =
            match self.active_space {
                None => (self.tabs.clone(), self.groups.clone(), self.active_id()),
                Some(_) => match self.default_park.as_ref() {
                    Some(park) => (park.tabs.clone(), park.groups.clone(), park.active_id),
                    // Invariant break (active named, no park): persist an
                    // empty default rather than another tree's tabs.
                    None => {
                        warn!("workspace save: active space without default park");
                        (Vec::new(), Vec::new(), None)
                    }
                },
            };
        let live: Vec<TabState> = def_tabs.into_iter().filter(|t| !t.is_private).collect();
        let membered: std::collections::HashSet<Uuid> =
            live.iter().filter_map(|t| t.group_id).collect();
        let session = SessionFile {
            version: 1,
            active: def_active_id
                .and_then(|aid| live.iter().position(|t| t.id == aid))
                .unwrap_or(0),
            tabs: live
                .iter()
                .map(|t| SessionTab {
                    url: t.url.clone(),
                    title: t.title.clone(),
                    is_pinned: t.is_pinned,
                    group_id: t.group_id,
                    history: t.history.truncated(SESSION_HISTORY_LIMIT),
                    scroll: t.scroll_position,
                })
                .collect(),
            // Only groups with a persisted member survive: empty and
            // private-only groups never reach disk.
            groups: def_groups
                .into_iter()
                .filter(|g| membered.contains(&g.id))
                .collect(),
            spaces: self
                .spaces
                .iter()
                .map(|s| {
                    // The active workspace's tree is live; parked ones rest
                    // in their entries. Both normalize + drop private.
                    let (tabs, groups, active_id) = if Some(s.id) == self.active_space {
                        (self.tabs.clone(), self.groups.clone(), self.active_id())
                    } else {
                        (s.tabs.clone(), s.groups.clone(), s.active_id)
                    };
                    let tabs: Vec<TabState> = tabs
                        .into_iter()
                        .filter(|t| !t.is_private)
                        .map(|t| {
                            let mut p = t.parked();
                            p.history = p.history.truncated(SESSION_HISTORY_LIMIT);
                            p
                        })
                        .collect();
                    let membered: std::collections::HashSet<Uuid> =
                        tabs.iter().filter_map(|t| t.group_id).collect();
                    let live_aid = active_id.filter(|aid| tabs.iter().any(|t| &t.id == aid));
                    Workspace {
                        id: s.id,
                        name: s.name.clone(),
                        tabs,
                        groups: groups
                            .into_iter()
                            .filter(|g| membered.contains(&g.id))
                            .collect(),
                        active_id: live_aid,
                    }
                })
                .collect(),
            active_space: self.active_space,
            last_saved_at: wall_millis(),
        };
        serde_json::to_string_pretty(&session).map_err(|e| TabError::SessionIo {
            message: format!("serialize: {e}"),
        })
    }

    /// Load persisted tabs as Discarded (caller restores the active one with
    /// a window). Corrupt/foreign input yields an empty session, never a panic.
    pub fn load_json(&mut self, data: &str) -> usize {
        let session: SessionFile = match serde_json::from_str(data) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "session file corrupt - starting fresh");
                return 0;
            }
        };
        if session.version != 1 {
            warn!(
                version = session.version,
                "unknown session version - starting fresh"
            );
            return 0;
        }
        let mut restored: usize = 0;
        // Groups first so membership below can resolve. Invalid rows
        // (bad color, blank/oversize name) are dropped, never fatal:
        // a corrupt group must not sink the session.
        for g in session.groups.iter().take(100) {
            if (g.color as usize) >= GROUP_COLORS.len() {
                continue;
            }
            let name = g.name.trim();
            if name.is_empty() || name.chars().count() > MAX_GROUP_NAME_CHARS {
                continue;
            }
            if self.groups.iter().any(|e| e.id == g.id) {
                continue;
            }
            self.groups.push(TabGroup {
                id: g.id,
                name: name.to_owned(),
                color: g.color,
                collapsed: g.collapsed,
            });
        }
        for t in session.tabs.iter().take(200) {
            if t.url.len() > 2048 {
                continue;
            }
            if self
                .new_discarded_tab(t.url.clone(), t.title.clone(), t.is_pinned)
                .is_ok()
            {
                // Membership resolves only to a restored group, and never
                // onto a pinned tab (pinned live outside groups).
                if let Some(gid) = t.group_id
                    && !t.is_pinned
                    && self.groups.iter().any(|g| g.id == gid)
                    && let Some(tab) = self.tabs.last_mut()
                {
                    tab.group_id = Some(gid);
                }
                // Back/forward stacks restore cleaned (empties/overlong
                // dropped, newest 50 kept); scroll restores finite (NaN
                // would poison layout math).
                if let Some(tab) = self.tabs.last_mut() {
                    tab.history = t.history.cleaned(SESSION_HISTORY_LIMIT);
                    let (sx, sy) = t.scroll;
                    tab.scroll_position = if sx.is_finite() && sy.is_finite() {
                        (sx.clamp(-1e7, 1e7), sy.clamp(-1e7, 1e7))
                    } else {
                        (0.0, 0.0)
                    };
                }
                restored += 1;
            }
        }
        self.prune_empty_groups();
        if restored > 0 {
            self.active_index = Some(session.active.min(restored.saturating_sub(1)));
        }
        // Workspaces: validated trees, parked. One corrupt space drops
        // alone, never the session. Counts join the restored total (the
        // boot gate treats any restored tree as a session worth keeping).
        for s in session.spaces.iter().take(MAX_WORKSPACES) {
            let name = s.name.trim();
            if name.is_empty() || name.chars().count() > MAX_WORKSPACE_NAME_CHARS {
                continue;
            }
            if self.spaces.iter().any(|e| e.id == s.id) {
                continue;
            }
            let groups = clean_session_groups(&s.groups);
            let tabs = clean_space_tabs(&s.tabs, &groups);
            let membered: std::collections::HashSet<Uuid> =
                tabs.iter().filter_map(|t| t.group_id).collect();
            let groups: Vec<TabGroup> = groups
                .into_iter()
                .filter(|g| membered.contains(&g.id))
                .collect();
            let active_id = s.active_id.filter(|aid| tabs.iter().any(|t| &t.id == aid));
            restored += tabs.len();
            self.spaces.push(Workspace {
                id: s.id,
                name: name.to_owned(),
                tabs,
                groups,
                active_id,
            });
        }
        // A valid active id goes live at once (its tree was the open one):
        // the default tree just loaded parks aside first.
        if let Some(aid) = session.active_space
            && self.spaces.iter().any(|s| s.id == aid)
        {
            let park_tabs = std::mem::take(&mut self.tabs);
            let park_groups = std::mem::take(&mut self.groups);
            let park_active = self
                .active_index
                .and_then(|i| park_tabs.get(i).map(|t| t.id));
            self.default_park = Some(ParkedDefault {
                tabs: park_tabs,
                groups: park_groups,
                active_id: park_active,
            });
            let pos = self.spaces.iter().position(|s| s.id == aid).unwrap_or(0);
            let space = self.spaces.remove(pos);
            let (sname, stabs, sgroups, said) = (
                space.name.clone(),
                space.tabs,
                space.groups,
                space.active_id,
            );
            self.active_space = Some(aid);
            self.unpark_into_live(stabs, sgroups, said);
            // Name shell stays listed while active (counts read live).
            self.spaces.push(Workspace {
                id: aid,
                name: sname,
                tabs: Vec::new(),
                groups: Vec::new(),
                active_id: said,
            });
        } else {
            self.active_space = None;
            self.default_park = None;
        }
        info!(restored, "session restored as discarded tabs");
        restored
    }

    /// Discard suspended tabs past the pressure-weighted idle timeout.
    /// Returns the discarded ids for per-tab resource release. Exemptions
    /// (`sleep_exempt`: signals, pinned-under-pressure, crashed, listed
    /// hosts) first, then idle time. Explicit sleep gestures bypass the
    /// list; this path never does.
    pub fn discard_idle_tabs(&mut self, now: Instant) -> Vec<Uuid> {
        let timeout = self.effective_idle_timeout();
        let mut discarded = Vec::new();
        for idx in 0..self.tabs.len() {
            let should_discard = {
                let tab = match self.tabs.get(idx) {
                    Some(t) => t,
                    None => continue,
                };
                if self.sleep_exempt(tab) {
                    false
                } else {
                    tab.lifecycle == TabLifecycle::Suspended
                        && tab
                            .suspended_since
                            .is_some_and(|since| now.duration_since(since) >= timeout)
                }
            };
            if should_discard {
                let id = self.tabs.get(idx).map(|t| t.id);
                match self.discard_tab(idx) {
                    Ok(()) => discarded.extend(id),
                    Err(e) => debug!(error = %e, "idle discard failed"),
                }
            }
        }
        discarded
    }
}
