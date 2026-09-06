//! `ferrite://` protocol handler.

use std::borrow::Cow;

use http::{Request, Response};

pub const NEWTAB_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://newtab -->
<html><head><meta charset="utf-8"><title>New Tab - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;height:100%;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
.wrap{max-width:640px;margin:0 auto;padding:48px 24px;display:flex;flex-direction:column;align-items:center;justify-content:center;min-height:100vh}
.logo{font-size:28px;font-weight:800;letter-spacing:-0.02em;color:var(--f-fg);margin:0 0 8px}
.sub{color:var(--f-muted);font-size:13px;margin:0 0 24px;letter-spacing:0.08em;text-transform:uppercase;font-family:var(--f-font-pixel)}
.search{width:100%;max-width:560px;position:relative}
.search input{width:100%;padding:16px 16px;font-size:15px;border:0.5px solid var(--f-border);border-radius:0;background:var(--f-surface-1);color:var(--f-fg);outline:none;font-family:var(--f-font-mono)}
.search input:focus{border-color:var(--f-accent)}
.hint{margin-top:8px;color:var(--f-muted);font-size:11px;font-family:var(--f-font-pixel);letter-spacing:0.04em}
.shortcuts{display:grid;grid-template-columns:repeat(4,80px);gap:16px;margin-top:32px;justify-content:center}
.tile{width:80px;height:80px;background:var(--f-surface-1);border:0.5px solid var(--f-border);border-radius:0;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:6px;cursor:pointer;transition:border-color 100ms,transform 100ms}
.tile:hover{border-color:var(--f-accent);transform:translateY(-1px)}
.tile span{font-size:11px;color:var(--f-muted);font-family:var(--f-font-pixel)}
.tile strong{font-size:14px;color:var(--f-fg)}
</style></head>
<body>
<div class="wrap">
<p class="logo">Ferrite</p>
<p class="sub">Sovereign Browser</p>
<div class="search">
<input type="text" id="q" placeholder="Search with DuckDuckGo or enter URL - press Enter" autocomplete="off" spellcheck="false">
<p class="hint">Press Enter to search or navigate. Shortcuts below are instant.</p>
</div>
<div class="shortcuts">
<div class="tile" onclick="go('https://duckduckgo.com')"><strong>DDG</strong><span>DuckDuckGo</span></div>
<div class="tile" onclick="go('https://google.com')"><strong>G</strong><span>Google</span></div>
<div class="tile" onclick="go('https://github.com')"><strong>GH</strong><span>GitHub</span></div>
<div class="tile" onclick="go('https://news.ycombinator.com')"><strong>HN</strong><span>Hacker News</span></div>
</div>
</div>
<script>
(function(){
  const el=document.getElementById('q');
  function navigate(url){
    try{ window.ipc.postMessage(JSON.stringify({type:'navigate', url:url})); } catch(e){ location.href=url; }
  }
  window.go=navigate;
  el.addEventListener('keydown',function(e){
    if(e.key==='Enter'){
      const v=e.target.value.trim();
      if(!v) return;
      // Try to let Rust classify (URL vs search) via ipc, fallback to DuckDuckGo
      try{
        // If Rust handles search, it will navigate; if not, fallback after 50ms
        window.ipc.postMessage(JSON.stringify({type:'search', query:v}));
        setTimeout(function(){
          // fallback if no navigation happened (Rust didn't handle)
          if(document.visibilityState==='visible') location.href='https://duckduckgo.com/?q='+encodeURIComponent(v);
        },200);
      }catch(err){
        location.href='https://duckduckgo.com/?q='+encodeURIComponent(v);
      }
    }
  });
  // Do not autofocus content search per BROWSER_NAVIGATION_PATTERNS.md 6.1 - URL bar has focus, not content search
  // Ctrl+K also focuses
  document.addEventListener('keydown',function(e){ if((e.ctrlKey||e.metaKey) && e.key.toLowerCase()==='k'){ e.preventDefault(); el.focus(); el.select(); } });
})();
</script>
</body></html>
"#;

pub const TABSEARCH_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://tabsearch -->
<html><head><meta charset="utf-8"><title>Tab Search - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;height:100%;background:rgba(0,0,0,.62);font-family:var(--f-font-ui);overflow:hidden}
.backdrop{position:fixed;inset:0}
.panel{width:640px;max-width:calc(100vw - 64px);margin:16vh auto 0;background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:2px;animation:rise 140ms ease-out}
@keyframes rise{from{opacity:0;transform:translateY(-4px)}to{opacity:1;transform:none}}
.eyebrow{display:flex;justify-content:space-between;align-items:center;padding:10px 16px 0;font-family:var(--f-font-mono);font-size:11px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-muted)}
.inputrow{display:flex;align-items:center;gap:10px;padding:10px 16px 12px;border-bottom:1px solid var(--f-border)}
.inputrow svg{flex:none;color:var(--f-muted)}
#q{flex:1;background:transparent;border:0;outline:none;color:var(--f-fg);font-size:17px;font-family:var(--f-font-ui);padding:2px 0;min-width:0}
#q::placeholder{color:var(--f-faint)}
.kbd{font-family:var(--f-font-mono);font-size:11px;color:var(--f-muted);border:1px solid var(--f-border);border-radius:3px;padding:1px 6px;flex:none}
#results{max-height:336px;overflow-y:auto;padding:4px}
.row{display:flex;align-items:center;gap:12px;padding:9px 12px;border-left:3px solid transparent;cursor:pointer;animation:rowin 160ms ease-out backwards}
.row:nth-child(2){animation-delay:25ms}.row:nth-child(3){animation-delay:50ms}.row:nth-child(4){animation-delay:75ms}.row:nth-child(5){animation-delay:100ms}.row:nth-child(6){animation-delay:125ms}.row:nth-child(7){animation-delay:150ms}.row:nth-child(8){animation-delay:175ms}
@keyframes rowin{from{opacity:0;transform:translateX(-4px)}to{opacity:1;transform:none}}
.row.sel{background:var(--f-surface-2);border-left-color:var(--f-accent)}
.row .idx{font-family:var(--f-font-mono);font-size:11px;color:var(--f-faint);width:20px;flex:none}
.row .tt{display:block;min-width:0;flex:1}
.row .t{display:block;font-size:14px;color:var(--f-fg);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.row .u{display:block;font-size:11px;color:var(--f-muted);font-family:var(--f-font-mono);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.row .tag{font-family:var(--f-font-mono);font-size:10px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-muted);border:1px solid var(--f-border);border-radius:3px;padding:1px 6px;flex:none}
.row .tag.on{color:var(--f-accent);border-color:var(--f-accent)}
.footer{display:flex;justify-content:space-between;padding:8px 16px;border-top:1px solid var(--f-border);font-family:var(--f-font-mono);font-size:11px;color:var(--f-muted)}
.footer b{color:var(--f-fg);font-weight:400}
.empty{padding:24px;text-align:center;font-family:var(--f-font-mono);font-size:12px;letter-spacing:.06em;color:var(--f-faint)}
@media (prefers-reduced-motion:reduce){.panel,.row{animation:none}}
</style></head>
<body>
<div class="backdrop" id="backdrop"></div>
<div class="panel" id="panel">
<div class="eyebrow"><span>Tab search</span><span id="count"></span></div>
<div class="inputrow">
<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><line x1="16.5" y1="16.5" x2="20" y2="20"/></svg>
<input id="q" placeholder="Search tabs…" autocomplete="off" spellcheck="false">
<span class="kbd">ESC</span>
</div>
<div id="results"></div>
<div class="footer"><span><b>&#8593;&#8595;</b> navigate &middot; <b>Enter</b> switch &middot; <b>Esc</b> close</span><span>FERRITE</span></div>
</div>
<script>
(function(){
  var q=document.getElementById('q'), results=document.getElementById('results'),
      count=document.getElementById('count'), sel=0, items=[];
  function post(o){ try{ window.ipc.postMessage(JSON.stringify(o)); }catch(e){} }
  function esc(s){ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
  window.__ferriteResults=function(list){
    items=list||[]; sel=0; render();
  };
  window.__ferriteShow=function(){
    var p=document.getElementById('panel');
    p.style.animation='none'; void p.offsetWidth; p.style.animation='';
    q.value=''; q.focus(); post({type:'tabsearch',action:'query',query:''});
  };
  function render(){
    count.textContent = items.length ? (items.length + (items.length===1?' TAB':' TABS')) : '';
    if(!items.length){ results.innerHTML='<div class="empty">NO TABS MATCH</div>'; return; }
    var h='';
    for(var i=0;i<items.length;i++){ var t=items[i];
      h+='<div class="row'+(i===sel?' sel':'')+'" data-i="'+i+'">'
        +'<span class="idx">'+('0'+(i+1)).slice(-2)+'</span>'
        +'<span class="tt"><span class="t">'+esc(t.title||t.url)+'</span><span class="u">'+esc(t.url)+'</span></span>'
        +(t.active?'<span class="tag on">ACTIVE</span>':(t.discarded?'<span class="tag">DISCARDED</span>':''))
        +'</div>';
    }
    results.innerHTML=h;
    var rows=results.querySelectorAll('.row'), j;
    for(j=0;j<rows.length;j++){ (function(el){ el.addEventListener('click',function(){ choose(parseInt(el.getAttribute('data-i'),10)); }); })(rows[j]); }
  }
  function choose(i){ if(i>=0&&i<items.length){ post({type:'tabsearch',action:'select',id:items[i].id}); } }
  q.addEventListener('input',function(){ post({type:'tabsearch',action:'query',query:q.value}); });
  q.addEventListener('keydown',function(e){
    if(e.key==='ArrowDown'){ e.preventDefault(); if(items.length){ sel=(sel+1)%items.length; render(); } }
    else if(e.key==='ArrowUp'){ e.preventDefault(); if(items.length){ sel=(sel-1+items.length)%items.length; render(); } }
    else if(e.key==='Enter'){ e.preventDefault(); choose(sel); }
    else if(e.key==='Escape'){ e.preventDefault(); post({type:'tabsearch',action:'close'}); }
  });
  document.getElementById('backdrop').addEventListener('click',function(){ post({type:'tabsearch',action:'close'}); });
  document.getElementById('panel').addEventListener('click',function(e){ e.stopPropagation(); });
  document.addEventListener('keydown',function(e){ if(e.key==='Escape'){ post({type:'tabsearch',action:'close'}); } });
  window.__ferriteShow();
})();
</script>
</body></html>
"#;

pub const CRASHED_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Tab Crashed</title>
<style>/*__FERRITE_THEME__*/html,body{margin:0;height:100%;background:var(--f-bg);color:var(--f-danger);font-family:var(--f-font-ui);display:flex;align-items:center;justify-content:center;flex-direction:column}</style>
</head><body><h1>This tab has crashed.</h1><p><button onclick="location.reload()">Reload Tab</button></p></body></html>
"#;

pub const SECURITY_WARNING_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Security Warning - Ferrite</title>
<style>/*__FERRITE_THEME__*/html,body{margin:0;height:100%;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui);display:flex;align-items:center;justify-content:center;flex-direction:column;text-align:center}</style>
</head><body><h1 style="color:var(--f-danger)">⚠ Security Warning</h1><p>Ferrite has determined this site is a known phishing/malware vector.</p><p><button onclick="history.back()">Go Back</button> <button onclick="location.href='about:blank'">Proceed at your own risk</button></p></body></html>
"#;

pub const SETTINGS_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Settings - Ferrite</title>
<style>/*__FERRITE_THEME__*/html,body{margin:0;padding:24px;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}h1{color:var(--f-fg)}h2{color:var(--f-fg);margin-top:24px}label{display:flex;align-items:center;gap:8px;margin:12px 0;font-size:14px}input[type=checkbox]{accent-color:var(--f-accent)}button{margin:4px;padding:6px 12px}</style>
</head><body><h1>Privacy Settings</h1>
<label><input type="checkbox" id="adblock" checked onchange="post('adblock',this.checked)"> Adblock (EasyList)</label>
<label><input type="checkbox" id="gpc" checked onchange="post('gpc',this.checked)"> Send Sec-GPC: 1</label>
<label><input type="checkbox" id="realtime" onchange="post('realtime',this.checked)"> Real-time hash-prefix lookup (proxy-routed, 300ms fail-open)</label>
<label><input type="checkbox" id="https_up" checked onchange="post('https_up',this.checked)"> HTTPS-upgrade (try 443 before 80)</label>
<h2>Vault Recovery</h2>
<p><button onclick="enableRecovery()">Enable Recovery Phrase</button> <button onclick="disableRecovery()">Disable Recovery</button> <button onclick="showRecovery()">Show Recovery Phrase</button></p>
<p id="recoveryOut"></p>
<h2>Import / Export</h2>
<p><button onclick="importBookmarks()">Import Bookmarks (Netscape HTML)</button> <button onclick="importHistory()">Import History (Chrome/Firefox DB)</button> <button onclick="exportData()">Export Data</button></p>
<script>
function post(key, value){ try{ window.ipc.postMessage(JSON.stringify({type:'settings',key,value})); }catch(e){} }
function enableRecovery(){ try{ window.ipc.postMessage(JSON.stringify({type:'recovery',action:'enable'})); }catch(e){} }
function disableRecovery(){ try{ window.ipc.postMessage(JSON.stringify({type:'recovery',action:'disable'})); }catch(e){} }
function showRecovery(){ try{ window.ipc.postMessage(JSON.stringify({type:'recovery',action:'show'})); }catch(e){} }
function importBookmarks(){ try{ window.ipc.postMessage(JSON.stringify({type:'import',kind:'bookmarks'})); }catch(e){} }
function importHistory(){ try{ window.ipc.postMessage(JSON.stringify({type:'import',kind:'history'})); }catch(e){} }
function exportData(){ try{ window.ipc.postMessage(JSON.stringify({type:'import',kind:'export'})); }catch(e){} }
try{ window.ipc.postMessage(JSON.stringify({type:'settings_query'})); }catch(e){}
window.__ferriteSetSettings = function(s){
  try{
    document.getElementById('adblock').checked = !!s.adblock_enabled;
    document.getElementById('gpc').checked = !!s.gpc_enabled;
    document.getElementById('realtime').checked = !!s.realtime_lookup_enabled;
    document.getElementById('https_up').checked = !!s.https_upgrade_enabled;
  }catch(e){}
};
window.__ferriteShowRecovery = function(p){ document.getElementById('recoveryOut').textContent = p; };
</script>
</body></html>
"#;

/// Assets served under `ferrite://assets/<name>`.
pub type AssetTable = std::collections::BTreeMap<String, std::sync::Arc<[u8]>>;

/// Marker inside each privileged page's `<style>` where the shell's theme
/// CSS (`ThemeConfig::theme_css`) is substituted at serve time. Pages keep
/// working with zero theme (marker left as a harmless empty comment) so
/// tests and bare handlers never depend on shell tokens.
pub const THEME_MARKER: &str = "/*__FERRITE_THEME__*/";

/// Substitute the theme CSS block for [`THEME_MARKER`]. Plain string
/// replace (no regex): an empty `css` leaves the marker comment in place.
pub fn apply_theme_css(html: &str, css: &str) -> String {
    html.replacen(THEME_MARKER, css, 1)
}

/// Minimal HTML escaping for interpolated values (`&<>"'`).
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const PRIVILEGED_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; font-src http://ferrite.assets https://ferrite.assets; img-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
const NOSNIFF: &str = "nosniff";

fn html_response(status: u16, body: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    let len = body.len().to_string();
    Response::builder()
        .status(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Content-Security-Policy", PRIVILEGED_CSP)
        .header("X-Content-Type-Options", NOSNIFF)
        .header("Content-Length", len)
        .body(Cow::Owned(body))
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"protocol error" as &[u8])))
}

fn asset_mime(name: &str) -> &'static str {
    if name.ends_with(".ttf") {
        "font/ttf"
    } else if name.ends_with(".otf") {
        "font/otf"
    } else if name.ends_with(".woff2") {
        "font/woff2"
    } else {
        "application/octet-stream"
    }
}

/// Handler that serves assets then delegates to the page handler.
pub fn ferrite_protocol_handler_with_assets(
    id: wry::WebViewId<'_>,
    request: Request<Vec<u8>>,
    assets: &AssetTable,
    theme_css: &str,
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri().to_string();
    // Handle both ferrite://assets/ and http(s)://ferrite.assets/ for Windows WebView2 proxy mapping
    let asset_prefixes = [
        "ferrite://assets/",
        "http://ferrite.assets/",
        "https://ferrite.assets/",
        "http://ferrite/assets/",
        "https://ferrite/assets/",
    ];
    for prefix in asset_prefixes {
        if let Some(rest) = uri.strip_prefix(prefix) {
            let name = rest.split(['?', '#']).next().unwrap_or("");
            if let Some(bytes) = assets.get(name) {
                let mime = asset_mime(name);
                let len = bytes.len().to_string();
                return Response::builder()
                    .status(200)
                    .header("Content-Type", mime)
                    .header("X-Content-Type-Options", NOSNIFF)
                    .header("Cache-Control", "public, max-age=31536000, immutable")
                    .header("Content-Length", len)
                    .body(Cow::Owned(bytes.to_vec()))
                    .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"asset error" as &[u8])));
            }
            // `text/plain` does not execute, but escape anyway (info-leak hygiene).
            let safe = html_escape(name);
            return Response::builder()
                .status(404)
                .header("Content-Type", "text/plain; charset=utf-8")
                .header("X-Content-Type-Options", NOSNIFF)
                .body(Cow::Owned(format!("asset not found: {safe}").into_bytes()))
                .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"" as &[u8])));
        }
    }
    // Also handle http(s)://ferrite.newtab etc via exact host match (never
    // substring: `https://evil.com/?a=ferrite&b=newtab` must not match).
    if is_ferrite_asset_uri(&uri)
        && let Some(rest) = uri
            .split_once("ferrite.assets/")
            .map(|(_, r)| r)
            .or_else(|| uri.split_once("ferrite/assets/").map(|(_, r)| r))
    {
        let name = rest.split(['?', '#']).next().unwrap_or("");
        if let Some(bytes) = assets.get(name) {
            let mime = asset_mime(name);
            let len = bytes.len().to_string();
            return Response::builder()
                .status(200)
                .header("Content-Type", mime)
                .header("X-Content-Type-Options", NOSNIFF)
                .header("Cache-Control", "public, max-age=31536000, immutable")
                .header("Content-Length", len)
                .body(Cow::Owned(bytes.to_vec()))
                .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"asset error" as &[u8])));
        }
    }
    ferrite_protocol_handler(id, request, theme_css)
}

/// Handler for `ferrite://` page requests. `theme_css` (the shell's
/// `ThemeConfig::theme_css` block) is substituted for [`THEME_MARKER`] in
/// the privileged page templates; empty string leaves pages unthemed but
/// functional.
pub fn ferrite_protocol_handler(
    _id: wry::WebViewId<'_>,
    request: Request<Vec<u8>>,
    theme_css: &str,
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri().to_string();
    let themed = |html: &str| apply_theme_css(html, theme_css).into_bytes();
    match ferrite_route(&uri) {
        FerritePage::NewTab => html_response(200, themed(NEWTAB_HTML)),
        FerritePage::Crashed => html_response(200, themed(CRASHED_HTML)),
        FerritePage::Security => html_response(200, themed(SECURITY_WARNING_HTML)),
        FerritePage::Settings => html_response(200, themed(SETTINGS_HTML)),
        FerritePage::TabSearch => html_response(200, themed(TABSEARCH_HTML)),
        FerritePage::Search => html_response(
            200,
            b"<html><body>ferrite search handled via egui</body></html>".to_vec(),
        ),
        FerritePage::Unknown => {
            let safe = html_escape(&uri);
            // Truncate absurd URIs (10KB paste) to bound the error page.
            let shown = if safe.len() > 512 {
                &safe[..512]
            } else {
                &safe
            };
            html_response(404, format!("<h1>404 - {shown}</h1>").into_bytes())
        }
        FerritePage::External => html_response(404, b"<h1>404 - not a ferrite page</h1>".to_vec()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FerritePage {
    NewTab,
    Crashed,
    Security,
    Settings,
    Search,
    TabSearch,
    Unknown,
    External,
}

/// Exact-match routing for privileged pages. Only `ferrite:` scheme or an
/// exact `ferrite.*` host (including the Windows WebView2
/// `http(s)://ferrite.*` mapping) routes here. Query/fragment never affect
/// the match, and substrings in path/query of external URLs never match.
fn ferrite_route(uri: &str) -> FerritePage {
    let (scheme, rest) = match uri.split_once("://") {
        Some(v) => v,
        None => return FerritePage::External,
    };
    let scheme = scheme.to_ascii_lowercase();
    // Authority is up to the first / ? #.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .split('@')
        .next_back()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or(authority)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    // Path is the /... portion without query/fragment, lowercased.
    let after_auth = &rest[authority.len()..];
    let path = after_auth
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let page = if path.is_empty() || path == "/" {
        host.as_str()
    } else {
        path.trim_start_matches('/').split('/').next().unwrap_or("")
    };
    // Also accept `ferrite.newtab` style hosts from the WebView2 mapping.
    let host_page = host.strip_suffix(".newtab").map(|_| "newtab").or_else(|| {
        [
            "newtab",
            "crashed",
            "security-warning",
            "settings",
            "search",
            "tabsearch",
            "assets",
        ]
        .iter()
        .find(|p| host == format!("ferrite.{p}").as_str())
        .copied()
    });
    let is_internal = scheme == "ferrite"
        || host == "ferrite"
        || host.ends_with(".ferrite")
        || host == "ferrite.invalid"
        || host.ends_with(".ferrite.invalid")
        || scheme == "about" && host == "blank";
    if !is_internal {
        return FerritePage::External;
    }
    let name = host_page.unwrap_or(page);
    match name {
        "newtab" => FerritePage::NewTab,
        "crashed" => FerritePage::Crashed,
        "security-warning" => FerritePage::Security,
        "settings" => FerritePage::Settings,
        "search" => FerritePage::Search,
        "tabsearch" => FerritePage::TabSearch,
        "assets" => FerritePage::Unknown,
        _ => FerritePage::Unknown,
    }
}

fn is_ferrite_asset_uri(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    lower.contains("://ferrite.assets/")
        || lower.contains("://ferrite/assets/")
        || lower.starts_with("ferrite://assets/")
}

/// Decode a single query parameter value (form-urlencoding: `+` → space,
/// `%XX` escapes). Returns `None` when the key is absent. Only the first
/// occurrence wins.
pub fn decode_query_param(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// Encode a single query component value (form-urlencoding).
pub fn encode_query_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtab_handler_returns_html() {
        let req = Request::builder()
            .uri("ferrite://newtab")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert_eq!(resp.status(), 200);
        assert!(resp.headers().contains_key("Content-Type"));
        let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
        assert!(body.contains("ferrite://newtab"));
    }

    #[test]
    fn crashed_handler_returns_crashed_html() {
        let req = Request::builder()
            .uri("ferrite://crashed")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert!(resp.into_body().windows(5).any(|w| w == b"crash"));
    }

    #[test]
    fn unknown_route_returns_404_body() {
        let req = Request::builder()
            .uri("ferrite://unknown")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn html_escape_neutralizes_script_markup() {
        assert_eq!(
            html_escape("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(html_escape("a&b\"c'd"), "a&amp;b&quot;c&#39;d");
    }

    #[test]
    fn reflected_xss_in_unknown_route_is_escaped() {
        // `<>"` cannot appear in a valid `http::Uri` (the HTTP layer
        // rejects them before the handler runs), so exercise the escaping
        // with URI-valid metacharacters `&` and `'`.
        let req = Request::builder()
            .uri("ferrite://unknown?x=a&y='evil")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert_eq!(resp.status(), 404);
        let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
        assert!(!body.contains("y='evil"));
        assert!(body.contains("&#39;evil"));
        assert!(body.contains("&amp;"));
    }

    #[test]
    fn external_url_containing_ferrite_is_not_hijacked() {
        for uri in [
            "https://evil.com/?a=ferrite&b=newtab",
            "https://ferrite.evil.com/newtab",
            "https://notferrite.example/settings",
        ] {
            let req = Request::builder().uri(uri).body(Vec::new()).unwrap();
            let resp = ferrite_protocol_handler("test", req, "");
            assert_eq!(resp.status(), 404, "uri: {uri}");
            let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
            assert!(
                !body.contains("Sovereign Browser"),
                "must not serve internal page for {uri}"
            );
        }
    }

    #[test]
    fn query_codec_round_trips_ampersands() {
        let url = "https://example.com/a?b=c&d=e";
        let enc = encode_query_component(url);
        assert!(!enc.contains('&'));
        assert_eq!(
            decode_query_param(&format!("url={enc}"), "url").as_deref(),
            Some(url)
        );
        assert_eq!(
            decode_query_param("q=hello+world", "q").as_deref(),
            Some("hello world")
        );
        assert_eq!(decode_query_param("a=1&q=x", "q").as_deref(), Some("x"));
        assert_eq!(decode_query_param("a=1", "q"), None);
    }

    #[test]
    fn privileged_pages_carry_csp_and_nosniff() {
        let req = Request::builder()
            .uri("ferrite://newtab")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert_eq!(resp.status(), 200);
        let csp = resp.headers().get("Content-Security-Policy").unwrap();
        assert!(csp.to_str().unwrap().contains("default-src"));
        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
    }

    fn make_assets() -> AssetTable {
        let mut t = AssetTable::new();
        t.insert(
            "geist-pixel-square.ttf".to_owned(),
            std::sync::Arc::from(&b"FAKE_TTF_BYTES"[..]),
        );
        t
    }

    #[test]
    fn asset_route_serves_registered_bytes_with_font_mime() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://assets/geist-pixel-square.ttf")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, "");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers().get("Content-Type").unwrap(), "font/ttf");
        assert_eq!(&*resp.into_body(), b"FAKE_TTF_BYTES");
    }

    #[test]
    fn asset_route_unknown_name_is_404() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://assets/nope.ttf")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, "");
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn asset_query_and_fragment_are_stripped() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://assets/geist-pixel-square.ttf?v=2#x")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, "");
        assert_eq!(resp.status(), 200);
    }

    #[test]
    fn non_asset_uris_fall_through_to_page_handler() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://newtab")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, "");
        assert_eq!(resp.status(), 200);
        let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
        assert!(body.contains("ferrite://newtab"));
    }

    #[test]
    fn theme_css_replaces_marker_once() {
        let out = apply_theme_css("a/*__FERRITE_THEME__*/b/*__FERRITE_THEME__*/", ":root{}");
        assert_eq!(out, "a:root{}b/*__FERRITE_THEME__*/");
        assert_eq!(apply_theme_css("no marker", ":root{}"), "no marker");
    }

    #[test]
    fn themed_newtab_has_vars_and_no_hardcoded_colors() {
        let css = ":root{--f-bg:#090a0f}";
        let body = themed_body("ferrite://newtab", css);
        assert!(body.contains(":root{--f-bg:#090a0f}"), "injected");
        assert!(!body.contains(THEME_MARKER), "marker consumed");
        for page in [
            NEWTAB_HTML,
            CRASHED_HTML,
            SECURITY_WARNING_HTML,
            SETTINGS_HTML,
            TABSEARCH_HTML,
        ] {
            assert!(page.contains(THEME_MARKER), "marker present");
            for hard in [
                "#020203", "#E5E2E1", "#9CA3AF", "#2A2A2A", "#6366F1", "Inter,",
            ] {
                assert!(
                    !page.contains(hard),
                    "hardcoded {hard} left in privileged template"
                );
            }
        }
    }

    #[test]
    fn privileged_csp_allows_first_party_fonts_only() {
        let req = Request::builder()
            .uri("ferrite://settings")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        let csp = resp.headers().get("Content-Security-Policy").unwrap();
        let csp = csp.to_str().unwrap();
        assert!(
            csp.contains("font-src http://ferrite.assets https://ferrite.assets"),
            "{csp}"
        );
        assert!(!csp.contains("font-src *"), "no wildcard fonts");
    }

    fn themed_body(uri: &str, css: &str) -> String {
        let assets = make_assets();
        let req = Request::builder().uri(uri).body(Vec::new()).unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, css);
        String::from_utf8(resp.into_body().into_owned()).unwrap()
    }

    #[test]
    fn tabsearch_route_serves_overlay_page() {
        let body = themed_body("ferrite://tabsearch", ":root{}");
        assert!(body.contains("Tab search"), "eyebrow");
        assert!(body.contains("__ferriteResults"), "results hook");
        assert!(body.contains("__ferriteShow"), "show hook");
        assert!(body.contains("prefers-reduced-motion"), "motion guard");
    }
}
