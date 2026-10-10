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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn increment_starts_at_one() {
        let mut a = VectorClock::new();
        a.increment("A");
        a.increment("A");
        assert_eq!(a.0["A"], 2);
    }
}
