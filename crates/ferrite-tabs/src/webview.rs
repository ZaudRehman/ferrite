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
}

/// Mock for tests.
#[derive(Debug, Default)]
pub struct MockHandle {
    visible: Mutex<bool>,
    bounds: Mutex<Option<wry::Rect>>,
    last_url: Mutex<Option<String>>,
    scripts: Mutex<Vec<String>>,
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

#[derive(Debug)]
pub struct WryFactory {
    ipc_sender: tokio::sync::mpsc::UnboundedSender<IpcMessage>,
    assets: std::sync::Arc<crate::protocol::AssetTable>,
    proxy: Option<(String, u16)>,
}

impl WryFactory {
    pub fn new(ipc_sender: tokio::sync::mpsc::UnboundedSender<IpcMessage>) -> Self {
        Self {
            ipc_sender,
            assets: std::sync::Arc::new(crate::protocol::AssetTable::new()),
            proxy: None,
        }
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
        let ipc_tab_id = tab_id;

        let mut builder = wry::WebViewBuilder::new()
            .with_url(url)
            .with_custom_protocol(
                "ferrite".to_string(),
                crate::protocol::ferrite_protocol_handler,
            )
            .with_initialization_script(audio_listener_js())
            .with_initialization_script(ferrite_privacy::js::param_stripping_js())
            .with_initialization_script(ferrite_privacy::js::deamp_js())
            .with_initialization_script(ferrite_privacy::js::fingerprint_spoof_js())
            .with_ipc_handler(move |request: wry::http::Request<String>| {
                let payload = request.body().clone();
                let msg = IpcMessage {
                    tab_id: ipc_tab_id,
                    payload,
                };
                if let Err(err) = sender.send(msg) {
                    warn!(%err, tab_hint, "ipc channel closed: dropping message");
                }
            })
            .with_navigation_handler(|uri: String| {
                debug!(%uri, "navigation request");
                true
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

        if std::env::var("WRY_WAYLAND").is_ok() {
            tracing::warn!(
                "WRY_WAYLAND set: attempting build_as_child; Wayland build_gtk deferred"
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
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let f = WryFactory::new(tx);
        assert!(format!("{f:?}").contains("WryFactory"));
    }
}
