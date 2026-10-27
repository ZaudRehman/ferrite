//! Server driven chrome state. Shell owns state and pushes it with
//! [`push_script`]. Chrome renders and posts commands back.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TabRow {
    pub id: String,
    pub title: String,
    pub url: String,
    pub loading: bool,
    pub discarded: bool,
    pub crashed: bool,
    pub active: bool,
    pub pinned: bool,
    pub muted: bool,
    pub playing: bool,
    pub pip: bool,
    /// Offer the PiP button when true.
    pub pip_available: bool,
    /// Background tab waiting on a script dialog.
    pub dialog_pending: bool,
    pub can_back: bool,
    pub can_forward: bool,
    /// Manager index of this tab. Rendered as `data-idx`: drag-reorder
    /// posts real indices, so collapsing a group (which hides rows) can
    /// never skew a visible-position guess into the wrong tab.
    pub idx: usize,
    /// Owning group id, if any. Headers render where it changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Private (ephemeral) tab inside user mode: incognito engine store,
    /// never persisted. Drives the per-tab private marker (distinct from the
    /// window-level `incognito` below).
    #[serde(default)]
    pub private: bool,
    /// Ready-to-render icon (`data:image/...;base64,...`, raster only).
    /// `None` renders the letter avatar. Skipped when absent to keep pushes small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
}

/// One tab-group header row. The strip renders headers inline in tab
/// order; collapse hides members client-side (indices stay real via
/// `TabRow.idx`, see above).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupRow {
    pub id: String,
    pub name: String,
    /// Palette index (see `ferrite_tabs::manager::GROUP_COLORS`).
    pub color: u8,
    pub color_hex: String,
    pub collapsed: bool,
    pub count: usize,
}

/// One workspace row for the switcher. The active workspace counts its
/// live tabs; parked ones count stored trees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WsRow {
    pub id: String,
    pub name: String,
    pub count: usize,
    pub active: bool,
}

/// One profile row: id, display name, avatar initial, whether this
/// process runs under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileRow {
    pub id: String,
    pub name: String,
    pub initial: String,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BarItem {
    pub id: String,
    pub title: String,
    pub url: String,
    pub folder: bool,
}

/// Suggestion row kind, set by the shell: `answer` (instant math, pins above
/// all rows), `tab` (open-tab match, switches instead of navigating),
/// `history`, `bookmark`, `search` (history-backed web search), or
/// `tabsearch` (carries the engine id in `provider` and commits into
/// provider mode on Tab instead of navigating).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SugRow {
    pub url: String,
    pub title: String,
    pub tab: Option<String>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermRow {
    pub kind: String,
    pub label: String,
    pub origin: String,
    /// Backoff tripped: show without the attention animation.
    pub quiet: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineRow {
    pub id: String,
    pub name: String,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusRow {
    pub tabs: usize,
    pub proxy_alive: bool,
    pub blocked: u64,
    pub blocked_ad: u64,
    pub blocked_tracker: u64,
    /// Fingerprinting-list hits (enforced as tracker blocks, tagged apart).
    pub blocked_fingerprinting: u64,
    pub blocked_malware: u64,
    pub blocked_mixed: u64,
    pub recent_trackers: Vec<String>,
    pub fail_open: u64,
    /// Blocked-request count for the ACTIVE page only. The `blocked*`
    /// counters above are session totals; this one attributes per site so
    /// the dropdown can promise "on this site" honestly. Zero for
    /// internal pages (nothing to attribute).
    #[serde(default)]
    pub site_blocked: u64,
    /// Active page's blocking paused by the user (persists; Incognito keeps
    /// it session-only). Never inferred client-side.
    #[serde(default)]
    pub site_paused: bool,
    pub downloads: usize,
    pub last_download: Option<String>,
    pub last_download_id: Option<String>,
    pub show: bool,
    pub dl_active: bool,
    pub dl_frac: f32,
    pub dl_flash: bool,
    pub dl_failed: bool,
    /// Page reported count only. The engine console is not exposed.
    pub console_errors: u32,
}

/// Split in Rust. Never parsed in JS (display is a trust surface).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct DispParts {
    pub sch: String,
    pub sub: String,
    pub dom: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChromeState {
    pub tabs: Vec<TabRow>,
    pub groups: Vec<GroupRow>,
    /// Vertical tab rail: the strip docks left, the content slot shifts.
    pub tabs_vertical: bool,
    /// Workspaces for the switcher (empty when none exist).
    pub workspaces: Vec<WsRow>,
    /// Active workspace name. None = default session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Omnibox source scope (`tabs`, `bookmarks`, `history`): the
    /// dropdown filters to one provider. None = unscoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omni_scope: Option<String>,
    /// Translate offer for the active tab: page language differs from
    /// the target (toolbar button shows; click translates the page).
    pub translate_offer: bool,
    /// Profiles for the switcher (current first, then the rest).
    pub profiles: Vec<ProfileRow>,
    pub raw_url: String,
    pub disp: DispParts,
    pub sec: String,
    pub omnibox_text: String,
    pub suggestions: Vec<SugRow>,
    pub perm: Vec<PermRow>,
    pub zoom_pct: u32,
    pub pip_eligible: bool,
    pub pip_active: bool,
    pub engines: Vec<EngineRow>,
    pub status: StatusRow,
    pub accent: String,
    pub pinned_view: bool,
    pub starred: bool,
    pub starred_folder: Option<String>,
    pub themes: Vec<EngineRow>,
    pub bar_visible: bool,
    pub bar_mode: String,
    pub bar_items: Vec<BarItem>,
    pub folders: Vec<EngineRow>,
    pub panel_open: bool,
    pub panel_section: String,
    pub panel_w: u32,
    pub crash_offer: bool,
    pub crash_tabs: u32,
    pub version: String,
    pub hover: DispParts,
    pub hover_mine: bool,
    /// This window is incognito (ephemeral): drives the toolbar indicator.
    pub incognito: bool,
    /// Session kind for the chrome badge/theme variant: `"user"` or
    /// `"incognito"`. Single source of truth: the renderer never infers it.
    /// Kept alongside `incognito` (derived) for backward-compat pushes.
    #[serde(default = "default_session_kind")]
    pub session_kind: String,
    /// Active tab is a private (ephemeral) tab inside user mode.
    #[serde(default)]
    pub private_tab: bool,
}

impl ChromeState {
    pub fn empty() -> Self {
        Self {
            tabs: Vec::new(),
            groups: Vec::new(),
            tabs_vertical: false,
            workspaces: Vec::new(),
            workspace: None,
            omni_scope: None,
            translate_offer: false,
            profiles: Vec::new(),
            raw_url: String::new(),
            disp: DispParts::default(),
            sec: "shield".to_owned(),
            omnibox_text: String::new(),
            suggestions: Vec::new(),
            perm: Vec::new(),
            zoom_pct: 100,
            pip_eligible: false,
            pip_active: false,
            engines: Vec::new(),
            status: StatusRow {
                tabs: 0,
                proxy_alive: false,
                blocked: 0,
                blocked_ad: 0,
                blocked_tracker: 0,
                blocked_fingerprinting: 0,
                blocked_malware: 0,
                blocked_mixed: 0,
                recent_trackers: Vec::new(),
                fail_open: 0,
                site_blocked: 0,
                site_paused: false,
                downloads: 0,
                last_download: None,
                last_download_id: None,
                show: false,
                dl_active: false,
                dl_frac: 0.0,
                dl_flash: false,
                dl_failed: false,
                console_errors: 0,
            },
            accent: "#ffc670".to_owned(),
            pinned_view: false,
            starred: false,
            starred_folder: None,
            themes: Vec::new(),
            bar_visible: false,
            bar_mode: "never".to_owned(),
            bar_items: Vec::new(),
            folders: Vec::new(),
            panel_open: false,
            panel_section: "bookmarks".to_owned(),
            panel_w: 320,
            crash_offer: false,
            crash_tabs: 0,
            version: String::new(),
            hover: DispParts::default(),
            hover_mine: false,
            incognito: false,
            session_kind: default_session_kind(),
            private_tab: false,
        }
    }
}

fn default_session_kind() -> String {
    "user".to_owned()
}

/// Keep the domain visible. Short strings pass through.
pub fn mid_truncate(s: &str, head: usize, tail: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= head + tail + 1 {
        return s.to_owned();
    }
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// `</` is escaped so titles and URLs cannot break the script string.
pub fn state_json(state: &ChromeState) -> String {
    serde_json::to_string(state)
        .unwrap_or_else(|_| "{}".to_owned())
        .replace("</", "<\\/")
}

pub fn push_script(state: &ChromeState) -> String {
    format!("__ferriteChrome({})", state_json(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_json_escapes_script_breakout() {
        let mut s = ChromeState::empty();
        s.tabs.push(TabRow {
            id: "1".into(),
            title: "</script><script>alert(1)</script>".into(),
            url: "https://example.com".into(),
            loading: false,
            discarded: false,
            crashed: false,
            can_back: false,
            can_forward: false,
            active: true,
            pinned: false,
            muted: false,
            playing: false,
            pip: false,
            pip_available: false,
            dialog_pending: false,
            idx: 0,
            group: None,
            private: false,
            favicon: None,
        });
        let json = state_json(&s);
        assert!(!json.contains("</"), "script context must stay closed");
        assert!(
            json.contains("alert(1)"),
            "content preserved, only markup neutered"
        );
    }

    #[test]
    fn push_script_invokes_renderer_hook() {
        let script = push_script(&ChromeState::empty());
        assert!(script.starts_with("__ferriteChrome("));
        assert!(script.ends_with(')'));
    }

    #[test]
    fn mid_truncate_keeps_domain_visible() {
        assert_eq!(mid_truncate("/a", 44, 36), "/a");
        let long = format!("/{}", "x".repeat(200));
        let cut = mid_truncate(&long, 44, 36);
        assert!(cut.starts_with("/xxxx"));
        assert!(cut.contains('…'));
        assert!(cut.ends_with(&"x".repeat(36)));
    }
}
