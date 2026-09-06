//! Application state: theme, tabs, omnibox.

use std::rc::Rc;

use ferrite_omnibox::{AutocompleteCache, OmniboxState};
use ferrite_tabs::{MockFactory, TabManager, WebViewFactory};

use crate::theme::{AccentContext, ThemeConfig};

#[derive(Debug)]
pub struct AppContext {
    pub is_initialized: bool,
    pub window_dimensions: (u32, u32),
    pub theme: ThemeConfig,
    pub tabs: TabManager,
    pub omnibox: OmniboxState,
    pub autocomplete: AutocompleteCache,
    /// Cached tab-title text widths keyed by (tab id, font size, title).
    /// Avoids font-shaping every tab every frame; invalidated when the
    /// title changes and pruned when tabs close.
    pub tab_text_widths: std::collections::HashMap<(uuid::Uuid, u32, String), f32>,
}

impl Default for AppContext {
    #[inline]
    fn default() -> Self {
        Self::new(ThemeConfig::default())
    }
}

impl AppContext {
    pub fn new(theme: ThemeConfig) -> Self {
        let factory: Rc<dyn WebViewFactory> = Rc::new(MockFactory);
        Self::new_with_factory(theme, factory)
    }

    pub fn new_with_factory(theme: ThemeConfig, factory: Rc<dyn WebViewFactory>) -> Self {
        Self {
            is_initialized: true,
            window_dimensions: (1280, 800),
            theme,
            tabs: TabManager::new(factory),
            omnibox: OmniboxState::default(),
            autocomplete: AutocompleteCache::new(),
            tab_text_widths: std::collections::HashMap::new(),
        }
    }

    /// Sync accent context to the active tab. Returns true if it changed.
    pub fn sync_theme_from_tabs(&mut self) -> bool {
        let target = if self.tabs.active_is_archive_view() {
            AccentContext::Archive
        } else {
            AccentContext::Standard
        };
        if self.theme.accent_context != target {
            tracing::debug!(?target, "accent context synced from active tab");
            self.theme.accent_context = target;
            true
        } else {
            false
        }
    }

    pub fn update_window_dimensions(&mut self, logical_width: f64, logical_height: f64) {
        self.window_dimensions = (
            if logical_width.is_finite() && logical_width > 0.0 {
                logical_width.round() as u32
            } else {
                0
            },
            if logical_height.is_finite() && logical_height > 0.0 {
                logical_height.round() as u32
            } else {
                0
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dimensions_match_spec() {
        let ctx = AppContext::default();
        assert!(ctx.is_initialized);
        assert_eq!(ctx.window_dimensions, (1280, 800));
    }

    #[test]
    fn dimension_update_uses_logical_pixels() {
        let mut ctx = AppContext::default();
        ctx.update_window_dimensions(640.0, 480.0);
        assert_eq!(ctx.window_dimensions, (640, 480));
    }

    #[test]
    fn dimension_update_rounds_and_saturates() {
        let mut ctx = AppContext::default();
        ctx.update_window_dimensions(1280.6, 800.4);
        assert_eq!(ctx.window_dimensions, (1281, 800));
        ctx.update_window_dimensions(-10.0, f64::NAN);
        assert_eq!(ctx.window_dimensions, (0, 0));
    }

    #[test]
    fn dimension_update_rejects_infinity() {
        let mut ctx = AppContext::default();
        ctx.update_window_dimensions(f64::INFINITY, f64::NEG_INFINITY);
        assert_eq!(ctx.window_dimensions, (0, 0));
    }

    #[test]
    fn omnibox_default_is_empty() {
        let ctx = AppContext::default();
        assert!(ctx.omnibox.input.is_empty());
        assert_eq!(ctx.omnibox.registry.current_id(), "duckduckgo");
    }

    #[test]
    fn autocomplete_default_is_empty() {
        let ctx = AppContext::default();
        assert!(ctx.autocomplete.is_empty());
    }
}
