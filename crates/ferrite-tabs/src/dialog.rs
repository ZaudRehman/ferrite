//! Script dialog state: types, suppression, per tab queue. Origin tagged
//! and tab modal, so page boxes cannot pose as browser chrome. Windows
//! intercepts natively, other platforms keep engine defaults.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use uuid::Uuid;

/// Fixed beforeunload text. Page text is never used for this kind.
pub const BEFOREUNLOAD_TEXT: &str = "Leave site? Changes you made may not be saved.";

/// Display cap for page text. Overlong input gets an ellipsis, never panics.
pub const DIALOG_MESSAGE_CAP: usize = 2048;

/// Per tab depth past the shown head. Beyond this new dialogs autocancel.
pub const DIALOG_QUEUE_CAP: usize = 4;

/// Repeat window for the suppression offer.
pub const SUPPRESS_WINDOW: Duration = Duration::from_secs(30);

/// Kind mapping matches WebView2 SDK order: Alert 0, Confirm 1, Prompt 2,
/// BeforeUnload 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
    BeforeUnload,
}

impl DialogKind {
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Alert),
            1 => Some(Self::Confirm),
            2 => Some(Self::Prompt),
            3 => Some(Self::BeforeUnload),
            _ => None,
        }
    }

    /// Only prompts carry an input (and a result). Everything else ignores
    /// response text, there is nowhere for it to go.
    pub fn takes_input(self) -> bool {
        matches!(self, Self::Prompt)
    }

    /// Wire name for the overlay frame (matches the serde rendering).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alert => "alert",
            Self::Confirm => "confirm",
            Self::Prompt => "prompt",
            Self::BeforeUnload => "beforeunload",
        }
    }
}

/// Shorten untrusted dialog text for display. Mirrors the toast helper.
/// Kept local so this module stays dependency-light.
fn short(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_owned();
    }
    let mut out: String = chars[..max].iter().collect();
    out.push('…');
    out
}

/// Attribution key for suppression and repeat counting: lowercased host
/// when the URI parses, otherwise the capped URI itself. Both the platform
/// fast-path check and the shell policy call this function.
pub fn origin_key(uri: &str) -> String {
    if let Ok(url) = url::Url::parse(uri)
        && let Some(host) = url.host_str()
    {
        return host.to_lowercase();
    }
    short(&uri.to_lowercase(), 256)
}

/// One intercepted dialog, deferral already held by the platform glue.
/// The page script stays blocked until the shell completes the matching
/// deferral (accept or cancel), never by dropping the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogRequest {
    /// Unpredictable frame id (random, not sequential): overlay posts
    /// pair by `seq`, and a guessable id would let one page answer
    /// another tab's dialog.
    pub seq: u64,
    pub tab_id: Uuid,
    pub kind: DialogKind,
    pub message: String,
    pub uri: String,
    pub default_text: String,
}

impl DialogRequest {
    pub fn new(
        seq: u64,
        tab_id: Uuid,
        kind: DialogKind,
        message: &str,
        uri: &str,
        default_text: &str,
    ) -> Self {
        Self {
            seq,
            tab_id,
            kind,
            message: short(message, DIALOG_MESSAGE_CAP),
            uri: uri.to_owned(),
            default_text: short(default_text, DIALOG_MESSAGE_CAP),
        }
    }

    /// Text actually shown: page message for alert/confirm/prompt, the
    /// fixed browser-authored line for beforeunload (page text ignored).
    pub fn display_text(&self) -> &str {
        if self.kind == DialogKind::BeforeUnload {
            BEFOREUNLOAD_TEXT
        } else {
            &self.message
        }
    }
}

/// One intercepted dialog as the shell loop receives it (platform glue
/// wraps [`DialogRequest`]; the deferral stays stashed under `seq`).
#[derive(Debug, Clone)]
pub struct DialogOpened {
    pub req: DialogRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogPush {
    /// Head of its tab queue: render immediately (if the tab is active).
    Shown,
    /// Behind an earlier dialog on the same tab: queued, tab badged.
    Queued,
    /// Suppressed origin or queue overflow: the caller must complete the
    /// deferral as cancelled right away. The script must never hang.
    AutoCancelled,
}

/// Per-tab dialog queues plus session suppression state.
#[derive(Debug, Default)]
pub struct DialogStore {
    pending: HashMap<Uuid, VecDeque<DialogRequest>>,
    suppressed: HashSet<String>,
    counts: HashMap<String, (u32, Instant)>,
}

impl DialogStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, req: DialogRequest) -> DialogPush {
        // Suppression covers alert/confirm/prompt only: an auto-cancelled
        // beforeunload reads as "stay", which would silently trap a
        // navigating user with no UI to explain it. beforeunload always
        // surfaces (it cannot loop without navigation gestures anyway).
        if req.kind != DialogKind::BeforeUnload && self.suppressed.contains(&origin_key(&req.uri)) {
            return DialogPush::AutoCancelled;
        }
        let q = self.pending.entry(req.tab_id).or_default();
        if q.len() > DIALOG_QUEUE_CAP {
            return DialogPush::AutoCancelled;
        }
        q.push_back(req);
        if q.len() == 1 {
            DialogPush::Shown
        } else {
            DialogPush::Queued
        }
    }

    /// Head of a tab's queue (the frame to render), if any.
    pub fn head(&self, tab_id: Uuid) -> Option<&DialogRequest> {
        self.pending.get(&tab_id).and_then(|q| q.front())
    }

    /// Pop the head after it resolves (respond, dismiss, or teardown).
    pub fn pop_head(&mut self, tab_id: Uuid) -> Option<DialogRequest> {
        let mut empty = false;
        let out = self.pending.get_mut(&tab_id).and_then(|q| {
            let head = q.pop_front();
            empty = q.is_empty();
            head
        });
        if empty {
            self.pending.remove(&tab_id);
        }
        out
    }

    /// Drop a tab's whole queue, returning every request so the caller
    /// can complete each outstanding deferral (cancel semantics).
    pub fn drop_tab(&mut self, tab_id: Uuid) -> Vec<DialogRequest> {
        self.pending
            .remove(&tab_id)
            .map(|q| q.into_iter().collect())
            .unwrap_or_default()
    }

    /// Any dialog waiting on this tab (head or queued).
    pub fn has_pending(&self, tab_id: Uuid) -> bool {
        self.pending.get(&tab_id).is_some_and(|q| !q.is_empty())
    }

    /// Tabs with any pending dialog (badge rendering, liveness sweeps).
    pub fn tabs_with_pending(&self) -> Vec<Uuid> {
        self.pending
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .map(|(id, _)| *id)
            .collect()
    }

    /// Record that a dialog from `origin` was shown now. Returns whether
    /// the frame should offer the suppression checkbox: the second dialog
    /// from one origin inside [`SUPPRESS_WINDOW`].
    pub fn record_shown(&mut self, origin: &str, now: Instant) -> bool {
        let (count, _) = self
            .counts
            .entry(origin.to_owned())
            .and_modify(|(n, at)| {
                if now.duration_since(*at) <= SUPPRESS_WINDOW {
                    *n += 1;
                } else {
                    *n = 1;
                    *at = now;
                }
            })
            .or_insert((1, now));
        *count >= 2
    }

    pub fn set_suppressed(&mut self, origin: &str, suppress: bool) {
        if suppress {
            self.suppressed.insert(origin.to_owned());
        } else {
            self.suppressed.remove(origin);
        }
    }

    pub fn is_suppressed(&self, origin: &str) -> bool {
        self.suppressed.contains(origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(seq: u64, tab: Uuid, kind: DialogKind, uri: &str) -> DialogRequest {
        DialogRequest::new(seq, tab, kind, "msg", uri, "")
    }

    #[test]
    fn kind_maps_verified_sdk_values() {
        assert_eq!(DialogKind::from_i32(0), Some(DialogKind::Alert));
        assert_eq!(DialogKind::from_i32(1), Some(DialogKind::Confirm));
        assert_eq!(DialogKind::from_i32(2), Some(DialogKind::Prompt));
        assert_eq!(DialogKind::from_i32(3), Some(DialogKind::BeforeUnload));
        assert_eq!(DialogKind::from_i32(4), None);
        assert_eq!(DialogKind::from_i32(-1), None);
        assert!(DialogKind::Prompt.takes_input());
        assert!(!DialogKind::Confirm.takes_input());
        assert!(!DialogKind::Alert.takes_input());
    }

    #[test]
    fn origin_key_scopes_by_host() {
        assert_eq!(
            origin_key("https://Example.COM:443/path?q=1"),
            "example.com"
        );
        assert_eq!(origin_key("https://sub.example.com/"), "sub.example.com");
        // Subdomains are distinct origins (no suffix collapsing here,
        // suppression must never leak across sites).
        assert_ne!(
            origin_key("https://a.example.com/"),
            origin_key("https://b.example.com/")
        );
        // Opaque origins scope to themselves, never a shared bucket.
        assert_eq!(origin_key("about:blank"), "about:blank");
        assert!(origin_key("about:blank") != origin_key("https://example.com/"));
    }

    #[test]
    fn message_truncates_without_panic() {
        let r = DialogRequest::new(
            1,
            Uuid::new_v4(),
            DialogKind::Alert,
            &"x".repeat(5000),
            "https://e.com/",
            "",
        );
        assert!(r.message.chars().count() <= DIALOG_MESSAGE_CAP + 1);
        let wide = DialogRequest::new(
            2,
            Uuid::new_v4(),
            DialogKind::Alert,
            &"é".repeat(5000),
            "https://e.com/",
            "",
        );
        assert!(wide.message.contains('…'));
    }

    #[test]
    fn beforeunload_ignores_page_text() {
        let r = DialogRequest::new(
            1,
            Uuid::new_v4(),
            DialogKind::BeforeUnload,
            "Fake urgent text, act now!",
            "https://e.com/",
            "",
        );
        assert_eq!(r.display_text(), BEFOREUNLOAD_TEXT);
        let a = DialogRequest::new(
            2,
            Uuid::new_v4(),
            DialogKind::Alert,
            "Real msg",
            "https://e.com/",
            "",
        );
        assert_eq!(a.display_text(), "Real msg");
    }

    #[test]
    fn queue_shows_head_and_queues_rest() {
        let mut store = DialogStore::new();
        let tab = Uuid::new_v4();
        assert_eq!(
            store.push(req(1, tab, DialogKind::Alert, "https://a.com/")),
            DialogPush::Shown
        );
        assert_eq!(
            store.push(req(2, tab, DialogKind::Confirm, "https://a.com/")),
            DialogPush::Queued
        );
        assert_eq!(store.head(tab).map(|r| r.seq), Some(1));
        assert!(store.has_pending(tab));
        assert_eq!(store.pop_head(tab).map(|r| r.seq), Some(1));
        assert_eq!(store.head(tab).map(|r| r.seq), Some(2));
        store.pop_head(tab);
        assert!(!store.has_pending(tab));
    }

    #[test]
    fn queue_overflow_auto_cancels() {
        let mut store = DialogStore::new();
        let tab = Uuid::new_v4();
        for i in 0..(DIALOG_QUEUE_CAP + 1) {
            let p = store.push(req(i as u64, tab, DialogKind::Alert, "https://a.com/"));
            assert!(matches!(p, DialogPush::Shown | DialogPush::Queued), "i={i}");
        }
        assert_eq!(
            store.push(req(99, tab, DialogKind::Alert, "https://a.com/")),
            DialogPush::AutoCancelled
        );
    }

    #[test]
    fn suppressed_origins_auto_cancel() {
        let mut store = DialogStore::new();
        let tab = Uuid::new_v4();
        store.set_suppressed("evil.com", true);
        assert!(store.is_suppressed("evil.com"));
        assert_eq!(
            store.push(req(1, tab, DialogKind::Alert, "https://evil.com/paynowa")),
            DialogPush::AutoCancelled
        );
        store.set_suppressed("evil.com", false);
        assert_eq!(
            store.push(req(2, tab, DialogKind::Alert, "https://evil.com/paynowa")),
            DialogPush::Shown
        );
    }

    #[test]
    fn suppression_never_silences_beforeunload() {
        let mut store = DialogStore::new();
        let tab = Uuid::new_v4();
        store.set_suppressed("evil.com", true);
        // Auto-cancelling beforeunload would trap navigation silently
        // (cancel reads as "stay") with no UI to explain it.
        assert_eq!(
            store.push(req(
                1,
                tab,
                DialogKind::BeforeUnload,
                "https://evil.com/app"
            )),
            DialogPush::Shown
        );
    }

    #[test]
    fn suppression_checkbox_on_second_in_window() {
        let mut store = DialogStore::new();
        let t = Instant::now();
        assert!(!store.record_shown("a.com", t));
        assert!(store.record_shown("a.com", t + Duration::from_secs(5)));
        // Outside the window the count resets, stale history never arms it.
        assert!(!store.record_shown("a.com", t + SUPPRESS_WINDOW + Duration::from_secs(1)));
        // A fresh origin starts its own count.
        assert!(!store.record_shown("b.com", t + SUPPRESS_WINDOW + Duration::from_secs(1)));
        assert!(store.record_shown("b.com", t + SUPPRESS_WINDOW + Duration::from_secs(2)));
    }

    #[test]
    fn drop_tab_returns_all_for_completion() {
        let mut store = DialogStore::new();
        let tab = Uuid::new_v4();
        store.push(req(1, tab, DialogKind::Alert, "https://a.com/"));
        store.push(req(2, tab, DialogKind::Alert, "https://a.com/"));
        let dropped = store.drop_tab(tab);
        assert_eq!(dropped.len(), 2);
        assert!(!store.has_pending(tab));
        assert!(store.drop_tab(tab).is_empty());
    }
}
