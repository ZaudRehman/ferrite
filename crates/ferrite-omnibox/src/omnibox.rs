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
    pub last_edit: Option<std::time::Instant>,
    pub registry: SearchEngineRegistry,
    pub focused: bool,
}

impl OmniboxState {
    /// Maximum accepted input length (chars). Pastes beyond this are
    /// truncated so a 10KB paste cannot build a 10KB search URL.
    pub const MAX_INPUT_LEN: usize = 2048;

    pub fn new(registry: SearchEngineRegistry) -> Self {
        Self {
            input: String::new(),
            last_edit: None,
            registry,
            focused: false,
        }
    }

    pub fn set_input(&mut self, raw: String) {
        self.input = truncate_chars(&raw, Self::MAX_INPUT_LEN);
        self.note_edit();
    }

    pub fn request_focus(&mut self) {
        self.focused = true;
    }

    pub fn consume_focus(&mut self) -> bool {
        let was = self.focused;
        self.focused = false;
        was
    }

    pub const AUTOCOMPLETE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(50);

    pub fn note_edit(&mut self) {
        self.last_edit = Some(std::time::Instant::now());
    }

    pub fn effective_query(&self, now: std::time::Instant) -> Option<&str> {
        if self.input.trim().is_empty() {
            return None;
        }
        let settled = match self.last_edit {
            Some(t) => now.duration_since(t) >= Self::AUTOCOMPLETE_DEBOUNCE,
            None => true,
        };
        if settled {
            Some(self.input.as_str())
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.input.clear();
        self.last_edit = None;
    }

    pub fn classify(&self, raw: &str) -> OmniboxResult<OmniboxAction> {
        let trimmed = truncate_chars(raw.trim(), Self::MAX_INPUT_LEN);
        let trimmed = trimmed.as_str();
        if trimmed.is_empty() {
            return Err(OmniboxError::EmptyInput);
        }

        // Internal scheme, case-insensitive, and ALWAYS internal — even with
        // trailing spaces — so `ferrite://...` text never leaks to an
        // external search engine.
        if trimmed.len() >= 10 && trimmed[..10].eq_ignore_ascii_case("ferrite://") {
            return Ok(OmniboxAction::Internal {
                uri: trimmed.to_owned(),
            });
        }

        if let Some(url) = try_as_url(trimmed) {
            return Ok(OmniboxAction::Navigate { url });
        }

        // Fallback: search
        let query = trimmed.to_owned();
        let engine_id = self.registry.current_id().to_owned();
        let search_url = self.registry.search_url(None, &query)?;
        Ok(OmniboxAction::Search {
            query,
            engine_id,
            search_url,
        })
    }

    pub fn url_for_input(&self, raw: &str) -> OmniboxResult<String> {
        match self.classify(raw)? {
            OmniboxAction::Navigate { url } => Ok(url),
            OmniboxAction::Search { search_url, .. } => Ok(search_url),
            OmniboxAction::Internal { uri } => Ok(uri),
        }
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    // Byte cut may split a char boundary; walk back to one.
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// Schemes that must never become navigations (script execution / local
/// file access via a crafted omnibox paste). They fall through to Search.
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

    // A bare `:` no longer forces host-like: `foo:123` and `a:b` are
    // searches (the old rule turned credential-like `user:pass@host`
    // input into navigations). Port forms still work via the
    // localhost/IP branches below.
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
        // Trailing spaces must not leak internal text to the search engine.
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
    fn input_truncated_to_2k() {
        let mut s = state();
        s.set_input("x".repeat(10_000));
        assert_eq!(s.input.len(), OmniboxState::MAX_INPUT_LEN);
        // Classify also truncates defensively.
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
    fn debounce_holds_then_settles() {
        let mut s = state();
        s.set_input("rust".to_owned());
        assert_eq!(s.effective_query(std::time::Instant::now()), None);
        std::thread::sleep(
            OmniboxState::AUTOCOMPLETE_DEBOUNCE + std::time::Duration::from_millis(10),
        );
        assert_eq!(s.effective_query(std::time::Instant::now()), Some("rust"));
    }

    #[test]
    fn debounce_resets_on_new_keystroke_and_clear() {
        let mut s = state();
        s.set_input("ru".to_owned());
        std::thread::sleep(OmniboxState::AUTOCOMPLETE_DEBOUNCE);
        s.note_edit();
        assert_eq!(s.effective_query(std::time::Instant::now()), None);
        s.clear();
        assert_eq!(s.effective_query(std::time::Instant::now()), None);
    }

    #[test]
    fn whitespace_only_never_queries() {
        let mut s = state();
        s.set_input("   ".to_owned());
        std::thread::sleep(OmniboxState::AUTOCOMPLETE_DEBOUNCE);
        assert_eq!(s.effective_query(std::time::Instant::now()), None);
    }

    #[test]
    fn programmatic_fill_without_note_settles_immediately() {
        // Direct field write (e.g. restoring state) bypasses the keystroke
        // path, so no debounce applies.
        let mut s = state();
        s.input = "abc".to_owned();
        assert_eq!(s.effective_query(std::time::Instant::now()), Some("abc"));
    }
    #[test]
    fn focus_request_consume() {
        let mut s = state();
        assert!(!s.consume_focus());
        s.request_focus();
        assert!(s.consume_focus());
        assert!(!s.consume_focus());
    }
}
