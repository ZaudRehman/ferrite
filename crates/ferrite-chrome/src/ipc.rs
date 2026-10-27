//! Typed chrome IPC. Narrow on purpose: no eval, no file or network
//! access. Unknown shapes are rejected at deserialization.

use serde::{Deserialize, Serialize};

/// Chrome-owned surfaces that used to be DOM popovers. The shell maps each
/// one onto the shared overlay and builds its rows/actions from live state;
/// chrome only supplies the surface name, validated target ids and geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromeSurface {
    Star,
    Workspaces,
    Profiles,
    BarMenu,
    TabContext,
    TabsContext,
    GroupContext,
    BookmarkContext,
}

/// The complete value vocabulary a chrome form may submit. Deliberately not
/// an arbitrary JSON object: the shell can validate every field without
/// walking an attacker-shaped tree, and the enum stays `Eq`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopValues {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub color: u8,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ChromeCommand {
    Navigate {
        url: String,
    },
    NewTab {
        url: Option<String>,
    },
    CloseTab {
        id: String,
    },
    SelectTab {
        id: String,
    },
    ToggleTabSearch,
    FocusContent,
    DismissOverlay,
    OmniboxInput {
        text: String,
    },
    /// `provider` commits a tab to search row via Tab. Absent on old posts.
    OmniboxSubmit {
        text: String,
        url: Option<String>,
        #[serde(default)]
        provider: Option<String>,
    },
    /// Per row history delete. Vault plus cache drop the URL.
    SugDelete {
        url: String,
    },
    /// The denied request already died. This grants future ones.
    PermDecide {
        origin: String,
        kind: String,
        always: bool,
    },
    /// Toggle blocking pause for the ACTIVE tab's page. No host argument:
    /// the shell derives it from the live tab, so a compromised page cannot
    /// pause protection for an arbitrary origin.
    ShieldPause,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    SelectEngine {
        id: String,
    },
    /// Unknown ids are logged and ignored.
    MenuItem {
        id: String,
    },
    Reload,
    GoBack {
        steps: usize,
    },
    GoForward {
        steps: usize,
    },
    ToggleMute {
        /// Target tab id. Absent (toolbar button, palette, old posts) means
        /// the active tab; the strip icon always sends its own tab.
        #[serde(default)]
        id: Option<String>,
    },
    TogglePin {
        /// Target tab id, same fallback rule as `ToggleMute`.
        #[serde(default)]
        id: Option<String>,
    },
    /// Visible tab strip indices.
    Reorder {
        from: usize,
        to: usize,
    },
    ToggleStar {
        url: String,
        title: String,
    },
    /// `parent` None files at the bar root.
    BookmarkAdd {
        url: String,
        title: String,
        parent: Option<String>,
    },
    FolderAdd {
        name: String,
        /// Parent folder id; absent = bookmarks-bar root (old posts).
        #[serde(default)]
        parent: Option<String>,
    },
    BookmarkAllTabs,
    FolderList {
        id: String,
    },
    BarReorder {
        ids: Vec<String>,
    },
    /// Push the 8 most recent downloads to `__ferriteDownloadsRecent`
    /// (downloads bubble source; same builder as the downloads page).
    /// Desktop's bubble is now a shell-hosted overlay pop (`PopKind::
    /// Downloads`, tasks/toolbar-overlay-3c.md 3c-3), but the bundled
    /// mobile chrome template still posts this and the bubble actions
    /// below, so the vocabulary stays until that surface migrates too.
    DownloadsRecent,
    /// Bubble actions by download id (resolved server-side to filename).
    DlBubbleOpen {
        id: String,
    },
    DlBubbleFolder {
        id: String,
    },
    DlBubbleRetry {
        id: String,
    },
    DlBubbleRemove {
        id: String,
    },
    Home,
    ToggleMaximize,
    PanelToggle,
    PanelSection {
        id: String,
    },
    /// Clamped and persisted shell side.
    PanelResize {
        w: u32,
    },
    PanelHistory {
        q: String,
    },
    ReadingAdd {
        url: String,
        title: String,
    },
    ReadingRead {
        id: String,
    },
    ReadingRemove {
        id: String,
    },
    ReadingList,
    FindOpen,
    /// Mobile-only contract below the find/toast group: desktop's find card
    /// and toast are overlay windows now (tasks/toolbar-overlay-3c.md 3c-5),
    /// but the bundled mobile chrome template still posts this vocabulary
    /// with its own DOM card, and
    /// `mobile_template_shares_contract_and_stays_bundled` pins that it can
    /// be served unchanged. Desktop never sends these.
    FindSearch {
        query: String,
        case: bool,
    },
    FindNext,
    FindPrev,
    FindClose,
    OpenDevTools,
    ReaderOpen,
    EnterPip,
    ExitPip,
    Share,
    CrashRestore,
    CrashDismiss,
    /// Restore everything except the saved-active tab (the likely crash
    /// culprit). The dropped tab lands on the closed-tab stack, so
    /// Ctrl+Shift+T reopens it if it was innocent.
    CrashRestoreExceptActive,
    CtxReload {
        id: String,
    },
    /// Duplicate carries the source tab id (not its URL): history, mute,
    /// pin, private flag, and scroll round-trip server-side.
    CtxDuplicate {
        id: String,
    },
    CtxPin {
        id: String,
    },
    CtxMute {
        id: String,
    },
    CtxCopyUrl {
        id: String,
    },
    CtxCloseOthers {
        id: String,
    },
    CtxCloseRight {
        id: String,
    },
    /// Unload one tab now (sleep): renderer freed, full state kept,
    /// click restores. Explicit gesture bypasses sleep exceptions.
    CtxSleep {
        id: String,
    },
    CtxBookmarkDelete {
        id: String,
    },
    /// Create a group from tab ids (empty name rejected shell-side with a
    /// toast; unknown/pinned ids skipped and counted, never fatal).
    /// `color`/`collapsed` ride along so creation is one atomic post: the
    /// editor picks them before Save, and follow-up posts would race a
    /// group the client cannot name yet.
    GroupCreate {
        name: String,
        tabs: Vec<String>,
        #[serde(default)]
        color: u8,
        #[serde(default)]
        collapsed: bool,
    },
    GroupRename {
        id: String,
        name: String,
    },
    /// Palette index into the manager's `GROUP_COLORS`. Out of range is
    /// rejected, never wrapped.
    GroupColor {
        id: String,
        color: u8,
    },
    GroupCollapse {
        id: String,
        collapsed: bool,
    },
    /// Delete the group; members stay open, ungrouped.
    GroupDelete {
        id: String,
    },
    /// Close every member tab (pinned members refuse per-tab, counted).
    GroupClose {
        id: String,
    },
    /// Move the group's run as one unit to a manager index.
    GroupMove {
        id: String,
        to: usize,
    },
    /// Adopt one tab into a group (drag-adopt, editor). Pinned tabs refuse.
    GroupAddTab {
        id: String,
        tab: String,
    },
    /// Remove one tab from its group (stays open, in place).
    GroupRemoveTab {
        tab: String,
    },
    /// Bulk close by ids (multi-select). Pinned refuse per-tab and count
    /// as kept; unknown ids are skipped. Never partial-panics.
    TabsClose {
        ids: Vec<String>,
    },
    /// Bulk mute/unmute by ids. Already-correct tabs are no-ops.
    TabsMute {
        ids: Vec<String>,
        muted: bool,
    },
    /// Bulk pin/unpin by ids. Grouped tabs refuse pinning (a pinned tab
    /// cannot carry a group); counted as kept.
    TabsPin {
        ids: Vec<String>,
        pinned: bool,
    },
    /// Move several tabs as one block (multi-select drag), preserving
    /// their strip order. `to` counts in the post-removal list; pinned
    /// ids are skipped and counted as kept.
    TabsMove {
        ids: Vec<String>,
        to: usize,
    },
    /// Create a workspace and switch into it (starts empty).
    WsCreate {
        name: String,
    },
    WsRename {
        id: String,
        name: String,
    },
    /// Delete: active returns to default first; dropped non-private
    /// tabs land on the closed-tab stack via the shell.
    WsDelete {
        id: String,
    },
    /// Switch into a workspace (parks the current tree). No-op if there.
    WsSwitch {
        id: String,
    },
    /// Return to the default session (parks the current workspace).
    WsDefault,
    /// Unload the active tab now (palette "Sleep Active Tab"). Same
    /// explicit-gesture semantics as `CtxSleep`.
    SleepActive,
    /// Translate the active page now (toolbar offer button). Runs the
    /// on-device page translation; availability toasts come from the
    /// engine's own state channel.
    TranslatePage,
    /// Open the profile in a new process (side-by-side windows, like
    /// Chrome). Unknown ids are ignored.
    ProfileSwitch {
        id: String,
    },
    /// Create a profile (sanitized, created on demand) and open it.
    /// Empty/invalid names toast, never create junk dirs.
    ProfileCreate {
        name: String,
    },
    /// Delete a profile's data. Current and Default refuse.
    ProfileDelete {
        id: String,
    },
    FocusOmnibox,
    RequestState,
    /// Mobile-only toast vocabulary, same reason as the find commands above:
    /// the bundled template renders its own card and posts both actions.
    /// Desktop posts `toast_action`/`toast_dismiss` as overlay `type`
    /// messages instead.
    ToastAction {
        seq: u64,
    },
    ToastDismiss {
        seq: u64,
    },
    /// In-page JS error report from the chrome UI (window.onerror).
    /// Display-only diagnostics: the shell logs it, never acts on it.
    /// Length-capped so a throwing loop cannot flood the log through its
    /// own reporter (plus client-side dedupe in chrome.html).
    ChromeError {
        message: String,
    },
    /// Open the main menu in the overlay window (topmost layer, above
    /// content in z-order — chrome DOM popups are invisible there).
    /// x/y are the button's window-relative right/bottom edges in whole
    /// CSS pixels (integers keep the command `Eq`; sub-pixel anchoring
    /// is meaningless for a menu). The shell clamps and validates.
    MenuOpen {
        x: i32,
        y: i32,
    },
    /// Open a toolbar popover on the shared overlay webview. `kind` names the
    /// popup (`omnibox`). x/y/w/h are the anchor's window-relative top-left
    /// and size in whole CSS pixels, so the shell can hang the popup below it
    /// (integers keep the command `Eq`; sub-pixel anchoring is meaningless).
    /// `hl` is the highlighted row (-1 for none); `clip` carries an optional
    /// clipboard row, display snippet only — the full text never leaves chrome.
    PopoverOpen {
        kind: String,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        #[serde(default)]
        hl: i32,
        #[serde(default)]
        clip: Option<String>,
    },
    /// Close the named popover (blur, Esc, submit, empty results).
    PopoverClose {
        kind: String,
    },
    /// Highlight moved while the field keeps keyboard focus. -1 clears.
    PopoverHighlight {
        index: i32,
    },
    /// Clipboard snippet for the popover's clipboard row; absent clears it.
    PopoverClipboard {
        #[serde(default)]
        clip: Option<String>,
    },
    /// Open one of the chrome surfaces that used to be a DOM popover on the
    /// shared overlay. `id` names a folder/group/bookmark target; `ids` names
    /// a tab set; `items` carries chrome's measured overflow list (bar menu
    /// only). `point` anchors at (x,y) instead of below an owner rect.
    ChromeSurfaceOpen {
        surface: ChromeSurface,
        #[serde(default)]
        id: String,
        #[serde(default)]
        ids: Vec<String>,
        #[serde(default)]
        items: Vec<String>,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        point: bool,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
}

impl ChromeCommand {
    /// Empty and oversize input is rejected, never truncated. 8 KiB covers
    /// a bar-overflow id list (a 100-item bar is ~3.6 KiB of ids) plus a long
    /// URL; chrome is the only producer of this channel, and an unparseable
    /// envelope still never reaches a behavior.
    pub fn parse(json: &str) -> Result<Self, ChromeError> {
        if json.is_empty() || json.len() > 8192 {
            return Err(ChromeError::BadEnvelope);
        }
        serde_json::from_str(json).map_err(|_| ChromeError::BadEnvelope)
    }

    /// Best-effort `cmd` discriminant for rejection logs. Never fails and
    /// never allocates beyond 64 chars: unparseable payloads report
    /// `(unparseable)`, so a hostile 64KB payload cannot flood the log
    /// through its own rejection. Parsed regardless of the 2048B cap, so
    /// oversize posts still name their command.
    pub fn envelope_cmd(json: &str) -> String {
        serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .and_then(|v| {
                v.get("cmd")
                    .and_then(|c| c.as_str())
                    .map(|s| s.chars().take(64).collect())
            })
            .unwrap_or_else(|| "(unparseable)".to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChromeError {
    #[error("rejected chrome envelope")]
    BadEnvelope,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigate_round_trips() {
        let cmd = ChromeCommand::Navigate {
            url: "https://example.com".into(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);
    }

    #[test]
    fn chrome_error_round_trips() {
        // The in-page reporter must survive the strict parser: a rejected
        // reporter is silent UI death all over again.
        let cmd = ChromeCommand::ChromeError {
            message: "TypeError: x is null @ ferrite://chrome:1766".into(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);
    }

    #[test]
    fn menu_open_round_trips() {
        // Overlay menu anchor: window-relative button edges, whole pixels.
        let cmd = ChromeCommand::MenuOpen { x: 1250, y: 80 };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);
    }

    #[test]
    fn popover_open_round_trips_with_and_without_clip() {
        // Popover anchor: window-relative top-left + size, plus the field
        // highlight and an optional clipboard row snippet.
        let cmd = ChromeCommand::PopoverOpen {
            kind: "omnibox".into(),
            x: 40,
            y: 6,
            w: 720,
            h: 48,
            hl: -1,
            clip: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);

        let with_clip = ChromeCommand::PopoverOpen {
            kind: "omnibox".into(),
            x: 0,
            y: 0,
            w: 100,
            h: 40,
            hl: 2,
            clip: Some("https://example.com".into()),
        };
        let json = serde_json::to_string(&with_clip).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), with_clip);
    }

    #[test]
    fn popover_close_highlight_and_clipboard_round_trip() {
        for cmd in [
            ChromeCommand::PopoverClose {
                kind: "omnibox".into(),
            },
            ChromeCommand::PopoverHighlight { index: 3 },
            ChromeCommand::PopoverHighlight { index: -1 },
            ChromeCommand::PopoverClipboard {
                clip: Some("paste and go".into()),
            },
            ChromeCommand::PopoverClipboard { clip: None },
        ] {
            let json = serde_json::to_string(&cmd).unwrap();
            assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);
        }
    }

    #[test]
    fn popover_open_requires_anchor_fields() {
        // No anchor: rejected, never defaulted into a stray popup.
        assert!(ChromeCommand::parse(r#"{"cmd":"popover_open","kind":"omnibox"}"#).is_err());
    }

    #[test]
    fn unknown_cmd_is_rejected_not_ignored() {
        assert!(ChromeCommand::parse(r#"{"cmd":"eval","js":"x"}"#).is_err());
    }

    #[test]
    fn oversize_and_empty_are_rejected() {
        assert!(ChromeCommand::parse("").is_err());
        assert!(ChromeCommand::parse(&"x".repeat(8193)).is_err());
    }

    #[test]
    fn envelope_cmd_names_rejections_without_failing() {
        assert_eq!(
            ChromeCommand::envelope_cmd(r#"{"cmd":"toggle_star","url":"x"}"#),
            "toggle_star"
        );
        // Oversize still names the command (parsed past the length cap).
        let big = format!(
            r#"{{"cmd":"go_back","steps":1,"pad":"{}"}}"#,
            "x".repeat(9000)
        );
        assert!(ChromeCommand::parse(&big).is_err());
        assert_eq!(ChromeCommand::envelope_cmd(&big), "go_back");
        // Long discriminants are capped, garbage never panics.
        assert_eq!(ChromeCommand::envelope_cmd("").as_str(), "(unparseable)");
        assert_eq!(
            ChromeCommand::envelope_cmd("not json").as_str(),
            "(unparseable)"
        );
        assert_eq!(ChromeCommand::envelope_cmd(r#"{"cmd":""}"#).as_str(), "");
        let long = format!(r#"{{"cmd":"{}"}}"#, "y".repeat(200));
        assert_eq!(ChromeCommand::envelope_cmd(&long).len(), 64);
    }

    #[test]
    fn omnibox_submit_round_trips_with_optional_url() {
        let cmd = ChromeCommand::OmniboxSubmit {
            text: "exa".into(),
            url: Some("https://example.com".into()),
            provider: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);
        let bare = ChromeCommand::OmniboxSubmit {
            text: "hello world".into(),
            url: None,
            provider: None,
        };
        let json = serde_json::to_string(&bare).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), bare);
        // Old posts without provider still parse.
        let prov = ChromeCommand::OmniboxSubmit {
            text: "query".into(),
            url: None,
            provider: Some("google".into()),
        };
        let json = serde_json::to_string(&prov).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), prov);
        assert_eq!(
            ChromeCommand::parse(r#"{"cmd":"omnibox_submit","text":"x"}"#).unwrap(),
            ChromeCommand::OmniboxSubmit {
                text: "x".into(),
                url: None,
                provider: None,
            }
        );
    }

    #[test]
    fn perm_decide_round_trips() {
        let cmd = ChromeCommand::PermDecide {
            origin: "https://example.com".into(),
            kind: "camera".into(),
            always: true,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);
        assert!(ChromeCommand::parse(r#"{"cmd":"perm_decide"}"#).is_err());
    }

    #[test]
    fn find_open_and_simple_commands_round_trip() {
        for simple in [
            ChromeCommand::FindOpen,
            ChromeCommand::OpenDevTools,
            ChromeCommand::ReaderOpen,
            ChromeCommand::EnterPip,
            ChromeCommand::ExitPip,
            ChromeCommand::Share,
            ChromeCommand::ZoomIn,
            ChromeCommand::ZoomOut,
            ChromeCommand::ZoomReset,
        ] {
            let json = serde_json::to_string(&simple).unwrap();
            assert_eq!(ChromeCommand::parse(&json).unwrap(), simple);
        }
    }

    #[test]
    fn mobile_find_and_toast_vocabulary_round_trips() {
        // The bundled mobile chrome template is not served yet, but it must
        // keep parsing unchanged: desktop migrated these to overlay `type`
        // messages in 3c-5 without breaking the template's contract.
        for cmd in [
            ChromeCommand::FindSearch {
                query: "needle".into(),
                case: true,
            },
            ChromeCommand::FindNext,
            ChromeCommand::FindPrev,
            ChromeCommand::FindClose,
            ChromeCommand::ToastAction { seq: 4 },
            ChromeCommand::ToastDismiss { seq: 4 },
        ] {
            let json = serde_json::to_string(&cmd).unwrap();
            assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);
        }
        // Shape checks: the case flag and seq are required, never defaulted.
        assert!(ChromeCommand::parse(r#"{"cmd":"find_search","query":"x"}"#).is_err());
        assert!(ChromeCommand::parse(r#"{"cmd":"toast_action"}"#).is_err());
    }

    #[test]
    fn ctx_commands_round_trip_with_ids() {
        for cmd in [
            ChromeCommand::CtxReload { id: "abc".into() },
            ChromeCommand::CtxDuplicate { id: "abc".into() },
            ChromeCommand::CtxPin { id: "abc".into() },
            ChromeCommand::CtxMute { id: "abc".into() },
            ChromeCommand::CtxCopyUrl { id: "abc".into() },
            ChromeCommand::CtxCloseOthers { id: "abc".into() },
            ChromeCommand::CtxCloseRight { id: "abc".into() },
            ChromeCommand::CtxSleep { id: "abc".into() },
            ChromeCommand::CtxBookmarkDelete { id: "abc".into() },
            ChromeCommand::GroupCreate {
                name: "Work".into(),
                tabs: vec!["abc".into()],
                color: 1,
                collapsed: false,
            },
            ChromeCommand::GroupRename {
                id: "abc".into(),
                name: "Play".into(),
            },
            ChromeCommand::GroupColor {
                id: "abc".into(),
                color: 1,
            },
            ChromeCommand::GroupCollapse {
                id: "abc".into(),
                collapsed: true,
            },
            ChromeCommand::GroupDelete { id: "abc".into() },
            ChromeCommand::GroupClose { id: "abc".into() },
            ChromeCommand::GroupMove {
                id: "abc".into(),
                to: 0,
            },
            ChromeCommand::GroupAddTab {
                id: "abc".into(),
                tab: "def".into(),
            },
            ChromeCommand::GroupRemoveTab { tab: "def".into() },
            ChromeCommand::TabsClose {
                ids: vec!["abc".into()],
            },
            ChromeCommand::TabsMute {
                ids: vec!["abc".into()],
                muted: true,
            },
            ChromeCommand::TabsPin {
                ids: vec!["abc".into()],
                pinned: false,
            },
            ChromeCommand::TabsMove {
                ids: vec!["abc".into(), "def".into()],
                to: 0,
            },
            ChromeCommand::WsCreate {
                name: "Deep".into(),
            },
            ChromeCommand::WsRename {
                id: "abc".into(),
                name: "Deeper".into(),
            },
            ChromeCommand::WsDelete { id: "abc".into() },
            ChromeCommand::WsSwitch { id: "abc".into() },
            ChromeCommand::WsDefault,
            ChromeCommand::SleepActive,
            ChromeCommand::TranslatePage,
            ChromeCommand::DownloadsRecent,
            ChromeCommand::DlBubbleOpen { id: "abc".into() },
            ChromeCommand::DlBubbleFolder { id: "abc".into() },
            ChromeCommand::DlBubbleRetry { id: "abc".into() },
            ChromeCommand::DlBubbleRemove { id: "abc".into() },
            ChromeCommand::ProfileSwitch { id: "Work".into() },
            ChromeCommand::ProfileCreate {
                name: "Work".into(),
            },
            ChromeCommand::ProfileDelete { id: "Work".into() },
            ChromeCommand::CrashRestore,
            ChromeCommand::CrashRestoreExceptActive,
            ChromeCommand::CrashDismiss,
            ChromeCommand::ShieldPause,
        ] {
            let json = serde_json::to_string(&cmd).unwrap();
            assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);
        }
    }

    #[test]
    fn mute_pin_carry_optional_tab_id() {
        // Strip icons name their tab; toolbar/palette posts omit it (active
        // fallback server-side). Old bare posts must still parse.
        let with_id = ChromeCommand::ToggleMute {
            id: Some("abc".into()),
        };
        let json = serde_json::to_string(&with_id).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), with_id);
        assert_eq!(
            ChromeCommand::parse(r#"{"cmd":"toggle_mute"}"#).unwrap(),
            ChromeCommand::ToggleMute { id: None }
        );
        assert_eq!(
            ChromeCommand::parse(r#"{"cmd":"toggle_pin"}"#).unwrap(),
            ChromeCommand::TogglePin { id: None }
        );
        let pin = ChromeCommand::TogglePin {
            id: Some("abc".into()),
        };
        let json = serde_json::to_string(&pin).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), pin);
    }

    #[test]
    fn menu_and_engine_commands_parse() {
        assert!(ChromeCommand::parse(r#"{"cmd":"menu_item","id":"settings"}"#).is_ok());
        assert!(ChromeCommand::parse(r#"{"cmd":"select_engine","id":"ddg"}"#).is_ok());
        assert!(ChromeCommand::parse(r#"{"cmd":"request_state"}"#).is_ok());
        assert!(ChromeCommand::parse(r#"{"cmd":"toggle_maximize"}"#).is_ok());
        // The history surface posts popover_open now; the old hook commands
        // (history_recent / hist_list / closed_reopen / closed_restore_all)
        // are deleted envelope surface and must stay rejected.
        for gone in [
            r#"{"cmd":"history_recent"}"#,
            r#"{"cmd":"hist_list","dir":"back"}"#,
            r#"{"cmd":"closed_reopen","index":0}"#,
            r#"{"cmd":"closed_restore_all"}"#,
        ] {
            assert!(
                ChromeCommand::parse(gone).is_err(),
                "deleted still parsed: {gone}"
            );
        }
    }

    #[test]
    fn star_and_reorder_commands_parse() {
        let star = r#"{"cmd":"toggle_star","url":"https://example.com","title":"Example"}"#;
        assert!(matches!(
            ChromeCommand::parse(star).unwrap(),
            ChromeCommand::ToggleStar { .. }
        ));
        let reorder = r#"{"cmd":"reorder","from":2,"to":0}"#;
        assert_eq!(
            ChromeCommand::parse(reorder).unwrap(),
            ChromeCommand::Reorder { from: 2, to: 0 }
        );
    }

    #[test]
    fn chrome_surface_open_round_trips_and_requires_geometry() {
        let cmd = ChromeCommand::ChromeSurfaceOpen {
            surface: ChromeSurface::BarMenu,
            id: "folder-a".into(),
            ids: vec!["tab-1".into(), "tab-2".into()],
            items: vec!["bookmark-a".into()],
            parent: Some("root".into()),
            point: false,
            x: 120,
            y: 40,
            w: 80,
            h: 24,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(ChromeCommand::parse(&json).unwrap(), cmd);
        // Missing anchor geometry is rejected, never defaulted into a popup
        // at (0,0).
        assert!(ChromeCommand::parse(r#"{"cmd":"chrome_surface_open","surface":"star"}"#).is_err());
        assert!(
            ChromeCommand::parse(
                r#"{"cmd":"chrome_surface_open","surface":"nope","x":0,"y":0,"w":0,"h":0}"#
            )
            .is_err()
        );
    }

    #[test]
    fn pop_values_defaults_and_round_trips() {
        assert_eq!(
            PopValues::default(),
            PopValues {
                name: String::new(),
                folder: String::new(),
                color: 0,
                collapsed: false
            }
        );
        let v = PopValues {
            name: "Docs".into(),
            folder: "f1".into(),
            color: 3,
            collapsed: true,
        };
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<PopValues>(&json).unwrap(), v);
        // Missing fields default instead of failing the whole submit.
        assert_eq!(
            serde_json::from_str::<PopValues>(r#"{"name":"Docs"}"#)
                .unwrap()
                .color,
            0
        );
    }
}
