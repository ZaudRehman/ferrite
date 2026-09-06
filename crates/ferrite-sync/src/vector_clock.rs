//! Vector clock per 3.1.

use std::collections::HashMap;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct VectorClock(pub HashMap<String, u64>);

impl VectorClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment(&mut self, device_id: &str) {
        *self.0.entry(device_id.to_owned()).or_insert(0) += 1;
    }

    pub fn merge(&mut self, other: &VectorClock) {
        for (k, v) in &other.0 {
            let e = self.0.entry(k.clone()).or_insert(0);
            *e = (*e).max(*v);
        }
    }

    /// Returns true if self dominates other (all counters >= and at least one >)
    pub fn dominates(&self, other: &VectorClock) -> bool {
        let mut ge_all = true;
        let mut gt_one = false;
        for (k, v) in &other.0 {
            let sv = self.0.get(k).copied().unwrap_or(0);
            if sv < *v {
                ge_all = false;
                break;
            }
            if sv > *v {
                gt_one = true;
            }
        }
        if other.0.len() > self.0.len() {
            ge_all = false;
        }
        ge_all && (gt_one || self.0.len() > other.0.len())
    }

    pub fn concurrent(&self, other: &VectorClock) -> bool {
        !self.dominates(other) && !other.dominates(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dominates() {
        let mut a = VectorClock::new();
        a.increment("A");
        a.increment("A");
        let mut b = VectorClock::new();
        b.increment("A");
        assert!(a.dominates(&b));
        assert!(!b.dominates(&a));
        assert!(!a.concurrent(&b));
    }
    #[test]
    fn concurrent() {
        let mut a = VectorClock::new();
        a.increment("A");
        let mut b = VectorClock::new();
        b.increment("B");
        assert!(a.concurrent(&b));
    }
}
