//! IPC bridge between JS and Rust.
//!
//! Bounded channel (256) with per-tab rate limiting (10 msgs/s sliding
//! window) and a 64KB payload cap. A compromised page posting the whole DOM
//! on every mutation (see `inspector/elements.rs`) can no longer grow memory
//! without bound: excess messages are dropped with a warning and counted.
//!
//! Rate-limit state is shared (`SharedLimitsHandle`) across the bridge and
//! every cloned sender (notably the wry IPC handler), so all producers draw
//! from the same per-tab budget.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use tokio::sync::mpsc::{Receiver, Sender, channel};
use tracing::warn;
use uuid::Uuid;

use crate::webview::IpcMessage;

/// Maximum queued IPC messages process-wide.
pub const IPC_QUEUE_CAP: usize = 256;
/// Maximum accepted payload bytes per message.
pub const IPC_MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// Per-tab rate limit: messages per sliding 1s window.
pub const IPC_PER_TAB_PER_SEC: usize = 10;
/// Maximum accepted title length (chars); longer titles are truncated.
pub const IPC_MAX_TITLE_CHARS: usize = 500;

type SharedLimits = std::sync::Arc<std::sync::Mutex<LimitsInner>>;

#[derive(Debug, Default)]
struct LimitsInner {
    per_tab: HashMap<Uuid, VecDeque<Instant>>,
    dropped_rate: u64,
}

/// Cloneable handle to the shared per-tab rate-limit state.
#[derive(Debug, Clone)]
pub struct SharedLimitsHandle {
    inner: SharedLimits,
}

enum SendOutcome {
    Sent,
    DroppedFull,
    DroppedOversize,
    DroppedRate,
    Closed,
}

fn send_checked(
    sender: &Sender<IpcMessage>,
    limits: &SharedLimits,
    msg: IpcMessage,
) -> SendOutcome {
    if msg.payload.len() > IPC_MAX_PAYLOAD_BYTES {
        warn!(
            tab_id = %msg.tab_id,
            bytes = msg.payload.len(),
            "ipc oversize: dropping message"
        );
        return SendOutcome::DroppedOversize;
    }
    let now = Instant::now();
    if let Ok(mut l) = limits.lock() {
        let stamps = l.per_tab.entry(msg.tab_id).or_default();
        while stamps
            .front()
            .map(|t| now.duration_since(*t).as_secs() >= 1)
            .unwrap_or(false)
        {
            stamps.pop_front();
        }
        if stamps.len() >= IPC_PER_TAB_PER_SEC {
            l.dropped_rate += 1;
            warn!(tab_id = %msg.tab_id, "ipc rate limit: dropping message");
            return SendOutcome::DroppedRate;
        }
        stamps.push_back(now);
    }
    match sender.try_send(msg) {
        Ok(()) => SendOutcome::Sent,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            warn!("ipc queue full: dropping message");
            SendOutcome::DroppedFull
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            warn!("ipc channel closed: dropping message");
            SendOutcome::Closed
        }
    }
}

/// Channel between webview JS and the shell.
#[derive(Debug)]
pub struct IpcBridge {
    pub sender: Sender<IpcMessage>,
    pub receiver: Receiver<IpcMessage>,
    limits: SharedLimits,
    pub dropped_full: u64,
    pub dropped_oversize: u64,
}

impl IpcBridge {
    pub fn new() -> Self {
        let (sender, receiver) = channel(IPC_QUEUE_CAP);
        Self {
            sender,
            receiver,
            limits: std::sync::Arc::new(std::sync::Mutex::new(LimitsInner::default())),
            dropped_full: 0,
            dropped_oversize: 0,
        }
    }

    /// Shared rate-limit state for cloned senders (e.g. the wry handler).
    pub fn limiter(&self) -> SharedLimitsHandle {
        SharedLimitsHandle {
            inner: std::sync::Arc::clone(&self.limits),
        }
    }

    pub fn dropped_rate(&self) -> u64 {
        self.limits.lock().map(|l| l.dropped_rate).unwrap_or(0)
    }

    /// Validate + enqueue. Returns false when dropped (full / rate / size).
    /// Called on the wry IPC thread: never blocks.
    pub fn try_send(&mut self, msg: IpcMessage) -> bool {
        match send_checked(&self.sender, &self.limits, msg) {
            SendOutcome::Sent => true,
            SendOutcome::DroppedFull => {
                self.dropped_full += 1;
                false
            }
            SendOutcome::DroppedOversize => {
                self.dropped_oversize += 1;
                false
            }
            SendOutcome::DroppedRate | SendOutcome::Closed => false,
        }
    }

    /// Non-blocking send from threads that own a cloned sender (e.g. wry).
    /// Enforces the shared per-tab rate limit, the oversize cap and the
    /// queue bound. Prefer `try_send` on the bridge when available.
    pub fn try_send_via(
        sender: &Sender<IpcMessage>,
        limits: &SharedLimitsHandle,
        msg: IpcMessage,
    ) -> bool {
        matches!(send_checked(sender, &limits.inner, msg), SendOutcome::Sent)
    }

    pub fn drain(&mut self) -> Vec<IpcMessage> {
        let mut out = Vec::new();
        while let Ok(msg) = self.receiver.try_recv() {
            // Defense in depth: the wry handler already caps, but messages
            // could arrive from older senders or tests.
            if msg.payload.len() > IPC_MAX_PAYLOAD_BYTES {
                self.dropped_oversize += 1;
                continue;
            }
            out.push(msg);
            if out.len() >= IPC_QUEUE_CAP {
                break;
            }
        }
        out
    }

    pub fn has_pending(&self) -> bool {
        !self.receiver.is_empty()
    }
}

impl Default for IpcBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_bridge_is_empty() {
        let bridge = IpcBridge::new();
        assert!(!bridge.has_pending());
    }

    #[test]
    fn try_send_and_drain_round_trips() {
        let mut bridge = IpcBridge::new();
        let id = Uuid::new_v4();
        let msg = IpcMessage {
            tab_id: id,
            payload: r#"{"type":"title","title":"Example"}"#.to_string(),
        };
        assert!(bridge.try_send(msg.clone()));
        assert!(bridge.has_pending());
        let drained = bridge.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0], msg);
        assert!(!bridge.has_pending());
    }

    #[test]
    fn drain_empty_is_noop() {
        let mut bridge = IpcBridge::new();
        assert!(bridge.drain().is_empty());
    }

    #[test]
    fn oversize_payload_is_dropped_and_counted() {
        let mut bridge = IpcBridge::new();
        let msg = IpcMessage {
            tab_id: Uuid::new_v4(),
            payload: "x".repeat(IPC_MAX_PAYLOAD_BYTES + 1),
        };
        assert!(!bridge.try_send(msg));
        assert_eq!(bridge.dropped_oversize, 1);
        assert!(!bridge.has_pending());
    }

    #[test]
    fn per_tab_rate_limit_drops_burst() {
        let mut bridge = IpcBridge::new();
        let id = Uuid::new_v4();
        let mut ok = 0;
        for _ in 0..(IPC_PER_TAB_PER_SEC + 5) {
            if bridge.try_send(IpcMessage {
                tab_id: id,
                payload: "{}".to_string(),
            }) {
                ok += 1;
            }
        }
        assert_eq!(ok, IPC_PER_TAB_PER_SEC);
        assert!(bridge.dropped_rate() > 0);
    }

    #[test]
    fn different_tabs_have_independent_budgets() {
        let mut bridge = IpcBridge::new();
        for _ in 0..IPC_PER_TAB_PER_SEC {
            assert!(bridge.try_send(IpcMessage {
                tab_id: Uuid::new_v4(),
                payload: "{}".to_string(),
            }));
        }
    }

    #[test]
    fn cloned_sender_shares_rate_budget() {
        let bridge = IpcBridge::new();
        let sender = bridge.sender.clone();
        let limits = bridge.limiter();
        let id = Uuid::new_v4();
        let mut ok = 0;
        for _ in 0..(IPC_PER_TAB_PER_SEC + 5) {
            if IpcBridge::try_send_via(
                &sender,
                &limits,
                IpcMessage {
                    tab_id: id,
                    payload: "{}".to_string(),
                },
            ) {
                ok += 1;
            }
        }
        assert_eq!(ok, IPC_PER_TAB_PER_SEC);
        assert!(bridge.dropped_rate() > 0);
    }
}
