//! Regex sanitizer for archived HTML. SVG and MathML are kept.

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

/// Heuristic pre-filter for well-known tracking pixels/beacons in STORED
/// snapshots only. This is hygiene, not enforcement: the name list is trivial
/// to evade and is not updated like the filter lists. Real tracker blocking
/// happens proxy-side (`ferrite-privacy`); archived pages additionally serve
/// with `script-src 'none'`, so a missed beacon here can neither run nor phone
/// home. Kept because stripping the obvious ones keeps snapshots smaller and
/// the FTS index cleaner.
fn tracker_re() -> &'static Regex {
    TRACKER_RE.get_or_init(|| {
        Regex::new(r#"(?i)<(img|link)\b[^>]*\b(src|href)\s*=\s*["'][^"']*(tracker|analytics|doubleclick|googletag|facebook|hotjar|segment|mixpanel)[^"']*["'][^>]*>"#).unwrap()
    })
}

fn handler_re() -> &'static Regex {
    HANDLER_RE
        .get_or_init(|| Regex::new(r#"(?i)\s+on[a-z]+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#).unwrap())
}

fn js_url_re() -> &'static Regex {
    JS_URL_RE.get_or_init(|| {
        Regex::new(r#"(?i)\b(href|src|action|xlink:href)\s*=\s*("([^"]*)"|'([^']*)'|[^\s>]+)"#)
            .unwrap()
    })
}

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

fn same_host(url_src: &str, page_host: &str) -> bool {
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
    // Protocol relative URLs are parsed, never trusted on sight.
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
    // No host: same document, root, or relative reference.
    url_src.starts_with('#') || url_src.starts_with('/') || !url_src.contains(':')
}

/// Keep same origin frames, drop the rest. `None` drops all frames.
pub fn sanitize(html: &str, page_host: Option<&str>) -> String {
    let mut out = html.to_owned();
    // Nested input reforms tags after one pass, so run up to 3.
    for _ in 0..3 {
        let before = out.len();
        out = script_re().replace_all(&out, "").into_owned();
        out = script_open_re().replace_all(&out, "").into_owned();
        if out.len() == before {
            break;
        }
    }
    out = handler_re().replace_all(&out, "").into_owned();
    out = neutralize_js_urls(&out);
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
                if lower.contains("charset") || lower.contains("name=") {
                    return tag.to_owned();
                }
                return String::new();
            }
            String::new()
        })
        .into_owned();

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
        // srcdoc runs in the archive origin, so always remove it.
        out = iframe_re()
            .replace_all(&out, |caps: &regex::Captures| {
                let tag = caps.get(0).unwrap().as_str();
                let lower = tag.to_ascii_lowercase();
                if lower.contains("srcdoc") {
                    return String::new();
                }
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

pub fn extract_text(html: &str) -> String {
    let text = tag_re().replace_all(html, " ");
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
    fn adversarial_markup_never_executes() {
        // Deterministic adversarial corpus (no external fuzzer needed for
        // this surface): sanitize is total over hostile markup: it must
        // never panic and must never emit executable shapes. Bare inert text
        // (e.g. the word "javascript:" with no tag or attribute around it)
        // is fine: the archive viewer serves `script-src 'none'` regardless.
        let mut state: u64 = 0xC2B280B5E1479355;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const SHARDS: &[&str] = &[
            "<script",
            "</script",
            "<scr<script>ipt>",
            "javascript:",
            "JaVaScRiPt:",
            "onload=",
            "onerror=",
            "<iframe",
            "srcdoc=",
            "<svg",
            "<!--",
            "-->",
            "%3c",
            "&lt;",
            "<object",
            "<embed",
            "<meta",
            "<base",
            "<form",
            "<link",
            "<style",
            "expression(",
            "url(",
            "data:text/html",
        ];
        for _ in 0..2000 {
            let len = (next() % 12) as usize;
            let mut input = String::from("<p>safe</p>");
            for _ in 0..len {
                input.push_str(SHARDS[(next() % SHARDS.len() as u64) as usize]);
            }
            let out = sanitize(&input, Some("example.com"));
            let lower = out.to_ascii_lowercase();
            assert!(!has_script_opener(&lower), "exec tag for {input:?}");
            assert!(!has_js_url(&lower), "exec url for {input:?}");
        }
    }

    /// `javascript:` URL in a URL-bearing attribute (`href`/`src`/`action`/
    /// `xlink:href`, quoted or bare). Bare inert text mentioning the scheme
    /// correctly survives (the viewer runs `script-src 'none'` regardless).
    fn has_js_url(lower: &str) -> bool {
        const ATTRS: &[&str] = &["href", "src", "action", "xlink:href"];
        for attr in ATTRS {
            let mut rest = lower;
            while let Some(i) = rest.find(attr) {
                let after_attr = &rest[i + attr.len()..];
                let val = after_attr.trim_start_matches([' ', '\t', '\n', '\r']);
                let Some(eq) = val.strip_prefix('=') else {
                    rest = after_attr;
                    continue;
                };
                let val = eq.trim_start_matches([' ', '\t', '\n', '\r', '"', '\'']);
                if val.starts_with("javascript:") {
                    return true;
                }
                rest = after_attr;
            }
        }
        false
    }

    /// `<script` tag opener that a browser could act on: the name followed
    /// by whitespace, `/`, or `>`. `<scriptdata` is inert tag soup and a
    /// trailing unterminated `<script` at EOF tokenizes as text per the HTML
    /// spec; both correctly survive sanitizing.
    fn has_script_opener(lower: &str) -> bool {
        let mut rest = lower;
        while let Some(i) = rest.find("<script") {
            let after = rest[i + "<script".len()..].chars().next();
            match after {
                Some(c) if c.is_whitespace() || c == '>' || c == '/' => return true,
                _ => {
                    rest = &rest[i + "<script".len()..];
                }
            }
        }
        false
    }

    #[test]
    fn extract_text_strips_tags() {
        let html = "<h1>Hello</h1><p>World</p>";
        assert_eq!(extract_text(html), "Hello World");
    }
}
