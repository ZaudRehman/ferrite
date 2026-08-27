//! `ferrite://` protocol handler.

use std::borrow::Cow;

use http::{Request, Response};

pub const NEWTAB_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>New Tab - Ferrite</title>
<style>html,body{margin:0;height:100%;background:#020203;color:#E5E2E1;font-family:system-ui;display:flex;align-items:center;justify-content:center}</style>
</head><body><h2>ferrite://newtab - Sovereign Browser</h2></body></html>
"#;

pub const CRASHED_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Tab Crashed</title>
<style>html,body{margin:0;height:100%;background:#020203;color:#FF003F;font-family:system-ui;display:flex;align-items:center;justify-content:center;flex-direction:column}</style>
</head><body><h1>This tab has crashed.</h1><p><button onclick="location.reload()">Reload Tab</button></p></body></html>
"#;

pub const SECURITY_WARNING_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Security Warning - Ferrite</title>
<style>html,body{margin:0;height:100%;background:#020203;color:#E5E2E1;font-family:system-ui;display:flex;align-items:center;justify-content:center;flex-direction:column;text-align:center}</style>
</head><body><h1 style="color:#FF003F">⚠ Security Warning</h1><p>Ferrite has determined this site is a known phishing/malware vector.</p><p><button onclick="history.back()">Go Back</button> <button onclick="location.href='about:blank'">Proceed at your own risk</button></p></body></html>
"#;

pub const SETTINGS_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Settings - Ferrite</title>
<style>html,body{margin:0;padding:24px;background:#020203;color:#E5E2E1;font-family:system-ui}h1{color:#E5E5E5}label{display:flex;align-items:center;gap:8px;margin:12px 0;font-size:14px}input[type=checkbox]{accent-color:#4F46E5}</style>
</head><body><h1>Privacy Settings</h1>
<label><input type="checkbox" id="adblock" checked onchange="post('adblock',this.checked)"> Adblock (EasyList)</label>
<label><input type="checkbox" id="gpc" checked onchange="post('gpc',this.checked)"> Send Sec-GPC: 1</label>
<label><input type="checkbox" id="realtime" onchange="post('realtime',this.checked)"> Real-time hash-prefix lookup (proxy-routed, 300ms fail-open)</label>
<label><input type="checkbox" id="https_up" checked onchange="post('https_up',this.checked)"> HTTPS-upgrade (try 443 before 80)</label>
<script>
function post(key, value){ try{ window.ipc.postMessage(JSON.stringify({type:'settings',key,value})); }catch(e){} }
try{ window.ipc.postMessage(JSON.stringify({type:'settings_query'})); }catch(e){}
window.__ferriteSetSettings = function(s){
  try{
    document.getElementById('adblock').checked = !!s.adblock_enabled;
    document.getElementById('gpc').checked = !!s.gpc_enabled;
    document.getElementById('realtime').checked = !!s.realtime_lookup_enabled;
    document.getElementById('https_up').checked = !!s.https_upgrade_enabled;
  }catch(e){}
};
</script>
</body></html>
"#;

/// Assets served under `ferrite://assets/<name>`.
pub type AssetTable = std::collections::BTreeMap<String, std::sync::Arc<[u8]>>;

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
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri().to_string();
    if let Some(rest) = uri.strip_prefix("ferrite://assets/") {
        let name = rest.split(['?', '#']).next().unwrap_or("");
        if let Some(bytes) = assets.get(name) {
            let mime = asset_mime(name);
            let len = bytes.len().to_string();
            return Response::builder()
                .status(200)
                .header("Content-Type", mime)
                .header("Content-Length", len)
                .body(Cow::Owned(bytes.to_vec()))
                .unwrap_or_else(|_| {
                    Response::builder()
                        .status(500)
                        .body(Cow::Borrowed(b"asset handler error" as &[u8]))
                        .unwrap()
                });
        }
        return Response::builder()
            .status(404)
            .header("Content-Type", "text/plain")
            .body(Cow::Owned(format!("asset not found: {name}").into_bytes()))
            .unwrap_or_else(|_| {
                Response::builder()
                    .status(500)
                    .body(Cow::Borrowed(b"" as &[u8]))
                    .unwrap()
            });
    }
    ferrite_protocol_handler(id, request)
}

/// Handler for `ferrite://` page requests.
pub fn ferrite_protocol_handler(
    _id: wry::WebViewId<'_>,
    request: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri().to_string();
    let (body, mime) = if uri.starts_with("ferrite://newtab") {
        (NEWTAB_HTML.as_bytes().to_vec(), "text/html")
    } else if uri.starts_with("ferrite://crashed") {
        (CRASHED_HTML.as_bytes().to_vec(), "text/html")
    } else if uri.starts_with("ferrite://security-warning") {
        (SECURITY_WARNING_HTML.as_bytes().to_vec(), "text/html")
    } else if uri.starts_with("ferrite://settings") {
        (SETTINGS_HTML.as_bytes().to_vec(), "text/html")
    } else {
        (format!("<h1>404 - {}</h1>", uri).into_bytes(), "text/html")
    };

    let len = body.len().to_string();
    Response::builder()
        .status(200)
        .header("Content-Type", mime)
        .header("Content-Length", len)
        .body(Cow::Owned(body))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(500)
                .body(Cow::Borrowed(b"protocol handler error" as &[u8]))
                .unwrap()
        })
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
        let resp = ferrite_protocol_handler("test", req);
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
        let resp = ferrite_protocol_handler("test", req);
        assert!(resp.into_body().windows(5).any(|w| w == b"crash"));
    }

    #[test]
    fn unknown_route_returns_404_body() {
        let req = Request::builder()
            .uri("ferrite://unknown")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req);
        assert_eq!(resp.status(), 200);
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
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets);
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
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets);
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn asset_query_and_fragment_are_stripped() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://assets/geist-pixel-square.ttf?v=2#x")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets);
        assert_eq!(resp.status(), 200);
    }

    #[test]
    fn non_asset_uris_fall_through_to_page_handler() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://newtab")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets);
        assert_eq!(resp.status(), 200);
        let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
        assert!(body.contains("ferrite://newtab"));
    }
}
