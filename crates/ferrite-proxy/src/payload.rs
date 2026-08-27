//! Forked payload consumer.

use crossbeam_channel::Receiver;

use crate::event::ForkedPayload;

#[derive(Debug)]
pub struct DebugBufferSink {
    rx: Receiver<ForkedPayload>,
    ring: std::collections::VecDeque<ForkedPayload>,
    cap: usize,
    dropped_by_channel: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl DebugBufferSink {
    pub fn new(
        rx: Receiver<ForkedPayload>,
        dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
        cap: usize,
    ) -> Self {
        Self {
            rx,
            ring: std::collections::VecDeque::new(),
            cap,
            dropped_by_channel: dropped,
        }
    }

    pub fn pump(&mut self) {
        while let Ok(p) = self.rx.try_recv() {
            if self.ring.len() == self.cap {
                self.ring.pop_front();
            }
            self.ring.push_back(p);
        }
    }

    pub fn snapshot(&self) -> Vec<ForkedPayload> {
        self.ring.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    pub fn channel_dropped(&self) -> u64 {
        self.dropped_by_channel
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ForkTx;

    fn payload(n: usize) -> ForkedPayload {
        ForkedPayload {
            url: format!("http://a/{n}"),
            content_type: "text/html".into(),
            head: vec![b'x'; n],
        }
    }

    #[test]
    fn sink_evicts_oldest_beyond_cap() {
        let (tx, rx) = ForkTx::new();
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut sink = DebugBufferSink::new(rx, dropped, 2);
        tx.emit(payload(1));
        tx.emit(payload(2));
        tx.emit(payload(3));
        sink.pump();
        assert_eq!(sink.len(), 2);
        let snap = sink.snapshot();
        assert_eq!(snap[0].url, "http://a/2");
        assert_eq!(snap[1].url, "http://a/3");
    }

    #[test]
    fn pump_empty_is_noop() {
        let (_tx, rx) = ForkTx::new();
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut sink = DebugBufferSink::new(rx, dropped, 4);
        sink.pump();
        assert!(sink.is_empty());
    }
}
