//! Search engines and switching.

use crate::error::{OmniboxError, OmniboxResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchEngine {
    pub id: &'static str,
    pub name: &'static str,
    pub query_template: &'static str,
}

pub const ENGINES: &[SearchEngine] = &[
    SearchEngine {
        id: "google",
        name: "Google",
        query_template: "https://www.google.com/search?q={query}",
    },
    SearchEngine {
        id: "duckduckgo",
        name: "DuckDuckGo",
        query_template: "https://duckduckgo.com/?q={query}",
    },
    SearchEngine {
        id: "brave",
        name: "Brave Search",
        query_template: "https://search.brave.com/search?q={query}",
    },
    SearchEngine {
        id: "bing",
        name: "Bing",
        query_template: "https://www.bing.com/search?q={query}",
    },
];

fn find_engine(id: &str) -> Option<SearchEngine> {
    ENGINES.iter().copied().find(|e| e.id == id)
}

pub(crate) fn encode_query(query: &str) -> String {
    url::form_urlencoded::byte_serialize(query.as_bytes()).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CustomEngine {
    pub id: String,
    pub name: String,
    /// Template with a literal `%s` for the encoded query.
    pub template: String,
}

#[derive(Debug, Clone)]
pub struct SearchEngineRegistry {
    current_id: String,
    /// Private default. None follows the regular default.
    private_id: Option<String>,
    custom: Vec<CustomEngine>,
}

impl Default for SearchEngineRegistry {
    fn default() -> Self {
        Self {
            current_id: "duckduckgo".to_owned(),
            private_id: None,
            custom: Vec::new(),
        }
    }
}

impl SearchEngineRegistry {
    pub fn new(default_id: &str) -> Self {
        if find_engine(default_id).is_some() {
            Self {
                current_id: default_id.to_owned(),
                private_id: None,
                custom: Vec::new(),
            }
        } else {
            Self::default()
        }
    }

    pub fn current_id(&self) -> &str {
        &self.current_id
    }

    /// Private default when set, else the regular default. Never dangles.
    pub fn effective_id(&self, private: bool) -> &str {
        if private {
            self.private_id.as_deref().unwrap_or(&self.current_id)
        } else {
            &self.current_id
        }
    }

    pub fn private_id(&self) -> Option<&str> {
        self.private_id.as_deref()
    }

    pub fn current(&self) -> SearchEngine {
        find_engine(&self.current_id).unwrap_or(ENGINES[1])
    }

    pub fn all(&self) -> &'static [SearchEngine] {
        ENGINES
    }

    pub fn set_current(&mut self, id: &str) -> OmniboxResult<String> {
        if find_engine(id).is_none() && !self.custom.iter().any(|e| e.id == id) {
            return Err(OmniboxError::UnknownEngine { id: id.to_owned() });
        }
        let prev = self.current_id.clone();
        self.current_id = id.to_owned();
        Ok(prev)
    }

    pub fn set_private(&mut self, id: &str) -> OmniboxResult<()> {
        if find_engine(id).is_none() && !self.custom.iter().any(|e| e.id == id) {
            return Err(OmniboxError::UnknownEngine { id: id.to_owned() });
        }
        self.private_id = Some(id.to_owned());
        Ok(())
    }

    pub fn clear_private(&mut self) {
        self.private_id = None;
    }

    pub fn search_url(&self, engine_id: Option<&str>, query: &str) -> OmniboxResult<String> {
        match engine_id {
            Some(id) => self.search_url_for(id, query),
            None => self.search_url_for(&self.current_id.clone(), query),
        }
    }

    /// Ids are `custom:` namespaced, so they never collide with built-ins.
    pub fn custom_engines(&self) -> &[CustomEngine] {
        &self.custom
    }

    /// Template must contain one `%s` and start with http(s).
    pub fn add_custom(&mut self, name: &str, template: &str) -> OmniboxResult<String> {
        let name = name.trim();
        let template = template.trim();
        if name.is_empty() || !template.contains("%s") {
            return Err(OmniboxError::UnknownEngine {
                id: name.to_owned(),
            });
        }
        if !(template.starts_with("https://") || template.starts_with("http://")) {
            return Err(OmniboxError::UnknownEngine {
                id: name.to_owned(),
            });
        }
        let slug: String = name
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        if slug.is_empty() {
            return Err(OmniboxError::UnknownEngine {
                id: name.to_owned(),
            });
        }
        let id = format!("custom:{slug}");
        if let Some(e) = self.custom.iter_mut().find(|e| e.id == id) {
            e.name = name.to_owned();
            e.template = template.to_owned();
        } else {
            self.custom.push(CustomEngine {
                id: id.clone(),
                name: name.to_owned(),
                template: template.to_owned(),
            });
        }
        Ok(id)
    }

    pub fn remove_custom(&mut self, id: &str) -> bool {
        let before = self.custom.len();
        self.custom.retain(|e| e.id != id);
        if self.current_id == id {
            self.current_id = "duckduckgo".to_owned();
        }
        if self.private_id.as_deref() == Some(id) {
            self.private_id = None;
        }
        self.custom.len() != before
    }

    pub fn customs_clear(&mut self) {
        self.custom.clear();
        if self.current_id.starts_with("custom:") {
            self.current_id = "duckduckgo".to_owned();
        }
        self.private_id = None;
    }

    /// Search URL honoring custom engines (built-ins take precedence).
    pub fn search_url_any(&self, query: &str) -> OmniboxResult<String> {
        self.search_url_for(&self.current_id.clone(), query)
    }

    /// Unknown ids are an error. Callers fall back to classify.
    pub fn search_url_for(&self, id: &str, query: &str) -> OmniboxResult<String> {
        let encoded = encode_query(query);
        if let Some(c) = self.custom.iter().find(|e| e.id == id) {
            return Ok(c.template.replace("%s", &encoded));
        }
        let engine =
            find_engine(id).ok_or_else(|| OmniboxError::UnknownEngine { id: id.to_owned() })?;
        Ok(engine.query_template.replace("{query}", &encoded))
    }

    /// Match typed text against provider hosts. Submit fills the match.
    pub fn match_tab_search(&self, text: &str) -> Option<TabSearchMatch> {
        tab_search_match(text, self.provider_entries())
    }

    fn provider_entries(&self) -> Vec<(&str, &str, Option<String>)> {
        let mut out = Vec::with_capacity(ENGINES.len() + self.custom.len());
        for e in ENGINES {
            out.push((e.id, e.name, provider_host(e.query_template)));
        }
        for c in &self.custom {
            out.push((c.id.as_str(), c.name.as_str(), provider_host(&c.template)));
        }
        out
    }

    pub fn contains(&self, id: &str) -> bool {
        find_engine(id).is_some()
    }

    pub fn knows(&self, id: &str) -> bool {
        find_engine(id).is_some() || self.custom.iter().any(|e| e.id == id)
    }
}

/// Tab-to-search match: the engine id + display name to commit to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabSearchMatch {
    pub id: String,
    pub name: String,
}

/// Lowercase template host. None when unparseable.
pub fn provider_host(template: &str) -> Option<String> {
    url::Url::parse(template)
        .ok()?
        .host_str()
        .map(|h| h.to_lowercase())
}

/// First token matches on equality or prefix (min 3 chars, `www` ignored).
/// Spaces mean a query, never a provider name.
pub fn tab_search_match(
    text: &str,
    providers: Vec<(&str, &str, Option<String>)>,
) -> Option<TabSearchMatch> {
    // Scheme stripped, then the first token up to space or slash.
    let mut rest = text.trim_start().to_lowercase();
    for scheme in ["https://", "http://"] {
        if let Some(stripped) = rest.strip_prefix(scheme) {
            rest = stripped.to_owned();
            break;
        }
    }
    let token: String = rest
        .split([' ', '\t', '\n', '/'])
        .next()
        .unwrap_or("")
        .to_owned();
    let frag = token.trim_matches('.').to_owned();
    if frag.chars().count() < 3 || frag.contains(char::is_whitespace) {
        return None;
    }
    let norm = frag.strip_prefix("www.").unwrap_or(&frag);
    for (id, name, host) in providers {
        let Some(h) = host.as_deref() else {
            continue;
        };
        let hn = h.strip_prefix("www.").unwrap_or(h);
        if norm == hn || hn.starts_with(norm) {
            return Some(TabSearchMatch {
                id: id.to_owned(),
                name: name.to_owned(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_duckduckgo() {
        let r = SearchEngineRegistry::default();
        assert_eq!(r.current_id(), "duckduckgo");
        assert_eq!(r.current().name, "DuckDuckGo");
    }

    #[test]
    fn new_with_unknown_falls_back() {
        let r = SearchEngineRegistry::new("unknown");
        assert_eq!(r.current_id(), "duckduckgo");
    }

    #[test]
    fn set_current_switches() {
        let mut r = SearchEngineRegistry::default();
        let prev = r.set_current("google").unwrap();
        assert_eq!(prev, "duckduckgo");
        assert_eq!(r.current_id(), "google");
        assert!(r.set_current("nope").is_err());
    }

    #[test]
    fn search_url_encodes() {
        let r = SearchEngineRegistry::default();
        let url = r.search_url(None, "rust async libraries").unwrap();
        assert_eq!(
            url,
            "https://duckduckgo.com/?q=rust+async+libraries".replace(' ', "+")
        );
        let url2 = r.search_url(Some("google"), "hello world").unwrap();
        assert!(url2.contains("google.com"));
        assert!(url2.contains("hello+world"));
    }

    #[test]
    fn search_url_unknown_engine_err() {
        let r = SearchEngineRegistry::default();
        assert!(r.search_url(Some("nope"), "hi").is_err());
    }

    #[test]
    fn all_contains_four() {
        assert_eq!(SearchEngineRegistry::default().all().len(), 4);
    }

    #[test]
    fn custom_engine_add_search_remove() {
        let mut r = SearchEngineRegistry::default();
        let id = r.add_custom("My Search", "https://s.test/?q=%s").unwrap();
        assert_eq!(id, "custom:mysearch");
        r.set_current(&id).unwrap();
        assert_eq!(r.search_url_any("a b").unwrap(), "https://s.test/?q=a+b");
        assert!(
            r.search_url(Some("google"), "x")
                .unwrap()
                .contains("google")
        );
        r.add_custom("My Search", "https://s2.test/?q=%s").unwrap();
        assert_eq!(r.custom_engines().len(), 1);
        assert!(r.remove_custom(&id));
        assert_eq!(r.current_id(), "duckduckgo");
        assert!(!r.remove_custom(&id));
        assert!(r.add_custom("Bad", "https://s.test/?q=").is_err());
        assert!(r.add_custom("Bad", "ftp://s.test/%s").is_err());
        assert!(r.add_custom("", "https://s.test/%s").is_err());
    }

    #[test]
    fn encode_special_chars() {
        let r = SearchEngineRegistry::default();
        let url = r.search_url(None, "a/b&c=d").unwrap();
        assert!(url.contains("a%2Fb%26c%3Dd") || url.contains("a%2Fb"));
    }

    #[test]
    fn private_default_follows_regular_until_set() {
        let mut r = SearchEngineRegistry::default();
        assert_eq!(r.private_id(), None);
        assert_eq!(r.effective_id(true), "duckduckgo");
        assert_eq!(r.effective_id(false), "duckduckgo");
        r.set_private("google").unwrap();
        assert_eq!(r.private_id(), Some("google"));
        assert_eq!(r.effective_id(true), "google");
        assert_eq!(r.effective_id(false), "duckduckgo");
        assert!(r.set_private("nope").is_err());
        r.clear_private();
        assert_eq!(r.private_id(), None);
        let id = r.add_custom("Priv", "https://p.test/?q=%s").unwrap();
        r.set_private(&id).unwrap();
        assert!(r.remove_custom(&id));
        assert_eq!(r.private_id(), None);
        assert_eq!(r.effective_id(true), "duckduckgo");
    }

    #[test]
    fn search_url_for_covers_builtin_and_custom() {
        let mut r = SearchEngineRegistry::default();
        assert!(
            r.search_url_for("google", "a b")
                .unwrap()
                .contains("google.com")
        );
        assert!(r.search_url_for("nope", "x").is_err());
        let id = r.add_custom("CS", "https://cs.test/?q=%s").unwrap();
        assert_eq!(
            r.search_url_for(&id, "a b").unwrap(),
            "https://cs.test/?q=a+b"
        );
    }

    #[test]
    fn tab_search_matches_provider_hosts() {
        let mut r = SearchEngineRegistry::default();
        let m = r.match_tab_search("google.com").unwrap();
        assert_eq!(m.id, "google");
        assert_eq!(r.match_tab_search("www.google.com").unwrap().id, "google");
        assert_eq!(
            r.match_tab_search("https://google.com/search?q=x")
                .unwrap()
                .id,
            "google"
        );
        assert_eq!(r.match_tab_search("duck").unwrap().id, "duckduckgo");
        assert!(r.match_tab_search("go").is_none());
        assert!(r.match_tab_search("example.com").is_none());
        assert!(r.match_tab_search("").is_none());
        assert!(r.match_tab_search("   ").is_none());
        let id = r.add_custom("Shop", "https://shop.test/find?q=%s").unwrap();
        let m = r.match_tab_search("shop.test shoes").unwrap();
        assert_eq!(m.id, id);
        assert_eq!(m.name, "Shop");
    }

    #[test]
    fn search_url_resolves_custom_explicit_and_current() {
        let mut r = SearchEngineRegistry::default();
        let id = r.add_custom("CS", "https://cs.test/?q=%s").unwrap();
        assert_eq!(
            r.search_url(Some(&id), "a b").unwrap(),
            "https://cs.test/?q=a+b"
        );
        r.set_current(&id).unwrap();
        assert_eq!(
            r.search_url(None, "a b").unwrap(),
            "https://cs.test/?q=a+b",
            "custom current must not silently fall back to DDG"
        );
    }

    #[test]
    fn search_url_unknown_ids_error() {
        let r = SearchEngineRegistry::default();
        assert!(r.search_url(Some("nope"), "x").is_err());
        assert!(r.search_url(Some("custom:nope"), "x").is_err());
    }
}
