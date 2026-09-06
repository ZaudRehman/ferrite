//! Webview abstraction: `WebViewHandle` with `wry` and mock implementations.

use std::rc::Rc;
use std::sync::Mutex;

use tracing::{debug, warn};
use uuid::Uuid;

use crate::error::{TabError, TabResult};

pub trait WebViewHandle: std::fmt::Debug {
    fn set_visible(&self, visible: bool) -> TabResult<()>;
    fn set_bounds(&self, bounds: wry::Rect) -> TabResult<()>;
    fn load_url(&self, url: &str) -> TabResult<()>;
    fn evaluate_script(&self, js: &str) -> TabResult<()>;
    /// Real engine reload (wry `reload()`), preserving session history.
    fn reload(&self) -> TabResult<()>;
    /// Move OS keyboard focus into this webview. Needed when two native
    /// child windows (chrome overlay + content) share one parent: egui and
    /// the content webview do not yield focus to each other on their own.
    fn focus(&self) -> TabResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcMessage {
    pub tab_id: Uuid,
    pub payload: String,
}

/// JS that notifies Rust when audio starts or stops.
pub fn audio_listener_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteAudioInstalled) return;
  window.__ferriteAudioInstalled = true;
  function anyPlaying() {
    var nodes = document.querySelectorAll('audio,video');
    for (var i = 0; i < nodes.length; i++) {
      if (!nodes[i].paused && !nodes[i].ended) return true;
    }
    return false;
  }
  function report() {
    try { window.ipc.postMessage(JSON.stringify({type:'audio', playing:anyPlaying()})); } catch(e) {}
  }
  function attach(root) {
    var nodes = (root || document).querySelectorAll('audio,video');
    nodes.forEach(function(m){
      if (m.__ferriteWired) return;
      m.__ferriteWired = true;
      ['play','pause','ended','volumechange'].forEach(function(ev){
        m.addEventListener(ev, report, true);
      });
    });
  }
  attach(document);
  var obs = new MutationObserver(function(muts){
    for (var i = 0; i < muts.length; i++) {
      for (var j = 0; j < muts[i].addedNodes.length; j++) {
        var n = muts[i].addedNodes[j];
        if (n.nodeType === 1) {
          if (n.matches && n.matches('audio,video')) attach(n.parentElement || document);
          else if (n.querySelectorAll) attach(n);
        }
      }
    }
  });
  obs.observe(document.documentElement, {childList:true, subtree:true});
  report();
})();"#
}

pub struct WryHandle {
    inner: wry::WebView,
}

impl std::fmt::Debug for WryHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WryHandle").finish_non_exhaustive()
    }
}

impl WryHandle {
    pub fn new(inner: wry::WebView) -> Self {
        Self { inner }
    }
}

impl WebViewHandle for WryHandle {
    fn set_visible(&self, visible: bool) -> TabResult<()> {
        self.inner
            .set_visible(visible)
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn set_bounds(&self, bounds: wry::Rect) -> TabResult<()> {
        self.inner
            .set_bounds(bounds)
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn load_url(&self, url: &str) -> TabResult<()> {
        self.inner
            .load_url(url)
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn evaluate_script(&self, js: &str) -> TabResult<()> {
        self.inner
            .evaluate_script(js)
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn reload(&self) -> TabResult<()> {
        self.inner
            .reload()
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn focus(&self) -> TabResult<()> {
        self.inner
            .focus()
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }
}

/// Mock for tests.
#[derive(Debug, Default)]
pub struct MockHandle {
    visible: Mutex<bool>,
    bounds: Mutex<Option<wry::Rect>>,
    last_url: Mutex<Option<String>>,
    scripts: Mutex<Vec<String>>,
    focused: Mutex<bool>,
}

impl MockHandle {
    pub fn new_visible(visible: bool) -> Self {
        Self {
            visible: Mutex::new(visible),
            ..Default::default()
        }
    }

    pub fn was_visible(&self) -> bool {
        *self.visible.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn last_bounds(&self) -> Option<wry::Rect> {
        *self.bounds.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn last_url(&self) -> Option<String> {
        self.last_url
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn scripts(&self) -> Vec<String> {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn was_focused(&self) -> bool {
        *self.focused.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl WebViewHandle for MockHandle {
    fn set_visible(&self, visible: bool) -> TabResult<()> {
        if let Ok(mut guard) = self.visible.lock() {
            *guard = visible;
        }
        Ok(())
    }

    fn set_bounds(&self, bounds: wry::Rect) -> TabResult<()> {
        if let Ok(mut guard) = self.bounds.lock() {
            *guard = Some(bounds);
        }
        Ok(())
    }

    fn load_url(&self, url: &str) -> TabResult<()> {
        if let Ok(mut guard) = self.last_url.lock() {
            *guard = Some(url.to_owned());
        }
        Ok(())
    }

    fn evaluate_script(&self, js: &str) -> TabResult<()> {
        if let Ok(mut guard) = self.scripts.lock() {
            guard.push(js.to_owned());
        }
        Ok(())
    }

    fn reload(&self) -> TabResult<()> {
        // Mock records the reload as navigation to the last URL so tests can
        // assert history-preserving reloads without a real engine.
        if let Ok(mut guard) = self.scripts.lock() {
            guard.push("location.reload()".to_owned());
        }
        Ok(())
    }

    fn focus(&self) -> TabResult<()> {
        if let Ok(mut guard) = self.focused.lock() {
            *guard = true;
        }
        Ok(())
    }
}

pub trait WebViewFactory: std::fmt::Debug {
    fn create(
        &self,
        window: &winit::window::Window,
        url: &str,
        bounds: Option<wry::Rect>,
    ) -> TabResult<Rc<dyn WebViewHandle>>;

    fn create_with_id(
        &self,
        window: &winit::window::Window,
        url: &str,
        bounds: Option<wry::Rect>,
        tab_id: uuid::Uuid,
    ) -> TabResult<Rc<dyn WebViewHandle>> {
        let _ = tab_id;
        self.create(window, url, bounds)
    }
}

#[derive(Debug, Clone)]
pub struct WryFactory {
    ipc_sender: tokio::sync::mpsc::Sender<IpcMessage>,
    ipc_limits: crate::ipc::SharedLimitsHandle,
    assets: std::sync::Arc<crate::protocol::AssetTable>,
    /// Shell theme CSS block, substituted for the marker in privileged
    /// pages at serve time. Empty until the shell provides it.
    theme_css: String,
    proxy: Option<(String, u16)>,
    archive: Option<std::sync::Arc<ferrite_archive::Archive>>,
    downloads: Option<crate::downloads::DownloadManager>,
    /// Destination path → download id, correlating started/completed hooks
    /// (wry passes no id through).
    pending_downloads: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
}

impl WryFactory {
    pub fn new(
        ipc_sender: tokio::sync::mpsc::Sender<IpcMessage>,
        ipc_limits: crate::ipc::SharedLimitsHandle,
    ) -> Self {
        Self {
            ipc_sender,
            ipc_limits,
            assets: std::sync::Arc::new(crate::protocol::AssetTable::new()),
            theme_css: String::new(),
            proxy: None,
            archive: None,
            downloads: None,
            pending_downloads: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    #[must_use]
    pub fn with_archive(mut self, archive: std::sync::Arc<ferrite_archive::Archive>) -> Self {
        self.archive = Some(archive);
        self
    }

    #[must_use]
    pub fn with_proxy(mut self, host: &str, port: u16) -> Self {
        self.proxy = Some((host.to_owned(), port));
        self
    }

    #[must_use]
    pub fn with_asset(mut self, name: &str, bytes: &'static [u8]) -> Self {
        let mut table = (*self.assets).clone();
        table.insert(name.to_owned(), std::sync::Arc::from(bytes));
        self.assets = std::sync::Arc::new(table);
        self
    }

    /// Theme CSS block served into privileged pages (see
    /// `crate::protocol::THEME_MARKER`). Captured per webview at creation.
    #[must_use]
    pub fn with_theme_css(mut self, css: impl Into<String>) -> Self {
        self.theme_css = css.into();
        self
    }

    /// Enable real downloads: started hooks pick a sanitized path under the
    /// Downloads directory and report to `manager`; completed hooks report
    /// success/failure. Without this, wry's default allows downloads with
    /// no visibility.
    #[must_use]
    pub fn with_downloads(mut self, manager: crate::downloads::DownloadManager) -> Self {
        self.downloads = Some(manager);
        self
    }

    #[cfg(test)]
    pub fn has_asset(&self, name: &str) -> bool {
        self.assets.contains_key(name)
    }
}

impl WebViewFactory for WryFactory {
    fn create(
        &self,
        window: &winit::window::Window,
        url: &str,
        bounds: Option<wry::Rect>,
    ) -> TabResult<Rc<dyn WebViewHandle>> {
        self.create_with_id(window, url, bounds, Uuid::new_v4())
    }

    fn create_with_id(
        &self,
        window: &winit::window::Window,
        url: &str,
        bounds: Option<wry::Rect>,
        tab_id: Uuid,
    ) -> TabResult<Rc<dyn WebViewHandle>> {
        let tab_hint = url.to_owned();
        let sender = self.ipc_sender.clone();
        let limits = self.ipc_limits.clone();
        let ipc_tab_id = tab_id;
        let dl_manager = self.downloads.clone();
        let dl_pending = std::sync::Arc::clone(&self.pending_downloads);

        let assets = self.assets.clone();
        let theme_css = self.theme_css.clone();
        let archive = self.archive.clone();
        #[allow(clippy::collapsible_if)]
        let mut builder = wry::WebViewBuilder::new()
            .with_url(url)
            .with_custom_protocol(
                "ferrite".to_string(),
                move |id, req: http::Request<Vec<u8>>| {
                    let uri = req.uri().to_string();
                    if uri.starts_with("ferrite://archive/") {
                        if let Some(arch) = &archive {
                            if uri.starts_with("ferrite://archive/view") {
                                let query = uri.split('?').nth(1).unwrap_or("");
                                if let Some(target) =
                                    crate::protocol::decode_query_param(query, "url")
                                {
                                    if let Ok(Some(snap)) = arch.get_latest(&target) {
                                        // Timestamp is i64 (no escaping needed); the
                                        // snapshot body is archived third-party HTML
                                        // served with scripts disabled (CSP below) so
                                        // stored scripts cannot reach `window.ipc`.
                                        // The banner uses theme vars; the style block
                                        // defines them (body content keeps its own).
                                        let banner = format!("<style>{}</style><div style=\"background:var(--f-surface-2);color:var(--f-fg);padding:8px;text-align:center;font-family:var(--f-font-ui)\">Archive snapshot from {}</div>", crate::protocol::apply_theme_css(crate::protocol::THEME_MARKER, &theme_css), snap.timestamp);
                                        let body = format!("{}{}", banner, snap.html);
                                        return http::Response::builder()
                                            .status(200)
                                            .header("Content-Type", "text/html; charset=utf-8")
                                            .header("Content-Security-Policy", "default-src 'self' ferrite:; style-src 'unsafe-inline'; script-src 'none'; object-src 'none'; base-uri 'none'")
                                            .header("X-Content-Type-Options", "nosniff")
                                            .header("Content-Length", body.len().to_string())
                                            .body(std::borrow::Cow::Owned(body.into_bytes()))
                                            .unwrap_or_else(|_| {
                                                http::Response::new(std::borrow::Cow::Borrowed(
                                                    b"archive error" as &[u8],
                                                ))
                                            });
                                    }
                                }
                                let body = "<h1>Archive not found</h1>".as_bytes().to_vec();
                                return http::Response::builder()
                                    .status(404)
                                    .header("Content-Type", "text/html; charset=utf-8")
                                    .header("X-Content-Type-Options", "nosniff")
                                    .body(std::borrow::Cow::Owned(body))
                                    .unwrap_or_else(|_| {
                                        http::Response::new(std::borrow::Cow::Borrowed(
                                            b"" as &[u8],
                                        ))
                                    });
                            }
                            if uri.starts_with("ferrite://archive/search") {
                                let query = uri.split('?').nth(1).unwrap_or("");
                                let q = crate::protocol::decode_query_param(query, "q")
                                    .unwrap_or_default();
                                let results = arch.search(&q, 20).unwrap_or_default();
                                let mut html = String::from("<html><body><h1>Archive search</h1><ul>");
                                for (url, snippet) in results {
                                    let href = crate::protocol::encode_query_component(&url);
                                    let safe_url = crate::protocol::html_escape(&url);
                                    let safe_snippet = crate::protocol::html_escape(&snippet);
                                    html.push_str(&format!("<li><a href=\"ferrite://archive/view?url={href}\">{safe_url}</a> - {safe_snippet}</li>"));
                                }
                                html.push_str("</ul></body></html>");
                                return http::Response::builder()
                                    .status(200)
                                    .header("Content-Type", "text/html; charset=utf-8")
                                    .header("X-Content-Type-Options", "nosniff")
                                    .body(std::borrow::Cow::Owned(html.into_bytes()))
                                    .unwrap_or_else(|_| {
                                        http::Response::new(std::borrow::Cow::Borrowed(
                                            b"" as &[u8],
                                        ))
                                    });
                            }
                        }
                    }
                    crate::protocol::ferrite_protocol_handler_with_assets(
                        id, req, &assets, &theme_css
                    )
                },
            )
            .with_initialization_script(audio_listener_js())
            .with_initialization_script(ferrite_privacy::js::param_stripping_js())
            .with_initialization_script(ferrite_privacy::js::deamp_js())
            .with_initialization_script(ferrite_privacy::js::fingerprint_spoof_js())
            .with_initialization_script(ferrite_credentials::scanner::scanner_js())
            .with_ipc_handler(move |request: wry::http::Request<String>| {
                let payload = request.body().clone();
                let msg = IpcMessage {
                    tab_id: ipc_tab_id,
                    payload,
                };
                // Bounded, non-blocking, rate-limited: oversize/full/burst
                // messages are dropped with a warning instead of growing
                // memory without bound.
                if !crate::ipc::IpcBridge::try_send_via(&sender, &limits, msg) {
                    warn!(%tab_hint, "ipc message dropped (bound)");
                }
            })
            .with_navigation_handler(|uri: String| {
                debug!(%uri, "navigation request");
                true
            })
            .with_download_started_handler({
                let dl_manager = dl_manager.clone();
                let dl_pending = std::sync::Arc::clone(&dl_pending);
                move |url: String, path: &mut std::path::PathBuf| {
                    let Some(manager) = dl_manager.as_ref() else {
                        // No download sink configured: deny rather than
                        // downloading invisibly to a default location.
                        warn!("download denied (no sink): {url}");
                        return false;
                    };
                    let filename = crate::downloads::sanitize_filename(
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("download"),
                    );
                    let dest = crate::downloads::default_download_dir().join(&filename);
                    let id = manager.start(&url, &filename);
                    if let Ok(mut pending) = dl_pending.lock() {
                        pending.insert(dest.to_string_lossy().to_string(), id);
                    }
                    *path = dest;
                    debug!(url, filename, "download started");
                    true
                }
            })
            .with_download_completed_handler({
                let dl_manager = dl_manager.clone();
                let dl_pending = std::sync::Arc::clone(&dl_pending);
                move |url: String, path: Option<std::path::PathBuf>, success: bool| {
                    let Some(manager) = dl_manager.as_ref() else {
                        return;
                    };
                    let (key, filename) = match &path {
                        Some(p) => (
                            p.to_string_lossy().to_string(),
                            p.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("download")
                                .to_owned(),
                        ),
                        None => (url.clone(), "download".to_owned()),
                    };
                    let id = dl_pending
                        .lock()
                        .ok()
                        .and_then(|mut m| m.remove(&key));
                    match (id, success) {
                        (Some(id), true) => {
                            manager.complete(&id, &url, &filename);
                            debug!(url, filename, "download completed");
                        }
                        (Some(id), false) => {
                            manager.fail(&id, &url, &filename);
                            warn!(url, filename, "download failed");
                        }
                        (None, _) => {
                            // macOS reports no path: track by URL so the
                            // event is still visible.
                            let id = manager.start(&url, &filename);
                            if success {
                                manager.complete(&id, &url, &filename);
                            } else {
                                manager.fail(&id, &url, &filename);
                            }
                        }
                    }
                }
            });

        if let Some(rect) = bounds {
            builder = builder.with_bounds(rect);
        }

        if let Some((ref host, port)) = self.proxy {
            builder = builder.with_proxy_config(wry::ProxyConfig::Http(wry::ProxyEndpoint {
                host: host.clone(),
                port: port.to_string(),
            }));
            debug!(%host, port, "webview routed through ferrite proxy");
        }

        #[cfg(target_os = "linux")]
        if std::env::var("WRY_WAYLAND").is_ok() {
            #[cfg(feature = "gtk")]
            {
                use gtk::prelude::*;
                let _ = gtk::init();
                let fixed = gtk::Fixed::new();
                let webview = builder
                    .build_gtk(&fixed)
                    .map_err(|source| TabError::WebViewCreationFailed { source })?;
                return Ok(Rc::new(WryHandle::new(webview)) as Rc<dyn WebViewHandle>);
            }
            #[cfg(not(feature = "gtk"))]
            tracing::warn!(
                "WRY_WAYLAND set but gtk feature not enabled; falling back to build_as_child"
            );
        }
        let webview = builder
            .build_as_child(window)
            .map_err(|source| TabError::WebViewCreationFailed { source })?;

        Ok(Rc::new(WryHandle::new(webview)) as Rc<dyn WebViewHandle>)
    }
}

/// Test factory that returns a `MockHandle`.
#[derive(Debug, Default, Clone)]
pub struct MockFactory;

impl WebViewFactory for MockFactory {
    fn create(
        &self,
        _window: &winit::window::Window,
        url: &str,
        _bounds: Option<wry::Rect>,
    ) -> TabResult<Rc<dyn WebViewHandle>> {
        let handle = Rc::new(MockHandle::default());
        // Record the initial URL so tests can assert it.
        let _ = handle.load_url(url);
        Ok(handle as Rc<dyn WebViewHandle>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_handle_records_visibility() {
        let h = MockHandle::default();
        assert!(!h.was_visible());
        h.set_visible(true).unwrap();
        assert!(h.was_visible());
        h.set_visible(false).unwrap();
        assert!(!h.was_visible());
    }

    #[test]
    fn mock_handle_records_url_and_script() {
        let h = MockHandle::default();
        h.load_url("https://example.com").unwrap();
        assert_eq!(h.last_url().unwrap(), "https://example.com");
        h.evaluate_script("alert(1)").unwrap();
        assert_eq!(h.scripts(), vec!["alert(1)"]);
    }

    #[test]
    fn mock_handle_bounds_round_trips() {
        let h = MockHandle::default();
        let rect = wry::Rect {
            position: wry::dpi::Position::Logical(wry::dpi::LogicalPosition::new(10.0, 20.0)),
            size: wry::dpi::Size::Logical(wry::dpi::LogicalSize::new(800.0, 600.0)),
        };
        h.set_bounds(rect).unwrap();
        let stored = h.last_bounds().unwrap();
        assert_eq!(stored.size, rect.size);
    }

    #[test]
    fn wry_factory_debug_is_stable() {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let bridge = crate::ipc::IpcBridge::new();
        let f = WryFactory::new(tx, bridge.limiter());
        assert!(format!("{f:?}").contains("WryFactory"));
    }
}
