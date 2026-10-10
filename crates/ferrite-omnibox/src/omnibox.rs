//! Omnibox state and classification.

use crate::{
    error::{OmniboxError, OmniboxResult},
    search::SearchEngineRegistry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OmniboxAction {
    Navigate {
        url: String,
    },
    Search {
        query: String,
        engine_id: String,
        search_url: String,
    },
    Internal {
        uri: String,
    },
}

#[derive(Debug, Default)]
pub struct OmniboxState {
    pub input: String,
    pub registry: SearchEngineRegistry,
}

impl OmniboxState {
    /// Max input chars. Longer pastes are truncated.
    pub const MAX_INPUT_LEN: usize = 2048;

    pub fn new(registry: SearchEngineRegistry) -> Self {
        Self {
            input: String::new(),
            registry,
        }
    }

    pub fn set_input(&mut self, raw: String) {
        self.input = truncate_chars(&raw, Self::MAX_INPUT_LEN);
    }

    pub fn clear(&mut self) {
        self.input.clear();
    }

    pub fn classify(&self, raw: &str) -> OmniboxResult<OmniboxAction> {
        self.classify_for(raw, false)
    }

    /// Private windows use the private default when set. Routing never differs.
    pub fn classify_for(&self, raw: &str, private: bool) -> OmniboxResult<OmniboxAction> {
        let trimmed = truncate_chars(raw.trim(), Self::MAX_INPUT_LEN);
        let trimmed = trimmed.as_str();
        if trimmed.is_empty() {
            return Err(OmniboxError::EmptyInput);
        }

        // Always internal, even with trailing text, so it never leaks to search.
        if trimmed.len() >= 10 && trimmed[..10].eq_ignore_ascii_case("ferrite://") {
            return Ok(OmniboxAction::Internal {
                uri: trimmed.to_owned(),
            });
        }

        // `view-source:` maps to the internal viewer, target kept verbatim.
        if trimmed.len() >= 12 && trimmed[..12].eq_ignore_ascii_case("view-source:") {
            let target = trimmed[12..].trim();
            if target.is_empty() {
                return Err(OmniboxError::EmptyInput);
            }
            return Ok(OmniboxAction::Internal {
                uri: format!(
                    "ferrite://viewsource?u={}",
                    crate::search::encode_query(target)
                ),
            });
        }

        if let Some(url) = try_as_url(trimmed) {
            return Ok(OmniboxAction::Navigate { url });
        }

        let query = trimmed.to_owned();
        let engine_id = self.registry.effective_id(private).to_owned();
        let search_url = self.registry.search_url_for(&engine_id, &query)?;
        Ok(OmniboxAction::Search {
            query,
            engine_id,
            search_url,
        })
    }

    pub fn url_for_input(&self, raw: &str) -> OmniboxResult<String> {
        self.url_for_input_for(raw, false)
    }

    pub fn url_for_input_for(&self, raw: &str, private: bool) -> OmniboxResult<String> {
        match self.classify_for(raw, private)? {
            OmniboxAction::Navigate { url } => Ok(url),
            OmniboxAction::Search { search_url, .. } => Ok(search_url),
            OmniboxAction::Internal { uri } => Ok(uri),
        }
    }
}

/// Split into (scheme, subdomain, domain, path). Host is punycode, so
/// homograph glyphs cannot reach display. None for hostless input.
/// The domain is the PSL registrable root (`example.co.uk`, not `co.uk`),
/// so emphasis never bolds a public suffix.
pub fn display_parts(raw: &str) -> Option<(String, String, String, String)> {
    let url = url::Url::parse(raw).ok()?;
    let host = url.host_str()?;
    if host.is_empty() {
        return None;
    }
    let domain = host.to_owned();
    let (sub, dom) = split_registrable(&domain);
    let mut path = url.path().to_owned();
    if path == "/" {
        path.clear();
    }
    if let Some(q) = url.query() {
        path.push('?');
        path.push_str(q);
    }
    Some((url.scheme().to_owned(), sub, dom, path))
}

/// Split a host into (subdomain-prefix, registrable domain) with the
/// trailing-dot convention the renderer expects (`mail.` + `example.com`).
/// PSL root when known (`mail.example.co.uk` → `mail.` + `example.co.uk`);
/// IPs and unknown shapes fall back to last-two-labels, never to the bare
/// host (which would leave nothing emphasized and hide the real domain).
fn split_registrable(domain: &str) -> (String, String) {
    if let Ok(name) = addr::parse_domain_name(domain)
        && name.has_known_suffix()
        && let Some(root) = name.root()
        && !root.is_empty()
    {
        let sub = domain
            .strip_suffix(root)
            .unwrap_or("")
            .trim_end_matches('.')
            .to_owned();
        let sub = if sub.is_empty() {
            String::new()
        } else {
            format!("{sub}.")
        };
        return (sub, root.to_owned());
    }
    let parts: Vec<&str> = domain.split('.').collect();
    if parts.len() >= 2 {
        let d = parts[parts.len() - 2..].join(".");
        (domain[..domain.len() - d.len()].to_owned(), d)
    } else {
        (String::new(), domain.to_owned())
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    // Walk back to a char boundary after the byte cut.
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// Blocked schemes fall through to search, never navigate.
fn is_blocked_scheme(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    ["javascript:", "data:", "file:", "vbscript:", "blob:"]
        .iter()
        .any(|s| lower.starts_with(s))
}

/// Windows drive paths (`C:\...`, `C:/...`) are searches, not URLs.
fn is_windows_drive_path(input: &str) -> bool {
    let b = input.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

fn try_as_url(input: &str) -> Option<String> {
    if input.contains(' ') {
        return None;
    }
    // Control characters never reach `Url::parse`: WHATWG strips ASCII
    // tab/newline before parsing, so `java\tscript:`-shaped input would
    // dodge the scheme blocklist below yet normalize into a blocked scheme
    // (or a userinfo swap like `trusted.com\t@evil.com`). Searches may
    // contain anything; navigations may not contain controls.
    if input.chars().any(|c| c.is_control()) {
        return None;
    }
    if is_blocked_scheme(input) || is_windows_drive_path(input) {
        return None;
    }

    if input.contains("://") {
        if let Ok(url) = url::Url::parse(input)
            && url.has_host()
        {
            return Some(url.to_string());
        }
        return None;
    }

    let with_scheme = if input.starts_with("localhost")
        || input.starts_with("127.0.0.1")
        || input.starts_with("[::1]")
    {
        format!("http://{input}")
    } else {
        format!("https://{input}")
    };

    let parsed = url::Url::parse(&with_scheme).ok()?;
    let host = parsed.host_str()?;

    // Bare `:` is a search (`user:pass@host`, `a:b`). Ports still work
    // via the localhost and IP branches above.
    let is_host_like =
        host.contains('.') || host == "localhost" || host.parse::<std::net::IpAddr>().is_ok();

    if is_host_like {
        Some(parsed.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> OmniboxState {
        OmniboxState::default()
    }

    #[test]
    fn empty_is_err() {
        assert!(matches!(
            state().classify(""),
            Err(OmniboxError::EmptyInput)
        ));
        assert!(matches!(
            state().classify("   "),
            Err(OmniboxError::EmptyInput)
        ));
        assert!(matches!(
            state().classify("view-source:"),
            Err(OmniboxError::EmptyInput)
        ));
        assert!(matches!(
            state().classify("view-source:   "),
            Err(OmniboxError::EmptyInput)
        ));
    }

    #[test]
    fn classify_survives_byte_soup() {
        // Deterministic adversarial corpus. Classify is total over pastes:
        // hostile bytes must resolve to Navigate/Search/Internal or a typed
        // error, never panic. Dangerous schemes must never Navigate.
        let s = state();
        let mut state_rng: u64 = 0x2545F4914F6CDD1D;
        let mut next = move || {
            state_rng ^= state_rng << 13;
            state_rng ^= state_rng >> 7;
            state_rng ^= state_rng << 17;
            state_rng
        };
        const SHARDS: &[&str] = &[
            "javascript:",
            "data:",
            "vbscript:",
            "file://",
            "blob:",
            "ferrite://",
            "https://",
            "user:pass@",
            ":1234",
            "C:\\",
            "..",
            "%00",
            "\u{7f}",
            "view-source:",
            "?",
            "#",
            "@",
            " ",
        ];
        for _ in 0..3000 {
            let len = (next() % 40) as usize;
            let mut input = String::new();
            for _ in 0..len {
                input.push_str(SHARDS[(next() % SHARDS.len() as u64) as usize]);
            }
            if let Ok(OmniboxAction::Navigate { url }) = s.classify(&input) {
                let lower = url.to_lowercase();
                assert!(
                    !lower.starts_with("javascript:")
                        && !lower.starts_with("data:")
                        && !lower.starts_with("vbscript:")
                        && !lower.starts_with("file:"),
                    "blocked scheme navigated for {input:?}"
                );
            }
        }
    }

    #[test]
    fn view_source_prefix_maps_to_internal_viewer() {
        let s = state();
        assert_eq!(
            s.classify("view-source:https://example.com/a?b=c").unwrap(),
            OmniboxAction::Internal {
                uri: "ferrite://viewsource?u=https%3A%2F%2Fexample.com%2Fa%3Fb%3Dc".to_owned()
            }
        );
        // Case-insensitive prefix, verbatim target.
        assert_eq!(
            s.classify("VIEW-SOURCE:https://example.com").unwrap(),
            OmniboxAction::Internal {
                uri: "ferrite://viewsource?u=https%3A%2F%2Fexample.com".to_owned()
            }
        );
    }

    #[test]
    fn ferrite_internal() {
        let s = state();
        assert_eq!(
            s.classify("ferrite://settings").unwrap(),
            OmniboxAction::Internal {
                uri: "ferrite://settings".to_owned()
            }
        );
        assert_eq!(
            s.classify("ferrite://newtab").unwrap(),
            OmniboxAction::Internal {
                uri: "ferrite://newtab".to_owned()
            }
        );
    }

    #[test]
    fn ferrite_scheme_case_insensitive_and_never_searches() {
        let s = state();
        assert!(matches!(
            s.classify("FERRITE://newtab").unwrap(),
            OmniboxAction::Internal { .. }
        ));
        // Trailing text stays internal, never leaks to search.
        assert!(matches!(
            s.classify("ferrite://newtab evil").unwrap(),
            OmniboxAction::Internal { .. }
        ));
    }

    #[test]
    fn credential_and_path_inputs_are_searches() {
        let s = state();
        assert!(matches!(
            s.classify("user:pass@host").unwrap(),
            OmniboxAction::Search { .. }
        ));
        assert!(matches!(
            s.classify("a:b").unwrap(),
            OmniboxAction::Search { .. }
        ));
        assert!(matches!(
            s.classify("foo:123").unwrap(),
            OmniboxAction::Search { .. }
        ));
        assert!(matches!(
            s.classify(r"C:\Windows\file").unwrap(),
            OmniboxAction::Search { .. }
        ));
        assert!(matches!(
            s.classify("javascript:alert(1)").unwrap(),
            OmniboxAction::Search { .. }
        ));
        assert!(matches!(
            s.classify("data:text/html,hi").unwrap(),
            OmniboxAction::Search { .. }
        ));
    }

    #[test]
    fn control_characters_never_navigate() {
        // WHATWG strips tab/newline before parsing, so blocklist checks on
        // the raw text would miss `java\tscript:`-shaped input that
        // normalizes into a blocked scheme (or a userinfo swap). Controls
        // force Search, no matter the shape around them.
        let s = state();
        for hostile in [
            "java\tscript:alert(1)",
            "java\nscript:alert(1)",
            "java\rscript:alert(1)",
            "javascript\n://evil.test",
            "https://trusted.test\t@evil.test/",
            "https://example.com/\u{7f}",
            "data\t:text/html,hi",
        ] {
            assert!(
                matches!(s.classify(hostile).unwrap(), OmniboxAction::Search { .. }),
                "must search, not navigate: {hostile:?}"
            );
        }
        // Ordinary URLs are untouched by the gate.
        assert!(matches!(
            s.classify("https://example.com/a?b=1").unwrap(),
            OmniboxAction::Navigate { .. }
        ));
    }

    #[test]
    fn input_truncated_to_2k() {
        let mut s = state();
        s.set_input("x".repeat(10_000));
        assert_eq!(s.input.len(), OmniboxState::MAX_INPUT_LEN);
        let action = s.classify(&"y".repeat(10_000)).unwrap();
        match action {
            OmniboxAction::Search { search_url, .. } => {
                assert!(search_url.len() < 10_000);
            }
            _ => panic!("expected Search"),
        }
    }

    #[test]
    fn url_with_scheme() {
        let s = state();
        assert_eq!(
            s.classify("https://example.com").unwrap(),
            OmniboxAction::Navigate {
                url: "https://example.com/".to_owned()
            }
        );
        assert_eq!(
            s.classify("http://example.com/path?q=1").unwrap(),
            OmniboxAction::Navigate {
                url: "http://example.com/path?q=1".to_owned()
            }
        );
    }

    #[test]
    fn url_without_scheme_prepends_https() {
        let s = state();
        let action = s.classify("example.com").unwrap();
        match action {
            OmniboxAction::Navigate { url } => assert!(url.starts_with("https://example.com")),
            _ => panic!("expected Navigate"),
        }
    }

    #[test]
    fn localhost_uses_http() {
        let s = state();
        let action = s.classify("localhost:3000").unwrap();
        match action {
            OmniboxAction::Navigate { url } => assert!(url.starts_with("http://localhost")),
            _ => panic!(),
        }
        let action2 = s.classify("127.0.0.1:8080").unwrap();
        match action2 {
            OmniboxAction::Navigate { url } => assert!(url.starts_with("http://127.0.0.1")),
            _ => panic!(),
        }
    }

    #[test]
    fn search_with_spaces() {
        let s = state();
        let action = s.classify("rust async libraries").unwrap();
        match action {
            OmniboxAction::Search {
                query, search_url, ..
            } => {
                assert_eq!(query, "rust async libraries");
                assert!(search_url.contains("duckduckgo.com"));
                assert!(search_url.contains("rust+async+libraries"));
            }
            _ => panic!("expected Search"),
        }
    }

    #[test]
    fn single_word_without_dot_is_search() {
        let s = state();
        let action = s.classify("rust").unwrap();
        assert!(matches!(action, OmniboxAction::Search { .. }));
    }

    #[test]
    fn url_for_input_navigate() {
        let s = state();
        assert_eq!(
            s.url_for_input("example.com").unwrap(),
            "https://example.com/"
        );
    }

    #[test]
    fn url_for_input_search() {
        let s = state();
        let url = s.url_for_input("hello world").unwrap();
        assert!(url.contains("duckduckgo.com"));
    }

    #[test]
    fn url_for_input_internal() {
        let s = state();
        assert_eq!(
            s.url_for_input("ferrite://archive").unwrap(),
            "ferrite://archive"
        );
    }

    #[test]
    fn set_and_clear() {
        let mut s = state();
        s.set_input("hello".to_owned());
        assert_eq!(s.input, "hello");
        s.clear();
        assert!(s.input.is_empty());
    }

    #[test]
    fn display_parts_splits_domain() {
        let (sch, sub, dom, path) = display_parts("https://mail.example.com/a/b?q=1").unwrap();
        assert_eq!(sch, "https");
        assert_eq!(sub, "mail.");
        assert_eq!(dom, "example.com");
        assert_eq!(path, "/a/b?q=1");
    }

    #[test]
    fn display_parts_bolds_registrable_not_suffix() {
        // Multi-label public suffixes must not steal the emphasis:
        // `co.uk` is infrastructure, `example.co.uk` is the domain.
        let (_, sub, dom, _) = display_parts("https://mail.example.co.uk/").unwrap();
        assert_eq!(dom, "example.co.uk");
        assert_eq!(sub, "mail.");
        let (_, sub, dom, _) = display_parts("https://example.co.uk/").unwrap();
        assert_eq!(dom, "example.co.uk");
        assert_eq!(sub, "");
    }

    #[test]
    fn display_parts_homograph_goes_punycode() {
        let (_, _, dom, _) = display_parts("https://ex\u{430}mple.com/").unwrap();
        assert!(
            dom.starts_with("xn--"),
            "mixed script must not render: {dom}"
        );
        // Parser output is ASCII only.
        let (_, _, dom, _) = display_parts("https://m\u{fc}nchen.de/").unwrap();
        assert!(dom.starts_with("xn--"), "{dom}");
        assert!(dom.is_ascii(), "{dom}");
        let (_, _, dom, _) = display_parts("https://example.com/").unwrap();
        assert_eq!(dom, "example.com");
        assert!(display_parts("not a url").is_none());
    }
}
