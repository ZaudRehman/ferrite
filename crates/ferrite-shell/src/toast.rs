//! Toast queue. Confirms what happened, never asks. Same source coalesces,
//! different sources queue one at a time. Queue capped, head never dropped.
//! `seq` pairs clicks with the shown frame. Undo stages rows while live.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ferrite_chrome::chrome::{ToastActionView, ToastAnchor, ToastView};
use ferrite_vault::vault::{DownloadRecord, HistoryEntry, ReadingItem};

/// Undo staging cap. Larger deletes apply without Undo. Matches the bulk cap.
pub const UNDO_SNAPSHOT_CAP: usize = 500;

/// Queue depth past the visible head.
pub const TOAST_QUEUE_CAP: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastSource {
    Downloads,
    Failure,
    Clipboard,
    Delete,
    /// Password-save prompt. Queues like a delete (no coalescing with
    /// other sources). The secret lives in shell staging, never the item.
    Credentials,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastActionKind {
    Undo,
    OpenDownloads { id: Option<String> },
    SaveLogin,
}

impl ToastActionKind {
    fn label(&self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::OpenDownloads { .. } => "Open",
            Self::SaveLogin => "Save",
        }
    }

    fn view(&self) -> ToastActionView {
        match self {
            Self::Undo => ToastActionView::Undo,
            Self::OpenDownloads { id } => ToastActionView::OpenDownloads { id: id.clone() },
            Self::SaveLogin => ToastActionView::SaveLogin,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToastKind {
    Downloads {
        count: u32,
        last_name: String,
        last_id: String,
    },
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastItem {
    seq: u64,
    source: ToastSource,
    icon: &'static str,
    text: String,
    action: Option<ToastActionKind>,
    anchor_downloads: bool,
    duration: Duration,
    shown_at: Option<Instant>,
    kind: ToastKind,
}

/// Shorten untrusted text (filenames, URLs, titles) for one-line display.
/// Character-boundary safe. Overlong input gets an ellipsis, never a panic.
fn short(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_owned();
    }
    let mut out: String = chars[..max].iter().collect();
    out.push('…');
    out
}

impl ToastItem {
    /// Download completion. Coalescing happens in [`ToastQueue::push`].
    /// This builds the single-download frame.
    pub fn download(filename: &str, id: &str) -> Self {
        Self {
            seq: 0,
            source: ToastSource::Downloads,
            icon: "dl",
            text: format!("Download complete: {}", short(filename, 80)),
            action: Some(ToastActionKind::OpenDownloads {
                id: Some(id.to_owned()),
            }),
            anchor_downloads: true,
            duration: Duration::from_secs(4),
            shown_at: None,
            kind: ToastKind::Downloads {
                count: 1,
                last_name: filename.to_owned(),
                last_id: id.to_owned(),
            },
        }
    }

    /// Worker/loop failure text (save/export/fetch). Latest wins.
    pub fn failure(text: String) -> Self {
        Self {
            seq: 0,
            source: ToastSource::Failure,
            icon: "alert",
            text: short(&text, 160),
            action: None,
            anchor_downloads: false,
            duration: Duration::from_secs(5),
            shown_at: None,
            kind: ToastKind::Plain,
        }
    }

    /// Clipboard confirmation (the generalized "Copied" primitive).
    pub fn clipboard(text: &str) -> Self {
        Self {
            seq: 0,
            source: ToastSource::Clipboard,
            icon: "check",
            text: short(text, 120),
            action: None,
            anchor_downloads: false,
            duration: Duration::from_secs(3),
            shown_at: None,
            kind: ToastKind::Plain,
        }
    }

    /// Success receipt (no action: the work is done, this only confirms).
    pub fn saved(text: &str) -> Self {
        Self {
            seq: 0,
            source: ToastSource::Credentials,
            icon: "check",
            text: short(text, 120),
            action: None,
            anchor_downloads: false,
            duration: Duration::from_secs(3),
            shown_at: None,
            kind: ToastKind::Plain,
        }
    }

    /// Undoable delete. The staged rows live in [`PendingUndo`] under the
    /// same `seq`. The action restores only while that staging is live.
    pub fn undoable(text: String) -> Self {
        Self {
            seq: 0,
            source: ToastSource::Delete,
            icon: "check",
            text: short(&text, 160),
            action: Some(ToastActionKind::Undo),
            anchor_downloads: false,
            duration: Duration::from_secs(6),
            shown_at: None,
            kind: ToastKind::Plain,
        }
    }

    /// Plain delete confirmation when the delete was too large to stage
    /// (no Undo. The toast confirms, it never promises a restore).
    pub fn delete_done(text: String) -> Self {
        let mut item = Self::undoable(text);
        item.action = None;
        item.duration = Duration::from_secs(4);
        item
    }

    /// Password-save prompt. The secret lives in shell staging under the
    /// same `seq`: the item carries origin and username text only, never the
    /// password. 8s: a security decision gets longer than a receipt.
    pub fn save_login(origin: &str, username: &str, update: bool) -> Self {
        let text = if update {
            format!(
                "Update saved login for {} ({})?",
                short(origin, 60),
                short(username, 40)
            )
        } else {
            format!(
                "Save login for {} ({})?",
                short(origin, 60),
                short(username, 40)
            )
        };
        Self {
            seq: 0,
            source: ToastSource::Credentials,
            icon: "key",
            text,
            action: Some(ToastActionKind::SaveLogin),
            anchor_downloads: false,
            duration: Duration::from_secs(8),
            shown_at: None,
            kind: ToastKind::Plain,
        }
    }

    fn merge_download(&mut self, filename: &str, id: &str) {
        if let ToastKind::Downloads {
            count,
            last_name,
            last_id,
        } = &mut self.kind
        {
            *count += 1;
            *last_name = filename.to_owned();
            *last_id = id.to_owned();
            let n = *count;
            self.text = if n > 1 {
                format!("{n} downloads complete")
            } else {
                format!("Download complete: {}", short(filename, 80))
            };
            // A deep link names one row. A coalesced toast links the page.
            self.action = Some(ToastActionKind::OpenDownloads {
                id: if n > 1 { None } else { Some(id.to_owned()) },
            });
        }
    }
}

#[derive(Debug, Default)]
pub struct ToastQueue {
    current: Option<ToastItem>,
    pending: VecDeque<ToastItem>,
    next_seq: u64,
}

impl ToastQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one toast. Downloads coalesce into the visible or queued
    /// downloads frame. Failures replace the visible failure. Everything
    /// else queues behind the head (cap drops the oldest queued entry).
    pub fn push(&mut self, mut item: ToastItem, now: Instant) {
        self.next_seq += 1;
        item.seq = self.next_seq;
        match item.source {
            ToastSource::Downloads => {
                let (filename, id) = match &item.kind {
                    ToastKind::Downloads {
                        last_name, last_id, ..
                    } => (last_name.clone(), last_id.clone()),
                    ToastKind::Plain => return,
                };
                if let Some(cur) = self.current.as_mut()
                    && cur.source == ToastSource::Downloads
                {
                    cur.merge_download(&filename, &id);
                    self.next_seq += 1;
                    cur.seq = self.next_seq;
                    cur.shown_at = Some(now);
                    return;
                }
                if let Some(slot) = self
                    .pending
                    .iter_mut()
                    .find(|t| t.source == ToastSource::Downloads)
                {
                    slot.merge_download(&filename, &id);
                    self.next_seq += 1;
                    slot.seq = self.next_seq;
                    return;
                }
                self.show(item, now);
            }
            ToastSource::Failure => {
                if let Some(cur) = self.current.as_mut()
                    && cur.source == ToastSource::Failure
                {
                    // Latest failure wins (the long-standing notice rule).
                    *cur = item;
                    cur.shown_at = Some(now);
                    return;
                }
                self.show_or_queue(item, now);
            }
            ToastSource::Clipboard | ToastSource::Delete | ToastSource::Credentials => {
                self.show_or_queue(item, now)
            }
        }
    }

    /// Push an undoable-delete toast. At most one Undo toast exists at a
    /// time (newer replaces visible or queued, older staging superseded).
    /// Returns the frame seq for pairing [`PendingUndo`].
    pub fn push_undo(&mut self, mut item: ToastItem, now: Instant) -> u64 {
        debug_assert!(matches!(item.source, ToastSource::Delete));
        debug_assert!(
            item.action
                .as_ref()
                .is_some_and(|a| matches!(a, ToastActionKind::Undo))
        );
        self.next_seq += 1;
        item.seq = self.next_seq;
        let seq = item.seq;
        if let Some(cur) = self.current.as_mut()
            && cur.source == ToastSource::Delete
        {
            *cur = item;
            cur.shown_at = Some(now);
            return seq;
        }
        if let Some(slot) = self
            .pending
            .iter_mut()
            .find(|t| t.source == ToastSource::Delete)
        {
            *slot = item;
            return seq;
        }
        self.show_or_queue(item, now);
        seq
    }

    /// Push a password-save prompt. Single slot like Undo (newer replaces
    /// visible or queued, superseded secret dropped with its staging).
    /// Returns the frame seq for pairing the shell secret staging.
    pub fn push_save(&mut self, mut item: ToastItem, now: Instant) -> u64 {
        debug_assert!(matches!(item.source, ToastSource::Credentials));
        debug_assert!(
            item.action
                .as_ref()
                .is_some_and(|a| matches!(a, ToastActionKind::SaveLogin))
        );
        self.next_seq += 1;
        item.seq = self.next_seq;
        let seq = item.seq;
        if let Some(cur) = self.current.as_mut()
            && cur.source == ToastSource::Credentials
        {
            *cur = item;
            cur.shown_at = Some(now);
            return seq;
        }
        if let Some(slot) = self
            .pending
            .iter_mut()
            .find(|t| t.source == ToastSource::Credentials)
        {
            *slot = item;
            return seq;
        }
        self.show_or_queue(item, now);
        seq
    }

    fn show_or_queue(&mut self, mut item: ToastItem, now: Instant) {
        if self.current.is_none() {
            self.show(item, now);
        } else {
            item.shown_at = None;
            if self.pending.len() >= TOAST_QUEUE_CAP {
                self.pending.pop_front();
            }
            self.pending.push_back(item);
        }
    }

    fn show(&mut self, mut item: ToastItem, now: Instant) {
        item.shown_at = Some(now);
        self.current = Some(item);
    }

    /// Expire the head past its duration and advance the queue.
    /// Returns true when chrome needs a re-push.
    pub fn tick(&mut self, now: Instant) -> bool {
        let expired = self.current.as_ref().is_some_and(|c| {
            c.shown_at
                .is_some_and(|at| now.duration_since(at) >= c.duration)
        });
        if !expired {
            if self.current.is_none() && !self.pending.is_empty() {
                if let Some(mut next) = self.pending.pop_front() {
                    next.shown_at = Some(now);
                    self.current = Some(next);
                }
                return true;
            }
            return false;
        }
        self.current = self.pending.pop_front();
        if let Some(cur) = self.current.as_mut() {
            cur.shown_at = Some(now);
        }
        true
    }

    /// Dismiss the visible toast early (manual close). Stale seqs are ignored.
    pub fn dismiss(&mut self, seq: u64, now: Instant) -> bool {
        if self.current.as_ref().is_some_and(|c| c.seq == seq) {
            self.current = self.pending.pop_front();
            if let Some(cur) = self.current.as_mut() {
                cur.shown_at = Some(now);
            }
            return true;
        }
        false
    }

    /// Action for the visible toast, if `seq` matches it.
    pub fn action_for(&self, seq: u64) -> Option<ToastActionKind> {
        self.current
            .as_ref()
            .filter(|c| c.seq == seq)
            .and_then(|c| c.action.clone())
    }

    /// Action on the visible head, if any (deadline refresh for undo).
    pub fn current_action(&self) -> Option<ToastActionKind> {
        self.current.as_ref().and_then(|c| c.action.clone())
    }

    /// Current frame sequence (for pairing undo staging).
    pub fn current_seq(&self) -> Option<u64> {
        self.current.as_ref().map(|c| c.seq)
    }

    /// Whether a frame is still in the system (head or queued). Undo
    /// staging survives exactly as long as its toast does.
    pub fn contains_seq(&self, seq: u64) -> bool {
        self.current.as_ref().is_some_and(|c| c.seq == seq)
            || self.pending.iter().any(|t| t.seq == seq)
    }

    /// Render the head for the chrome push. `None` hides the popup.
    pub fn current_view(&self) -> Option<ToastView> {
        self.current.as_ref().map(|c| ToastView {
            seq: c.seq,
            icon: c.icon.to_owned(),
            text: c.text.clone(),
            action_label: c.action.as_ref().map(|a| a.label().to_owned()),
            action: c.action.as_ref().map(|a| a.view()),
            anchor: if c.anchor_downloads {
                ToastAnchor::Downloads
            } else {
                ToastAnchor::Floating
            },
            duration_ms: c.duration.as_millis() as u64,
        })
    }
}

/// Staged rows behind an Undo toast. Single slot: a newer undoable delete
/// supersedes the older one (its toast is gone, so its action is dead,
/// expiring with the toast keeps action and staging in lockstep).
#[derive(Debug)]
pub enum UndoPayload {
    Bookmarks(Vec<String>),
    History(Vec<HistoryEntry>),
    Downloads(Vec<DownloadRecord>),
    Reading(Vec<ReadingItem>),
    /// Time-ranged clear-data (history + download rows together).
    ClearData {
        history: Vec<HistoryEntry>,
        downloads: Vec<DownloadRecord>,
    },
}

#[derive(Debug)]
pub struct PendingUndo {
    pub seq: u64,
    pub payload: UndoPayload,
    pub expires: Instant,
}

impl PendingUndo {
    pub fn is_live(&self, seq: u64, now: Instant) -> bool {
        self.seq == seq && now < self.expires
    }
}

/// Staged secret behind a Save-login toast. Single slot like Undo, the
/// secret dies with the toast. `Drop` zeroizes, consume paths zeroize
/// the moved buffer explicitly after encrypting.
#[derive(Debug)]
pub struct PendingSave {
    pub seq: u64,
    pub origin: String,
    pub username: String,
    pub password: Vec<u8>,
    pub expires: Instant,
}

impl PendingSave {
    pub fn is_live(&self, seq: u64, now: Instant) -> bool {
        self.seq == seq && now < self.expires
    }

    /// Take the password bytes for one encrypt operation. The staging is
    /// consumed with them. Callers zeroize the taken buffer after use.
    pub fn take_password(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.password)
    }
}

impl Drop for PendingSave {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.password.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn first_push_shows_immediately() {
        let mut q = ToastQueue::new();
        q.push(ToastItem::failure("boom".into()), now());
        let v = q.current_view().expect("visible");
        assert_eq!(v.text, "boom");
        assert_eq!(v.icon, "alert");
        assert!(v.action.is_none());
    }

    #[test]
    fn downloads_coalesce_into_one_frame() {
        let mut q = ToastQueue::new();
        let t = now();
        q.push(ToastItem::download("a.zip", "dl-1"), t);
        q.push(ToastItem::download("b.zip", "dl-2"), t);
        let v = q.current_view().expect("visible");
        assert_eq!(v.text, "2 downloads complete");
        // Coalesced frame links the page, not one row.
        assert!(matches!(
            v.action,
            Some(ToastActionView::OpenDownloads { id: None })
        ));
        assert!(matches!(v.anchor, ToastAnchor::Downloads));
    }

    #[test]
    fn different_sources_queue_one_at_a_time() {
        let mut q = ToastQueue::new();
        let t = now();
        q.push(ToastItem::failure("boom".into()), t);
        q.push(ToastItem::clipboard("Copied"), t);
        assert_eq!(q.current_view().expect("head").text, "boom");
        // Failure TTL is 5s: advancing 6s retires it onto the clipboard.
        q.tick(t + Duration::from_secs(6));
        assert_eq!(q.current_view().expect("next").text, "Copied");
    }

    #[test]
    fn failure_replaces_visible_failure() {
        let mut q = ToastQueue::new();
        let t = now();
        q.push(ToastItem::failure("one".into()), t);
        q.push(ToastItem::failure("two".into()), t);
        assert_eq!(q.current_view().expect("head").text, "two");
    }

    #[test]
    fn queue_cap_drops_oldest_queued_never_head() {
        let mut q = ToastQueue::new();
        let t = now();
        q.push(ToastItem::failure("head".into()), t);
        for i in 0..(TOAST_QUEUE_CAP + 2) {
            q.push(ToastItem::clipboard(format!("c{i}").as_str()), t);
        }
        assert_eq!(q.current_view().expect("head").text, "head");
        q.tick(t + Duration::from_secs(6));
        // c0 and c1 dropped. C2 survived the cap.
        assert_eq!(q.current_view().expect("next").text, "c2");
    }

    #[test]
    fn dismiss_advances_and_stale_seq_ignored() {
        let mut q = ToastQueue::new();
        let t = now();
        q.push(ToastItem::clipboard("one"), t);
        q.push(ToastItem::clipboard("two"), t);
        let seq = q.current_seq().expect("seq");
        assert!(!q.dismiss(seq + 99, t));
        assert!(q.dismiss(seq, t));
        assert_eq!(q.current_view().expect("next").text, "two");
    }

    #[test]
    fn action_pairs_with_visible_seq_only() {
        let mut q = ToastQueue::new();
        q.push(ToastItem::undoable("Bookmark deleted".into()), now());
        let seq = q.current_seq().expect("seq");
        assert!(matches!(q.action_for(seq), Some(ToastActionKind::Undo)));
        assert!(q.action_for(seq + 1).is_none());
    }

    #[test]
    fn undo_counts_as_delete_source_and_queues() {
        let mut q = ToastQueue::new();
        let t = now();
        q.push(ToastItem::failure("boom".into()), t);
        q.push(ToastItem::undoable("3 bookmarks deleted".into()), t);
        // Undo waits behind the failure instead of clobbering it.
        assert_eq!(q.current_view().expect("head").text, "boom");
        q.tick(t + Duration::from_secs(6));
        let v = q.current_view().expect("undo");
        assert_eq!(v.text, "3 bookmarks deleted");
        assert_eq!(v.duration_ms, 6000);
    }

    #[test]
    fn toast_text_truncates_without_panic() {
        let long = "x".repeat(500);
        let item = ToastItem::failure(long);
        assert!(item.text.chars().count() <= 161);
        // Character-boundary safe on multibyte input.
        let wide = "é".repeat(500);
        assert!(ToastItem::failure(wide).text.contains('…'));
    }

    #[test]
    fn push_undo_replaces_older_undo_never_two() {
        let mut q = ToastQueue::new();
        let t = now();
        let s1 = q.push_undo(ToastItem::undoable("one deleted".into()), t);
        let s2 = q.push_undo(ToastItem::undoable("two deleted".into()), t);
        assert_ne!(s1, s2);
        // Single head, latest text. The older staging's action is dead
        // with it, so two live Undos can never disagree.
        assert_eq!(q.current_view().expect("head").text, "two deleted");
        assert!(q.action_for(s1).is_none());
        assert!(q.action_for(s2).is_some());
    }

    #[test]
    fn save_login_carries_no_secret() {
        let item = ToastItem::save_login("https://bank.test", "user@test", false);
        assert!(item.text.contains("bank.test"));
        assert!(item.text.contains("user@test"));
        assert!(item.text.contains("Save login"));
        let item = ToastItem::save_login("https://bank.test", "user@test", true);
        assert!(item.text.contains("Update saved login"));
        assert!(matches!(item.action, Some(ToastActionKind::SaveLogin)));
    }

    #[test]
    fn push_save_replaces_older_save_never_two() {
        let mut q = ToastQueue::new();
        let t = now();
        let s1 = q.push_save(ToastItem::save_login("https://a.test", "u1", false), t);
        let s2 = q.push_save(ToastItem::save_login("https://b.test", "u2", false), t);
        assert_ne!(s1, s2);
        assert!(q.current_view().expect("head").text.contains("b.test"));
        assert!(q.action_for(s1).is_none());
        assert!(matches!(q.action_for(s2), Some(ToastActionKind::SaveLogin)));
    }

    #[test]
    fn pending_save_take_empties_staging() {
        let mut s = PendingSave {
            seq: 1,
            origin: "https://a.test".to_owned(),
            username: "u".to_owned(),
            password: b"secret".to_vec(),
            expires: now() + Duration::from_secs(8),
        };
        assert!(s.is_live(1, now()));
        assert!(!s.is_live(2, now()));
        let taken = s.take_password();
        assert_eq!(taken, b"secret");
        assert!(s.password.is_empty());
    }
}
