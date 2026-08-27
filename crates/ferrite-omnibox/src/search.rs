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

fn encode_query(query: &str) -> String {
    url::form_urlencoded::byte_serialize(query.as_bytes()).collect()
}

#[derive(Debug, Clone)]
pub struct SearchEngineRegistry {
    current_id: String,
}

impl Default for SearchEngineRegistry {
    fn default() -> Self {
        Self {
            current_id: "duckduckgo".to_owned(),
        }
    }
}

impl SearchEngineRegistry {
    pub fn new(default_id: &str) -> Self {
        if find_engine(default_id).is_some() {
            Self {
                current_id: default_id.to_owned(),
            }
        } else {
            Self::default()
        }
    }

    pub fn current_id(&self) -> &str {
        &self.current_id
    }

    pub fn current(&self) -> SearchEngine {
        find_engine(&self.current_id).unwrap_or(ENGINES[1])
    }

    pub fn all(&self) -> &'static [SearchEngine] {
        ENGINES
    }

    pub fn set_current(&mut self, id: &str) -> OmniboxResult<String> {
        if find_engine(id).is_none() {
            return Err(OmniboxError::UnknownEngine { id: id.to_owned() });
        }
        let prev = self.current_id.clone();
        self.current_id = id.to_owned();
        Ok(prev)
    }

    pub fn search_url(&self, engine_id: Option<&str>, query: &str) -> OmniboxResult<String> {
        let engine = match engine_id {
            Some(id) => {
                find_engine(id).ok_or_else(|| OmniboxError::UnknownEngine { id: id.to_owned() })?
            }
            None => self.current(),
        };
        let encoded = encode_query(query);
        Ok(engine.query_template.replace("{query}", &encoded))
    }

    pub fn contains(&self, id: &str) -> bool {
        find_engine(id).is_some()
    }
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
    fn encode_special_chars() {
        let r = SearchEngineRegistry::default();
        let url = r.search_url(None, "a/b&c=d").unwrap();
        assert!(url.contains("a%2Fb%26c%3Dd") || url.contains("a%2Fb"));
    }
}
