//! Share payloads: clean URLs, builders, QR SVG, and OG extraction. One
//! module serves every path, so stripping applies everywhere.

use serde::{Deserialize, Serialize};

/// Max URL chars. Overlong pastes fail closed, never huge QR.
pub const SHARE_MAX_URL_CHARS: usize = 2048;
/// Max bytes fed to the QR encoder.
pub const SHARE_MAX_QR_BYTES: usize = 2000;
/// Display caps for page metadata. Same rule as titles.
pub const SHARE_MAX_TITLE_CHARS: usize = 300;
pub const SHARE_MAX_DESC_CHARS: usize = 500;
/// QR quiet zone in modules. ISO minimum is 4.
const QR_QUIET: usize = 4;

/// Page Open Graph metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ShareMeta {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
}

/// Built payload: clean URL plus display metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePayload {
    pub url: String,
    pub title: String,
    pub description: String,
}

/// Only http(s) shares. Chrome, script, data, and file URLs never reach a
/// picker, clipboard, or QR code.
pub fn is_sharable_url(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Strip tracking params (same set as history storage) and enforce the
/// length cap. Returns `None` for unsharable schemes and overlong input.
pub fn clean_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !is_sharable_url(trimmed) || trimmed.chars().count() > SHARE_MAX_URL_CHARS {
        return None;
    }
    Some(ferrite_privacy::js::strip_tracking_params(trimmed))
}

/// Build the payload: clean URL first, then metadata with caps. The tab
/// title backs up a missing/empty `og:title`; the URL backs up both.
pub fn share_payload(
    raw_url: &str,
    tab_title: &str,
    meta: Option<&ShareMeta>,
) -> Option<SharePayload> {
    let url = clean_url(raw_url)?;
    let meta_title: String = meta
        .map(|m| m.title.chars().take(SHARE_MAX_TITLE_CHARS).collect())
        .unwrap_or_default();
    let title = if meta_title.trim().is_empty() {
        let t: String = tab_title.chars().take(SHARE_MAX_TITLE_CHARS).collect();
        if t.trim().is_empty() || t.trim() == url {
            url.clone()
        } else {
            t
        }
    } else {
        meta_title
    };
    let description: String = meta
        .map(|m| m.description.chars().take(SHARE_MAX_DESC_CHARS).collect())
        .unwrap_or_default();
    Some(SharePayload {
        url,
        title,
        description,
    })
}

/// `[title](url)` for notes and docs. Brackets in the title are escaped so
/// the link survives Markdown renderers.
pub fn markdown_link(title: &str, url: &str) -> String {
    let safe = title.replace('[', "\\[").replace(']', "\\]");
    format!("[{safe}]({url})")
}

/// Percent-encode for `mailto:` query values (RFC 3986 unreserved set passes
/// through; everything else, including space, becomes `%XX`).
pub fn percent_encode(s: &str) -> String {
    const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if UNRESERVED.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `mailto:?subject=<title>&body=<url>`, launches the default mail client
/// everywhere with no API, no account, no integration.
pub fn mailto_url(title: &str, url: &str) -> String {
    format!(
        "mailto:?subject={}&body={}",
        percent_encode(title),
        percent_encode(url)
    )
}

/// Parse and cap a page-reported `share_meta` envelope. Never fails on
/// shape drift, unknown fields ignored, missing fields empty.
pub fn parse_meta(v: &serde_json::Value) -> ShareMeta {
    let mut meta = serde_json::from_value::<ShareMeta>(v.clone()).unwrap_or_default();
    meta.title = meta.title.chars().take(SHARE_MAX_TITLE_CHARS).collect();
    meta.description = meta
        .description
        .chars()
        .take(SHARE_MAX_DESC_CHARS)
        .collect();
    meta
}

/// On-demand Open Graph extractor, evaluated at share time (not a content
/// script). Read-only DOM inspection. Reports `{type:'share_meta', title,
/// description}`.
pub fn og_extract_js() -> &'static str {
    r#"(function(){
  function meta(prop){
    try{
      var m=document.querySelector('meta[property="'+prop+'"]')||document.querySelector('meta[name="'+prop+'"]');
      return m&&m.content?m.content:"";
    }catch(e){ return ""; }
  }
  try{
    window.ipc.postMessage(JSON.stringify({type:'share_meta',title:meta('og:title'),description:meta('og:description')}));
  }catch(e){}
})();"#
}

/// QR encoding failures (surfaced as toasts, never panics).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShareQrError {
    #[error("link is too long for a QR code")]
    TooLong,
    #[error("QR encoding failed")]
    Encode,
}

/// Render the clean URL as a self-contained SVG (black modules on white,
/// quiet zone included). Encoded locally. The URL never leaves the device
/// for rendering, unlike web apps that hand it to a remote QR API.
pub fn qr_svg(url: &str) -> Result<String, ShareQrError> {
    if url.len() > SHARE_MAX_QR_BYTES {
        return Err(ShareQrError::TooLong);
    }
    let code = qrcode::QrCode::new(url.as_bytes()).map_err(|_| ShareQrError::Encode)?;
    let w = code.width();
    let span = w + QR_QUIET * 2;
    let mut svg = String::with_capacity(256 + w * w / 4);
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {span} {span}\" shape-rendering=\"crispEdges\" role=\"img\" aria-label=\"QR code\">\
         <rect width=\"{span}\" height=\"{span}\" fill=\"white\"/>"
    ));
    for y in 0..w {
        for x in 0..w {
            if code[(x, y)] == qrcode::Color::Dark {
                let (qx, qy) = (x + QR_QUIET, y + QR_QUIET);
                svg.push_str(&format!(
                    "<rect x=\"{qx}\" y=\"{qy}\" width=\"1\" height=\"1\"/>"
                ));
            }
        }
    }
    svg.push_str("</svg>");
    Ok(svg)
}

/// Server-rendered share page body (Linux fallback, QR home everywhere).
/// Values are escaped at serve time. `qr` carries the pre-rendered SVG or
/// `None` when no code is available. `actions` is false for empty state.
pub fn render_share_page(
    url: &str,
    title: &str,
    markdown: &str,
    mailto: &str,
    qr: Option<&str>,
    actions: bool,
) -> String {
    use crate::protocol::html_escape;
    let safe_url = html_escape(url);
    let safe_title = html_escape(title);
    let safe_mailto = html_escape(mailto);
    // JSON-embedded state for the copy buttons: escape `</` like every
    // other script-context push so values cannot break out.
    let data = serde_json::json!({"url": url, "markdown": markdown})
        .to_string()
        .replace("</", "<\\/");
    let qr_block = match qr {
        Some(svg) => format!(
            "<div class=\"set\" id=\"set-share-qr\"><div class=\"t\">QR code</div>\
             <div class=\"d\">Scan with a phone camera to open this link. Encoded on this device, nothing was sent anywhere.</div>\
             <div id=\"qr\">{svg}</div></div>"
        ),
        None => "<div class=\"set\" id=\"set-share-qr\"><div class=\"t\">QR code</div>\
             <div class=\"d\">No QR code for this link.</div></div>"
            .to_owned(),
    };
    let actions_block = if actions {
        format!(
            "<p><button onclick=\"copyLink()\">Copy link</button> <button onclick=\"copyMarkdown()\">Copy as Markdown</button> <a href=\"{safe_mailto}\"><button>Email link</button></a></p><p id=\"out\"></p>"
        )
    } else {
        "<p id=\"out\"></p>".to_owned()
    };
    format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Share - Ferrite</title>
<style>/*__FERRITE_THEME__*/html,body{{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}}#wrap{{padding:24px 32px;max-width:640px}}h1{{margin-top:0}}.set{{padding:12px 0;border-bottom:1px solid var(--f-border)}}.set .t{{font-weight:600}}.set .d{{color:var(--f-muted);font-size:13px;margin-top:4px;max-width:560px;overflow-wrap:anywhere}}label.row{{display:flex;align-items:center;gap:8px;margin:4px 0;font-size:14px}}button{{margin:4px;padding:6px 12px;background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:4px;cursor:pointer}}button:hover{{border-color:var(--f-accent)}}a{{color:var(--f-accent)}}#qr svg{{width:200px;height:200px;margin-top:8px;background:#fff}}code{{overflow-wrap:anywhere}}</style>
</head><body><div id="wrap">
<h1>Share</h1>
<div class="set" id="set-share-link"><div class="t">{safe_title}</div><div class="d"><code>{safe_url}</code></div>
{actions_block}</div>
{qr_block}
</div>
<script>
window.__ferriteShare={data};
function post(o){{ try{{ window.ipc.postMessage(JSON.stringify(o)); }}catch(e){{}} }}
function copyLink(){{ post({{type:'share_copy',url:window.__ferriteShare.url}}); }}
function copyMarkdown(){{ post({{type:'share_markdown',url:window.__ferriteShare.url,title:document.title}}); }}
window.__ferriteShared=function(msg){{ document.getElementById('out').textContent=msg; }};
</script></body></html>"#,
        safe_title = safe_title,
        safe_url = safe_url,
        qr_block = qr_block,
        actions_block = actions_block,
        data = data,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharable_schemes() {
        assert!(is_sharable_url("https://example.com/a"));
        assert!(is_sharable_url("http://example.com/"));
        assert!(is_sharable_url("  HTTPS://example.com/"));
        assert!(!is_sharable_url("ferrite://settings"));
        assert!(!is_sharable_url("javascript:alert(1)"));
        assert!(!is_sharable_url("data:text/html,x"));
        assert!(!is_sharable_url("file:///etc/passwd"));
        assert!(!is_sharable_url("view-source:https://example.com/"));
    }

    #[test]
    fn clean_strips_tracking_and_rejects() {
        assert_eq!(
            clean_url("https://example.com/a?utm_source=x&b=1"),
            Some("https://example.com/a?b=1".to_owned())
        );
        assert_eq!(clean_url("javascript:alert(1)"), None);
        assert_eq!(
            clean_url(&format!("https://example.com/{}", "x".repeat(3000))),
            None
        );
    }

    #[test]
    fn payload_title_fallback_chain() {
        let m = ShareMeta {
            title: "OG Title".into(),
            description: "OG desc".into(),
        };
        let p = share_payload("https://example.com/", "Tab", Some(&m)).unwrap();
        assert_eq!(p.title, "OG Title");
        assert_eq!(p.description, "OG desc");
        // Empty OG falls back to the tab title, then to the URL.
        let p = share_payload("https://example.com/", "Tab", None).unwrap();
        assert_eq!(p.title, "Tab");
        let p = share_payload("https://example.com/", "", None).unwrap();
        assert_eq!(p.title, "https://example.com/");
        // Caps apply.
        let long = "t".repeat(2000);
        let p = share_payload("https://example.com/", &long, None).unwrap();
        assert_eq!(p.title.chars().count(), SHARE_MAX_TITLE_CHARS);
    }

    #[test]
    fn markdown_escapes_brackets() {
        assert_eq!(
            markdown_link("a [b] c", "https://example.com/"),
            "[a \\[b\\] c](https://example.com/)"
        );
    }

    #[test]
    fn mailto_encodes() {
        let m = mailto_url("Hello & bye", "https://example.com/a b");
        assert_eq!(
            m,
            "mailto:?subject=Hello%20%26%20bye&body=https%3A%2F%2Fexample.com%2Fa%20b"
        );
        assert_eq!(percent_encode("abc-._~"), "abc-._~");
    }

    #[test]
    fn qr_svg_renders_and_rejects_long() {
        let svg = qr_svg("https://example.com/").unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("viewBox"));
        assert!(svg.contains("role=\"img\""));
        assert!(svg.ends_with("</svg>"));
        // Deterministic: same input, same modules.
        assert_eq!(svg, qr_svg("https://example.com/").unwrap());
        assert_eq!(
            qr_svg(&"x".repeat(SHARE_MAX_QR_BYTES + 1)),
            Err(ShareQrError::TooLong)
        );
    }

    #[test]
    fn meta_parse_caps_and_drift() {
        let v = serde_json::json!({"title": "t".repeat(2000), "nonsense": 1});
        let m = parse_meta(&v);
        assert_eq!(m.title.chars().count(), SHARE_MAX_TITLE_CHARS);
        assert!(m.description.is_empty());
    }

    #[test]
    fn extractor_reports_share_meta() {
        let js = og_extract_js();
        assert!(js.contains("share_meta"));
        assert!(js.contains("og:title"));
        assert!(js.contains("og:description"));
    }

    #[test]
    fn page_escapes_and_embeds() {
        let html = render_share_page(
            "https://example.com/?a=1",
            "</script><script>alert(1)</script>",
            "[t](https://example.com/?a=1)",
            "mailto:?subject=x",
            Some("<svg></svg>"),
            true,
        );
        assert!(!html.contains("</script><script>"), "breakout neutered");
        assert!(html.contains("alert(1)"), "content preserved");
        assert!(html.contains("share_copy") && html.contains("share_markdown"));
        assert!(html.contains("mailto:?subject=x"));
        assert!(html.contains("<svg></svg>"));
        let noqr = render_share_page("https://example.com/", "t", "m", "mailto:?x", None, true);
        assert!(noqr.contains("No QR code"), "missing QR explained");
        let empty = render_share_page("", "Nothing to share", "", "mailto:?", None, false);
        assert!(!empty.contains("<button"), "empty state has no actions");
        assert!(empty.contains("Nothing to share"), "empty state");
    }
}
