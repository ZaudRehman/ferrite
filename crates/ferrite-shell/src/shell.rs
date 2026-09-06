#![allow(
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::too_many_arguments
)]
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use egui::{ClippedPrimitive, Context, Rect, RichText, Ui};
use egui_winit::{EventResponse, State as EguiState};
use tracing::{debug, error, info, trace, warn};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, KeyEvent, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Fullscreen, Window, WindowAttributes, WindowId},
};

use ferrite_omnibox::{OmniboxAction, hint_generation_js};
use ferrite_tabs::{IpcBridge, WryFactory, spatial::egui_rect_to_wry_rect};

use crate::{
    ROOT_VIEWPORT,
    app_context::AppContext,
    error::{ShellError, ShellResult},
    renderer::{GpuRenderer, frame_metrics},
    theme::{
        ACCENT_DANGER, BORDER_ACTIVE, BORDER_SUBTLE, FOREGROUND, SURFACE_1, SURFACE_2, TEXT_MUTED,
        TEXT_SECONDARY, ThemeConfig,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepaintPolicy {
    Continuous,
    OnDemand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullscreenMode {
    Windowed,
    Borderless,
}
impl FullscreenMode {
    fn toggle(self) -> Self {
        match self {
            Self::Windowed => Self::Borderless,
            Self::Borderless => Self::Windowed,
        }
    }
    fn debug_label(self) -> &'static str {
        match self {
            Self::Windowed => "windowed",
            Self::Borderless => "borderless fullscreen",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShellConfig {
    pub title: String,
    pub initial_size: LogicalSize<f32>,
    pub min_size: Option<LogicalSize<f32>>,
    pub resizable: bool,
    pub theme: ThemeConfig,
    pub repaint_policy: RepaintPolicy,
    pub diagnostics_on: bool,
    /// Initial tab URL (`--url` / positional CLI arg). Falls back to session
    /// restore, then `ferrite://newtab`.
    pub initial_url: Option<String>,
}
impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            title: "Ferrite".to_string(),
            initial_size: LogicalSize::new(1280.0, 800.0),
            min_size: Some(LogicalSize::new(640.0, 480.0)),
            resizable: true,
            theme: ThemeConfig::default(),
            repaint_policy: RepaintPolicy::OnDemand,
            // Off by default: the panel shows the active URL + cursor, which
            // leaks PII in screenshots/screen-shares (F3 toggles).
            diagnostics_on: false,
            initial_url: None,
        }
    }
}
impl ShellConfig {
    fn window_attributes(&self) -> WindowAttributes {
        let mut attrs = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(self.initial_size)
            .with_active(true)
            .with_resizable(self.resizable)
            .with_visible(true);
        if let Some(min) = self.min_size {
            attrs = attrs.with_min_inner_size(min);
        }
        attrs
    }
}

pub(crate) struct ShellApp {
    config: ShellConfig,
    ctx: Context,
    proxy: Option<ferrite_proxy::ProxyHandle>,
    proxy_requests: u64,
    proxy_dropped: u64,
    privacy: std::sync::Arc<ferrite_privacy::PrivacyEngine>,
    tab_search_open: bool,
    tab_search_query: String,
    modifiers: winit::keyboard::ModifiersState,
    state: Option<EguiState>,
    renderer: Option<GpuRenderer>,
    window: Option<Arc<Window>>,
    app: AppContext,
    fullscreen: FullscreenMode,
    diagnostics: bool,
    ema_frame_secs: f32,
    last_frame: Instant,
    pending_error: Option<ShellError>,
    requested_repaint: bool,
    ipc_bridge: IpcBridge,
    content_rect: Cell<Option<Rect>>,
    /// Last UI signature for OnDemand dirty tracking (tab count, loading
    /// count, active id, search open, omnibox focus).
    last_ui_sig: (usize, usize, Option<uuid::Uuid>, bool, bool),
    /// Heartbeat for diagnostics/EMA/spinners under OnDemand (2Hz instead
    /// of 60Hz full repaint).
    last_heartbeat: Instant,
    downloads: ferrite_tabs::DownloadManager,
    /// Recent download events for the diagnostics panel (capped ring).
    download_log: std::collections::VecDeque<ferrite_tabs::DownloadEvent>,
}
impl Default for ShellApp {
    fn default() -> Self {
        Self::new(ShellConfig::default())
    }
}
impl ShellApp {
    fn new(config: ShellConfig) -> Self {
        let theme = config.theme.clone();
        let diagnostics = config.diagnostics_on;
        let ipc_bridge = IpcBridge::new();
        let downloads = ferrite_tabs::DownloadManager::new();

        let fixed_port = std::env::var("FERRITE_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok());
        let privacy_engine = {
            let engine = std::sync::Arc::new(
                ferrite_privacy::PrivacyEngine::new()
                    .unwrap_or_else(|_| ferrite_privacy::PrivacyEngine::default()),
            );
            engine.load_settings_from_file("ferrite-settings.json");
            let _ = engine.load_filters(
                &[("easylist_startup", "||ads.example^\n||tracker.test^")],
                None,
            );
            let _ = engine.load_threat_snapshot(&["phishing.test", "malware.test"]);
            let updater = std::sync::Arc::clone(&engine);
            updater.spawn_hourly_updater(
                || {
                    if let Ok(url) = std::env::var("FERRITE_FILTER_URL") {
                        tracing::debug!(%url, "filter fetch stub");
                        None
                    } else if let Ok(text) = std::fs::read_to_string("easylist.txt") {
                        Some((text, None))
                    } else {
                        None
                    }
                },
                || {
                    if let Ok(path) = std::env::var("FERRITE_THREAT_URL") {
                        tracing::debug!(%path, "threat fetch stub");
                        None
                    } else if let Ok(data) = std::fs::read_to_string("threats.json") {
                        serde_json::from_str::<Vec<String>>(&data).ok()
                    } else {
                        None
                    }
                },
            );
            engine
        };
        let proxy = match ferrite_proxy::spawn(
            ferrite_proxy::ProxyConfig {
                fixed_port,
                archive: None,
            },
            privacy_engine.clone() as std::sync::Arc<dyn ferrite_proxy::DecisionHook>,
        ) {
            Ok(handle) => {
                info!(
                    port = handle.port,
                    "Ferrite proxy listening: all webview traffic routes through it"
                );
                Some(handle)
            }
            Err(err) => {
                error!(error = %err, "proxy spawn failed: traffic will flow direct");
                None
            }
        };
        let proxy_port = proxy.as_ref().map(|p| p.port);

        let factory: Rc<dyn ferrite_tabs::WebViewFactory> = {
            let mut f = WryFactory::new(ipc_bridge.sender.clone(), ipc_bridge.limiter());
            f = f.with_downloads(downloads.clone());
            if let Some(port) = proxy_port {
                f = f.with_proxy("127.0.0.1", port);
            }
            f = f.with_asset(
                "geist-pixel-square.ttf",
                crate::fonts::GEIST_PIXEL_SQUARE_TTF,
            );
            Rc::new(f)
        };
        let mut app = AppContext::new_with_factory(theme, factory);
        // Restore the persisted search engine (JSON-merge alongside the
        // privacy settings; unknown ids fall back to the default).
        if let Ok(data) = std::fs::read_to_string("ferrite-settings.json") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(id) = v.get("omnibox_engine").and_then(|x| x.as_str()) {
                    let _ = app.omnibox.registry.set_current(id);
                }
            }
        }
        Self {
            config,
            ctx: Context::default(),
            proxy,
            proxy_requests: 0,
            proxy_dropped: 0,
            privacy: privacy_engine,
            tab_search_open: false,
            tab_search_query: String::new(),
            modifiers: winit::keyboard::ModifiersState::default(),
            state: None,
            renderer: None,
            window: None,
            app,
            fullscreen: FullscreenMode::Windowed,
            diagnostics,
            ema_frame_secs: 0.0,
            last_frame: Instant::now(),
            pending_error: None,
            requested_repaint: false,
            ipc_bridge,
            content_rect: Cell::new(None),
            last_ui_sig: (0, 0, None, false, false),
            last_heartbeat: Instant::now(),
            downloads,
            download_log: std::collections::VecDeque::new(),
        }
    }
    fn bootstrap(&mut self, elwt: &ActiveEventLoop) {
        let config = self.config.clone();
        let window = match elwt.create_window(config.window_attributes()) {
            Ok(w) => Arc::new(w),
            Err(s) => {
                self.pending_error = Some(ShellError::WindowCreation { source: s });
                elwt.exit();
                return;
            }
        };
        window.set_cursor_visible(true);
        let renderer = match GpuRenderer::new(&window, &self.ctx) {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "GPU init failed");
                self.pending_error = Some(e);
                elwt.exit();
                return;
            }
        };
        let state = EguiState::new(
            self.ctx.clone(),
            ROOT_VIEWPORT,
            &*window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        crate::fonts::install(&self.ctx);
        config.theme.apply(&self.ctx);
        self.window = Some(window.clone());
        self.state = Some(state);
        self.renderer = Some(renderer);
        self.app.is_initialized = true;
        let scale = window.scale_factor();
        let size = window.inner_size();
        self.app
            .update_window_dimensions(size.width as f64 / scale, size.height as f64 / scale);
        if self.app.tabs.is_empty() {
            // Crash recovery first: tabs restore as Discarded, then the
            // active one rebuilds its webview below.
            let restored = self
                .app
                .tabs
                .load_from_file(std::path::Path::new("ferrite-session.json"));
            if restored == 0 {
                // CLI --url wins over a fresh newtab (explicit user intent).
                let first = self
                    .config
                    .initial_url
                    .clone()
                    .filter(|u| !u.trim().is_empty());
                open_tab(
                    &mut self.app.tabs,
                    &window,
                    first.unwrap_or_else(|| "ferrite://newtab".to_owned()),
                );
            } else if let Some(idx) = self.app.tabs.active_index() {
                if let Err(e) = self.app.tabs.switch_tab_with_window(&window, idx, None) {
                    debug!(error = %e, "session restore switch failed");
                }
            }
        }
        info!(%scale, logical_width = size.width as f64 / scale, logical_height = size.height as f64 / scale, "shell bootstrapped");
        window.request_redraw();
    }
    fn draw_frame(&mut self, elwt: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let scale = window.scale_factor();
        let size = window.inner_size();
        self.app
            .update_window_dimensions(size.width as f64 / scale, size.height as f64 / scale);
        // Drain proxy telemetry for diagnostics.
        let mut telemetry_count = 0u64;
        if let Some(proxy) = self.proxy.as_ref() {
            while let Ok(ev) = proxy.telemetry.try_recv() {
                telemetry_count += 1;
                self.proxy_requests += 1;
                if ev.classification != ferrite_proxy::Classification::Allowed {
                    trace!(
                        host = %ev.request_domain,
                        ?ev.classification,
                        bytes = ev.bytes_transferred,
                        "proxy request classified"
                    );
                }
            }
            let dropped = proxy
                .telemetry_dropped
                .load(std::sync::atomic::Ordering::Relaxed);
            if dropped != self.proxy_dropped && dropped > 0 && dropped % 100 == 0 {
                warn!(dropped, "proxy telemetry dropped (backpressure)");
            }
            self.proxy_dropped = dropped;
        }

        // Drain download events into a capped ring for diagnostics.
        let dl_events = self.downloads.drain(32);
        let dl_count = dl_events.len();
        for ev in dl_events {
            if matches!(
                ev.state,
                ferrite_tabs::DownloadStateKind::Completed
                    | ferrite_tabs::DownloadStateKind::Failed
            ) {
                info!(url = %ev.url, filename = %ev.filename, state = ?ev.state, "download finished");
            }
            self.download_log.push_back(ev);
            while self.download_log.len() > 200 {
                self.download_log.pop_front();
            }
        }

        let mut theme_changed = false;
        let ipc_msgs = self.ipc_bridge.drain();
        let ipc_count = ipc_msgs.len();
        for msg in ipc_msgs {
            // Defense in depth: the bridge caps at 64KB, but re-check here
            // (older senders, tests, future callers).
            if msg.payload.len() > ferrite_tabs::IPC_MAX_PAYLOAD_BYTES {
                warn!(
                    tab_id = %msg.tab_id,
                    bytes = msg.payload.len(),
                    "ipc oversize at drain: dropping"
                );
                continue;
            }
            debug!(tab_id = %msg.tab_id, payload_len = msg.payload.len(), "ipc");
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg.payload) else {
                continue;
            };
            let msg_type = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match msg_type {
                "audio" => {
                    let playing = v.get("playing").and_then(|x| x.as_bool()).unwrap_or(false);
                    if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                        tab.set_audio_playing(playing);
                    }
                }
                "title" | "" => {
                    // Titles are accepted ONLY on title-typed (or legacy
                    // untyped) messages. Any other typed message carrying a
                    // `title` field must not clobber the tab title
                    // (title-spoof from a compromised page).
                    if let Some(t) = v.get("title").and_then(|x| x.as_str()) {
                        let title: String =
                            t.chars().take(ferrite_tabs::IPC_MAX_TITLE_CHARS).collect();
                        if !title.is_empty() {
                            if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                                tab.set_title(title);
                            }
                        }
                    }
                }
                "settings" => {
                    let key = v.get("key").and_then(|x| x.as_str()).unwrap_or("");
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                    match key {
                        "adblock" => self.privacy.update_settings(|s| s.adblock_enabled = value),
                        "gpc" => self.privacy.update_settings(|s| s.gpc_enabled = value),
                        "realtime" => self
                            .privacy
                            .update_settings(|s| s.realtime_lookup_enabled = value),
                        "https_up" => self
                            .privacy
                            .update_settings(|s| s.https_upgrade_enabled = value),
                        _ => {}
                    };
                    self.privacy.save_settings_to_file("ferrite-settings.json");
                    info!(key, value, "privacy setting toggled");
                }
                "settings_query" => {
                    let s = self.privacy.settings();
                    if let Ok(json) = serde_json::to_string(&s) {
                        let js = format!(
                            "window.__ferriteSetSettings && window.__ferriteSetSettings({json})"
                        );
                        if let Some(tab) = self.app.tabs.get_tab(msg.tab_id) {
                            if let Some(h) = tab.webview.as_ref() {
                                let _ = h.evaluate_script(&js);
                            }
                        }
                    }
                }
                // First-party page messages with no Rust consumer yet.
                // Explicitly ignored (logged at debug): they must NOT fall
                // through to title handling. Wiring is P1+ work.
                "navigate" | "search" | "recovery" | "import" | "console" | "elements"
                | "storage" | "autofill_scan" => {
                    debug!(msg_type, tab_id = %msg.tab_id, "ipc: known type without consumer - ignored");
                }
                unknown => {
                    debug!(msg_type = unknown, tab_id = %msg.tab_id, "ipc: unknown type - ignored");
                }
            }
        }
        self.app.tabs.discard_idle_tabs(Instant::now());
        theme_changed |= self.app.sync_theme_from_tabs();
        if theme_changed {
            self.app.theme.apply(&self.ctx);
        }
        // OnDemand dirty tracking: repaint on IPC, telemetry, theme, or any
        // structural UI change (tabs/loading/active/search/focus). Steady
        // state (idle browser) issues no frames; diagnostics tick at 2Hz via
        // the about_to_wait heartbeat.
        if ipc_count > 0 || telemetry_count > 0 || theme_changed || dl_count > 0 {
            self.requested_repaint = true;
        }
        {
            let loading = self.app.tabs.iter().filter(|t| t.is_loading).count();
            let sig = (
                self.app.tabs.len(),
                loading,
                self.app.tabs.active_id(),
                self.tab_search_open,
                self.app.omnibox.focused,
            );
            if sig != self.last_ui_sig {
                self.last_ui_sig = sig;
                self.requested_repaint = true;
            }
        }
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.ema_frame_secs = frame_metrics::ema(self.ema_frame_secs, dt, 0.1);
        self.last_frame = now;
        let raw_input = state.take_egui_input(&window);
        let diagnostics = self.diagnostics;
        let frame_secs = self.ema_frame_secs;
        let content_rect_cell = &self.content_rect;
        let (tab_search_open, tab_search_query) = {
            let open = &mut self.tab_search_open as *mut bool;
            let query = &mut self.tab_search_query as *mut String;
            (open, query)
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let app = &mut self.app;
            let window_clone = window.clone();
            let dl_count = self.download_log.len();
            let dl_last = self
                .download_log
                .back()
                .map(|e| format!("{} {:?}", e.filename, e.state));
            unsafe {
                self.ctx.run_ui(raw_input, |ui| {
                    build_shell_ui(
                        ui,
                        app,
                        &window_clone,
                        scale,
                        PhysicalSize::new(
                            size.width as f32 / scale as f32,
                            size.height as f32 / scale as f32,
                        ),
                        frame_secs,
                        diagnostics,
                        self.proxy_requests,
                        self.proxy_dropped,
                        dl_count,
                        dl_last.clone(),
                        content_rect_cell,
                        &mut *tab_search_open,
                        &mut *tab_search_query,
                    )
                })
            }
        }));
        let Ok(mut output) = outcome else {
            warn!("panic in ui");
            window.request_redraw();
            return;
        };
        state.handle_platform_output_with_event_loop(&window, elwt, output.platform_output);
        // Honor egui's own repaint requests (spinners, cursors, hover) so
        // OnDemand never freezes animated chrome.
        if self.ctx.has_requested_repaint() {
            self.requested_repaint = true;
        }
        // Move shapes out instead of cloning: tessellation borrows the taken
        // vec, saving a full clone of every frame's primitives per frame.
        let primitives: Vec<ClippedPrimitive> = self
            .ctx
            .tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        renderer.paint_frame(
            &window,
            output.pixels_per_point,
            &primitives,
            &mut output.textures_delta,
        );
        if let Some(rect) = self.content_rect.get() {
            if let Some(wry_rect) = egui_rect_to_wry_rect(rect, scale) {
                let _ = self.app.tabs.update_active_bounds(wry_rect);
            }
        }
    }
    fn on_local_key(&mut self, window: &Window, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed || event.repeat {
            return false;
        }
        if self.modifiers.control_key() && self.modifiers.shift_key() {
            if let Key::Character(ref s) = event.logical_key {
                if s.eq_ignore_ascii_case("a") {
                    self.tab_search_open = !self.tab_search_open;
                    if self.tab_search_open {
                        self.tab_search_query.clear();
                    }
                    window.request_redraw();
                    return true;
                }
            }
        }
        if self.tab_search_open && event.logical_key == Key::Named(NamedKey::Escape) {
            self.tab_search_open = false;
            window.request_redraw();
            return true;
        }
        match event.logical_key {
            Key::Named(NamedKey::F11) => {
                self.toggle_fullscreen(window);
                true
            }
            Key::Named(NamedKey::F3) => {
                self.diagnostics = !self.diagnostics;
                window.request_redraw();
                true
            }
            Key::Named(NamedKey::F4) => {
                let ctx = self.app.theme.toggle_context();
                self.app.theme.apply(&self.ctx);
                info!(context = ctx.debug_label(), "accent context toggled");
                window.request_redraw();
                true
            }
            Key::Character(ref s) if (s == "f" || s == "F") && !self.app.omnibox.focused => {
                if let Some(tab) = self.app.tabs.active_tab() {
                    if !tab.is_loading && !tab.is_crashed {
                        let theme = &self.app.theme;
                        let hint_theme = ferrite_omnibox::HintTheme {
                            accent_hex: crate::theme::ThemeConfig::to_hex(
                                theme.resolved_interactive_accent(),
                            ),
                            text_hex: crate::theme::ThemeConfig::to_hex(theme.palette.substrate),
                            ..ferrite_omnibox::HintTheme::iris()
                        };
                        let js = hint_generation_js("asdfghjkl", &hint_theme);
                        if let Some(handle) = tab.webview.as_ref() {
                            let _ = handle.evaluate_script(&js);
                        }
                    }
                }
                true
            }
            // Ctrl-gated shortcuts: bare l/t/w must never fire while typing
            // (the old code opened/closed tabs mid-omnibox-typing).
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("l")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                self.app.omnibox.request_focus();
                window.request_redraw();
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("t")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                open_tab(&mut self.app.tabs, window, "ferrite://newtab".to_owned());
                window.request_redraw();
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("w")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                if let Some(id) = self.app.tabs.active_id() {
                    // Pinned tabs refuse normal close (PinnedTab error);
                    // surface it in the log instead of failing silently.
                    if let Err(e) = self.app.tabs.close_tab(id) {
                        debug!(error = %e, "close tab ignored");
                    }
                }
                window.request_redraw();
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("r")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                if let Some(tab) = self.app.tabs.active_tab_mut() {
                    if let Err(e) = tab.reload() {
                        debug!(error = %e, "ctrl-r reload failed");
                    }
                }
                window.request_redraw();
                true
            }
            Key::Named(NamedKey::ArrowLeft) if self.modifiers.alt_key() => {
                if let Some(tab) = self.app.tabs.active_tab_mut() {
                    if let Some(url) = tab.go_back() {
                        if let Err(e) = tab.load_traversal(url) {
                            debug!(error = %e, "alt-back traversal failed");
                        }
                    }
                }
                window.request_redraw();
                true
            }
            Key::Named(NamedKey::ArrowRight) if self.modifiers.alt_key() => {
                if let Some(tab) = self.app.tabs.active_tab_mut() {
                    if let Some(url) = tab.go_forward() {
                        if let Err(e) = tab.load_traversal(url) {
                            debug!(error = %e, "alt-forward traversal failed");
                        }
                    }
                }
                window.request_redraw();
                true
            }
            Key::Named(NamedKey::Escape) => {
                // Stop a loading tab (matches the Stop (Esc) tooltip).
                if self
                    .app
                    .tabs
                    .active_tab()
                    .map(|t| t.is_loading)
                    .unwrap_or(false)
                {
                    if let Some(tab) = self.app.tabs.active_tab_mut() {
                        if let Err(e) = tab.stop() {
                            debug!(error = %e, "esc stop failed");
                        }
                    }
                    window.request_redraw();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
    fn toggle_fullscreen(&mut self, window: &Window) {
        self.fullscreen = self.fullscreen.toggle();
        let mode = match self.fullscreen {
            FullscreenMode::Borderless => window
                .current_monitor()
                .map(|m| Fullscreen::Borderless(Some(m))),
            FullscreenMode::Windowed => None,
        };
        window.set_fullscreen(mode);
        trace!("fullscreen -> {}", self.fullscreen.debug_label());
    }
    fn remember_repaint(&mut self, response: &EventResponse) {
        if response.repaint {
            self.requested_repaint = true;
        }
    }
}
impl ApplicationHandler<()> for ShellApp {
    fn new_events(&mut self, elwt: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::Init) {
            elwt.set_control_flow(ControlFlow::Wait);
        }
        if let Some(err) = self.pending_error.take() {
            error!(error = %err, "fatal");
            elwt.exit();
        }
    }
    fn resumed(&mut self, elwt: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let _span = tracing::debug_span!("shell.boot").entered();
        debug!("resumed");
        self.bootstrap(elwt);
    }
    fn window_event(&mut self, elwt: &ActiveEventLoop, _window_id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if let Some(state) = self.state.as_mut() {
            let response: EventResponse = state.on_window_event(&window, &event);
            if response.consumed {
                return;
            }
            self.remember_repaint(&response);
        }
        match event {
            WindowEvent::CloseRequested => {
                info!("close requested: stopping proxy and shutting down");
                // Best-effort session persist for crash recovery (atomic
                // tmp+rename inside save_to_file; failure only logs).
                if let Err(e) = self
                    .app
                    .tabs
                    .save_to_file(std::path::Path::new("ferrite-session.json"))
                {
                    warn!(error = %e, "session persist failed");
                }
                if let Some(proxy) = self.proxy.take() {
                    proxy.stop();
                }
                elwt.exit();
            }
            WindowEvent::Resized(size) => {
                if size.width == 0 || size.height == 0 {
                    trace!("minimised");
                    return;
                }
                let pp = window.scale_factor() as f32;
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.on_window_resized(size, pp);
                }
                self.app.update_window_dimensions(
                    size.width as f64 / pp as f64,
                    size.height as f64 / pp as f64,
                );
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let physical = window.inner_size();
                let pp = scale_factor as f32;
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.on_window_resized(physical, pp);
                }
                self.app.update_window_dimensions(
                    physical.width as f64 / scale_factor,
                    physical.height as f64 / scale_factor,
                );
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                self.draw_frame(elwt);
            }
            WindowEvent::KeyboardInput { event, .. } => if self.on_local_key(&window, &event) {},
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            _ => (),
        }
    }
    fn about_to_wait(&mut self, elwt: &ActiveEventLoop) {
        if self.window.is_none() {
            return;
        }
        if let Some(err) = self.pending_error.take() {
            error!(error = %err, "frame error");
            elwt.exit();
            return;
        }
        match &self.config.repaint_policy {
            RepaintPolicy::Continuous => {
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            RepaintPolicy::OnDemand => {
                if self.requested_repaint {
                    self.requested_repaint = false;
                    self.last_heartbeat = Instant::now();
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                } else if self.last_heartbeat.elapsed() > std::time::Duration::from_millis(500) {
                    // 2Hz heartbeat: diagnostics EMA, loading spinners and
                    // dismiss-timeout UI stay alive without 60Hz repaint.
                    self.last_heartbeat = Instant::now();
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                }
            }
        }
    }
    fn suspended(&mut self, _elwt: &ActiveEventLoop) {
        debug!("suspended");
    }
}
/// Cached tab-title text width: skips font-shaping for unchanged titles
/// (the old code ran `layout_no_wrap` under the font lock for every tab
/// every frame). Returns `(width_px, fresh_galley)` where the galley is
/// `Some` only on a cache miss so painters can reuse it without re-shaping.
fn cached_text_width(
    widths: &mut std::collections::HashMap<(uuid::Uuid, u32, String), f32>,
    ui: &Ui,
    id: uuid::Uuid,
    label: &str,
    font: egui::FontId,
    color: egui::Color32,
) -> (f32, Option<std::sync::Arc<egui::Galley>>) {
    let key = (id, font.size.to_bits(), label.to_owned());
    if let Some(w) = widths.get(&key) {
        return (*w, None);
    }
    let galley = ui
        .ctx()
        .fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font, color));
    let w = galley.size().x;
    widths.insert(key, w);
    (w, Some(galley))
}

fn build_shell_ui(
    ui: &mut Ui,
    app: &mut AppContext,
    window: &Window,
    scale: f64,
    logical: PhysicalSize<f32>,
    frame_secs: f32,
    diagnostics: bool,
    proxy_requests: u64,
    proxy_dropped: u64,
    downloads_seen: usize,
    last_download: Option<String>,
    content_rect_cell: &Cell<Option<Rect>>,
    tab_search_open: &mut bool,
    tab_search_query: &mut String,
) {
    egui::Panel::top("tab_bar")
        .frame(
            egui::Frame::default()
                .fill(SURFACE_1)
                .inner_margin(egui::Margin::symmetric(4, 2))
                .stroke(egui::Stroke::new(0.5, crate::theme::BORDER_SUBTLE)),
        )
        .show(ui, |ui| {
            let mut close_id: Option<uuid::Uuid> = None;
            let mut pin_idx: Option<usize> = None;
            let mut switch_idx: Option<usize> = None;

            let pinned_count = app.tabs.iter().filter(|t| t.is_pinned).count();
            let tab_count = app.tabs.len();

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(1.0, 0.0);

                // ── Pinned tabs: icon-only, fixed 32px, left-anchored ──
                for (idx, tab) in app.tabs.iter().enumerate() {
                    if !tab.is_pinned {
                        continue;
                    }
                    let is_active = app.tabs.active_index() == Some(idx);
                    let first_char = tab.title.chars().next().unwrap_or('·');
                    let label = format!("{}", first_char);
                    let (bg, fg, stroke_color) = if is_active {
                        (
                            SURFACE_2,
                            app.theme.resolved_interactive_accent(),
                            app.theme.resolved_interactive_accent().gamma_multiply(0.5),
                        )
                    } else {
                        (
                            egui::Color32::TRANSPARENT,
                            TEXT_SECONDARY,
                            egui::Color32::TRANSPARENT,
                        )
                    };
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(32.0, 28.0), egui::Sense::click());
                    // Fixed-size chips: gate only the paint calls.
                    let pinned_visible = ui.is_rect_visible(rect);
                    if pinned_visible {
                        ui.painter().rect_filled(rect, crate::theme::CORNER_SM, bg);
                    }
                    if pinned_visible && stroke_color != egui::Color32::TRANSPARENT {
                        ui.painter().rect_stroke(
                            rect,
                            crate::theme::CORNER_SM,
                            egui::Stroke::new(1.0, stroke_color),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let text_rect =
                        egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0));
                    if pinned_visible {
                        ui.painter().text(
                            text_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            &label,
                            egui::FontId::new(13.0, crate::fonts::geist_pixel_family()),
                            fg,
                        );
                    }
                    if pinned_visible && is_active {
                        let accent = app.theme.resolved_interactive_accent();
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(rect.left(), rect.bottom() - 2.0),
                                egui::vec2(rect.width(), 2.0),
                            ),
                            0.0,
                            accent,
                        );
                    }
                    if is_active {
                        ui.scroll_to_rect(rect, None);
                    }
                    if resp.clicked() {
                        switch_idx = Some(idx);
                    }
                    // Right-click to unpin
                    if resp.secondary_clicked() {
                        pin_idx = Some(idx);
                    }
                    // Hover tooltip
                    resp.on_hover_text(format!("{} — pinned, right-click to unpin", tab.title));
                }

                // Separator between pinned and regular tabs
                if pinned_count > 0 && pinned_count < tab_count {
                    let (sep_rect, _) =
                        ui.allocate_exact_size(egui::vec2(1.0, 20.0), egui::Sense::hover());
                    ui.painter().rect_filled(sep_rect, 0.0, BORDER_SUBTLE);
                    ui.spacing_mut().item_spacing.x = 4.0;
                } else {
                    ui.spacing_mut().item_spacing.x = 2.0;
                }

                // ── Regular tabs: scrollable ──
                let regular_count = tab_count - pinned_count;
                let available = if ui.available_width().is_finite() {
                    ui.available_width() - 40.0
                } else {
                    800.0
                };
                let spacing = 2.0;
                let full_w: f32 = 220.0;
                let compact_w: f32 = 110.0;
                let min_w: f32 = 36.0;
                let ideal_w = if regular_count == 0 {
                    full_w
                } else {
                    let total_ideal = regular_count as f32 * full_w
                        + (regular_count.saturating_sub(1) as f32 * spacing);
                    if total_ideal <= available {
                        full_w
                    } else {
                        let total_compact = regular_count as f32 * compact_w
                            + (regular_count.saturating_sub(1) as f32 * spacing);
                        if total_compact <= available {
                            (available - (regular_count.saturating_sub(1) as f32 * spacing))
                                / regular_count as f32
                        } else {
                            let total_min = regular_count as f32 * min_w
                                + (regular_count.saturating_sub(1) as f32 * spacing);
                            if total_min <= available {
                                (available - (regular_count.saturating_sub(1) as f32 * spacing))
                                    / regular_count as f32
                            } else {
                                min_w
                            }
                        }
                    }
                };
                let show_close_only_hover = ideal_w < 100.0;
                let show_title = ideal_w >= 50.0;

                let needs_scroll = (regular_count as f32 * min_w
                    + (regular_count.saturating_sub(1) as f32 * spacing))
                    > available;
                let mut scroll = egui::ScrollArea::horizontal()
                    .id_salt("tab_bar_scroller")
                    .auto_shrink([false, false]);
                if needs_scroll {
                    scroll = scroll.scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    );
                }
                scroll.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(spacing, 0.0);

                        for (idx, tab) in app.tabs.iter().enumerate() {
                            if tab.is_pinned {
                                continue;
                            }
                            let is_active = app.tabs.active_index() == Some(idx);

                            // ── Title label ──
                            let label = if !show_title {
                                String::new()
                            } else if tab.title.len() > 28 && ideal_w >= 200.0 {
                                format!("{}…", tab.title.chars().take(24).collect::<String>())
                            } else if tab.title.len() > 14 && ideal_w < 200.0 {
                                format!("{}…", tab.title.chars().take(12).collect::<String>())
                            } else {
                                tab.title.clone()
                            };
                            let text_color = if is_active {
                                FOREGROUND
                            } else if tab.is_discarded() {
                                TEXT_MUTED
                            } else {
                                TEXT_SECONDARY
                            };
                            // Virtualized shaping: unchanged titles reuse the
                            // cached width without touching the font lock.
                            let font_id =
                                egui::FontId::new(12.0, crate::fonts::geist_pixel_family());
                            let (text_w, fresh_galley) = cached_text_width(
                                &mut app.tab_text_widths,
                                ui,
                                tab.id,
                                &label,
                                font_id.clone(),
                                text_color,
                            );
                            let close_area_w: f32 =
                                if tab.is_pinned || (show_close_only_hover && !is_active) {
                                    0.0
                                } else {
                                    20.0
                                };
                            let inner_pad: f32 = if show_title { 10.0 } else { 0.0 };
                            let chip_w = if show_title {
                                (text_w + inner_pad + close_area_w + 8.0).clamp(min_w, ideal_w)
                            } else {
                                32.0
                            };
                            let chip_h: f32 = 28.0;

                            // ── Chip background ──
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(chip_w, chip_h),
                                egui::Sense::click(),
                            );
                            // Virtualized paint: layout stays stable via the
                            // allocated rect, but shaping/paint backends are
                            // skipped for fully clipped chips (200+ tabs).
                            let chip_visible = ui.is_rect_visible(rect);
                            let hovered = resp.hovered();
                            let bg = if is_active {
                                SURFACE_2
                            } else if hovered {
                                SURFACE_2.gamma_multiply(0.5)
                            } else {
                                egui::Color32::TRANSPARENT
                            };
                            if chip_visible {
                                ui.painter().rect_filled(rect, crate::theme::CORNER_SM, bg);
                            }
                            if chip_visible && (is_active || hovered) {
                                let stroke_col = if is_active {
                                    BORDER_ACTIVE
                                } else {
                                    BORDER_SUBTLE
                                };
                                ui.painter().rect_stroke(
                                    rect,
                                    crate::theme::CORNER_SM,
                                    egui::Stroke::new(0.5, stroke_col),
                                    egui::StrokeKind::Inside,
                                );
                            }

                            // ── Text ──
                            if chip_visible && show_title {
                                let text_area = egui::Rect::from_min_size(
                                    rect.min + egui::vec2(4.0, 0.0),
                                    egui::vec2(chip_w - close_area_w - 8.0, chip_h),
                                );
                                let text_galley = match fresh_galley {
                                    Some(g) => g,
                                    None => ui.ctx().fonts_mut(|f| {
                                        f.layout_no_wrap(
                                            label.clone(),
                                            egui::FontId::new(
                                                12.0,
                                                crate::fonts::geist_pixel_family(),
                                            ),
                                            text_color,
                                        )
                                    }),
                                };
                                ui.painter().galley(
                                    text_area.left_center() + egui::vec2(0.0, 0.0),
                                    text_galley,
                                    text_color,
                                );
                            }

                            // ── Close button (inside chip, right-aligned) ──
                            let show_close =
                                !tab.is_pinned && (!show_close_only_hover || is_active || hovered);
                            if chip_visible && show_close {
                                let close_size = 18.0;
                                let close_rect = egui::Rect::from_min_size(
                                    egui::pos2(
                                        rect.right() - close_area_w + 1.0,
                                        rect.center().y - close_size / 2.0,
                                    ),
                                    egui::vec2(close_size, close_size),
                                );
                                let pointer = ui.ctx().input(|i| i.pointer.clone());
                                let close_hovered = pointer
                                    .hover_pos()
                                    .map(|p| close_rect.contains(p))
                                    .unwrap_or(false);
                                let close_bg = if close_hovered {
                                    ACCENT_DANGER.gamma_multiply(0.18)
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                let close_fg = if close_hovered {
                                    ACCENT_DANGER
                                } else {
                                    TEXT_MUTED
                                };
                                ui.painter().rect_filled(
                                    close_rect,
                                    crate::theme::CORNER_SM,
                                    close_bg,
                                );
                                crate::icons::paint_centered(
                                    ui.painter(),
                                    close_rect,
                                    crate::icons::Icon::X,
                                    close_fg,
                                );
                                if close_hovered && pointer.any_click() {
                                    close_id = Some(tab.id);
                                }
                            }

                            // ── Active underline accent ──
                            if chip_visible && is_active {
                                let accent = app.theme.resolved_interactive_accent();
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_size(
                                        egui::pos2(rect.left() + 2.0, rect.bottom() - 1.5),
                                        egui::vec2(rect.width() - 4.0, 1.5),
                                    ),
                                    0.0,
                                    accent,
                                );
                                ui.scroll_to_rect(rect, None);
                            }

                            if resp.clicked() {
                                switch_idx = Some(idx);
                            }

                            // ── Audio / muted indicator ──
                            if chip_visible && (tab.is_playing_audio || tab.is_muted) {
                                let icon = if tab.is_muted {
                                    crate::icons::Icon::AudioMuted
                                } else {
                                    crate::icons::Icon::Audio
                                };
                                let fg = if tab.is_muted {
                                    TEXT_MUTED
                                } else {
                                    app.theme.resolved_interactive_accent()
                                };
                                let (dot_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(4.0, 4.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(dot_rect.center(), 2.0, fg);
                                let _ = icon;
                            }

                            // ── Crashed indicator ──
                            if chip_visible && tab.is_crashed {
                                let (dot_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(4.0, 4.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter()
                                    .circle_filled(dot_rect.center(), 2.0, ACCENT_DANGER);
                            }
                        }
                    });
                });

                // ── Width-cache hygiene: drop entries for closed tabs ──
                if app.tab_text_widths.len() > app.tabs.len() * 2 {
                    let live: std::collections::HashSet<uuid::Uuid> =
                        app.tabs.iter().map(|t| t.id).collect();
                    app.tab_text_widths.retain(|k, _| live.contains(&k.0));
                }

                // ── New tab button (right, outside scroll) ──
                ui.add_space(4.0);
                let new_resp = crate::icons::button(
                    ui,
                    egui::vec2(28.0, 28.0),
                    crate::icons::Icon::Plus,
                    TEXT_MUTED,
                    SURFACE_2,
                )
                .on_hover_text("New tab Ctrl+T");
                if new_resp.clicked() {
                    open_tab(&mut app.tabs, window, "ferrite://newtab".to_owned());
                    app.omnibox.clear();
                    app.omnibox.request_focus();
                }

                // ── Tab count badge (far right) ──
                ui.add_space(2.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{}", tab_count))
                            .font(ThemeConfig::technical_label_font(10.0))
                            .color(TEXT_MUTED),
                    );
                });
            });

            // ── Process deferred actions ──
            if let Some(idx) = switch_idx {
                let bounds = content_rect_cell
                    .get()
                    .and_then(|r| egui_rect_to_wry_rect(r, scale));
                let _ = app.tabs.switch_tab_with_window(window, idx, bounds);
                app.sync_theme_from_tabs();
                app.theme.apply(ui.ctx());
                app.omnibox.clear();
            }
            if let Some(idx) = pin_idx {
                let _ = app.tabs.toggle_pin(idx);
            }
            if let Some(id) = close_id {
                let _ = app.tabs.close_tab(id);
            }
        });
    egui::Panel::top("omnibox_bar")
        .frame(
            egui::Frame::default()
                .fill(SURFACE_1)
                .inner_margin(egui::Margin::symmetric(6, 5))
                .stroke(egui::Stroke::new(0.5, crate::theme::BORDER_SUBTLE)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
                let can_back = app
                    .tabs
                    .active_tab()
                    .map(|t| t.can_go_back())
                    .unwrap_or(false);
                let can_forward = app
                    .tabs
                    .active_tab()
                    .map(|t| t.can_go_forward())
                    .unwrap_or(false);
                let is_loading = app.tabs.active_tab().map(|t| t.is_loading).unwrap_or(false);
                ui.add_enabled_ui(can_back, |ui| {
                    let r = crate::icons::button(
                        ui,
                        egui::vec2(32.0, 28.0),
                        crate::icons::Icon::ArrowLeft,
                        app.theme.palette.foreground,
                        app.theme.palette.surface_2,
                    )
                    .on_hover_text("Back (Alt+←)");
                    if r.clicked() {
                        if let Some(tab) = app.tabs.active_tab_mut() {
                            if let Some(url) = tab.go_back() {
                                let u = url.clone();
                                if let Err(e) = tab.load_traversal(u.clone()) {
                                    debug!(error = %e, "back traversal failed");
                                }
                                app.autocomplete.insert(
                                    u,
                                    tab.title.clone(),
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_millis() as i64)
                                        .unwrap_or(0),
                                );
                            }
                        }
                    }
                });
                ui.add_enabled_ui(can_forward, |ui| {
                    let r = crate::icons::button(
                        ui,
                        egui::vec2(32.0, 28.0),
                        crate::icons::Icon::ArrowRight,
                        app.theme.palette.foreground,
                        app.theme.palette.surface_2,
                    )
                    .on_hover_text("Forward (Alt+→)");
                    if r.clicked() {
                        if let Some(tab) = app.tabs.active_tab_mut() {
                            if let Some(url) = tab.go_forward() {
                                if let Err(e) = tab.load_traversal(url) {
                                    debug!(error = %e, "forward traversal failed");
                                }
                            }
                        }
                    }
                });
                // Reload / Stop
                let (reload_icon, reload_tip, reload_fg) = if is_loading {
                    (
                        crate::icons::Icon::X,
                        "Stop (Esc)",
                        app.theme.palette.accent_danger,
                    )
                } else {
                    (
                        crate::icons::Icon::RotateCw,
                        "Reload (Ctrl+R)",
                        app.theme.palette.foreground,
                    )
                };
                let rr = crate::icons::button(
                    ui,
                    egui::vec2(32.0, 28.0),
                    reload_icon,
                    reload_fg,
                    app.theme.palette.surface_2,
                )
                .on_hover_text(reload_tip);
                if rr.clicked() {
                    if is_loading {
                        if let Some(tab) = app.tabs.active_tab_mut() {
                            if let Err(e) = tab.stop() {
                                debug!(error = %e, "stop failed");
                            }
                        }
                    } else if let Some(tab) = app.tabs.active_tab_mut() {
                        let _ = tab.set_visible(true);
                        if let Err(e) = tab.reload() {
                            debug!(error = %e, "reload failed");
                        }
                    }
                }
                let hr = crate::icons::button(
                    ui,
                    egui::vec2(32.0, 28.0),
                    crate::icons::Icon::House,
                    app.theme.palette.foreground,
                    app.theme.palette.surface_2,
                )
                .on_hover_text("Home (ferrite://newtab)");
                if hr.clicked() {
                    if let Some(tab) = app.tabs.active_tab_mut() {
                        let _ = tab.load_url("ferrite://newtab".to_owned());
                    } else {
                        open_tab(&mut app.tabs, window, "ferrite://newtab".to_owned());
                    }
                    app.omnibox.clear();
                    app.omnibox.request_focus();
                }
                ui.add_space(4.0);
                // ── Omnibox pill — Chrome 32h 16r SURFACE_2 flex 320-720 centered ──
                let avail_raw = ui.available_width();
                let avail = if avail_raw.is_finite() {
                    (avail_raw - 140.0).clamp(320.0, 720.0)
                } else {
                    520.0
                };
                // engine popover state
                let engine_popover_id = egui::Id::new("engine_popover_open");
                let mut pop_open = ui
                    .ctx()
                    .data(|d| d.get_temp::<bool>(engine_popover_id))
                    .unwrap_or(false);
                let focused_prev = ui
                    .ctx()
                    .data(|d| d.get_temp::<bool>(egui::Id::new("omnibox_focused")))
                    .unwrap_or(false);
                let active_url_raw = app.tabs.active_tab().map(|t| t.url.as_str()).unwrap_or("");
                let display_for_highlight =
                    if active_url_raw == "ferrite://newtab" || active_url_raw.is_empty() {
                        String::new()
                    } else {
                        active_url_raw.to_owned()
                    };
                let should_highlight = !focused_prev
                    && app.omnibox.input.is_empty()
                    && !display_for_highlight.is_empty();
                // security for leading icon
                let sec_is_internal = active_url_raw.starts_with("ferrite://");
                let sec_is_https = active_url_raw.starts_with("https://");
                let sec_icon = if sec_is_internal {
                    crate::icons::Icon::Shield
                } else if sec_is_https {
                    crate::icons::Icon::Lock
                } else {
                    crate::icons::Icon::Info
                };
                let sec_color = if sec_is_internal {
                    app.theme.palette.accent_interactive
                } else if sec_is_https {
                    crate::theme::TEXT_SECONDARY
                } else {
                    crate::theme::TEXT_MUTED
                };
                let (focused_now, rect) = if should_highlight {
                    let r = egui::Frame::default()
                        .fill(app.theme.palette.surface_2)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(crate::theme::CORNER_OMNIBOX)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_min_size(egui::vec2(avail, 32.0));
                            ui.set_max_width(avail);
                            let lr = ui
                                .allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover())
                                .0;
                            crate::icons::paint(ui.painter(), lr, sec_icon, sec_color);
                            ui.add_space(6.0);
                            let parsed = url::Url::parse(&display_for_highlight).ok();
                            let host = parsed.as_ref().and_then(|u| u.host_str()).unwrap_or("");
                            let scheme = parsed
                                .as_ref()
                                .map(|u| u.scheme().to_owned())
                                .unwrap_or_default();
                            let path = parsed
                                .as_ref()
                                .map(|u| {
                                    format!(
                                        "{}{}",
                                        u.path(),
                                        u.query().map(|q| format!("?{q}")).unwrap_or_default()
                                    )
                                })
                                .unwrap_or_default();
                            let domain_hl = {
                                let parts: Vec<&str> = host.split('.').collect();
                                if parts.len() >= 2 {
                                    format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1])
                                } else {
                                    host.to_owned()
                                }
                            };
                            let resp = ui.horizontal(|ui| {
                                if !scheme.is_empty() {
                                    ui.label(
                                        RichText::new(format!("{}://", scheme))
                                            .size(13.0)
                                            .color(app.theme.palette.foreground.gamma_multiply(0.5))
                                            .font(egui::FontId::monospace(13.0)),
                                    );
                                }
                                if !domain_hl.is_empty() {
                                    let sub = if host.len() > domain_hl.len() {
                                        host[..host.len() - domain_hl.len()].to_owned()
                                    } else {
                                        String::new()
                                    };
                                    if !sub.is_empty() {
                                        ui.label(
                                            RichText::new(sub)
                                                .size(13.0)
                                                .color(
                                                    app.theme
                                                        .palette
                                                        .foreground
                                                        .gamma_multiply(0.5),
                                                )
                                                .font(egui::FontId::monospace(13.0)),
                                        );
                                    }
                                    ui.label(
                                        RichText::new(domain_hl)
                                            .size(13.0)
                                            .color(app.theme.palette.foreground)
                                            .font(egui::FontId::monospace(13.0)),
                                    );
                                } else if !host.is_empty() {
                                    ui.label(
                                        RichText::new(host)
                                            .size(13.0)
                                            .color(app.theme.palette.foreground)
                                            .font(egui::FontId::monospace(13.0)),
                                    );
                                }
                                if !path.is_empty() && path != "/" {
                                    ui.label(
                                        RichText::new(path)
                                            .size(13.0)
                                            .color(app.theme.palette.foreground.gamma_multiply(0.5))
                                            .font(egui::FontId::monospace(13.0)),
                                    );
                                }
                                if display_for_highlight.is_empty() {
                                    ui.label(
                                        RichText::new("Search or enter URL (Ctrl+L)")
                                            .size(13.0)
                                            .color(app.theme.palette.foreground.gamma_multiply(0.4))
                                            .font(egui::FontId::monospace(13.0)),
                                    );
                                }
                            });
                            // trailing star
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let sr = ui
                                        .allocate_exact_size(
                                            egui::vec2(20.0, 20.0),
                                            egui::Sense::hover(),
                                        )
                                        .0;
                                    crate::icons::paint(
                                        ui.painter(),
                                        sr,
                                        crate::icons::Icon::Star,
                                        crate::theme::TEXT_MUTED,
                                    );
                                },
                            );
                            let rect = resp.response.rect;
                            if resp.response.interact(egui::Sense::click()).clicked() {
                                app.omnibox.set_input(display_for_highlight.clone());
                                app.omnibox.request_focus();
                            }
                            rect
                        })
                        .inner;
                    let foc = false;
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(egui::Id::new("omnibox_rect"), r);
                        d.insert_temp(egui::Id::new("omnibox_focused"), foc);
                    });
                    app.omnibox.focused = foc;
                    ui.painter().rect_stroke(
                        r,
                        crate::theme::CORNER_OMNIBOX,
                        egui::Stroke::new(1.0, crate::theme::BORDER_SUBTLE),
                        egui::StrokeKind::Inside,
                    );
                    (foc, r)
                } else {
                    let field = egui::Frame::default()
                        .fill(app.theme.palette.surface_2)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(crate::theme::CORNER_OMNIBOX)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_min_size(egui::vec2(avail, 32.0));
                            ui.set_max_width(avail);
                            ui.horizontal(|ui| {
                                let lr = ui
                                    .allocate_exact_size(
                                        egui::vec2(18.0, 18.0),
                                        egui::Sense::hover(),
                                    )
                                    .0;
                                crate::icons::paint(ui.painter(), lr, sec_icon, sec_color);
                                ui.add_space(6.0);
                                let mut input = app.omnibox.input.clone();
                                let wants_focus = app.omnibox.consume_focus();
                                if input == "ferrite://newtab" {
                                    input.clear();
                                }
                                let resp = egui::TextEdit::singleline(&mut input)
                                    .font(egui::TextStyle::Monospace)
                                    .id_salt("ferrite_omnibox_input")
                                    .hint_text("Search or enter URL (Ctrl+L)")
                                    .desired_width((avail - 60.0).max(200.0))
                                    .show(ui);
                                if wants_focus {
                                    resp.response.request_focus();
                                }
                                if resp.response.changed() {
                                    app.omnibox.set_input(input.clone());
                                }
                                // trailing star + clear
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let sr = ui
                                            .allocate_exact_size(
                                                egui::vec2(20.0, 20.0),
                                                egui::Sense::hover(),
                                            )
                                            .0;
                                        crate::icons::paint(
                                            ui.painter(),
                                            sr,
                                            crate::icons::Icon::Star,
                                            crate::theme::TEXT_MUTED,
                                        );
                                        if !input.is_empty() {
                                            let cr = ui
                                                .allocate_exact_size(
                                                    egui::vec2(18.0, 18.0),
                                                    egui::Sense::click(),
                                                )
                                                .0;
                                            let (cr_rect, c_resp) = (
                                                cr,
                                                ui.interact(
                                                    cr,
                                                    egui::Id::new("omnibox_clear"),
                                                    egui::Sense::click(),
                                                ),
                                            );
                                            crate::icons::paint(
                                                ui.painter(),
                                                cr_rect,
                                                crate::icons::Icon::X,
                                                crate::theme::TEXT_MUTED,
                                            );
                                            if c_resp.clicked() {
                                                app.omnibox.clear();
                                            }
                                        }
                                    },
                                );
                                (resp, input)
                            })
                            .inner
                        });
                    let (resp, _input) = field.inner;
                    let foc = resp.response.has_focus();
                    let r = resp.response.rect;
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(egui::Id::new("omnibox_rect"), r);
                        d.insert_temp(egui::Id::new("omnibox_focused"), foc);
                    });
                    app.omnibox.focused = foc;
                    ui.painter().rect_stroke(
                        r,
                        crate::theme::CORNER_OMNIBOX,
                        egui::Stroke::new(
                            1.0,
                            if foc {
                                crate::theme::BORDER_ACTIVE
                            } else {
                                crate::theme::BORDER_SUBTLE
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    if foc && !focused_prev {
                        let id = resp.response.id;
                        let mut st = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                        let len = app.omnibox.input.chars().count();
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(len),
                        )));
                        egui::TextEdit::store_state(ui.ctx(), id, st);
                    }
                    (foc, r)
                };
                let _ = rect;
                // engine selector small pill after omnibox
                let trigger_resp = ui.add(
                    egui::Button::new(
                        RichText::new(format!("v {}", app.omnibox.registry.current().name))
                            .size(11.0)
                            .font(egui::FontId::monospace(11.0)),
                    )
                    .fill(app.theme.palette.surface_2)
                    .corner_radius(crate::theme::CORNER_OMNIBOX)
                    .min_size(egui::vec2(64.0, 28.0)),
                );
                if trigger_resp.clicked() {
                    pop_open = !pop_open;
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(engine_popover_id, pop_open));
                }
                if pop_open {
                    let below = trigger_resp.rect.left_bottom() + egui::vec2(0.0, 4.0);
                    let mut keep_open = true;
                    egui::Area::new(egui::Id::new("engine_popover_panel"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(below)
                        .show(ui.ctx(), |inner| {
                            egui::Frame::default()
                                .fill(app.theme.palette.surface_2)
                                .stroke(egui::Stroke::new(1.0, crate::theme::BORDER_SUBTLE))
                                .corner_radius(crate::theme::CORNER_SM)
                                .inner_margin(egui::Margin::same(4))
                                .show(inner, |panel| {
                                    let engines = app.omnibox.registry.all();
                                    for (i, engine) in engines.iter().enumerate() {
                                        let selected =
                                            app.omnibox.registry.current_id() == engine.id;
                                        let row = panel.add_enabled(
                                            true,
                                            egui::Button::new(
                                                RichText::new(engine.name)
                                                    .size(12.5)
                                                    .font(egui::FontId::monospace(12.5)),
                                            )
                                            .fill(if selected {
                                                app.theme.palette.surface_3
                                            } else {
                                                egui::Color32::TRANSPARENT
                                            })
                                            .corner_radius(crate::theme::CORNER_SM)
                                            .min_size(egui::vec2(150.0, 20.0)),
                                        );
                                        if row.clicked() {
                                            let prev_input = app.omnibox.input.clone();
                                            let _ = app.omnibox.registry.set_current(engine.id);
                                            persist_omnibox_engine(engine.id);
                                            if !prev_input.trim().is_empty() {
                                                if let Ok(url) =
                                                    app.omnibox.url_for_input(&prev_input)
                                                {
                                                    if let Some(tab) = app.tabs.active_tab_mut() {
                                                        let _ = tab.load_url(url.clone());
                                                        app.autocomplete.insert(
                                                            url,
                                                            prev_input,
                                                            std::time::SystemTime::now()
                                                                .duration_since(
                                                                    std::time::UNIX_EPOCH,
                                                                )
                                                                .map(|d| d.as_millis() as i64)
                                                                .unwrap_or(0),
                                                        );
                                                    }
                                                }
                                            }
                                            keep_open = false;
                                        }
                                        if i + 1 < engines.len() {
                                            let sep_rect = egui::Rect::from_min_size(
                                                panel.cursor().left_bottom(),
                                                egui::vec2(panel.available_width(), 1.0),
                                            );
                                            panel.allocate_rect(sep_rect, egui::Sense::hover());
                                            panel.painter().rect_filled(
                                                sep_rect,
                                                0.0,
                                                crate::theme::BORDER_SUBTLE,
                                            );
                                        }
                                    }
                                });
                        });
                    if !keep_open
                        || trigger_resp.clicked_elsewhere()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(engine_popover_id, false));
                    }
                }
                let now_i = std::time::Instant::now();
                let effective = app
                    .omnibox
                    .effective_query(now_i)
                    .map(str::to_owned)
                    .unwrap_or_default();
                let suggestions = if effective.is_empty() {
                    Vec::new()
                } else {
                    app.autocomplete.query(&effective, 5)
                };
                let sel_id = egui::Id::new("omni_sug_sel");
                let mut sel = ui.ctx().data(|d| d.get_temp::<usize>(sel_id)).unwrap_or(0);
                if !suggestions.is_empty() {
                    sel = sel.min(suggestions.len() - 1);
                }
                if focused_now {
                    let (up, down) = ui.input(|i| {
                        (
                            i.key_pressed(egui::Key::ArrowUp),
                            i.key_pressed(egui::Key::ArrowDown),
                        )
                    });
                    if (!suggestions.is_empty()) && (up || down) {
                        if up {
                            sel = sel.saturating_sub(1);
                        } else {
                            sel = (sel + 1).min(suggestions.len() - 1);
                        }
                        ui.ctx().data_mut(|d| d.insert_temp(sel_id, sel));
                    }
                }
                let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let submit_via_lost_focus = enter_pressed && !focused_now && focused_prev;
                let pick_suggestion = |app: &mut AppContext, url: String, title: String| {
                    if let Some(tab) = app.tabs.active_tab_mut() {
                        if let Err(e) = tab.load_url(url.clone()) {
                            debug!(error = %e, "suggestion navigation failed");
                        }
                    } else {
                        open_tab(&mut app.tabs, window, url.clone());
                    }
                    app.autocomplete.insert(
                        url,
                        title,
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0),
                    );
                    app.omnibox.clear();
                };
                if enter_pressed
                    && focused_now
                    && !suggestions.is_empty()
                    && sel < suggestions.len()
                {
                    let sug = suggestions[sel].clone();
                    pick_suggestion(app, sug.url, sug.title);
                } else if submit_via_lost_focus {
                    let raw = app.omnibox.input.clone();
                    if let Ok(action) = app.omnibox.classify(&raw) {
                        let url = match action {
                            OmniboxAction::Navigate { url } => url,
                            OmniboxAction::Search { search_url, .. } => search_url,
                            OmniboxAction::Internal { uri } => uri,
                        };
                        pick_suggestion(app, url, raw);
                    }
                }
                // OmniboxPopupView — Area Foreground anchored, 320-720 pill width, 32px rows
                if !suggestions.is_empty() && focused_now {
                    let w = if rect.width().is_finite() {
                        rect.width().clamp(320.0, 720.0)
                    } else {
                        520.0
                    };
                    let pos = rect.left_bottom() + egui::vec2(0.0, 4.0);
                    let mut clicked_idx: Option<usize> = None;
                    egui::Area::new(egui::Id::new("omnibox_suggestions"))
                        .fixed_pos(pos)
                        .order(egui::Order::Foreground)
                        .show(ui.ctx(), |ui| {
                            egui::Frame::default()
                                .fill(app.theme.palette.surface_2)
                                .stroke(egui::Stroke::new(1.0, crate::theme::BORDER_SUBTLE))
                                .corner_radius(crate::theme::CORNER_SM)
                                .inner_margin(egui::Margin::symmetric(0, 4))
                                .show(ui, |ui| {
                                    ui.set_min_size(egui::vec2(w, 0.0));
                                    for (i, sug) in suggestions.iter().enumerate() {
                                        let is_sel = i == sel;
                                        let row_resp = ui
                                            .allocate_ui_with_layout(
                                                egui::vec2(w, 32.0),
                                                egui::Layout::left_to_right(egui::Align::Center),
                                                |ui| {
                                                    if is_sel {
                                                        let bar_rect = ui
                                                            .allocate_space(egui::vec2(3.0, 28.0))
                                                            .1;
                                                        ui.painter().rect_filled(
                                                            bar_rect,
                                                            0.0,
                                                            app.theme.resolved_interactive_accent(),
                                                        );
                                                        ui.add_space(4.0);
                                                    } else {
                                                        ui.add_space(7.0);
                                                    }
                                                    let icon_r = ui
                                                        .allocate_exact_size(
                                                            egui::vec2(16.0, 16.0),
                                                            egui::Sense::hover(),
                                                        )
                                                        .0;
                                                    crate::icons::paint(
                                                        ui.painter(),
                                                        icon_r,
                                                        crate::icons::Icon::Clock,
                                                        crate::theme::TEXT_MUTED,
                                                    );
                                                    ui.add_space(4.0);
                                                    ui.vertical(|ui| {
                                                        ui.add_space(2.0);
                                                        let title_color = if is_sel {
                                                            app.theme.palette.foreground
                                                        } else {
                                                            app.theme
                                                                .palette
                                                                .foreground
                                                                .gamma_multiply(0.9)
                                                        };
                                                        ui.label(
                                                            RichText::new(&sug.title)
                                                                .size(13.0)
                                                                .color(title_color)
                                                                .font(egui::FontId::monospace(
                                                                    13.0,
                                                                )),
                                                        );
                                                        if sug.url != sug.title
                                                            && !sug.url.is_empty()
                                                        {
                                                            ui.label(
                                                                RichText::new(&sug.url)
                                                                    .size(11.0)
                                                                    .color(crate::theme::TEXT_MUTED)
                                                                    .font(egui::FontId::monospace(
                                                                        11.0,
                                                                    )),
                                                            );
                                                        }
                                                    });
                                                },
                                            )
                                            .response;
                                        let row_rect = row_resp.rect;
                                        let row_interact =
                                            ui.allocate_rect(row_rect, egui::Sense::click());
                                        if row_interact.clicked() {
                                            clicked_idx = Some(i);
                                        }
                                        if is_sel || row_interact.hovered() {
                                            let bg = if is_sel {
                                                app.theme.palette.surface_3
                                            } else {
                                                app.theme.palette.surface_3.gamma_multiply(0.3)
                                            };
                                            ui.painter().rect_filled(
                                                row_rect,
                                                crate::theme::CORNER_SM,
                                                bg,
                                            );
                                        }
                                        if i + 1 < suggestions.len() {
                                            let sep_y = row_rect.bottom();
                                            ui.painter().line_segment(
                                                [
                                                    egui::pos2(row_rect.left() + 16.0, sep_y),
                                                    egui::pos2(row_rect.right(), sep_y),
                                                ],
                                                egui::Stroke::new(0.5, crate::theme::BORDER_SUBTLE),
                                            );
                                        }
                                    }
                                });
                        });
                    if let Some(idx) = clicked_idx {
                        if let Some(sug) = suggestions.get(idx) {
                            pick_suggestion(app, sug.url.clone(), sug.title.clone());
                        }
                    }
                }
                // ── Trailing cluster — Shield, Bookmark, Terminal, Menu (outside pill, 32×28) ──
                ui.add_space(6.0);
                let shield_id = egui::Id::new("shield_popover_open");
                let mut shield_pop = ui
                    .ctx()
                    .data(|d| d.get_temp::<bool>(shield_id))
                    .unwrap_or(false);
                let shield_count = proxy_requests;
                let shield_fg = if shield_count > 0 {
                    crate::theme::ACCENT_PRIMARY
                } else {
                    crate::theme::TEXT_MUTED
                };
                let shield_bg = if shield_count > 0 {
                    crate::theme::ACCENT_PRIMARY.gamma_multiply(0.12)
                } else {
                    app.theme.palette.surface_2
                };
                let (shield_rect, shield_resp) =
                    ui.allocate_exact_size(egui::vec2(58.0, 28.0), egui::Sense::click());
                {
                    let bg = if shield_resp.is_pointer_button_down_on() {
                        shield_bg.gamma_multiply(0.8)
                    } else if shield_resp.hovered() {
                        crate::theme::SURFACE_3
                    } else {
                        shield_bg
                    };
                    let stroke = if shield_resp.hovered() {
                        egui::Stroke::new(1.0, crate::theme::BORDER_ACTIVE)
                    } else {
                        egui::Stroke::new(1.0, crate::theme::BORDER_SUBTLE)
                    };
                    ui.painter()
                        .rect_filled(shield_rect, crate::theme::CORNER_SM, bg);
                    ui.painter().rect_stroke(
                        shield_rect,
                        crate::theme::CORNER_SM,
                        stroke,
                        egui::StrokeKind::Inside,
                    );
                    let icon_rect = egui::Rect::from_center_size(
                        egui::pos2(shield_rect.left() + 14.0, shield_rect.center().y),
                        egui::vec2(20.0, 20.0),
                    );
                    crate::icons::paint(
                        ui.painter(),
                        icon_rect,
                        if shield_count > 0 {
                            crate::icons::Icon::ShieldCheck
                        } else {
                            crate::icons::Icon::Shield
                        },
                        shield_fg,
                    );
                    ui.painter().text(
                        egui::pos2(shield_rect.right() - 8.0, shield_rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        format!("{}", shield_count),
                        crate::theme::ThemeConfig::technical_label_font(10.0),
                        shield_fg,
                    );
                }
                if shield_resp
                    .clone()
                    .on_hover_text("Shield — blocked")
                    .clicked()
                {
                    shield_pop = !shield_pop;
                    ui.ctx().data_mut(|d| d.insert_temp(shield_id, shield_pop));
                }
                let is_pinned = app
                    .tabs
                    .active_tab()
                    .map(|t| t.is_archive_view)
                    .unwrap_or(false);
                let pin_resp = crate::icons::button(
                    ui,
                    egui::vec2(32.0, 28.0),
                    if is_pinned {
                        crate::icons::Icon::PinFilled
                    } else {
                        crate::icons::Icon::Bookmark
                    },
                    if is_pinned {
                        app.theme.palette.accent_archive
                    } else {
                        crate::theme::TEXT_MUTED
                    },
                    if is_pinned {
                        app.theme.palette.accent_archive.gamma_multiply(0.14)
                    } else {
                        app.theme.palette.surface_2
                    },
                )
                .on_hover_text("Pin to Archive");
                let _ = pin_resp.clicked();
                let devtools_id = egui::Id::new("devtools_open");
                let devtools_open = ui
                    .ctx()
                    .data(|d| d.get_temp::<bool>(devtools_id))
                    .unwrap_or(false);
                let dt_resp = crate::icons::button(
                    ui,
                    egui::vec2(32.0, 28.0),
                    crate::icons::Icon::Terminal,
                    if devtools_open {
                        app.theme.resolved_interactive_accent()
                    } else {
                        crate::theme::TEXT_MUTED
                    },
                    if devtools_open {
                        app.theme.resolved_interactive_accent().gamma_multiply(0.15)
                    } else {
                        app.theme.palette.surface_2
                    },
                )
                .on_hover_text("Inspector F12");
                if dt_resp.clicked() {
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(devtools_id, !devtools_open));
                }
                let menu_id = egui::Id::new("menu_popover_open");
                let mut menu_open = ui
                    .ctx()
                    .data(|d| d.get_temp::<bool>(menu_id))
                    .unwrap_or(false);
                let menu_resp = crate::icons::button(
                    ui,
                    egui::vec2(32.0, 28.0),
                    crate::icons::Icon::Menu,
                    crate::theme::TEXT_MUTED,
                    app.theme.palette.surface_2,
                )
                .on_hover_text("Ferrite Menu");
                if menu_resp.clicked() {
                    menu_open = !menu_open;
                    ui.ctx().data_mut(|d| d.insert_temp(menu_id, menu_open));
                }
                if shield_pop {
                    let below = shield_rect.left_bottom() + egui::vec2(0.0, 4.0);
                    egui::Area::new(egui::Id::new("shield_popover_panel"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(below)
                        .show(ui.ctx(), |inner| {
                            egui::Frame::default()
                                .fill(app.theme.palette.surface_2)
                                .stroke(egui::Stroke::new(1.0, crate::theme::BORDER_SUBTLE))
                                .corner_radius(crate::theme::CORNER_SM)
                                .inner_margin(egui::Margin::same(8))
                                .show(inner, |panel| {
                                    panel.set_min_size(egui::vec2(240.0, 0.0));
                                    panel.label(
                                        RichText::new("Privacy Protection")
                                            .strong()
                                            .size(13.0)
                                            .color(app.theme.palette.foreground),
                                    );
                                    panel.add_space(4.0);
                                    panel.label(
                                        RichText::new(format!(
                                            "{} requests blocked",
                                            proxy_requests
                                        ))
                                        .size(12.0)
                                        .color(crate::theme::TEXT_SECONDARY)
                                        .font(egui::FontId::monospace(12.0)),
                                    );
                                    panel.add_space(4.0);
                                    let sep_rect = egui::Rect::from_min_size(
                                        panel.cursor().left_bottom(),
                                        egui::vec2(panel.available_width(), 1.0),
                                    );
                                    panel.allocate_rect(sep_rect, egui::Sense::hover());
                                    panel.painter().rect_filled(
                                        sep_rect,
                                        0.0,
                                        crate::theme::BORDER_SUBTLE,
                                    );
                                    panel.add_space(4.0);
                                    panel.label(
                                        RichText::new("Blockers active for this session")
                                            .size(11.0)
                                            .color(crate::theme::TEXT_MUTED),
                                    );
                                });
                        });
                    if shield_resp.clicked_elsewhere()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        ui.ctx().data_mut(|d| d.insert_temp(shield_id, false));
                    }
                }
                if menu_open {
                    let below = menu_resp.rect.left_bottom() + egui::vec2(0.0, 4.0);
                    egui::Area::new(egui::Id::new("menu_popover_panel"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(below)
                        .show(ui.ctx(), |inner| {
                            egui::Frame::default()
                                .fill(app.theme.palette.surface_2)
                                .stroke(egui::Stroke::new(1.0, crate::theme::BORDER_SUBTLE))
                                .corner_radius(crate::theme::CORNER_SM)
                                .inner_margin(egui::Margin::symmetric(0, 4))
                                .show(inner, |panel| {
                                    panel.set_min_size(egui::vec2(200.0, 0.0));
                                    let items = [
                                        ("New Tab", "Ctrl+T"),
                                        ("New Window", ""),
                                        ("---", ""),
                                        ("History", ""),
                                        ("Downloads", ""),
                                        ("Bookmarks", ""),
                                        ("---", ""),
                                        ("Settings", ""),
                                        ("Password Manager", ""),
                                        ("Archive", ""),
                                        ("---", ""),
                                        ("DevTools", ""),
                                    ];
                                    for (label, shortcut) in &items {
                                        if *label == "---" {
                                            let sep_rect = egui::Rect::from_min_size(
                                                panel.cursor().left_bottom(),
                                                egui::vec2(panel.available_width(), 1.0),
                                            );
                                            panel.allocate_rect(sep_rect, egui::Sense::hover());
                                            panel.painter().rect_filled(
                                                sep_rect,
                                                0.0,
                                                crate::theme::BORDER_SUBTLE,
                                            );
                                            panel.add_space(4.0);
                                            continue;
                                        }
                                        let row = panel.add(
                                            egui::Button::new(
                                                RichText::new(*label)
                                                    .size(12.5)
                                                    .font(egui::FontId::monospace(12.5)),
                                            )
                                            .fill(egui::Color32::TRANSPARENT)
                                            .corner_radius(crate::theme::CORNER_SM)
                                            .min_size(egui::vec2(200.0, 28.0)),
                                        );
                                        if row.hovered() {
                                            panel.painter().rect_filled(
                                                row.rect,
                                                crate::theme::CORNER_SM,
                                                app.theme.palette.surface_3.gamma_multiply(0.3),
                                            );
                                        }
                                        if !shortcut.is_empty() {
                                            panel.painter().text(
                                                egui::pos2(
                                                    row.rect.right() - 8.0,
                                                    row.rect.center().y,
                                                ),
                                                egui::Align2::RIGHT_CENTER,
                                                *shortcut,
                                                egui::FontId::monospace(11.0),
                                                crate::theme::TEXT_MUTED,
                                            );
                                        }
                                    }
                                });
                        });
                    if menu_resp.clicked_elsewhere()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        ui.ctx().data_mut(|d| d.insert_temp(menu_id, false));
                    }
                }
            });
        });
    if diagnostics {
        let hover = ui.input(|i| i.pointer.hover_pos());
        let fps = if frame_secs > 0.0 {
            1.0 / frame_secs
        } else {
            0.0
        };
        egui::Panel::bottom("diagnostics")
            .frame(egui::Frame::default().fill(SURFACE_1))
            .show(ui, |ui| {
                ui.set_min_height(56.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("diagnostics")
                            .strong()
                            .color(app.theme.resolved_interactive_accent()),
                    );
                    ui.label(
                        RichText::new(format!("cursor: {hover:?}"))
                            .color(app.theme.palette.foreground),
                    );
                    ui.label(
                        RichText::new(format!("tabs: {}", app.tabs.len()))
                            .color(app.theme.palette.foreground),
                    );
                    ui.label(
                        RichText::new(format!("net {} (drop {})", proxy_requests, proxy_dropped))
                            .font(crate::theme::ThemeConfig::telemetry_font(11.0))
                            .color(app.theme.palette.foreground),
                    );
                    ui.label(
                        RichText::new(match &last_download {
                            Some(last) => format!("dl {downloads_seen} ({last})"),
                            None => format!("dl {downloads_seen}"),
                        })
                        .font(crate::theme::ThemeConfig::telemetry_font(11.0))
                        .color(app.theme.palette.foreground),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("scale {scale:.3}"))
                            .color(app.theme.palette.foreground),
                    );
                    ui.label(
                        RichText::new(format!(
                            "logical {:.0}x{:.0}",
                            logical.width, logical.height
                        ))
                        .color(app.theme.palette.foreground),
                    );
                    ui.label(
                        RichText::new(format!("{fps:.1} fps")).color(app.theme.palette.foreground),
                    );
                    if let Some(active) = app.tabs.active_tab() {
                        ui.label(
                            RichText::new(format!("active: {}", active.url))
                                .color(app.theme.palette.foreground),
                        );
                        if active.is_crashed {
                            ui.colored_label(ACCENT_DANGER, "CRASHED");
                        }
                    }
                });
                egui::warn_if_debug_build(ui);
            });
    }
    let active_snapshot = app
        .tabs
        .active_tab()
        .map(|t| (t.id, t.url.clone(), t.is_crashed));
    let central_resp = egui::CentralPanel::default()
        .frame(egui::Frame::default().fill(app.theme.palette.substrate))
        .show(ui, |ui| {
            if let Some((id, url, is_crashed)) = active_snapshot.clone() {
                if is_crashed {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.label(
                            RichText::new("This tab has crashed.")
                                .heading()
                                .color(ACCENT_DANGER),
                        );
                        ui.label(RichText::new(&url).color(app.theme.palette.foreground));
                        ui.add_space(12.0);
                        if ui.button("Reload Tab").clicked() {
                            if let Some(tab) = app.tabs.get_tab_mut(id) {
                                // History-preserving engine reload (load_url
                                // would re-push history and clear forward).
                                if let Err(e) = tab.reload() {
                                    debug!(error = %e, "crashed-tab reload failed");
                                }
                            }
                        }
                        if ui.button("Close Tab").clicked() {
                            let _ = app.tabs.close_tab(id);
                        }
                    });
                    return;
                }
            }
            if app.tabs.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label(
                        RichText::new("Sovereign Browser")
                            .heading()
                            .size(36.0)
                            .color(app.theme.palette.accent_primary),
                    );
                    ui.label(
                        RichText::new("Foundation - Parts 1-3")
                            .size(13.0)
                            .color(app.theme.palette.foreground.gamma_multiply(0.75)),
                    );
                    ui.add_space(12.0);
                    if ui.button("New Tab (Ctrl+T)").clicked() {
                        open_tab(&mut app.tabs, window, "ferrite://newtab".to_owned());
                    }
                });
            } else {
                ui.label(
                    RichText::new("Content area - wry webview child window renders here")
                        .color(app.theme.palette.foreground.gamma_multiply(0.5)),
                );
                if let Some(tab) = app.tabs.active_tab() {
                    if tab.url.starts_with("ferrite://") {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("Internal: {}", tab.url))
                                .color(app.theme.resolved_interactive_accent()),
                        );
                    }
                }
            }
        });
    content_rect_cell.set(Some(central_resp.response.rect));

    if *tab_search_open {
        let mut should_close = false;
        let mut switch_to: Option<uuid::Uuid> = None;
        egui::Window::new("Tab Search (Ctrl+Shift+A)")
            .collapsible(false)
            .resizable(true)
            .default_size(egui::vec2(420.0, 280.0))
            .show(ui.ctx(), |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(tab_search_query)
                        .hint_text("Filter tabs by title or URL - Enter to switch, Esc to close")
                        .desired_width(f32::INFINITY),
                );
                resp.request_focus();
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    should_close = true;
                }
                ui.separator();
                let results = app.tabs.search(tab_search_query);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if results.is_empty() {
                        ui.label(
                            egui::RichText::new("No tabs match")
                                .color(app.theme.palette.foreground.gamma_multiply(0.5)),
                        );
                    } else {
                        for tab in results {
                            let label = format!("{} - {}", tab.title, tab.url);
                            let btn =
                                egui::Button::new(label).wrap_mode(egui::TextWrapMode::Truncate);
                            if ui.add(btn).clicked() {
                                switch_to = Some(tab.id);
                                should_close = true;
                            }
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !should_close {
                                // Enter picks first filtered tab.
                                if let Some(first) = app.tabs.search(tab_search_query).first() {
                                    switch_to = Some(first.id);
                                    should_close = true;
                                }
                            }
                        }
                    }
                });
            });
        if let Some(id) = switch_to {
            // Restore path (not bare switch_tab): discarded tabs rebuild
            // their webview instead of going blank.
            let bounds = content_rect_cell
                .get()
                .and_then(|r| egui_rect_to_wry_rect(r, scale));
            let idx = app.tabs.iter().position(|t| t.id == id);
            if let Some(idx) = idx {
                if let Err(e) = app.tabs.switch_tab_with_window(window, idx, bounds) {
                    debug!(error = %e, "tab search switch failed");
                }
            }
            app.sync_theme_from_tabs();
        }
        if should_close {
            *tab_search_open = false;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            *tab_search_open = false;
        }
    }
}

/// Open a tab, logging refusals (limit/invalid URL) instead of failing
/// silently and leaving the user on a dead button.
fn open_tab(tabs: &mut ferrite_tabs::TabManager, window: &Window, url: String) {
    if let Err(e) = tabs.new_tab(Some(window), Some(url), None) {
        debug!(error = %e, "new tab refused");
    }
}

/// Persist the selected search engine id alongside `ferrite-settings.json`
/// (JSON-merge so privacy toggles are preserved; atomic tmp+rename).
fn persist_omnibox_engine(engine_id: &str) {
    let path = std::path::Path::new("ferrite-settings.json");
    let mut v: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "omnibox_engine".to_owned(),
            serde_json::Value::String(engine_id.to_owned()),
        );
    }
    if let Ok(json) = serde_json::to_string_pretty(&v) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}
pub fn run(config: ShellConfig) -> ShellResult<()> {
    let event_loop = EventLoop::<()>::with_user_event().build()?;
    let mut app = ShellApp::new(config);
    event_loop.run_app(&mut app)?;
    Ok(())
}
