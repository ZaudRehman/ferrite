//! Per-tab state.

use std::rc::Rc;
use std::time::Instant;

use uuid::Uuid;

use crate::{error::TabResult, navigation::NavigationHistory, webview::WebViewHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabLifecycle {
    Active,
    Suspended,
    Discarded,
}

#[derive(Debug)]
pub struct TabState {
    pub id: Uuid,
    pub url: String,
    pub title: String,
    pub favicon: Option<Vec<u8>>,
    pub is_loading: bool,
    pub is_archive_view: bool,
    pub is_playing_audio: bool,
    pub is_muted: bool,
    pub is_pinned: bool,
    pub is_crashed: bool,
    pub lifecycle: TabLifecycle,
    pub suspended_since: Option<Instant>,
    pub webview: Option<Rc<dyn WebViewHandle>>,
    pub history: NavigationHistory,
}

impl TabState {
    pub fn new(id: Uuid, url: String, webview: Rc<dyn WebViewHandle>) -> Self {
        let is_archive_view = url.starts_with("ferrite://archive/");
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
            is_loading: true,
            is_archive_view,
            is_playing_audio: false,
            is_muted: false,
            is_pinned: false,
            is_crashed: false,
            lifecycle: TabLifecycle::Active,
            suspended_since: None,
            webview: Some(webview),
            history: NavigationHistory::new(),
        }
    }

    pub fn new_discarded(id: Uuid, url: String, title: String) -> Self {
        let is_archive_view = url.starts_with("ferrite://archive/");
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
            is_loading: false,
            is_archive_view,
            is_playing_audio: false,
            is_muted: false,
            is_pinned: false,
            is_crashed: false,
            lifecycle: TabLifecycle::Discarded,
            suspended_since: None,
            webview: None,
            history: NavigationHistory::new(),
        }
    }

    pub fn navigate(&mut self, new_url: String) {
        let current = self.url.clone();
        self.history.push(&current, &new_url);
        self.url = new_url.clone();
        self.title = new_url.clone();
        self.is_archive_view = self.url.starts_with("ferrite://archive/");
        self.is_loading = true;
        self.is_crashed = false;
    }

    /// Mark the tab as finished loading.
    pub fn set_loaded(&mut self) {
        self.is_loading = false;
    }

    /// Mark the tab as crashed (§7).
    pub fn mark_crashed(&mut self) {
        self.is_crashed = true;
        self.is_loading = false;
    }

    /// Update the title from an IPC `title` message.
    pub fn set_title(&mut self, title: String) {
        if !title.trim().is_empty() {
            self.title = title;
        }
    }

    /// Update the favicon bytes.
    pub fn set_favicon(&mut self, bytes: Vec<u8>) {
        self.favicon = Some(bytes);
    }

    /// Update audio playing state from IPC (`play`/`pause`).
    pub fn set_audio_playing(&mut self, playing: bool) {
        self.is_playing_audio = playing;
    }

    /// Toggle the user-controlled mute state (§3.6).
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

    pub fn suspend(&mut self) {
        if self.lifecycle == TabLifecycle::Active {
            if let Some(handle) = self.webview.as_ref() {
                let _ = handle.set_visible(false);
            }
            self.lifecycle = TabLifecycle::Suspended;
            self.suspended_since = Some(Instant::now());
        }
    }

    pub fn discard(&mut self) {
        if self.lifecycle == TabLifecycle::Discarded {
            return;
        }
        self.webview = None;
        self.lifecycle = TabLifecycle::Discarded;
        self.suspended_since = None;
        self.is_loading = false;
    }

    pub fn restore(&mut self, webview: Rc<dyn WebViewHandle>) {
        self.webview = Some(webview);
        self.lifecycle = TabLifecycle::Active;
        self.suspended_since = None;
        self.is_loading = true;
        self.is_crashed = false;
        // is_muted is reapplied after navigation completes (caller does evaluate_script)
    }

    /// Whether back is possible.
    pub fn can_go_back(&self) -> bool {
        self.history.can_go_back()
    }

    /// Whether forward is possible.
    pub fn can_go_forward(&self) -> bool {
        self.history.can_go_forward()
    }

    /// Attempt to go back.
    pub fn go_back(&mut self) -> Option<String> {
        let target = self.history.go_back(&self.url)?;
        self.url.clone_from(&target);
        self.is_archive_view = self.url.starts_with("ferrite://archive/");
        self.is_loading = true;
        Some(target)
    }

    /// Attempt to go forward.
    pub fn go_forward(&mut self) -> Option<String> {
        let target = self.history.go_forward(&self.url)?;
        self.url.clone_from(&target);
        self.is_archive_view = self.url.starts_with("ferrite://archive/");
        self.is_loading = true;
        Some(target)
    }

    /// Delegate `set_visible` to the underlying webview (no-op if discarded).
    pub fn set_visible(&self, visible: bool) -> TabResult<()> {
        if let Some(handle) = self.webview.as_ref() {
            handle.set_visible(visible)?;
        }
        Ok(())
    }

    /// Delegate `set_bounds` to the underlying webview (no-op if discarded).
    pub fn set_bounds(&self, bounds: wry::Rect) -> TabResult<()> {
        if let Some(handle) = self.webview.as_ref() {
            handle.set_bounds(bounds)?;
        }
        Ok(())
    }

    /// Load `url` via the webview and update bookkeeping.
    pub fn load_url(&mut self, url: String) -> TabResult<()> {
        self.navigate(url.clone());
        if let Some(handle) = self.webview.as_ref() {
            handle.load_url(&url)?;
        }
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
    fn restore_reactivates() {
        let mut tab = mock_discarded("https://a.com");
        let handle = Rc::new(MockHandle::default()) as Rc<dyn WebViewHandle>;
        tab.restore(handle);
        assert_eq!(tab.lifecycle, TabLifecycle::Active);
        assert!(tab.webview.is_some());
        assert!(tab.is_loading);
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
