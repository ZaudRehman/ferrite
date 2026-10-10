//! Per-tab back/forward history.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NavigationHistory {
    back_stack: Vec<String>,
    forward_stack: Vec<String>,
}

impl NavigationHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, current_url: &str, new_url: &str) {
        if !current_url.is_empty() && current_url != new_url {
            self.back_stack.push(current_url.to_owned());
        }
        self.forward_stack.clear();
    }

    pub fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }

    /// Most recent first, capped at 20.
    pub fn back_list(&self, limit: usize) -> Vec<String> {
        self.back_stack
            .iter()
            .rev()
            .take(limit.min(20))
            .cloned()
            .collect()
    }

    /// Most recent first, capped at 20.
    pub fn forward_list(&self, limit: usize) -> Vec<String> {
        self.forward_stack
            .iter()
            .rev()
            .take(limit.min(20))
            .cloned()
            .collect()
    }

    /// Navigate back, returning the URL to load, or `None` if impossible.
    ///
    /// Pushes `current_url` onto the forward stack.
    pub fn go_back(&mut self, current_url: &str) -> Option<String> {
        let prev = self.back_stack.pop()?;
        if !current_url.is_empty() {
            self.forward_stack.push(current_url.to_owned());
        }
        Some(prev)
    }

    /// Navigate forward, returning the URL to load, or `None` if impossible.
    pub fn go_forward(&mut self, current_url: &str) -> Option<String> {
        let next = self.forward_stack.pop()?;
        if !current_url.is_empty() {
            self.back_stack.push(current_url.to_owned());
        }
        Some(next)
    }

    /// Length of the back stack (for diagnostics and tests).
    pub fn back_len(&self) -> usize {
        self.back_stack.len()
    }

    /// Length of the forward stack.
    pub fn forward_len(&self) -> usize {
        self.forward_stack.len()
    }

    /// Peek the top of the back stack without mutating.
    pub fn peek_back(&self) -> Option<&str> {
        self.back_stack.last().map(String::as_str)
    }

    /// Peek the top of the forward stack.
    pub fn peek_forward(&self) -> Option<&str> {
        self.forward_stack.last().map(String::as_str)
    }

    /// Clear all history (used when closing a tab or resetting).
    pub fn clear(&mut self) {
        self.back_stack.clear();
        self.forward_stack.clear();
    }

    /// Session-load form: drops empty/overlong entries (a hostile file
    /// must not smuggle megabytes of URLs into live tabs) and keeps the
    /// newest `limit` per stack. Order preserved (oldest-first).
    pub fn cleaned(&self, limit: usize) -> Self {
        fn clean(v: &[String], limit: usize) -> Vec<String> {
            let kept: Vec<String> = v
                .iter()
                .filter(|u| !u.is_empty() && u.len() <= 2048)
                .cloned()
                .collect();
            let skip = kept.len().saturating_sub(limit);
            kept[skip..].to_vec()
        }
        Self {
            back_stack: clean(&self.back_stack, limit),
            forward_stack: clean(&self.forward_stack, limit),
        }
    }
    /// Copy keeping only the newest `limit` entries per stack. Session
    /// persistence uses this (50): stacks grow unbounded in memory, but
    /// the file must not.
    pub fn truncated(&self, limit: usize) -> Self {
        let take = |v: &[String]| {
            let skip = v.len().saturating_sub(limit);
            v[skip..].to_vec()
        };
        Self {
            back_stack: take(&self.back_stack),
            forward_stack: take(&self.forward_stack),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_adds_to_back_and_clears_forward() {
        let mut h = NavigationHistory::new();
        h.push("https://a.com", "https://b.com");
        assert_eq!(h.back_len(), 1);
        assert_eq!(h.peek_back(), Some("https://a.com"));
        assert_eq!(h.forward_len(), 0);

        // Simulate forward then new navigation should clear forward
        h.forward_stack.push("https://c.com".to_string());
        h.push("https:://b.com", "https://d.com");
        assert_eq!(h.forward_len(), 0);
    }

    #[test]
    fn back_and_forward_cycle() {
        let mut h = NavigationHistory::new();
        h.push("https://a.com", "https://b.com");
        h.push("https://b.com", "https://c.com");
        assert_eq!(h.back_len(), 2);
        assert!(h.can_go_back());
        assert!(!h.can_go_forward());

        let prev = h.go_back("https://c.com").unwrap();
        assert_eq!(prev, "https://b.com");
        assert_eq!(h.forward_len(), 1);
        assert!(h.can_go_forward());

        let next = h.go_forward("https://b.com").unwrap();
        assert_eq!(next, "https://c.com");
        assert_eq!(h.back_len(), 2);
    }

    #[test]
    fn go_back_when_empty_returns_none() {
        let mut h = NavigationHistory::new();
        assert!(h.go_back("https://a.com").is_none());
        assert!(h.go_forward("https://a.com").is_none());
    }

    #[test]
    fn truncated_and_cleaned_keep_newest_valid() {
        let mut h = NavigationHistory::new();
        for i in 0..10 {
            h.push(&format!("https://x.test/{i}"), "https://y.test");
        }
        // truncated keeps the newest 3, oldest-first order intact.
        let t = h.truncated(3);
        assert_eq!(t.back_len(), 3);
        assert_eq!(t.peek_back(), Some("https://x.test/9"));
        assert_eq!(
            t.back_list(3),
            vec!["https://x.test/9", "https://x.test/8", "https://x.test/7"]
        );
        // cleaned drops empties/overlong, keeps order.
        h.back_stack.push(String::new());
        h.back_stack.push("z".repeat(3000));
        let c = h.cleaned(100);
        assert_eq!(c.back_len(), 10);
        assert!(
            !c.back_list(100)
                .iter()
                .any(|u| u.is_empty() || u.len() > 2048)
        );
        let c2 = h.cleaned(4);
        assert_eq!(c2.back_len(), 4);
        assert_eq!(c2.peek_back(), Some("https://x.test/9"));
    }

    #[test]
    fn push_ignores_empty_current() {
        let mut h = NavigationHistory::new();
        h.push("", "https://a.com");
        assert_eq!(h.back_len(), 0);
    }

    #[test]
    fn push_ignores_same_url() {
        let mut h = NavigationHistory::new();
        h.push("https://a.com", "https://a.com");
        assert_eq!(h.back_len(), 0);
    }

    #[test]
    fn clear_empties_both_stacks() {
        let mut h = NavigationHistory::new();
        h.push("https://a.com", "https://b.com");
        h.push("https://b.com", "https://c.com");
        h.go_back("https://c.com");
        h.clear();
        assert_eq!(h.back_len(), 0);
        assert_eq!(h.forward_len(), 0);
    }
}
