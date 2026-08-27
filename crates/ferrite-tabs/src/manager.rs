use std::rc::Rc;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use uuid::Uuid;
use winit::window::Window;

use crate::{
    error::{TabError, TabResult},
    tab::{TabLifecycle, TabState},
    webview::WebViewFactory,
};

#[derive(Debug)]
pub struct TabManager {
    tabs: Vec<TabState>,
    active_index: Option<usize>,
    max_tabs: usize,
    factory: Rc<dyn WebViewFactory>,
    idle_timeout: Duration,
}

impl TabManager {
    pub fn new(factory: Rc<dyn WebViewFactory>) -> Self {
        Self::with_max_tabs(factory, 50)
    }
    pub fn with_max_tabs(factory: Rc<dyn WebViewFactory>, max_tabs: usize) -> Self {
        Self {
            tabs: Vec::new(),
            active_index: None,
            max_tabs,
            factory,
            idle_timeout: Duration::from_secs(30 * 60),
        }
    }
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
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
        if self.tabs.len() >= self.max_tabs {
            return Err(TabError::TabLimitExceeded {
                current: self.tabs.len(),
                limit: self.max_tabs,
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
        info!(%id, active_index = self.active_index.unwrap_or(0), "new tab");
        Ok(id)
    }
    pub fn new_discarded_tab(
        &mut self,
        url: String,
        title: String,
        is_pinned: bool,
    ) -> TabResult<Uuid> {
        if self.tabs.len() >= self.max_tabs {
            return Err(TabError::TabLimitExceeded {
                current: self.tabs.len(),
                limit: self.max_tabs,
            });
        }
        let id = Uuid::new_v4();
        let mut tab = TabState::new_discarded(id, url, title);
        tab.is_pinned = is_pinned;
        self.tabs.push(tab);
        if self.active_index.is_none() {
            self.active_index = Some(0);
        }
        Ok(id)
    }
    pub fn new_tab_mock(&mut self, url: Option<String>) -> TabResult<Uuid> {
        use crate::webview::MockHandle;
        if self.tabs.len() >= self.max_tabs {
            return Err(TabError::TabLimitExceeded {
                current: self.tabs.len(),
                limit: self.max_tabs,
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
            warn!(%removed_id, "close blocked for pinned tab: use force close");
            return Err(TabError::TabNotFound { id: removed_id });
        }
        self.tabs.remove(index);
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
                        if tab.is_discarded() {
                        } else {
                            tab.lifecycle = TabLifecycle::Active;
                            tab.suspended_since = None;
                            let _ = tab.set_visible(true);
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
            {
                tab.lifecycle = TabLifecycle::Active;
                tab.suspended_since = None;
                let _ = tab.set_visible(true);
            }
        }
        info!(closed, "bulk close");
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
            return Ok(());
        }
        tab.discard();
        info!(tab_id = %tab.id, "tab discarded");
        Ok(())
    }
    pub fn restore_tab(
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
        let url = self
            .tabs
            .get(index)
            .map(|t| t.url.clone())
            .unwrap_or_default();
        let is_muted = self.tabs.get(index).map(|t| t.is_muted).unwrap_or(false);
        let id = self.tabs.get(index).map(|t| t.id).unwrap_or_else(Uuid::nil);
        let webview = self.factory.create_with_id(window, &url, bounds, id)?;
        let tab = self.tabs.get_mut(index).unwrap();
        tab.restore(webview);
        if is_muted {
            let _ = tab.webview.as_ref().unwrap().evaluate_script(
                "Array.from(document.querySelectorAll('audio,video')).forEach(m=>m.muted=true)",
            );
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
            if let Some(tab) = self.tabs.get_mut(index) {
                tab.lifecycle = TabLifecycle::Active;
                tab.suspended_since = None;
            }
        } else {
            if let Some(next) = self.tabs.get_mut(index) {
                next.lifecycle = TabLifecycle::Active;
                next.suspended_since = None;
                let _ = next.set_visible(true);
            }
        }
        self.active_index = Some(index);
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
        if index >= self.tabs.len() {
            return Err(TabError::IndexOutOfBounds {
                index,
                len: self.tabs.len(),
            });
        }
        let tab = self.tabs.get_mut(index).unwrap();
        let muted = tab.toggle_mute();
        if let Some(handle) = tab.webview.as_ref() {
            let js = if muted {
                "Array.from(document.querySelectorAll('audio,video')).forEach(m=>m.muted=true)"
            } else {
                "Array.from(document.querySelectorAll('audio,video')).forEach(m=>m.muted=false)"
            };
            let _ = handle.evaluate_script(js);
        }
        Ok(muted)
    }
    pub fn toggle_pin(&mut self, index: usize) -> TabResult<bool> {
        if index >= self.tabs.len() {
            return Err(TabError::IndexOutOfBounds {
                index,
                len: self.tabs.len(),
            });
        }
        let tab = self.tabs.get_mut(index).unwrap();
        let pinned = tab.toggle_pinned();
        Ok(pinned)
    }
    pub fn search(&self, query: &str) -> Vec<&TabState> {
        if query.trim().is_empty() {
            return self.tabs.iter().collect();
        }
        let needle = query.to_lowercase();
        let mut matches: Vec<(&TabState, i64)> = self
            .tabs
            .iter()
            .filter_map(|t| {
                let hay_title = t.title.to_lowercase();
                let hay_url = t.url.to_lowercase();
                if hay_title.contains(&needle) || hay_url.contains(&needle) {
                    let score = if hay_title.contains(&needle) { 2 } else { 1 };
                    Some((t, score))
                } else {
                    None
                }
            })
            .collect();
        matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.title.cmp(&b.0.title)));
        matches.into_iter().map(|(t, _)| t).collect()
    }
    pub fn update_active_bounds(&self, rect: wry::Rect) -> TabResult<()> {
        let tab = self.active_tab().ok_or(TabError::NoActiveTab)?;
        if tab.is_discarded() {
            return Ok(());
        }
        tab.set_bounds(rect)
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
    pub fn discard_idle_tabs(&mut self, now: Instant) {
        for idx in 0..self.tabs.len() {
            let should_discard = {
                let tab = match self.tabs.get(idx) {
                    Some(t) => t,
                    None => continue,
                };
                !tab.is_pinned
                    && !tab.is_playing_audio
                    && !tab.is_crashed
                    && tab.lifecycle == TabLifecycle::Suspended
                    && tab
                        .suspended_since
                        .is_some_and(|since| now.duration_since(since) >= self.idle_timeout)
            };
            if should_discard {
                let _ = self.discard_tab(idx);
            }
        }
    }
}
