//! Autocomplete cache.

use dashmap::DashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub visit_count: u32,
    pub last_visit: i64,
    /// `typed` outranks `link` in scoring.
    pub transition: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub url: String,
    pub title: String,
    pub score: i64,
}

/// Entry ceiling: cold rows live in the vault with DB fallback, so the
/// hot map never needs to be the whole history. Eviction keeps the most
/// recently visited (10% headroom batches the O(n) scan).
pub const AUTOCOMPLETE_CAP: usize = 5000;

#[derive(Debug, Clone, Default)]
pub struct AutocompleteCache {
    inner: Arc<DashMap<String, HistoryEntry>>,
    /// Bumped on every mutation. A repeat query with the same (prefix,
    /// limit, generation) reuses the last result instead of rescanning.
    generation: Arc<std::sync::atomic::AtomicU64>,
    last_query: Arc<std::sync::Mutex<Option<CachedQuery>>>,
}

#[derive(Debug, Clone)]
struct CachedQuery {
    prefix: String,
    limit: usize,
    generation: u64,
    results: Vec<Suggestion>,
}

/// Visits dominate, recency breaks ties, typed outranks linked.
fn frecency_score(e: &HistoryEntry) -> i64 {
    let recency_bonus = e.last_visit / 1_000_000;
    let typed_bonus = if e.transition == "typed" { 10_000 } else { 0 };
    i64::from(e.visit_count) * 100 + recency_bonus + typed_bonus
}

fn top_by_score(mut matches: Vec<Suggestion>, limit: usize) -> Vec<Suggestion> {
    matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.url.cmp(&b.url)));
    matches.truncate(limit);
    matches
}

impl AutocompleteCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_query: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn insert(&self, url: String, title: String, now_ms: i64) {
        self.insert_with_transition(url, title, now_ms, "link");
    }

    pub fn insert_with_transition(
        &self,
        url: String,
        title: String,
        now_ms: i64,
        transition: &str,
    ) {
        let mut entry = self
            .inner
            .entry(url.clone())
            .or_insert_with(|| HistoryEntry {
                url: url.clone(),
                title: title.clone(),
                visit_count: 0,
                last_visit: now_ms,
                transition: "link".to_owned(),
            });
        entry.visit_count = entry.visit_count.saturating_add(1);
        entry.last_visit = now_ms;
        if !title.trim().is_empty() {
            entry.title = title;
        }
        entry.transition = transition.to_owned();
        drop(entry);
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.evict_overflow();
    }

    /// Shed stalest-visited rows down to the cap when 10% over. Cold
    /// rows stay queryable through the vault DB fallback; the map is a
    /// hot cache, not the store of record.
    fn evict_overflow(&self) {
        if self.inner.len() < AUTOCOMPLETE_CAP + AUTOCOMPLETE_CAP / 10 {
            return;
        }
        let mut rows: Vec<(String, i64)> = self
            .inner
            .iter()
            .map(|e| (e.key().clone(), e.value().last_visit))
            .collect();
        rows.sort_by_key(|(_, visit)| *visit);
        let drop = rows.len().saturating_sub(AUTOCOMPLETE_CAP);
        for (url, _) in rows.into_iter().take(drop) {
            self.inner.remove(&url);
        }
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn clear(&self) {
        self.inner.clear();
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Drop one URL. Returns whether it existed.
    pub fn remove(&self, url: &str) -> bool {
        let gone = self.inner.remove(url).is_some();
        if gone {
            self.generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        gone
    }

    /// Case-insensitive substring match, up to `limit`.
    pub fn query(&self, prefix: &str, limit: usize) -> Vec<Suggestion> {
        if prefix.trim().is_empty() {
            return Vec::new();
        }
        let current_gen = self.generation.load(std::sync::atomic::Ordering::Relaxed);
        if let Ok(cached) = self.last_query.lock()
            && let Some(q) = cached.as_ref()
            && q.generation == current_gen
            && q.prefix == prefix
            && q.limit == limit
        {
            return q.results.clone();
        }
        let needle = prefix.to_lowercase();
        let mut matches: Vec<Suggestion> = Vec::new();

        for entry in self.inner.iter() {
            let e = entry.value();
            let hay_url = e.url.to_lowercase();
            let hay_title = e.title.to_lowercase();
            if hay_url.contains(&needle) || hay_title.contains(&needle) {
                matches.push(Suggestion {
                    url: e.url.clone(),
                    title: e.title.clone(),
                    score: frecency_score(e),
                });
            }
        }

        let matches = top_by_score(matches, limit);
        if let Ok(mut cached) = self.last_query.lock() {
            *cached = Some(CachedQuery {
                prefix: prefix.to_owned(),
                limit,
                generation: current_gen,
                results: matches.clone(),
            });
        }
        matches
    }

    /// Top by frecency, unfiltered. No query cache (focus opens are rare).
    pub fn top(&self, limit: usize) -> Vec<Suggestion> {
        let matches: Vec<Suggestion> = self
            .inner
            .iter()
            .map(|entry| {
                let e = entry.value();
                Suggestion {
                    url: e.url.clone(),
                    title: e.title.clone(),
                    score: frecency_score(e),
                }
            })
            .collect();
        top_by_score(matches, limit)
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
    fn overflow_evicts_stalest_keeps_newest() {
        let c = AutocompleteCache::new();
        for i in 0..(AUTOCOMPLETE_CAP + 600) {
            c.insert(
                format!("https://site{i}.com"),
                format!("Site {i}"),
                i as i64,
            );
        }
        assert!(c.len() <= AUTOCOMPLETE_CAP + AUTOCOMPLETE_CAP / 10);
        assert!(c.len() >= AUTOCOMPLETE_CAP - 1);
        // Newest survives; oldest is gone.
        assert!(c.inner.contains_key("https://site5599.com"));
        assert!(!c.inner.contains_key("https://site0.com"));
    }

    #[test]
    fn query_substring_case_insensitive() {
        let c = cache_with_entries();
        let results = c.query("rust", 5);
        assert_eq!(results.len(), 2);
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
        c.insert("https://a.com/foo".to_string(), "A".to_string(), 101);
        c.insert("https://a.com/foo".to_string(), "A".to_string(), 102);
        let results = c.query("foo", 5);
        assert_eq!(results[0].url, "https://a.com/foo");
    }

    #[test]
    fn scoring_prefers_typed_over_linked() {
        let c = AutocompleteCache::new();
        for _ in 0..30 {
            c.insert("https://linked.test/foo".to_string(), "L".to_string(), 100);
        }
        c.insert_with_transition(
            "https://typed.test/foo".to_string(),
            "T".to_string(),
            100,
            "typed",
        );
        let results = c.query("foo", 5);
        assert_eq!(results[0].url, "https://typed.test/foo");
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

    #[test]
    fn repeated_query_served_from_cache() {
        let c = cache_with_entries();
        let first = c.query("rust", 5);
        let second = c.query("rust", 5);
        assert_eq!(first, second);
        c.insert(
            "https://rust-new.example".to_string(),
            "Rust New".to_string(),
            9999,
        );
        let third = c.query("rust", 5);
        assert!(third.iter().any(|s| s.url.contains("rust-new")));
    }

    #[test]
    fn top_returns_frecency_order_unfiltered() {
        let c = cache_with_entries();
        let top = c.top(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].url, "https://example.com");
        assert!(c.top(0).is_empty());
        assert!(AutocompleteCache::new().top(5).is_empty());
    }

    #[test]
    fn remove_drops_one_url() {
        let c = cache_with_entries();
        assert!(c.remove("https://example.com"));
        assert!(!c.remove("https://example.com"));
        assert!(c.query("example", 5).is_empty());
        assert_eq!(c.query("rust", 5).len(), 2);
    }
}
