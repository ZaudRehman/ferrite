//! Telemetry events and channels.

use crossbeam_channel::{Receiver, Sender, bounded};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    FirstParty,
    Allowed,
    BlockedAd,
    BlockedTracker,
    BlockedMalware,
    BlockedMixedContent,
}

/// Only for top level document failures the engine would render as its own
/// dead end page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Refused,
    TimedOut,
    Unreachable,
    ServerError,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::Refused => "refused",
            FailureKind::TimedOut => "timeout",
            FailureKind::Unreachable => "unreachable",
            FailureKind::ServerError => "server",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkEvent {
    pub timestamp: i64,
    pub origin_domain: String,
    pub request_domain: String,
    pub resource_type: crate::verdict::ResourceType,
    pub bytes_transferred: u64,
    pub classification: Classification,
    pub failure: Option<FailureKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkedPayload {
    pub url: String,
    pub content_type: String,
    pub head: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TelemetryTx {
    tx: Sender<NetworkEvent>,
    pub dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

pub const TELEMETRY_CAPACITY: usize = 4096;

impl TelemetryTx {
    pub fn new() -> (Self, Receiver<NetworkEvent>) {
        let (tx, rx) = bounded(TELEMETRY_CAPACITY);
        (
            Self {
                tx,
                dropped: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            rx,
        )
    }

    pub fn emit(&self, ev: NetworkEvent) {
        if let Err(crossbeam_channel::TrySendError::Full(_)) = self.tx.try_send(ev) {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for TelemetryTx {
    fn default() -> Self {
        Self::new().0
    }
}

pub const FORK_CAPACITY: usize = 64;
pub const FORK_CAP: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct ForkTx {
    tx: Sender<ForkedPayload>,
    pub dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl ForkTx {
    pub fn new() -> (Self, Receiver<ForkedPayload>) {
        let (tx, rx) = bounded(FORK_CAPACITY);
        (
            Self {
                tx,
                dropped: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            rx,
        )
    }

    pub fn emit(&self, p: ForkedPayload) {
        if let Err(crossbeam_channel::TrySendError::Full(_)) = self.tx.try_send(p) {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for ForkTx {
    fn default() -> Self {
        Self::new().0
    }
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::ResourceType;

    #[test]
    fn telemetry_round_trip_and_dropped_on_full() {
        let (tx, rx) = TelemetryTx::new();
        let ev = NetworkEvent {
            timestamp: now_millis(),
            origin_domain: "a.test".into(),
            request_domain: "cdn.test".into(),
            resource_type: ResourceType::Script,
            bytes_transferred: 128,
            classification: Classification::Allowed,
            failure: None,
        };
        tx.emit(ev.clone());
        assert_eq!(rx.try_recv().unwrap(), ev);
        assert_eq!(tx.dropped_count(), 0);
    }

    #[test]
    fn telemetry_drops_when_full_never_blocks() {
        let (tx, _rx_kept_alive) = {
            let (t, r) = TelemetryTx::new();
            (t, std::mem::forget(r))
        };
        for _ in 0..TELEMETRY_CAPACITY {
            tx.emit(NetworkEvent {
                timestamp: 0,
                origin_domain: "o".into(),
                request_domain: "d".into(),
                resource_type: ResourceType::Other,
                bytes_transferred: 0,
                classification: Classification::FirstParty,
                failure: None,
            });
        }
        tx.emit(NetworkEvent {
            timestamp: 0,
            origin_domain: "o".into(),
            request_domain: "d".into(),
            resource_type: ResourceType::Other,
            bytes_transferred: 0,
            classification: Classification::FirstParty,
            failure: None,
        });
        assert_eq!(tx.dropped_count(), 1);
    }

    #[test]
    fn fork_round_trip() {
        let (tx, rx) = ForkTx::new();
        tx.emit(ForkedPayload {
            url: "http://a/x.html".into(),
            content_type: "text/html".into(),
            head: b"<html>".to_vec(),
        });
        let got = rx.try_recv().unwrap();
        assert_eq!(got.content_type, "text/html");
        assert_eq!(got.head, b"<html>");
    }

    #[test]
    fn timestamp_is_epoch_millis() {
        let t = now_millis();
        assert!(t > 1_700_000_000_000);
    }
}
