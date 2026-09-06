//! HTML sanitization for archive.

use regex::Regex;
use std::sync::OnceLock;

static SCRIPT_RE: OnceLock<Regex> = OnceLock::new();
static SCRIPT_OPEN_RE: OnceLock<Regex> = OnceLock::new();
static IFRAME_RE: OnceLock<Regex> = OnceLock::new();
static IFRAME_SRC_RE: OnceLock<Regex> = OnceLock::new();
static TRACKER_RE: OnceLock<Regex> = OnceLock::new();
static HANDLER_RE: OnceLock<Regex> = OnceLock::new();
static JS_URL_RE: OnceLock<Regex> = OnceLock::new();
static DANGEROUS_TAG_RE: OnceLock<Regex> = OnceLock::new();
static STYLE_TAG_RE: OnceLock<Regex> = OnceLock::new();
static TAG_RE: OnceLock<Regex> = OnceLock::new();
static WS_RE: OnceLock<Regex> = OnceLock::new();

fn script_re() -> &'static Regex {
    SCRIPT_RE.get_or_init(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script>").unwrap())
}

/// Leftover `<script ...>` open tags without a closer (unclosed script):
/// the tag itself is removed; inner text stays as inert text.
fn script_open_re() -> &'static Regex {
    SCRIPT_OPEN_RE.get_or_init(|| Regex::new(r"(?i)<script\b[^>]*>").unwrap())
}

fn iframe_re() -> &'static Regex {
    IFRAME_RE.get_or_init(|| Regex::new(r#"(?is)<iframe\b[^>]*>.*?</iframe>"#).unwrap())
}

fn iframe_src_re() -> &'static Regex {
    IFRAME_SRC_RE.get_or_init(|| {
        Regex::new(r#"(?is)<iframe\b[^>]*\bsrc\s*=\s*["']([^"']+)["'][^>]*>.*?</iframe>"#).unwrap()
    })
}

fn tracker_re() -> &'static Regex {
    TRACKER_RE.get_or_init(|| {
        Regex::new(r#"(?i)<(img|link)\b[^>]*\b(src|href)\s*=\s*["'][^"']*(tracker|analytics|doubleclick|googletag|facebook|hotjar|segment|mixpanel)[^"']*["'][^>]*>"#).unwrap()
    })
}

/// Inline event handlers (` onclick=`, `onload=`, ...): the classic XSS
/// carrier the old sanitizer missed entirely.
fn handler_re() -> &'static Regex {
    HANDLER_RE
        .get_or_init(|| Regex::new(r#"(?i)\s+on[a-z]+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#).unwrap())
}

/// `javascript:` URLs in active attributes. Replaced with an inert `#`.
fn js_url_re() -> &'static Regex {
    JS_URL_RE.get_or_init(|| {
        Regex::new(r#"(?i)\b(href|src|action|xlink:href)\s*=\s*("([^"]*)"|'([^']*)'|[^\s>]+)"#)
            .unwrap()
    })
}

/// Tags that are never content: executable embeds, navigation hijacks,
/// base-url rewriters. SVG/MathML are KEPT (scripts inside them are
/// stripped as `<script>`, handlers stripped globally).
fn dangerous_tag_re() -> &'static Regex {
    DANGEROUS_TAG_RE.get_or_init(|| {
        Regex::new(r"(?is)<(object|embed|meta|base|form|link)\b[^>]*>(.*?</(object|embed|form)>)?")
            .unwrap()
    })
}

fn style_tag_re() -> &'static Regex {
    STYLE_TAG_RE.get_or_init(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style>").unwrap())
}

fn tag_re() -> &'static Regex {
    TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap())
}

fn ws_re() -> &'static Regex {
    WS_RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

fn neutralize_js_urls(html: &str) -> String {
    js_url_re()
        .replace_all(html, |caps: &regex::Captures| {
            let attr = caps.get(1).map(|m| m.as_str()).unwrap_or("href");
            let raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            // Preserve the original quoting style.
            let (quote, inner) = if (raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2)
                || (raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2)
            {
                (
                    raw[..1].to_owned(),
                    raw[1..raw.len() - 1].trim_start().to_owned(),
                )
            } else {
                (String::new(), raw.trim_start().to_owned())
            };
            // `&#...` / whitespace-obfuscated javascript: still decodes to
            // script in browsers; match case-insensitively after trimming
            // leading whitespace and control chars.
            let probe: String = inner
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '\0')
                .collect();
            if probe.len() >= 11 && probe[..11].eq_ignore_ascii_case("javascript:") {
                if quote.is_empty() {
                    format!("{attr}=#blocked")
                } else {
                    format!("{attr}={quote}#blocked{quote}")
                }
            } else {
                caps.get(0).unwrap().as_str().to_owned()
            }
        })
        .into_owned()
}

/// Extract an iframe src attribute value (quoted or unquoted), if present.
fn extract_iframe_src(tag: &str) -> Option<String> {
    static SRC_RE: OnceLock<Regex> = OnceLock::new();
    let re =
        SRC_RE.get_or_init(|| Regex::new(r#"(?i)\bsrc\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#).unwrap());
    re.captures(tag).and_then(|c| {
        c.get(1).map(|m| {
            let v = m.as_str();
            v.strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v)
                .to_owned()
        })
    })
}

/// Compare a URL host against the page host by exact-or-suffix match.
/// Substring matching (`evil-example.com`.contains(`example.com`)) is a
/// bypass and is never used here.
fn same_host(url_src: &str, page_host: &str) -> bool {
    // Absolute URL.
    if let Ok(url) = url::Url::parse(url_src) {
        return match url.host_str() {
            Some(h) => {
                h.eq_ignore_ascii_case(page_host)
                    || h.to_ascii_lowercase()
                        .ends_with(&format!(".{}", page_host.to_ascii_lowercase()))
            }
            None => false,
        };
    }
    // Protocol-relative URL (`//evil.com/x`): NOT same-origin despite the
    // leading slashes the old code trusted.
    if let Some(rest) = url_src.strip_prefix("//") {
        if let Ok(url) = url::Url::parse(&format!("https://{rest}")) {
            return match url.host_str() {
                Some(h) => {
                    h.eq_ignore_ascii_case(page_host)
                        || h.to_ascii_lowercase()
                            .ends_with(&format!(".{}", page_host.to_ascii_lowercase()))
                }
                None => false,
            };
        }
        return false;
    }
    // Same-document or root/relative reference without a host.
    url_src.starts_with('#') || url_src.starts_with('/') || !url_src.contains(':')
}

/// Strip scripts, third-party iframes, and tracker tags.
/// `page_host` is used to keep same-origin iframes.
pub fn sanitize(html: &str, page_host: Option<&str>) -> String {
    let mut out = html.to_owned();
    // Fixpoint (max 3) over BOTH pair and open-tag stripping: nested
    // `<scr<script>ipt>` re-forms a tag after a single pass, and unclosed
    // scripts leave open tags behind. Inner text stays as inert text.
    for _ in 0..3 {
        let before = out.len();
        out = script_re().replace_all(&out, "").into_owned();
        out = script_open_re().replace_all(&out, "").into_owned();
        if out.len() == before {
            break;
        }
    }
    // Inline event handlers and javascript: URLs before tag removal so
    // kept tags (same-origin iframes, svg) are inert too.
    out = handler_re().replace_all(&out, "").into_owned();
    out = neutralize_js_urls(&out);
    // Dangerous embeds/navigation: object, embed, meta refresh, base, form,
    // and non-stylesheet link tags. Keep link[rel=stylesheet].
    out = dangerous_tag_re()
        .replace_all(&out, |caps: &regex::Captures| {
            let tag = caps.get(0).unwrap().as_str();
            let lower = tag.to_ascii_lowercase();
            if lower.starts_with("<link") {
                // Keep stylesheets; tracker stylesheets are removed by
                // tracker_re below, non-stylesheet links are dropped.
                if lower.contains("stylesheet") {
                    return tag.to_owned();
                }
                return String::new();
            }
            if lower.starts_with("<meta") {
                // Keep charset/viewport; drop refresh and other actives.
                if lower.contains("charset") || lower.contains("name=") {
                    return tag.to_owned();
                }
                return String::new();
            }
            String::new()
        })
        .into_owned();

    // Remove iframes that are third-party. Keep same-origin.
    if let Some(host) = page_host {
        out = iframe_src_re()
            .replace_all(&out, |caps: &regex::Captures| {
                let src = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                if same_host(src, host) {
                    caps.get(0).unwrap().as_str().to_owned()
                } else {
                    String::new()
                }
            })
            .into_owned();
        // Fallback for iframes the src pass could not vet (unquoted src,
        // srcdoc, ...): re-check same-origin instead of blanket stripping,
        // because kept same-origin iframes still match this pattern.
        // srcdoc content runs in the archive origin — always strip.
        out = iframe_re()
            .replace_all(&out, |caps: &regex::Captures| {
                let tag = caps.get(0).unwrap().as_str();
                let lower = tag.to_ascii_lowercase();
                if lower.contains("srcdoc") {
                    return String::new();
                }
                // Extract any src= (quoted or not) and vet it.
                let src = extract_iframe_src(tag);
                match (src, host) {
                    (Some(s), h) if same_host(&s, h) => tag.to_owned(),
                    (None, _) => tag.to_owned(),
                    _ => String::new(),
                }
            })
            .into_owned();
    } else {
        out = iframe_re().replace_all(&out, "").into_owned();
    }

    out = tracker_re().replace_all(&out, "").into_owned();
    out
}

/// Extract text content for FTS indexing, after sanitization.
pub fn extract_text(html: &str) -> String {
    let text = tag_re().replace_all(html, " ");
    // Style contents are not prose.
    let text = style_tag_re().replace_all(&text, " ");
    ws_re().replace_all(&text, " ").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script() {
        let html = r#"<p>hi</p><script>alert(1)</script><p>bye</p>"#;
        let out = sanitize(html, None);
        assert!(!out.contains("script"));
        assert!(out.contains("hi"));
        assert!(out.contains("bye"));
    }

    #[test]
    fn strips_nested_and_unclosed_script() {
        let nested = r#"<p>a</p><scr<script>ipt>alert(1)</scr<script>ipt><p>b</p>"#;
        let out = sanitize(nested, None);
        assert!(!out.to_ascii_lowercase().contains("script"));
        let unclosed = r#"<p>a</p><script src="https://evil.com/x.js"><p>b</p>"#;
        let out2 = sanitize(unclosed, None);
        assert!(!out2.to_ascii_lowercase().contains("<script"));
    }

    #[test]
    fn strips_event_handlers_and_js_urls() {
        let html = r#"<p onclick="alert(1)" onload='x'>hi</p><a href="javascript:alert(1)">x</a><a HREF="  JaVaScRiPt:alert(2)">y</a>"#;
        let out = sanitize(html, None);
        assert!(!out.contains("onclick"));
        assert!(!out.contains("onload"));
        assert!(!out.to_ascii_lowercase().contains("javascript:"));
        assert!(out.contains("hi"));
    }

    #[test]
    fn strips_dangerous_embeds_keeps_svg() {
        let html = r#"<object data="https://evil.com/x.swf"></object><embed src="https://evil.com/y"><meta http-equiv="refresh" content="0;url=https://evil.com"><base href="https://evil.com/"><svg viewBox="0 0 1 1"><circle r="1"/></svg><p>ok</p>"#;
        let out = sanitize(html, None);
        assert!(!out.contains("evil.com"));
        assert!(out.contains("<svg"));
        assert!(out.contains("ok"));
    }

    #[test]
    fn keeps_same_origin_iframe() {
        let html = r#"<iframe src="https://example.com/embed"></iframe><p>x</p>"#;
        let out = sanitize(html, Some("example.com"));
        assert!(out.contains("iframe"));
    }

    #[test]
    fn strips_cross_origin_iframe() {
        let html = r#"<iframe src="https://evil.com/embed"></iframe><p>x</p>"#;
        let out = sanitize(html, Some("example.com"));
        assert!(!out.contains("evil.com"));
    }

    #[test]
    fn strips_protocol_relative_and_suffix_lookalike_iframe() {
        let html = r#"<iframe src="//evil.com/e"></iframe><iframe src="https://evil-example.com/e"></iframe><p>x</p>"#;
        let out = sanitize(html, Some("example.com"));
        assert!(!out.contains("evil.com"));
        assert!(!out.contains("evil-example.com"));
    }

    #[test]
    fn strips_tracker_img() {
        let html = r#"<img src="https://tracker.com/pixel.gif"><p>ok</p>"#;
        let out = sanitize(html, None);
        assert!(!out.contains("tracker.com"));
    }

    #[test]
    fn strips_extended_tracker_hosts() {
        let html = r#"<img src="https://connect.facebook.net/pixel"><img src="https://hotjar.com/c.js"><p>ok</p>"#;
        let out = sanitize(html, None);
        assert!(!out.contains("facebook.net"));
        assert!(!out.contains("hotjar.com"));
    }

    #[test]
    fn extract_text_strips_tags() {
        let html = "<h1>Hello</h1><p>World</p>";
        assert_eq!(extract_text(html), "Hello World");
    }
}
