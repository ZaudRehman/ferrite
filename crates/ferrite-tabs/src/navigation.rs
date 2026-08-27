//! Per-tab back/forward history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigationHistory {
    back_stack: Vec<String>,
    forward_stack: Vec<String>,
}

impl NavigationHistory {
    /// Create an empty history.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, current_url: &str, new_url: &str) {
        if !current_url.is_empty() && current_url != new_url {
            self.back_stack.push(current_url.to_owned());
        }
        self.forward_stack.clear();
    }

    /// Whether a back navigation is possible.
    pub fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    /// Whether a forward navigation is possible.
    pub fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
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

    /// Length of the back stack (for diagnostics / tests).
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
