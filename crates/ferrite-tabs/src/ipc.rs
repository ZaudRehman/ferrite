//! IPC bridge between JS and Rust.

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::warn;

use crate::webview::IpcMessage;

/// Channel between webview JS and the shell.
#[derive(Debug)]
pub struct IpcBridge {
    pub sender: UnboundedSender<IpcMessage>,
    pub receiver: UnboundedReceiver<IpcMessage>,
}

impl IpcBridge {
    pub fn new() -> Self {
        let (sender, receiver) = unbounded_channel();
        Self { sender, receiver }
    }

    pub fn try_send(&self, msg: IpcMessage) -> bool {
        if let Err(err) = self.sender.send(msg) {
            warn!(%err, "ipc channel closed: dropping message");
            false
        } else {
            true
        }
    }

    pub fn drain(&mut self) -> Vec<IpcMessage> {
        let mut out = Vec::new();
        while let Ok(msg) = self.receiver.try_recv() {
            out.push(msg);
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
    use uuid::Uuid;

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
    fn try_send_after_receiver_dropped_is_false() {
        let bridge = IpcBridge::new();
        let sender = bridge.sender.clone();
        drop(bridge);
        let msg = IpcMessage {
            tab_id: Uuid::new_v4(),
            payload: "hi".to_string(),
        };
        assert!(sender.send(msg).is_err());
    }
}
