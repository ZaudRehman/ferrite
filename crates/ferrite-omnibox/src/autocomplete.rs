//! Autocomplete cache.

use dashmap::DashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub visit_count: u32,
    pub last_visit: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub url: String,
    pub title: String,
    pub score: i64,
}

#[derive(Debug, Clone, Default)]
pub struct AutocompleteCache {
    inner: Arc<DashMap<String, HistoryEntry>>,
}

impl AutocompleteCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    pub fn insert(&self, url: String, title: String, now_ms: i64) {
        let mut entry = self
            .inner
            .entry(url.clone())
            .or_insert_with(|| HistoryEntry {
                url: url.clone(),
                title: title.clone(),
                visit_count: 0,
                last_visit: now_ms,
            });
        entry.visit_count = entry.visit_count.saturating_add(1);
        entry.last_visit = now_ms;
        if !title.trim().is_empty() {
            entry.title = title;
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Clear all entries.
    pub fn clear(&self) {
        self.inner.clear();
    }

    /// Query by case-insensitive substring, returning up to `limit` suggestions.
    pub fn query(&self, prefix: &str, limit: usize) -> Vec<Suggestion> {
        if prefix.trim().is_empty() {
            return Vec::new();
        }
        let needle = prefix.to_lowercase();
        let mut matches: Vec<Suggestion> = Vec::new();

        for entry in self.inner.iter() {
            let e = entry.value();
            let hay_url = e.url.to_lowercase();
            let hay_title = e.title.to_lowercase();
            if hay_url.contains(&needle) || hay_title.contains(&needle) {
                let recency_bonus = e.last_visit / 1_000_000;
                let score = i64::from(e.visit_count) * 100 + recency_bonus;
                matches.push(Suggestion {
                    url: e.url.clone(),
                    title: e.title.clone(),
                    score,
                });
            }
        }

        matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.url.cmp(&b.url)));
        matches.truncate(limit);
        matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with_entries() -> AutocompleteCache {
        let c = AutocompleteCache::new();
        c.insert(
            "https://example.com".to_string(),
            "Example".to_string(),
            1000,
        );
        c.insert(
            "https://rust-lang.org".to_string(),
            "Rust".to_string(),
            2000,
        );
        c.insert(
            "https://github.com/rust-lang/rust".to_string(),
            "GitHub Rust".to_string(),
            3000,
        );
        // bump example.com again
        c.insert(
            "https://example.com".to_string(),
            "Example".to_string(),
            4000,
        );
        c
    }

    #[test]
    fn insert_bumps_count() {
        let c = AutocompleteCache::new();
        c.insert("https://a.com".to_string(), "A".to_string(), 100);
        c.insert("https://a.com".to_string(), "A2".to_string(), 200);
        assert_eq!(c.len(), 1);
        let entry = c.inner.get("https://a.com").unwrap();
        assert_eq!(entry.visit_count, 2);
        assert_eq!(entry.title, "A2");
    }

    #[test]
    fn query_substring_case_insensitive() {
        let c = cache_with_entries();
        let results = c.query("rust", 5);
        assert_eq!(results.len(), 2);
        // github.com/rust-lang/rust has higher visit_count/recency than rust-lang.org? Both 1 visit, but github last_visit 3000 > 2000
        assert!(results[0].url.contains("github"));
        assert!(results[1].url.contains("rust-lang.org"));
    }

    #[test]
    fn query_limits_to_top_n() {
        let c = AutocompleteCache::new();
        for i in 0..10 {
            c.insert(
                format!("https://site{}.com", i),
                format!("Site {}", i),
                i as i64,
            );
        }
        let results = c.query("site", 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn query_empty_returns_none() {
        let c = cache_with_entries();
        assert!(c.query("", 5).is_empty());
        assert!(c.query("   ", 5).is_empty());
    }

    #[test]
    fn query_no_match_empty() {
        let c = cache_with_entries();
        assert!(c.query("nope", 5).is_empty());
    }

    #[test]
    fn scoring_prefers_visit_count() {
        let c = AutocompleteCache::new();
        c.insert("https://a.com/foo".to_string(), "A".to_string(), 100);
        c.insert("https://b.com/foo".to_string(), "B".to_string(), 100);
        // bump a
        c.insert("https://a.com/foo".to_string(), "A".to_string(), 101);
        c.insert("https://a.com/foo".to_string(), "A".to_string(), 102);
        let results = c.query("foo", 5);
        assert_eq!(results[0].url, "https://a.com/foo");
    }

    #[test]
    fn latency_under_16ms() {
        let c = AutocompleteCache::new();
        for i in 0..5000 {
            c.insert(
                format!("https://site{}.com/page", i),
                format!("Site {}", i),
                i as i64,
            );
        }
        let start = std::time::Instant::now();
        let results = c.query("site2500", 5);
        let elapsed = start.elapsed();
        assert!(!results.is_empty());
        assert!(
            elapsed.as_millis() < 16,
            "autocomplete took {}ms, expected <16ms",
            elapsed.as_millis()
        );
    }

    #[test]
    fn clear_empties() {
        let c = cache_with_entries();
        assert!(!c.is_empty());
        c.clear();
        assert!(c.is_empty());
    }
}
