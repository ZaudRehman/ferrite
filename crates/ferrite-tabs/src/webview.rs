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
    fn reload(&self) -> TabResult<()>;
    /// Needed when chrome overlay and content share one parent: they do not
    /// yield focus on their own.
    fn focus(&self) -> TabResult<()>;
    fn cookies_for_url(&self, url: &str) -> TabResult<Vec<(String, String)>>;
    /// Profile scoped, not per tab. Powers clear all plus per site viewer.
    fn cookies_all(&self) -> TabResult<Vec<wry::cookie::Cookie<'static>>>;
    /// Only write surface in pinned wry. Scoped resets loop per origin.
    /// Never blanket wipe for one site. Third party blocking stays proxy side.
    fn delete_cookie(&self, cookie: &wry::cookie::Cookie<'_>) -> TabResult<()>;
    /// Engine inspector, never a Ferrite panel or debug port.
    fn open_devtools(&self) -> TabResult<()>;
    /// Engine clearing call. Irreversible, callers say so.
    fn clear_browsing_data(&self) -> TabResult<()>;
    /// OS print dialog. Only print path wry exposes.
    fn print(&self) -> TabResult<()>;
    /// Factor with 1.0 as 100 percent. Shell tracks last applied per tab.
    fn set_zoom(&self, factor: f64) -> TabResult<()>;
}

/// Translate an internal URL for post-creation navigation (wry applies its
/// workaround only to the creation URL, WebView2 rejects unknown schemes
/// on live loads). External URLs pass through.
pub fn workaround_url(url: &str) -> String {
    match url.strip_prefix("ferrite://") {
        Some(rest) => format!("http://ferrite.{rest}"),
        None => url.to_owned(),
    }
}

/// Reverse of [`workaround_url`]: fold an engine-committed WebView2 URL
/// (`http://ferrite.<page>/...`) back to its canonical `ferrite://` form.
///
/// Without this fold the tab keeps the workaround URL, loses its privileged
/// (`ferrite://`) status, and every later IPC from the page (settings,
/// welcome, passwords, ...) is dropped as unprivileged. Only exact
/// `ferrite.{known-page}` hosts fold back; real sites such as
/// `http://ferrite.com/` pass through untouched. Path/query/fragment keep
/// their original case; only the compared scheme/host are lowercased.
pub fn canonical_engine_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return url.to_owned();
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, suffix) = rest.split_at(end);
    // Userinfo/ports never appear in workaround URLs; refuse to fold them
    // rather than risk mangling a real host.
    if authority.contains(['@', ':']) {
        return url.to_owned();
    }
    let host = authority.to_ascii_lowercase();
    let Some(page) = host.strip_prefix("ferrite.") else {
        return url.to_owned();
    };
    if !crate::protocol::INTERNAL_PAGE_NAMES.contains(&page) {
        return url.to_owned();
    }
    // Engines append a trailing slash to bare hosts
    // (`http://ferrite.newtab/`), while creation URLs carry none
    // (`ferrite://newtab`). Drop the empty slash so commits converge on
    // the exact creation URL instead of forking a second tab state that
    // misses every `== "ferrite://<page>"` comparison downstream.
    let suffix = if suffix == "/" { "" } else { suffix };
    format!("ferrite://{page}{suffix}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcMessage {
    pub tab_id: Uuid,
    pub payload: String,
}

/// Icon-link reporter: posts the engine-resolved absolute icon URL so Rust
/// can fetch the bytes through the privacy proxy. No bitmap read here, no
/// canvas, no fetch from the page. Re-reports when head link tags change.
pub fn favicon_report_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteFaviconInstalled) return;
  window.__ferriteFaviconInstalled = true;
  function post(o) { try { window.ipc.postMessage(JSON.stringify(o)); } catch(e) {} }
  function current() {
    try {
      var links = document.querySelectorAll('link[rel~="icon"]');
      var href = null;
      for (var i = 0; i < links.length; i++) {
        var h = links[i].getAttribute('href');
        if (h) { href = h; break; }
      }
      if (!href) href = '/favicon.ico';
      var abs = new URL(href, document.baseURI).href;
      if (abs.length <= 2048) post({type:'favicon', url:abs});
    } catch(e) {}
  }
  var deb = null;
  function soon() { if (deb) clearTimeout(deb); deb = setTimeout(current, 400); }
  document.addEventListener('DOMContentLoaded', current);
  window.addEventListener('load', current);
  if (document.head) {
    new MutationObserver(soon).observe(document.head, {childList:true, subtree:true, attributes:true});
  }
  current();
})();"#
}

/// JS that reports the real hovered-link URL to Rust. Reads `element.href`
/// (engine-resolved) on hover, never `window.status`, which pages can spoof.
/// Hovering reports only, it triggers no fetch, DNS, or preconnect. Hover
/// storms (fast mouse traversal across link grids) are tamed send-side:
/// same-href repeats never re-post, and posts are throttled to one per
/// 100ms with a trailing edge, so the 10/s per-tab budget survives bursts.
/// Leaves always post immediately (single message, no throttle).
pub fn link_hover_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteHoverInstalled) return;
  window.__ferriteHoverInstalled = true;
  var hoverLast = null, hoverAt = 0, hoverTrailing = null;
  function post(type, url) {
    try { window.ipc.postMessage(JSON.stringify(url ? {type:type, url:url} : {type:type})); } catch(e) {}
    hoverAt = Date.now();
  }
  function hover(href) {
    if (href === hoverLast) return;
    hoverLast = href;
    if (hoverTrailing) { clearTimeout(hoverTrailing); hoverTrailing = null; }
    if (Date.now() - hoverAt >= 100) { post('link_hover', href); }
    else {
      hoverTrailing = setTimeout(function(){ hoverTrailing = null; post('link_hover', href); }, 100);
    }
  }
  function leave() {
    if (hoverLast === null && !hoverTrailing) return;
    if (hoverTrailing) { clearTimeout(hoverTrailing); hoverTrailing = null; }
    hoverLast = null;
    post('link_leave');
  }
  document.addEventListener('mouseover', function(e){
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (a && a.href) hover(a.href); else leave();
  }, true);
  document.addEventListener('mouseout', function(e){
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (a) leave();
  }, true);
  document.addEventListener('mouseleave', function(){ leave(); });
})();"#
}

/// Find-in-page engine (Custom Highlight API, main frame only). Runs at
/// document-start so the closure captures a pristine `window.find`. The query
/// only ever travels inside Rust-built strings, which page JS cannot read.
// ponytail: main-frame text nodes only (no cross-origin iframes), match
// cap 1000, native stepping where highlights are missing. Full coverage
// wants a framed search plus chunked scan.
pub fn find_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteFind) return;
  var nativeFind = (typeof window.find === 'function') ? window.find.bind(window) : null;
  var canHi = !!(window.CSS && CSS.highlights && window.Highlight);
  var ranges = [], total = 0, index = 0;
  function post() {
    try { window.ipc.postMessage(JSON.stringify({type:'find_result', total:total, index:index})); } catch(e) {}
  }
  // Accent current match, neutral wash for the rest (the app's "one lit
  // thing" rule). No author rule = UA-default yellow, hence this element.
  function ensureStyle(accent) {
    var el = document.getElementById('__ferriteFindStyle');
    if (!el) {
      el = document.createElement('style');
      el.id = '__ferriteFindStyle';
      (document.head || document.documentElement).appendChild(el);
    }
    var rule = '::highlight(ferrite-find){background:rgba(128,128,128,.35);}'
      + '::highlight(ferrite-current){background:' + accent + ';}';
    if (el.textContent !== rule) el.textContent = rule;
  }
  function scan(query, cs) {
    var out = [];
    var needle = cs ? query : query.toLowerCase();
    var walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    var node, skip = {SCRIPT:1, STYLE:1, NOSCRIPT:1, TEXTAREA:1};
    while ((node = walker.nextNode())) {
      var p = node.parentNode;
      if (!p || (p.tagName && skip[p.tagName])) continue;
      var text = node.nodeValue || '';
      var hay = cs ? text : text.toLowerCase();
      var from = 0, at;
      while ((at = hay.indexOf(needle, from)) >= 0) {
        var r = document.createRange();
        try { r.setStart(node, at); r.setEnd(node, at + query.length); out.push(r); }
        catch(e) { break; }
        from = at + query.length;
        if (out.length >= 1000) break;
      }
      if (out.length >= 1000) break;
    }
    return out;
  }
  function currentEl() {
    if (index < 1 || index > ranges.length) return null;
    var n = ranges[index-1].startContainer;
    return n ? (n.parentNode || n) : null;
  }
  function show() {
    if (canHi) {
      try {
        CSS.highlights.delete('ferrite-find');
        CSS.highlights.delete('ferrite-current');
        if (ranges.length) {
          var rest = ranges.filter(function(_, i){ return i !== index-1; });
          CSS.highlights.set('ferrite-find', Reflect.construct(Highlight, rest));
          if (index > 0) CSS.highlights.set('ferrite-current', new Highlight(ranges[index-1]));
        }
      } catch(e) {}
    }
    var el = currentEl();
    if (el && el.scrollIntoView) { try { el.scrollIntoView({block:'center'}); } catch(e) {} }
    post();
  }
  window.__ferriteFind = function(cmd, query, caseSensitive, accent) {
    if (!document.body) { total = 0; index = 0; post(); return; }
    if (cmd === 'clear' || !query) {
      ranges = []; total = 0; index = 0;
      try { if (canHi) { CSS.highlights.delete('ferrite-find'); CSS.highlights.delete('ferrite-current'); } } catch(e) {}
      try { if (window.getSelection) window.getSelection().removeAllRanges(); } catch(e) {}
      post(); return;
    }
    if (cmd === 'search') {
      ranges = scan(String(query), !!caseSensitive);
      total = ranges.length; index = total ? 1 : 0;
      if (canHi) { ensureStyle(accent || '#ffc670'); show(); return; }
    }
    if (!canHi && nativeFind) {
      // No highlight API: native stepping carries the visual, the closure
      // carries the count. Wrap-around keeps index/total consistent.
      if (cmd === 'search') { nativeFind(query, !!caseSensitive, false, true, false, false, false); index = total ? 1 : 0; }
      else if (cmd === 'next' && total) { nativeFind(query, !!caseSensitive, false, true, false, false, false); index = index % total + 1; }
      else if (cmd === 'prev' && total) { nativeFind(query, !!caseSensitive, true, true, false, false, false); index = (index + total - 2) % total + 1; }
      post(); return;
    }
    if (cmd === 'next' && total) index = index % total + 1;
    else if (cmd === 'prev' && total) index = (index + total - 2) % total + 1;
    show();
  };
})();"#
}

/// Page-reported JS errors for the status readout (honest subset, the
/// engine console stream is not exposed by wry). Throttled to 1/sec
/// against error-loop spam.
pub fn console_errors_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteConsoleInstalled) return;
  window.__ferriteConsoleInstalled = true;
  var last = 0;
  function report() {
    var now = Date.now();
    if (now - last < 1000) return;
    last = now;
    try { window.ipc.postMessage(JSON.stringify({type:'console_error'})); } catch(e) {}
  }
  window.addEventListener('error', function(){ report(); }, true);
  window.addEventListener('unhandledrejection', function(){ report(); }, true);
})();"#
}

/// Right-click context payload (bubble-phase `contextmenu` listener).
/// Stands in only where the native menu would have shown. Editable fields
/// stay with the native text-edit menu. `href` and `currentSrc` come
// engine-resolved, never page-overridable copies.
// ponytail: main frame only; a page suppressing via capture-phase
// listeners wins over this bubble listener. Full coverage wants the
// per-platform native context hooks, which wry does not surface.
pub fn context_menu_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteCtxInstalled) return;
  window.__ferriteCtxInstalled = true;
  function post(o) { try { window.ipc.postMessage(JSON.stringify(o)); } catch(e) {} }
  function payloadFor(x, y) {
    var t = null;
    try { t = document.elementFromPoint(x, y); } catch(e) {}
    var link = (t && t.closest) ? t.closest('a[href]') : null;
    var img = (t && t.closest) ? t.closest('img') : null;
    var sel = '';
    try { sel = String(window.getSelection ? window.getSelection().toString() : ''); } catch(e) {}
    if (sel.length > 200) sel = sel.slice(0, 200);
    var editable = false;
    try {
      var ae = document.activeElement;
      editable = !!(ae && (ae.tagName === 'INPUT' || ae.tagName === 'TEXTAREA' || ae.isContentEditable));
      editable = editable || !!(t && t.closest && t.closest('input,textarea,[contenteditable]'));
    } catch(e) {}
    return {
      type:'context_menu', x:Math.round(x), y:Math.round(y),
      href: link ? (link.href || '') : '',
      src: img ? (img.currentSrc || img.src || '') : '',
      selection: sel, editable: editable
    };
  }
  document.addEventListener('contextmenu', function(e){
    if (e.defaultPrevented) return;
    var p = payloadFor(e.clientX, e.clientY);
    if (p.editable && !p.href && !p.src && !p.selection) return;
    e.preventDefault();
    post(p);
  }, false);
  // Keyboard path: Shift+F10 / Menu key at the focused element.
  // Best-effort: some platforms deliver F10 to the OS menu bar first.
  document.addEventListener('keydown', function(e){
    var menuKey = e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey);
    if (!menuKey) return;
    var ae = null;
    try { ae = document.activeElement; } catch(err) {}
    if (ae && (ae.tagName === 'INPUT' || ae.tagName === 'TEXTAREA' || ae.isContentEditable)) return;
    e.preventDefault();
    var r = {left:120, top:120, width:0, height:0};
    try { if (ae && ae.getBoundingClientRect) r = ae.getBoundingClientRect(); } catch(err) {}
    post(payloadFor(r.left + (r.width || 0) / 2, r.top + (r.height || 0) / 2));
  }, false);
})();"#
}

/// Nav-commit reporter: posts `{type:'nav', url}` when a top-level document
/// commits (full load + SPA pushState/replaceState + popstate/hashchange).
/// Main-frame gate (`window.self === window.top`) runs in-page because
/// `NavigationStarting` is all-frames: subframe navigations must never mint
/// history visits. Downloads never commit a document, cancelled navigations
/// never commit, redirects commit once (final URL), all correct for free.
/// Page-spoofable like the title channel: the shell validates scheme +
/// length and treats every commit as `"link"`, so the worst case is a junk
/// row, never code execution.
pub fn nav_commit_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteNavInstalled) return;
  window.__ferriteNavInstalled = true;
  if (window.self !== window.top) return;
  function post() {
    try {
      var u = String(location.href || '');
      if (u.length > 2048) u = u.slice(0, 2048);
      window.ipc.postMessage(JSON.stringify({type:'nav', url:u}));
    } catch(e) {}
  }
  var last = '';
  try { last = String(location.href || ''); } catch(e) {}
  function maybe() {
    var cur = '';
    try { cur = String(location.href || ''); } catch(e) {}
    if (cur && cur !== last) { last = cur; post(); }
  }
  document.addEventListener('DOMContentLoaded', function(){ last = ''; maybe(); }, false);
  window.addEventListener('popstate', maybe, false);
  window.addEventListener('hashchange', maybe, false);
  try {
    var ps = history.pushState, rs = history.replaceState;
    history.pushState = function(){ var r = ps.apply(this, arguments); maybe(); return r; };
    history.replaceState = function(){ var r = rs.apply(this, arguments); maybe(); return r; };
  } catch(e) {}
})();"#
}

/// Page-language reporter: posts `{type:'lang', lang}` with the
/// document's `lang` attribute once it exists (main frame). The shell
/// validates the tag and offers translation when it differs from the
/// target. Attribute-only: pages without `lang` simply never offer (no
/// detection API runs unprompted). Bounded (24 chars) like every
/// reporter envelope.
pub fn lang_reporter_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteLangInstalled) return;
  window.__ferriteLangInstalled = true;
  if (window.self !== window.top) return;
  function report() {
    try {
      var l = String(document.documentElement.getAttribute('lang') || '');
      if (l.length > 24) l = l.slice(0, 24);
      window.ipc.postMessage(JSON.stringify({type:'lang', lang:l}));
    } catch(e) {}
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', report, false);
  else report();
})();"#
}

/// Scroll reporter: posts `{type:'scroll', x, y}` with the live offset
/// so session restore re-lands where you were. Throttled (2s, movement
/// only) plus pagehide/hidden flushes; per-scroll posts would be a hot
/// IPC loop. Integer layout px, main frame only; the shell clamps
/// magnitude and never pushes on these posts (the autosave backstop
/// persists them).
pub fn scroll_reporter_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteScrollInstalled) return;
  window.__ferriteScrollInstalled = true;
  if (window.self !== window.top) return;
  var lastX = -1, lastY = -1, timer = 0;
  function send(force) {
    try {
      var x = Math.round(window.scrollX || 0), y = Math.round(window.scrollY || 0);
      if (!force && x === lastX && y === lastY) return;
      lastX = x; lastY = y;
      window.ipc.postMessage(JSON.stringify({type:'scroll', x:x, y:y}));
    } catch(e) {}
  }
  function arm() {
    if (timer) return;
    timer = setTimeout(function(){ timer = 0; send(false); }, 2000);
  }
  window.addEventListener('scroll', arm, {passive:true});
  document.addEventListener('visibilitychange', function(){ if (document.hidden) send(true); }, false);
  window.addEventListener('pagehide', function(){ send(true); }, false);
})();"#
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

/// Lifecycle reporter: unsaved-form and live-call signals for the idle
/// discard exemptions. One `input` event latches form-dirty (Rust clears it
/// on navigation); `RTCPeerConnection` is wrapped to count live peer
/// connections (recount on close/fail).
///
/// ponytail: main-frame only, inputs inside closed shadow roots, workers,
/// and RTC created before this script runs are invisible. Full coverage
/// wants per-platform hooks that pinned wry does not surface.
pub fn lifecycle_reporter_js() -> &'static str {
    r#"(function(){
  if (window.__ferriteLifecycleInstalled) return;
  window.__ferriteLifecycleInstalled = true;
  function post(o) { try { window.ipc.postMessage(JSON.stringify(o)); } catch(e) {} }
  var formDirty = false;
  document.addEventListener('input', function(){
    if (!formDirty) { formDirty = true; post({type:'form_dirty'}); }
  }, true);
  var OrigRTC = window.RTCPeerConnection;
  if (OrigRTC) {
    var live = 0;
    var last = false;
    function recount() {
      var active = live > 0;
      if (active !== last) { last = active; post({type:'live_call', active:active}); }
    }
    window.RTCPeerConnection = function(){
      var args = Array.prototype.slice.call(arguments);
      var pc = Reflect.construct(OrigRTC, args);
      live++; recount();
      pc.addEventListener('connectionstatechange', function(){
        var s = pc.connectionState;
        if (s === 'closed' || s === 'failed') { live = Math.max(0, live - 1); recount(); }
      });
      return pc;
    };
    window.RTCPeerConnection.prototype = OrigRTC.prototype;
  }
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
        // Internal pages must go through the workaround form (see
        // `workaround_url`); creation already does this inside wry.
        self.inner
            .load_url(&workaround_url(url))
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

    fn cookies_for_url(&self, url: &str) -> TabResult<Vec<(String, String)>> {
        self.inner
            .cookies_for_url(url)
            .map(|cs| {
                cs.iter()
                    .map(|c| (c.name().to_owned(), c.value().to_owned()))
                    .collect()
            })
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn cookies_all(&self) -> TabResult<Vec<wry::cookie::Cookie<'static>>> {
        self.inner
            .cookies()
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn delete_cookie(&self, cookie: &wry::cookie::Cookie<'_>) -> TabResult<()> {
        self.inner
            .delete_cookie(cookie)
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn open_devtools(&self) -> TabResult<()> {
        self.inner.open_devtools();
        Ok(())
    }

    fn clear_browsing_data(&self) -> TabResult<()> {
        self.inner
            .clear_all_browsing_data()
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn print(&self) -> TabResult<()> {
        self.inner
            .print()
            .map_err(|source| TabError::WebViewOperationFailed { source })
    }

    fn set_zoom(&self, factor: f64) -> TabResult<()> {
        self.inner
            .zoom(factor)
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
    devtools_opened: Mutex<bool>,
    cleared: Mutex<bool>,
    mock_cookies: Mutex<Vec<(String, String, String)>>,
    deleted_cookies: Mutex<Vec<String>>,
    printed: Mutex<bool>,
    zoom: Mutex<Option<f64>>,
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

    pub fn devtools_opened(&self) -> bool {
        *self
            .devtools_opened
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn printed(&self) -> bool {
        *self.printed.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn was_cleared(&self) -> bool {
        *self.cleared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Seed mock jar entries as (name, value, domain).
    pub fn set_mock_cookies(&self, cookies: Vec<(String, String, String)>) {
        if let Ok(mut guard) = self.mock_cookies.lock() {
            *guard = cookies;
        }
    }

    pub fn deleted_cookie_names(&self) -> Vec<String> {
        self.deleted_cookies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn last_zoom(&self) -> Option<f64> {
        *self.zoom.lock().unwrap_or_else(|e| e.into_inner())
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

    fn cookies_for_url(&self, _url: &str) -> TabResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    fn cookies_all(&self) -> TabResult<Vec<wry::cookie::Cookie<'static>>> {
        let guard = self.mock_cookies.lock().unwrap_or_else(|e| e.into_inner());
        Ok(guard
            .iter()
            .map(|(n, v, d)| {
                let mut c = wry::cookie::Cookie::new(n.clone(), v.clone());
                c.set_domain(d.clone());
                c.set_path("/".to_owned());
                c.into_owned()
            })
            .collect())
    }

    fn delete_cookie(&self, cookie: &wry::cookie::Cookie<'_>) -> TabResult<()> {
        if let Ok(mut guard) = self.deleted_cookies.lock() {
            guard.push(cookie.name().to_owned());
        }
        if let Ok(mut guard) = self.mock_cookies.lock() {
            guard.retain(|(n, _, _)| n != cookie.name());
        }
        Ok(())
    }

    fn open_devtools(&self) -> TabResult<()> {
        if let Ok(mut guard) = self.devtools_opened.lock() {
            *guard = true;
        }
        Ok(())
    }

    fn clear_browsing_data(&self) -> TabResult<()> {
        if let Ok(mut guard) = self.cleared.lock() {
            *guard = true;
        }
        Ok(())
    }

    fn print(&self) -> TabResult<()> {
        if let Ok(mut guard) = self.printed.lock() {
            *guard = true;
        }
        Ok(())
    }

    fn set_zoom(&self, factor: f64) -> TabResult<()> {
        if let Ok(mut guard) = self.zoom.lock() {
            *guard = Some(factor);
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

    /// Incognito creation (non-persistent engine store: Win InPrivate /
    /// macOS nonPersistent DataStore / Linux ephemeral context, all native,
    /// all per wry's pinned version). Default falls back to normal creation
    /// so mocks and headless consumers never depend on engine modes.
    fn create_private_with_id(
        &self,
        window: &winit::window::Window,
        url: &str,
        bounds: Option<wry::Rect>,
        tab_id: uuid::Uuid,
    ) -> TabResult<Rc<dyn WebViewHandle>> {
        self.create_with_id(window, url, bounds, tab_id)
    }

    /// Live-update the shared privileged-page theme block. Default is a
    /// no-op so mocks and headless consumers never depend on theming;
    /// `WryFactory` shares it across webviews for future navigations.
    fn set_theme_css(&self, _css: String) {}
}

/// Read the shared theme block for one serve (poison-tolerant). The custom
/// protocol closure calls this per request so pack switches reach reloads
/// and future navigations in already-open webviews; snapshotting once at
/// webview creation pinned every page to its birth pack (a reload reverted
/// to the default theme). Pure read, no bind, no side effects.
fn current_shared_css(shared: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
    shared
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|e| e.into_inner().clone())
}

/// Script that swaps the live theme on an already-loaded privileged page
/// without reload: upserts `<style id="ferrite-theme-live">` as the last
/// head child so it wins the cascade over the serve-time block, preserving
/// DOM, scroll, form, and find state. Callers must restrict this to
/// `ferrite://` documents only. Never run it in untrusted web content.
/// The CSS travels JSON-quoted (never interpolated raw), so a hostile pack
/// name or token cannot break out of the string context.
pub fn live_theme_script(css: &str) -> String {
    let quoted = serde_json::to_string(css).unwrap_or_else(|_| "\"\"".to_owned());
    format!(
        "(function(){{try{{var css={quoted};var el=document.getElementById('ferrite-theme-live');if(!el){{el=document.createElement('style');el.id='ferrite-theme-live';(document.head||document.documentElement).appendChild(el);}}el.textContent=css;}}catch(e){{}}}})();"
    )
}

/// An engine download denied for ask-every-time handling: the loop
/// shows the save dialog and refetches (GET + tab cookies). POST-body
/// downloads cannot survive this. The started event carries no method
/// or body, so refetching would save the wrong content; that ceiling
/// stays documented, not silently papered over.
#[derive(Debug, Clone)]
pub struct DeniedDownload {
    pub tab_id: Uuid,
    pub url: String,
    pub filename: String,
}

/// Engine PDF-link interception, vetoed in the navigation handler:
/// which tab clicked which document URL. The loop fetches/reads and
/// opens the isolated viewer; the handler itself can only veto.
#[derive(Debug, Clone)]
pub struct PdfIntercept {
    pub tab_id: Uuid,
    pub url: String,
}

/// Every initialization script with its frame scope, in injection order.
/// Single list so the one subframe entry stays reviewable (and tested):
/// the field scanner MUST run inside cross-origin iframes (Stripe
/// Elements et al) or those fields are silently invisible. Everything
/// else is top-page concern (menus, lifecycle, cosmetic, risk reporters)
/// and stays main-frame-only. Windows injects everywhere regardless;
/// the flag matters on Mac/Linux. Fill delivery stays main-frame
/// `evaluate_script` (no wry frame API, cross-origin iframe fills are
/// SOP-blocked regardless, closed-shadow Payment Elements by design).
fn init_scripts() -> Vec<(String, bool)> {
    vec![
        (audio_listener_js().to_owned(), true),
        (scroll_reporter_js().to_owned(), true),
        (lang_reporter_js().to_owned(), true),
        (lifecycle_reporter_js().to_owned(), true),
        (nav_commit_js().to_owned(), true),
        (crate::pip::pip_report_js().to_owned(), true),
        (favicon_report_js().to_owned(), true),
        (link_hover_js().to_owned(), true),
        (find_js().to_owned(), true),
        (context_menu_js().to_owned(), true),
        (console_errors_js().to_owned(), true),
        (crate::translate::translate_js().to_owned(), true),
        (crate::media::media_monitor_js().to_owned(), true),
        (ferrite_privacy::js::param_stripping_js(), true),
        (ferrite_privacy::js::deamp_js(), true),
        (ferrite_privacy::js::title_sanitize_js(), true),
        (ferrite_privacy::js::fingerprint_spoof_js(), true),
        (ferrite_privacy::js::cosmetic_query_js(), true),
        (ferrite_privacy::js::form_risk_js(), true),
        (ferrite_privacy::spellcheck_js(), true),
        (ferrite_credentials::scanner::scanner_js().to_owned(), false),
    ]
}

/// Download destination → (download id, originating tab UUID string).
/// Shared per webview creation for correlating started/completed hooks.
type PendingDownloads =
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (String, Option<String>)>>>;

#[derive(Clone)]
pub struct WryFactory {
    ipc_sender: tokio::sync::mpsc::Sender<IpcMessage>,
    ipc_limits: crate::ipc::SharedLimitsHandle,
    assets: std::sync::Arc<crate::protocol::AssetTable>,
    /// Shell theme CSS block, substituted for the marker in privileged
    /// pages at serve time. Shared across webviews so a pack switch applies
    /// to future navigations without recreating the factory. Empty until
    /// the shell provides it. Poison-tolerant: a poisoned lock falls back
    /// to empty rather than panicking the loop.
    theme_css: std::sync::Arc<std::sync::Mutex<String>>,
    proxy: Option<(String, u16)>,
    /// Session-partitioned proxy for private tabs: separate loopback port backed
    /// by its own `ConnPool`, so a private tab never reuses a warm keep-alive
    /// socket (or its TLS session) from a normal tab. Static rule data
    /// (filter lists) stays shared read-only in the `DecisionHook`; only the
    /// connection state partitions. `None` falls back to `proxy`.
    private_proxy: Option<(String, u16)>,
    archive: Option<std::sync::Arc<ferrite_archive::Archive>>,
    /// Engine profile root: the ONE user-data dir for this process, so the
    /// HTTP disk cache (engine-owned, unencrypted) lives in a predictable,
    /// backup/wipe-friendly place next to Ferrite's own stores instead of a
    /// platform default. `None` = engine default location.
    data_dir: Option<std::path::PathBuf>,
    /// Shared engine profile behind the data dir: one cache/cookie jar for
    /// all normal tabs (lazily built so `new()` stays infallible). Kept for
    /// process lifetime, dropping it mid-session loses custom-protocol
    /// handling on macOS (wry's documented warning). RefCell-guarded
    /// because the builder needs `&mut` at creation; creation is
    /// synchronous, so no await ever runs under this borrow.
    /// Single-threaded by architecture (the factory lives in an `Rc`,
    /// never crosses threads): `Rc`, not `Arc`, because
    /// `wry::WebContext` is `!Send` on some platforms and an
    /// `Arc<Mutex<...>>` around `!Send` data shares nothing.
    shared_context: std::rc::Rc<std::cell::RefCell<Option<wry::WebContext>>>,
    /// Download sink plus pending map, shared per webview creation.
    downloads: Option<crate::downloads::DownloadManager>,
    /// Download destination override, shared with the shell: Guest mode
    /// points this at a temp dir so engine downloads never land in the
    /// user's Downloads. Read per download (rare path); `None` keeps the
    /// default directory.
    download_dir_override: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    /// Private-tab spill dir: when set, downloads from `is_private` webviews
    /// land here even in a persistent process (never `~/Downloads`, never the
    /// profile dir). Set once at boot to `temp/ferrite/private-<pid>/`.
    private_download_dir: Option<std::path::PathBuf>,
    /// Destination path → (download id, originating tab UUID string),
    /// correlating started/completed hooks (wry passes no id through).
    /// The tab half powers the sleep exemption; absent when unattributed.
    pending_downloads: PendingDownloads,
    /// Ask-every-time divert: http(s) downloads denied in-handler and
    /// reported here for a dialog + refetch on the loop side. Absent =
    /// legacy silent-to-downloads behavior. wry's handler is synchronous
    /// (COM STA thread) so blocking for a dialog there is a deadlock,
    /// deny-and-refetch is the only safe shape without upstream API.
    download_ask: Option<(
        std::sync::mpsc::Sender<DeniedDownload>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    )>,
    /// PDF intercept: engine link clicks to `.pdf` URLs veto in-handler
    /// and report here for fetch-then-view on the loop side. Absent =
    /// legacy engine behavior (the engine's own viewer, outside Ferrite
    /// guarantees). Same deny-and-report shape as `download_ask`: the
    /// handler cannot open a viewer itself.
    pdf_intercept: Option<(
        std::sync::mpsc::Sender<PdfIntercept>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    )>,
    /// Wakes the shell event loop when a message lands (the loop otherwise
    /// only wakes on window events and its heartbeat timer). Optional so
    /// tests and headless consumers never depend on a loop.
    waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    /// Permission broker (fail-closed when absent: every request denies).
    /// Origins mirror latest navigation per tab; the shell owns grants.
    permission_store: Option<std::sync::Arc<crate::permissions::PermissionStore>>,
    permission_origins:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<uuid::Uuid, String>>>,
    /// Script-dialog interception sink (Windows WebView2 only). Absent =
    /// engine-default dialogs. The sink is shared across webviews: frame
    /// ids pair responses, and suppression is origin-scoped per session.
    #[cfg(target_os = "windows")]
    dialog_sink: Option<crate::dialog_win::DialogSink>,
}

impl std::fmt::Debug for WryFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self
            .theme_css
            .lock()
            .map(|g| g.len())
            .unwrap_or_else(|e| e.into_inner().len());
        f.debug_struct("WryFactory")
            .field("theme_css_len", &len)
            .field("proxy", &self.proxy)
            .field("has_waker", &self.waker.is_some())
            .finish_non_exhaustive()
    }
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
            theme_css: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            proxy: None,
            private_proxy: None,
            archive: None,
            data_dir: None,
            shared_context: std::rc::Rc::new(std::cell::RefCell::new(None)),
            downloads: None,
            download_dir_override: std::sync::Arc::new(std::sync::Mutex::new(None)),
            private_download_dir: None,
            pending_downloads: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            waker: None,
            download_ask: None,
            pdf_intercept: None,
            permission_store: None,
            permission_origins: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            #[cfg(target_os = "windows")]
            dialog_sink: None,
        }
    }

    /// Wake the shell event loop from the wry IPC thread when a message is
    /// accepted. Without this the shell only learns about messages on window
    /// events or its heartbeat tick (≤500ms stale omnibox suggestions).
    #[must_use]
    pub fn with_waker(mut self, waker: impl Fn() + Send + Sync + 'static) -> Self {
        self.waker = Some(std::sync::Arc::new(waker));
        self
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

    /// Session-partitioned proxy endpoint for private webviews (see field
    /// docs). Picked per webview at creation; no live move between proxies.
    #[must_use]
    pub fn with_private_proxy(mut self, host: &str, port: u16) -> Self {
        self.private_proxy = Some((host.to_owned(), port));
        self
    }

    /// Private-tab spill dir (see field docs). Created by the shell.
    #[must_use]
    pub fn with_private_download_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.private_download_dir = Some(dir);
        self
    }

    /// Share a download-directory override slot with the shell (guest
    /// temp dir). The handler reads it per download, so flipping it later
    /// takes effect without rebuilding webviews.
    #[must_use]
    pub fn with_download_dir_override(
        mut self,
        slot: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    ) -> Self {
        self.download_dir_override = slot;
        self
    }

    /// Pin the engine profile (cache included) to `dir`. The shell creates
    /// the dir; a missing/unwritable dir fails webview creation loudly at
    /// build time (never a silent fallback to another profile).
    #[must_use]
    pub fn with_data_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.data_dir = Some(dir);
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
    /// `crate::protocol::THEME_MARKER`). Shared with already-created
    /// webviews: the serve path snapshots it per request, so updating here
    /// re-themes future navigations without touching live documents.
    #[must_use]
    pub fn with_theme_css(self, css: impl Into<String>) -> Self {
        self.set_theme_css(css.into());
        self
    }

    /// Live-update the shared theme block (pack switch path). Infallible by
    /// design: lock poisoning stores through the poison instead of
    /// panicking the event loop. Open documents keep their DOM and are
    /// re-themed separately via [`live_theme_script`]; this covers future
    /// loads only, so no tab loses scroll, form, or find state here.
    pub fn set_theme_css(&self, css: String) {
        match self.theme_css.lock() {
            Ok(mut guard) => *guard = css,
            Err(poison) => *poison.into_inner() = css,
        }
    }

    /// Snapshot the shared theme block for the serve path. Poison-tolerant:
    /// falls back to the poisoned contents, never panics.
    pub fn theme_css_snapshot(&self) -> String {
        self.theme_css
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
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

    /// Ask-every-time divert: when the flag is set, http(s) engine
    /// downloads deny in-handler and report here (the loop shows a save
    /// dialog and refetches). Non-http(s) URLs always take the silent
    /// engine path, blob:/data: can't survive a refetch.
    #[must_use]
    pub fn with_download_ask(
        mut self,
        tx: std::sync::mpsc::Sender<DeniedDownload>,
        enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.download_ask = Some((tx, enabled));
        self
    }

    /// Route engine PDF-link navigations to the shell viewer: veto the
    /// engine navigation (its built-in viewer is outside Ferrite
    /// guarantees) and report (tab, url) for fetch/read-then-view.
    /// The viewer itself (`ferrite://pdf/`) always passes through.
    #[must_use]
    pub fn with_pdf_intercept(
        mut self,
        tx: std::sync::mpsc::Sender<PdfIntercept>,
        enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.pdf_intercept = Some((tx, enabled));
        self
    }

    /// Permission broker: sync fail-closed decisions with a pending queue
    /// for the omnibox chip. Without this every request denies silently.
    #[must_use]
    pub fn with_permissions(
        mut self,
        store: std::sync::Arc<crate::permissions::PermissionStore>,
    ) -> Self {
        self.permission_store = Some(store);
        self
    }

    /// Script-dialog interception (Windows): route alert/confirm/prompt/
    /// beforeunload through the shell's chrome overlay instead of engine
    /// defaults. The sink carries its own waker (cloned here), shared
    /// suppression set, and deferral stash, one per factory, all tabs.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub fn with_dialogs(
        mut self,
        tx: std::sync::mpsc::Sender<crate::dialog::DialogOpened>,
    ) -> Self {
        let sink = crate::dialog_win::DialogSink::new(tx, self.waker.clone());
        self.dialog_sink = Some(sink);
        self
    }

    /// Shared interception sink for the shell loop (completions,
    /// suppression updates). `None` without [`Self::with_dialogs`].
    #[cfg(target_os = "windows")]
    pub fn dialog_sink(&self) -> Option<crate::dialog_win::DialogSink> {
        self.dialog_sink.clone()
    }

    #[cfg(test)]
    pub fn has_asset(&self, name: &str) -> bool {
        self.assets.contains_key(name)
    }

    /// Shared creation path. `private` selects the engine's native
    /// non-persistent store (incognito): nothing written to the profile's
    /// disk cache, and the shared context is bypassed entirely.
    #[allow(clippy::collapsible_if)]
    fn create_inner(
        &self,
        window: &winit::window::Window,
        url: &str,
        bounds: Option<wry::Rect>,
        tab_id: Uuid,
        private: bool,
    ) -> TabResult<Rc<dyn WebViewHandle>> {
        let tab_hint = url.to_owned();
        let sender = self.ipc_sender.clone();
        let limits = self.ipc_limits.clone();
        let waker = self.waker.clone();
        let ipc_tab_id = tab_id;
        let dl_manager = self.downloads.clone();
        let dl_pending = std::sync::Arc::clone(&self.pending_downloads);
        let dl_dir = std::sync::Arc::clone(&self.download_dir_override);
        let dl_private_dir = self.private_download_dir.clone();
        let is_private_webview = private;
        let pdf_intercept = self.pdf_intercept.clone();

        let assets = self.assets.clone();
        // Live handle, not a snapshot: the serve path reads the current
        // block per request, so a pack switch re-themes reloads and future
        // navigations in already-open webviews (a frozen creation-time copy
        // is exactly how a reload reverted to the default pack).
        let theme_css_live = std::sync::Arc::clone(&self.theme_css);
        let archive = self.archive.clone();
        // Profile selection, fixed for the webview's lifetime (same class
        // of constraint as the proxy port: set at creation, no live move
        // between persistent and incognito stores, close + reopen instead).
        // The guard lives until build_as_child consumes the builder below;
        // creation is synchronous, so nothing awaits under this borrow.
        // No cache-size API exists in pinned wry: the engine sizes its own
        // disk cache from free disk space. A max-size setting would be a
        // fake control, so there isn't one (honest gap, not a stub).
        // Partitioning likewise needs no action: third-party storage
        // partitioning (cache included) ships default-on in the engines we
        // wrap, setting it via Windows browser-args would clobber wry's
        // own `--proxy-server` default, so hands off.
        let mut shared = self.shared_context.try_borrow_mut().ok();
        let ctx: Option<&mut wry::WebContext> = match (self.data_dir.clone(), shared.as_mut()) {
            (Some(dir), Some(guard)) => {
                let slot = guard.get_or_insert_with(|| wry::WebContext::new(Some(dir)));
                Some(slot)
            }
            _ => None,
        };
        #[allow(clippy::collapsible_if)]
        let mut builder = match ctx {
            Some(c) => wry::WebViewBuilder::new_with_web_context(c).with_url(url),
            None => wry::WebViewBuilder::new().with_url(url),
        };
        if private {
            builder = builder.with_incognito(true);
        }
        builder = builder
            // Inspector affordances on (the launch layer calls
            // open_devtools explicitly, nothing auto-opens, no port).
            .with_devtools(true)
            // General autofill off: Ferrite's own scanner + vault own
            // filling, so the engine must not compete on addresses/cards.
            // Password autofill is EXCLUDED from this flag by wry's own
            // docs ("does not disable password or credit card autofill")
            // and no wry API reaches WebView2's IsPasswordAutosaveEnabled
            // (or the WKWebKit/GTK equivalents), a native save prompt may
            // still appear; Ferrite's vault remains the store of record.
            // ponytail: recheck each wry upgrade for a password setting.
            .with_general_autofill_enabled(false)
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
                                        let banner = format!("<style>{}</style><div style=\"background:var(--f-surface-2);color:var(--f-fg);padding:8px;text-align:center;font-family:var(--f-font-ui)\">Archive snapshot from {}</div>", crate::protocol::apply_theme_css(crate::protocol::THEME_MARKER, &current_shared_css(&theme_css_live)), snap.timestamp);
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
                        id,
                        req,
                        &assets,
                        &current_shared_css(&theme_css_live),
                    )
                },
            );
        // Init scripts from the single table: frame scope reviewable and
        // tested in `init_scripts`, not scattered down a builder chain.
        for (js, main_only) in init_scripts() {
            builder = builder.with_initialization_script_for_main_only(js, main_only);
        }
        builder = builder
            .with_ipc_handler(move |request: wry::http::Request<String>| {
                let payload = request.body().clone();
                let msg = IpcMessage {
                    tab_id: ipc_tab_id,
                    payload,
                };
                // Bounded, non-blocking, rate-limited: oversize/full/burst
                // messages are dropped with a warning instead of growing
                // memory without bound. Accepted messages wake the shell so
                // the drain does not wait for the heartbeat tick.
                if crate::ipc::IpcBridge::try_send_via(&sender, &limits, msg) {
                    if let Some(wake) = waker.as_ref() {
                        wake();
                    }
                } else {
                    warn!(%tab_hint, "ipc message dropped (bound)");
                }
            })
            .with_navigation_handler({
                let perm_origins = std::sync::Arc::clone(&self.permission_origins);
                move |uri: String| {
                    debug!(%uri, "navigation request");
                    // Latest-navigation mirror for the permission handler
                    // (wry passes no origin, see with_permission_handler).
                    if let Ok(mut origins) = perm_origins.lock() {
                        origins.insert(ipc_tab_id, uri.clone());
                    }
                    // PDF links leave the engine: veto here and hand the
                    // URL to the shell viewer (fetch/read-then-view). The
                    // viewer itself always passes: vetoing it would strand
                    // the very page doing the viewing.
                    match crate::pdf::classify_pdf_url(&uri) {
                        crate::pdf::PdfNav::Viewer | crate::pdf::PdfNav::No => true,
                        crate::pdf::PdfNav::Remote | crate::pdf::PdfNav::Local => {
                            match &pdf_intercept {
                                Some((tx, enabled))
                                    if enabled.load(std::sync::atomic::Ordering::Relaxed) =>
                                {
                                    let _ = tx.send(PdfIntercept {
                                        tab_id: ipc_tab_id,
                                        url: uri,
                                    });
                                    false
                                }
                                _ => true,
                            }
                        }
                    }
                }
            })
            .with_permission_handler({
                let perm_store = self.permission_store.clone();
                let perm_origins = std::sync::Arc::clone(&self.permission_origins);
                let perm_waker = self.waker.clone();
                move |kind: wry::PermissionKind| {
                    let kind = kind.to_string();
                    let origin = perm_origins
                        .lock()
                        .ok()
                        .and_then(|o| o.get(&ipc_tab_id).cloned())
                        .and_then(|uri| crate::permissions::origin_of(&uri));
                    let allowed = match (perm_store.as_ref(), origin) {
                        (Some(store), Some(origin)) => {
                            let ok =
                                store.decide(ipc_tab_id, &origin, &kind, crate::tab::now_millis());
                            if !ok {
                                debug!(%origin, %kind, "permission denied + queued");
                            }
                            if let Some(wake) = perm_waker.as_ref() {
                                wake();
                            }
                            ok
                        }
                        // No broker, or a non-http(s) page asking: deny
                        // silently. Never default, Default continues the
                        // platform flow outside the broker.
                        _ => false,
                    };
                    if allowed {
                        wry::PermissionResponse::Allow
                    } else {
                        wry::PermissionResponse::Deny
                    }
                }
            })
            .with_download_started_handler({
                let dl_manager = dl_manager.clone();
                let dl_pending = std::sync::Arc::clone(&dl_pending);
                let dl_dir = std::sync::Arc::clone(&dl_dir);
                let dl_private_dir = dl_private_dir.clone();
                let dl_ask = self.download_ask.clone();
                let dl_waker = self.waker.clone();
                let dl_origins = std::sync::Arc::clone(&self.permission_origins);
                move |url: String, path: &mut std::path::PathBuf| {
                    // Mixed-content download blocking: an https page must
                    // never trigger an http download. The started hook
                    // carries no referrer, so the tab's latest origin is
                    // the page signal (best available under wry).
                    let page_origin: Option<String> = dl_origins
                        .lock()
                        .ok()
                        .and_then(|m| m.get(&ipc_tab_id).cloned());
                    if crate::downloads::should_block_mixed_content(page_origin.as_deref(), &url) {
                        tracing::warn!(url, "mixed-content download blocked");
                        return false;
                    }
                    // Ask-every-time divert (http(s) only): deny the engine
                    // download and let the loop dialog + refetch. Anything
                    // else, blob:, data:, file:, keeps the silent engine
                    // path, since a GET refetch could never reproduce it.
                    if let Some((tx, flag)) = dl_ask.as_ref()
                        && flag.load(std::sync::atomic::Ordering::Relaxed)
                        && {
                            let l = url.trim().to_lowercase();
                            l.starts_with("http://") || l.starts_with("https://")
                        }
                    {
                        let filename = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("download")
                            .to_owned();
                        let _ = tx.send(DeniedDownload {
                            tab_id: ipc_tab_id,
                            url,
                            filename,
                        });
                        if let Some(wake) = dl_waker.as_ref() {
                            wake();
                        }
                        return false;
                    }
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
                    // Conflict-free destination: never overwrite an
                    // existing file (engine has no auto-suffix). Precedence:
                    // private-tab spill dir first (never ~/Downloads), then
                    // the shell override slot (guest temp dir), then default.
                    let base = if is_private_webview && let Some(ref priv_dir) = dl_private_dir {
                        priv_dir.clone()
                    } else {
                        dl_dir
                            .lock()
                            .ok()
                            .and_then(|g| g.clone())
                            .unwrap_or_else(crate::downloads::default_download_dir)
                    };
                    let dest = crate::downloads::decide_destination(&base, &filename);
                    let id = manager.start_with_tab(
                        &url,
                        &filename,
                        Some(&dest),
                        Some(ipc_tab_id.to_string()),
                    );
                    if let Ok(mut pending) = dl_pending.lock() {
                        pending.insert(
                            dest.to_string_lossy().to_string(),
                            (id, Some(ipc_tab_id.to_string())),
                        );
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
                    let (key, filename, dest) = match &path {
                        Some(p) => (
                            p.to_string_lossy().to_string(),
                            p.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("download")
                                .to_owned(),
                            Some(p.clone()),
                        ),
                        None => (url.clone(), "download".to_owned(), None),
                    };
                    let id = dl_pending.lock().ok().and_then(|mut m| m.remove(&key));
                    let dest_ref = dest.as_deref();
                    match (id, success) {
                        (Some((id, tab)), true) => {
                            manager.complete_with_tab(&id, &url, &filename, dest_ref, tab);
                            debug!(url, filename, "download completed");
                        }
                        (Some((id, tab)), false) => {
                            manager.fail_with_tab(&id, &url, &filename, dest_ref, tab);
                            warn!(url, filename, "download failed");
                        }
                        (None, _) => {
                            // macOS reports no path: track by URL so the
                            // event is still visible.
                            let id = manager.start_with_path(&url, &filename, dest_ref);
                            if success {
                                manager.complete_with_path(&id, &url, &filename, dest_ref);
                            } else {
                                manager.fail_with_path(&id, &url, &filename, dest_ref);
                            }
                        }
                    }
                }
            });

        if let Some(rect) = bounds {
            builder = builder.with_bounds(rect);
        }

        // Session-partitioned routing: private webviews use the isolated
        // loopback port when present (own ConnPool, no warm-socket reuse
        // across modes). Falls back to the shared proxy, never direct.
        let proxy_choice = if private {
            self.private_proxy.clone().or_else(|| self.proxy.clone())
        } else {
            self.proxy.clone()
        };
        if let Some((ref host, port)) = proxy_choice {
            builder = builder.with_proxy_config(wry::ProxyConfig::Http(wry::ProxyEndpoint {
                host: host.clone(),
                port: port.to_string(),
            }));
            debug!(%host, port, private, "webview routed through ferrite proxy");
        }

        #[cfg(target_os = "linux")]
        if std::env::var("WRY_WAYLAND").is_ok() {
            #[cfg(feature = "gtk")]
            {
                use wry::WebViewBuilderExtUnix;
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

        // Script-dialog interception (Windows only): engine defaults off,
        // every dialog routed to the shell overlay. Failure here is
        // non-fatal, that webview keeps engine defaults, logged once.
        #[cfg(target_os = "windows")]
        if let Some(sink) = self.dialog_sink.as_ref()
            && let Err(e) = crate::dialog_win::attach_script_dialogs(&webview, tab_id, sink)
        {
            debug!(tab_id = %tab_id, error = %e, "script dialog interception unavailable");
        }

        Ok(Rc::new(WryHandle::new(webview)) as Rc<dyn WebViewHandle>)
    }
}

impl WebViewFactory for WryFactory {
    fn set_theme_css(&self, css: String) {
        WryFactory::set_theme_css(self, css);
    }

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
        self.create_inner(window, url, bounds, tab_id, false)
    }

    fn create_private_with_id(
        &self,
        window: &winit::window::Window,
        url: &str,
        bounds: Option<wry::Rect>,
        tab_id: Uuid,
    ) -> TabResult<Rc<dyn WebViewHandle>> {
        self.create_inner(window, url, bounds, tab_id, true)
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
    fn init_scripts_scanner_alone_runs_subframe() {
        let scripts = init_scripts();
        // 21 scripts, each non-empty, each registered exactly once.
        assert_eq!(scripts.len(), 21);
        assert!(scripts.iter().all(|(js, _)| !js.is_empty()));
        let subframes: Vec<&String> = scripts
            .iter()
            .filter(|(_, main_only)| !main_only)
            .map(|(js, _)| js)
            .collect();
        // Exactly one subframe entry: the field scanner (payment/login
        // fields inside cross-origin iframes must be visible).
        assert_eq!(subframes.len(), 1);
        assert!(subframes[0].contains("__ferriteScannerInstalled"));
        assert!(
            scripts
                .iter()
                .any(|(js, main_only)| *main_only && js.contains("__ferriteTranslate")),
            "translate engine injected main-frame"
        );
        assert!(
            scripts
                .iter()
                .any(|(js, main_only)| *main_only && js.contains("__ferriteMediaInstalled")),
            "media monitor injected main-frame"
        );
        assert!(
            scripts
                .iter()
                .any(|(js, main_only)| *main_only && js.contains("__ferriteLangInstalled")),
            "language reporter injected main-frame"
        );
        assert!(
            scripts
                .iter()
                .any(|(js, main_only)| *main_only && js.contains("__ferriteScrollInstalled")),
            "scroll reporter injected main-frame"
        );
        // Reporter envelope stays bounded (24-char cap like its peers).
        let lang = lang_reporter_js();
        assert!(lang.contains("slice(0, 24)"));
        assert!(lang.contains("window.self !== window.top"));
    }

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
    fn mock_handle_records_devtools_open() {
        let h = MockHandle::default();
        assert!(!h.devtools_opened());
        h.open_devtools().unwrap();
        assert!(h.devtools_opened());
    }

    #[test]
    fn mock_handle_records_print() {
        let h = MockHandle::default();
        assert!(!h.printed());
        h.print().unwrap();
        assert!(h.printed());
    }

    #[test]
    fn mock_jar_round_trips_and_scoped_delete() {
        let h = MockHandle::default();
        h.set_mock_cookies(vec![
            ("sid".to_owned(), "abc".to_owned(), "example.com".to_owned()),
            ("prefs".to_owned(), "1".to_owned(), "example.com".to_owned()),
        ]);
        let all = h.cookies_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name(), "sid");
        assert_eq!(all[0].domain(), Some("example.com"));
        // Scoped delete removes only the named cookie and records it.
        h.delete_cookie(&all[0]).unwrap();
        assert_eq!(h.deleted_cookie_names(), vec!["sid".to_owned()]);
        let rest = h.cookies_all().unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].name(), "prefs");
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

    #[test]
    fn workaround_url_maps_internal_pages_to_wry_form() {
        // Mirrors wry's creation-time custom-protocol workaround so
        // post-creation navigations don't die on unknown scheme.
        assert_eq!(
            workaround_url("ferrite://downloads"),
            "http://ferrite.downloads"
        );
        assert_eq!(
            workaround_url("ferrite://archive/view?url=https%3A%2F%2Fx.test"),
            "http://ferrite.archive/view?url=https%3A%2F%2Fx.test"
        );
        // Only the leading prefix is translated, never query content.
        assert_eq!(
            workaround_url("https://example.com/?q=ferrite://x"),
            "https://example.com/?q=ferrite://x"
        );
        assert_eq!(workaround_url("https://example.com"), "https://example.com");
    }

    #[test]
    fn canonical_engine_url_folds_workaround_form_back() {
        // ponytail: one runnable check for the branchy helper.
        // Exact round-trip with workaround_url, case preserved in path/query.
        assert_eq!(
            canonical_engine_url("http://ferrite.welcome/"),
            "ferrite://welcome"
        );
        assert_eq!(
            canonical_engine_url("http://ferrite.welcome"),
            "ferrite://welcome"
        );
        assert_eq!(
            canonical_engine_url("http://ferrite.share/?u=HTTPS%3A%2F%2Fx.test&t=Hi+There"),
            "ferrite://share/?u=HTTPS%3A%2F%2Fx.test&t=Hi+There"
        );
        assert_eq!(
            canonical_engine_url(workaround_url("ferrite://settings").as_str()),
            "ferrite://settings"
        );
        // Non-internal URLs pass through untouched.
        assert_eq!(
            canonical_engine_url("https://example.com"),
            "https://example.com"
        );
        assert_eq!(
            canonical_engine_url("http://ferrite.com/"),
            "http://ferrite.com/"
        );
        assert_eq!(
            canonical_engine_url("http://ferrite.welcome.evil.com/"),
            "http://ferrite.welcome.evil.com/"
        );
        assert_eq!(
            canonical_engine_url("http://ferrite.welcome:8080/"),
            "http://ferrite.welcome:8080/"
        );
        assert_eq!(
            canonical_engine_url("ferrite://welcome"),
            "ferrite://welcome"
        );
        assert_eq!(canonical_engine_url("not a url"), "not a url");
    }

    #[test]
    fn waker_fires_and_survives_clone() {
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&fired);
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let bridge = crate::ipc::IpcBridge::new();
        let f = WryFactory::new(tx, bridge.limiter()).with_waker(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let g = f.clone();
        let dbg = format!("{g:?}");
        assert!(dbg.contains("has_waker: true"), "{dbg}");
        // Invoke exactly the way the IPC handler does.
        g.waker.as_ref().expect("waker stored")();
        assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn link_hover_reports_real_href_never_window_status() {
        let js = link_hover_js();
        // Resolves the engine-resolved href attribute on hover...
        assert!(js.contains("closest('a[href]')"), "real attribute");
        assert!(js.contains("link_hover"), "hover type");
        assert!(js.contains("link_leave"), "leave type");
        // ...and never trusts the script-settable status property.
        assert!(!js.contains("window.status"), "spoofable API banned");
        // Guarded against double install.
        assert!(js.contains("__ferriteHoverInstalled"), "install guard");
        // Storm-tamed: same-href dedup plus a trailing-edge throttle so
        // fast traversal cannot flood the 10/s per-tab budget.
        assert!(js.contains("hoverLast"), "repeat dedup");
        assert!(js.contains("hoverTrailing"), "trailing edge");
    }

    #[test]
    fn theme_css_is_shared_for_future_loads() {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let bridge = crate::ipc::IpcBridge::new();
        let f = WryFactory::new(tx, bridge.limiter()).with_theme_css(":root{a:1}");
        assert_eq!(f.theme_css_snapshot(), ":root{a:1}");
        // Clones share the block: a pack switch lands without rebuilding.
        let g = f.clone();
        g.set_theme_css(":root{a:2}".to_owned());
        assert_eq!(f.theme_css_snapshot(), ":root{a:2}");
        // Trait path (the shell's handle) reaches the same store.
        let dyn_f: Rc<dyn WebViewFactory> = Rc::new(g);
        dyn_f.set_theme_css(":root{a:3}".to_owned());
        assert_eq!(f.theme_css_snapshot(), ":root{a:3}");
        // Mocks ignore theming without failing the switch.
        MockFactory.set_theme_css(":root{}".to_owned());
    }

    #[test]
    fn serve_path_reads_current_pack_not_creation_snapshot() {
        // Regression: the serve closure captured theme_css_snapshot() at
        // webview creation, so a reload after a pack switch reverted to the
        // birth pack. The closure now reads per request through this helper:
        // a pre-switch snapshot must stay stale while the helper sees new.
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let bridge = crate::ipc::IpcBridge::new();
        let f = WryFactory::new(tx, bridge.limiter()).with_theme_css(":root{a:1}");
        let stale = f.theme_css_snapshot();
        f.set_theme_css(":root{a:2}".to_owned());
        assert_eq!(stale, ":root{a:1}", "old snapshot stays stale");
        assert_eq!(
            current_shared_css(&f.theme_css),
            ":root{a:2}",
            "serve must see the switched pack"
        );
    }

    #[test]
    fn live_theme_script_upserts_without_reload() {
        let js = live_theme_script(":root{--f-bg:#0d0a06}");
        assert!(js.contains("ferrite-theme-live"), "style id");
        assert!(js.contains(":root{--f-bg:#0d0a06}"), "css carried");
        assert!(!js.contains("location.reload"), "no reload, state kept");
        assert!(!js.contains("</"), "no raw close tag in script");
        // Hostile CSS travels JSON-quoted: the payload stays a JS string
        // literal evaluated via evaluate_script (never inlined into HTML),
        // so markup inside it cannot execute; it only becomes style text.
        let hostile = "</style><script>alert(1)</script>";
        let evil = live_theme_script(hostile);
        let quoted = serde_json::to_string(hostile).expect("css quotes");
        assert!(evil.contains(&quoted), "payload is string-quoted");
        assert!(
            evil.contains("ferrite-theme-live"),
            "still targets live node"
        );
    }
}
