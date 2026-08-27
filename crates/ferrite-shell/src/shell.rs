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
    theme::{ACCENT_DANGER, SURFACE_1, SURFACE_3, ThemeConfig},
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
}
impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            title: "Ferrite".to_string(),
            initial_size: LogicalSize::new(1280.0, 800.0),
            min_size: Some(LogicalSize::new(640.0, 480.0)),
            resizable: true,
            theme: ThemeConfig::default(),
            repaint_policy: RepaintPolicy::Continuous,
            diagnostics_on: true,
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
            ferrite_proxy::ProxyConfig { fixed_port },
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
            let mut f = WryFactory::new(ipc_bridge.sender.clone());
            if let Some(port) = proxy_port {
                f = f.with_proxy("127.0.0.1", port);
            }
            f = f.with_asset(
                "geist-pixel-square.ttf",
                crate::fonts::GEIST_PIXEL_SQUARE_TTF,
            );
            Rc::new(f)
        };
        let app = AppContext::new_with_factory(theme, factory);
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
            let _ = self
                .app
                .tabs
                .new_tab(Some(&window), Some("ferrite://newtab".to_owned()), None);
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
        if let Some(proxy) = self.proxy.as_ref() {
            while let Ok(ev) = proxy.telemetry.try_recv() {
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

        let mut theme_changed = false;
        for msg in self.ipc_bridge.drain() {
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
                _ => {
                    if let Some(t) = v.get("title").and_then(|x| x.as_str()) {
                        if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                            tab.set_title(t.to_owned());
                        }
                    }
                }
            }
        }
        self.app.tabs.discard_idle_tabs(Instant::now());
        theme_changed |= self.app.sync_theme_from_tabs();
        if theme_changed {
            self.app.theme.apply(&self.ctx);
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
        let primitives: Vec<ClippedPrimitive> = self
            .ctx
            .tessellate(output.shapes.clone(), output.pixels_per_point);
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
            Key::Character(ref s) if s == "l" => {
                self.app.omnibox.request_focus();
                window.request_redraw();
                true
            }
            Key::Character(ref s) if s == "t" => {
                let _ =
                    self.app
                        .tabs
                        .new_tab(Some(window), Some("ferrite://newtab".to_owned()), None);
                window.request_redraw();
                true
            }
            Key::Character(ref s) if s == "w" => {
                if let Some(id) = self.app.tabs.active_id() {
                    let _ = self.app.tabs.close_tab(id);
                }
                window.request_redraw();
                true
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
    content_rect_cell: &Cell<Option<Rect>>,
    tab_search_open: &mut bool,
    tab_search_query: &mut String,
) {
    egui::Panel::top("tab_bar")
        .frame(egui::Frame::default().fill(SURFACE_1))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut close_id: Option<uuid::Uuid> = None;
                let mut mute_idx: Option<usize> = None;
                let mut pin_idx: Option<usize> = None;
                let mut switch_idx: Option<usize> = None;
                for (idx, tab) in app.tabs.iter().enumerate() {
                    let is_active = app.tabs.active_index() == Some(idx);
                    let label = if tab.is_pinned {
                        let first = tab.title.chars().next().unwrap_or('·');
                        first.to_string()
                    } else if tab.title.len() > 24 {
                        format!("{}...", tab.title.chars().take(24).collect::<String>())
                    } else {
                        tab.title.clone()
                    };
                    let mut chip = egui::Button::new(label)
                        .selected(is_active)
                        .corner_radius(egui::CornerRadius::same(u8::MAX));
                    if tab.is_discarded() {
                        chip = chip.stroke(egui::Stroke::new(1.0, SURFACE_3));
                    }
                    if ui.add(chip).clicked() {
                        switch_idx = Some(idx);
                    }
                    if tab.is_loading {
                        ui.spinner();
                    }
                    if tab.is_playing_audio || tab.is_muted {
                        let icon = if tab.is_muted { "🔇" } else { "🔊" };
                        if ui.small_button(icon).clicked() {
                            mute_idx = Some(idx);
                        }
                    }
                    if tab.is_crashed {
                        ui.colored_label(ACCENT_DANGER, "x");
                    }
                    let resp_pin = ui
                        .small_button(if tab.is_pinned { "📌" } else { "·" })
                        .on_hover_text("Pin/Unpin");
                    if resp_pin.clicked() || resp_pin.secondary_clicked() {
                        pin_idx = Some(idx);
                    }
                    if ui.small_button("x").clicked() && !tab.is_pinned {
                        close_id = Some(tab.id);
                    }
                    ui.separator();
                }
                if let Some(idx) = switch_idx {
                    let _ = app.tabs.switch_tab_by_index(idx);
                    app.sync_theme_from_tabs();
                    app.theme.apply(ui.ctx());
                }
                if let Some(idx) = mute_idx {
                    let _ = app.tabs.toggle_mute(idx);
                }
                if let Some(idx) = pin_idx {
                    let _ = app.tabs.toggle_pin(idx);
                }
                if let Some(id) = close_id {
                    let _ = app.tabs.close_tab(id);
                }
                if ui.small_button("+").clicked() {
                    let _ =
                        app.tabs
                            .new_tab(Some(window), Some("ferrite://newtab".to_owned()), None);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let badge = format!("{}/50", app.tabs.len());
                    ui.label(
                        RichText::new(badge)
                            .font(ThemeConfig::technical_label_font(11.0))
                            .color(app.theme.palette.foreground.gamma_multiply(0.6)),
                    );
                });
            });
        });
    egui::Panel::top("omnibox_bar")
        .frame(egui::Frame::default().fill(SURFACE_1))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
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
                ui.add_enabled_ui(can_back, |ui| {
                    if ui.small_button("<").clicked() {
                        if let Some(tab) = app.tabs.active_tab_mut() {
                            if let Some(url) = tab.go_back() {
                                let u = url.clone();
                                let _ = tab.load_url(u.clone());
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
                    if ui.small_button(">").clicked() {
                        if let Some(tab) = app.tabs.active_tab_mut() {
                            if let Some(url) = tab.go_forward() {
                                let _ = tab.load_url(url);
                            }
                        }
                    }
                });
                if ui.small_button("R").clicked() {
                    if let Some(tab) = app.tabs.active_tab() {
                        let _ = tab.set_visible(true);
                    }
                }
                let is_https = app
                    .tabs
                    .active_tab()
                    .map(|t| t.url.starts_with("https://"))
                    .unwrap_or(false);
                let icon = if is_https { "[lock]" } else { "[~]" };
                ui.label(icon);
                let engine_popover_id = egui::Id::new("engine_popover_open");
                let mut pop_open = ui
                    .ctx()
                    .data(|d| d.get_temp::<bool>(engine_popover_id))
                    .unwrap_or(false);
                let trigger_resp = ui.add(
                    egui::Button::new(
                        RichText::new(format!("v {}", app.omnibox.registry.current().name))
                            .size(11.0),
                    )
                    .fill(app.theme.palette.surface_3)
                    .corner_radius(egui::CornerRadius::same(u8::MAX)),
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
                                .stroke(egui::Stroke::new(1.0, app.theme.palette.surface_3))
                                .corner_radius(egui::CornerRadius::same(0))
                                .inner_margin(egui::Margin::same(4))
                                .show(inner, |panel| {
                                    let engines = app.omnibox.registry.all();
                                    for (i, engine) in engines.iter().enumerate() {
                                        let selected =
                                            app.omnibox.registry.current_id() == engine.id;
                                        let row = panel.add_enabled(
                                            true,
                                            egui::Button::new(
                                                RichText::new(engine.name).size(12.5),
                                            )
                                            .fill(if selected {
                                                app.theme.palette.surface_3
                                            } else {
                                                egui::Color32::TRANSPARENT
                                            })
                                            .corner_radius(egui::CornerRadius::same(0))
                                            .min_size(egui::vec2(150.0, 20.0)),
                                        );
                                        if row.clicked() {
                                            let prev_input = app.omnibox.input.clone();
                                            let _ = app.omnibox.registry.set_current(engine.id);
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
                                            let sep_h = 1.0;
                                            let sep_w = panel.available_width();
                                            let sep_rect = egui::Rect::from_min_size(
                                                panel.cursor().left_bottom(),
                                                egui::vec2(sep_w, sep_h),
                                            );
                                            panel.allocate_rect(sep_rect, egui::Sense::hover());
                                            panel.painter().rect_filled(
                                                sep_rect,
                                                0.0,
                                                app.theme.palette.surface_3.gamma_multiply(0.6),
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

                let focused_prev = app.omnibox.focused;
                let field = egui::Frame::default()
                    .fill(app.theme.palette.surface_1)
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(crate::theme::CORNER_INPUT)
                    .inner_margin(egui::Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        let mut input = app.omnibox.input.clone();
                        let wants_focus = app.omnibox.consume_focus();
                        let resp = egui::TextEdit::singleline(&mut input)
                            .id_salt("ferrite_omnibox_input")
                            .hint_text("Search or enter URL (Ctrl+L)")
                            .desired_width(ui.available_width() - 30.0)
                            .show(ui);
                        if wants_focus {
                            resp.response.request_focus();
                        }
                        if resp.response.changed() {
                            app.omnibox.set_input(input.clone());
                        }
                        (resp, input)
                    });
                let (resp, _input) = field.inner;
                let focused_now = resp.response.has_focus();
                let rect = resp.response.rect;
                let y = rect.bottom() - 1.0;
                ui.painter().line_segment(
                    [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                    app.theme.input_bottom_stroke(focused_now),
                );

                if focused_now && !focused_prev {
                    let id = resp.response.id;
                    let mut st = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                    let len = app.omnibox.input.chars().count();
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(len),
                    )));
                    egui::TextEdit::store_state(ui.ctx(), id, st);
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
                let submit_via_lost_focus = resp.response.lost_focus() && enter_pressed;

                let pick_suggestion = |app: &mut AppContext, url: String, title: String| {
                    if let Some(tab) = app.tabs.active_tab_mut() {
                        let _ = tab.load_url(url.clone());
                    } else {
                        let _ = app.tabs.new_tab(Some(window), Some(url.clone()), None);
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

                if !suggestions.is_empty() && focused_now {
                    egui::Frame::default()
                        .fill(app.theme.palette.surface_2)
                        .stroke(egui::Stroke::new(1.0, app.theme.palette.surface_3))
                        .corner_radius(egui::CornerRadius::same(0))
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            for (i, sug) in suggestions.iter().enumerate() {
                                let row = ui.add(
                                    egui::Button::new(
                                        RichText::new(format!("{} - {}", sug.title, sug.url))
                                            .size(12.5),
                                    )
                                    .fill(if i == sel {
                                        app.theme.palette.surface_3
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    })
                                    .corner_radius(egui::CornerRadius::same(0))
                                    .min_size(egui::vec2(ui.available_width().min(520.0), 20.0)),
                                );
                                if i == sel {
                                    let r = row.rect;
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(
                                            egui::pos2(r.left(), r.top()),
                                            egui::vec2(3.0, r.height()),
                                        ),
                                        0.0,
                                        app.theme.resolved_interactive_accent(),
                                    );
                                }
                                if row.clicked() {
                                    pick_suggestion(app, sug.url.clone(), sug.title.clone());
                                }
                            }
                        });
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
                                let u = tab.url.clone();
                                tab.is_crashed = false;
                                tab.is_loading = true;
                                let _ = tab.load_url(u);
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
                        let _ = app.tabs.new_tab(
                            Some(window),
                            Some("ferrite://newtab".to_owned()),
                            None,
                        );
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
            let _ = app.tabs.switch_tab(id);
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
pub fn run(config: ShellConfig) -> ShellResult<()> {
    let event_loop = EventLoop::<()>::with_user_event().build()?;
    let mut app = ShellApp::new(config);
    event_loop.run_app(&mut app)?;
    Ok(())
}
