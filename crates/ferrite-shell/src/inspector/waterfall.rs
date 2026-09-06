//! Network waterfall ring buffer and table.

use ferrite_proxy::NetworkEvent;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
pub struct WaterfallState {
    buffer: Arc<Mutex<VecDeque<NetworkEvent>>>,
}

impl WaterfallState {
    pub fn push(&self, ev: NetworkEvent) {
        if let Ok(mut buf) = self.buffer.lock() {
            if buf.len() >= 2000 {
                buf.pop_front();
            }
            buf.push_back(ev);
        }
    }

    pub fn snapshot(&self) -> Vec<NetworkEvent> {
        self.buffer
            .lock()
            .map(|buf| buf.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear(&mut self) {
        if let Ok(mut buf) = self.buffer.lock() {
            buf.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrite_proxy::{Classification, ResourceType};

    #[test]
    fn ring_evicts_oldest() {
        let s = WaterfallState::default();
        for i in 0..2500 {
            s.push(NetworkEvent {
                timestamp: i as i64,
                origin_domain: "a.test".into(),
                request_domain: format!("{i}.test"),
                resource_type: ResourceType::Other,
                bytes_transferred: 0,
                classification: Classification::Allowed,
            });
        }
        assert!(s.snapshot().len() <= 2000);
    }
}
