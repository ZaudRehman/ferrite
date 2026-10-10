//! Framed multiplex: 1B type, 4B length, bincode payload.
//!
//! Wire note: bincode 2 (varint) encoding, NOT compatible with the
//! pre-1.0 bincode 1.x framing. The break was taken deliberately before
//! any release shipped a peer: bincode 1.x is unmaintained
//! (RUSTSEC-2025-0141) and this crate is the only user in the tree, so
//! the migration window was now or never. Post-1.0, wire changes need a
//! versioned frame (see `MessageType`; that byte exists for this).

use crate::sync_actions::CrdtMessage;

/// Max framed payload (8 MB). Larger lengths are rejected without alloc.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub enum MessageType {
    Crdt = 0x01,
}

impl MessageType {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x01 => Some(MessageType::Crdt),
            _ => None,
        }
    }
}

pub fn encode_crdt(msg: &CrdtMessage) -> Vec<u8> {
    let payload =
        bincode::serde::encode_to_vec(msg, bincode::config::standard()).unwrap_or_default();
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(MessageType::Crdt as u8);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

pub enum Decoded {
    Crdt(CrdtMessage),
}

pub fn decode(buf: &[u8]) -> Option<(Decoded, usize)> {
    if buf.len() < 5 {
        return None;
    }
    let ty = MessageType::from_byte(buf[0])?;
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if len > MAX_FRAME_BYTES {
        return None;
    }
    if buf.len() < 5 + len {
        return None;
    }
    let payload = &buf[5..5 + len];
    let decoded = match ty {
        MessageType::Crdt => {
            let (msg, _): (CrdtMessage, usize) =
                bincode::serde::decode_from_slice(payload, bincode::config::standard()).ok()?;
            Decoded::Crdt(msg)
        }
    };
    Some((decoded, 5 + len))
}

/// Advisory bandwidth check: CRDT batches above 1MB should be split by the
/// sender (delta pagination) rather than slept on.
pub fn should_throttle(ty: MessageType, len: usize) -> bool {
    matches!(ty, MessageType::Crdt) && len > 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_actions::{CrdtMessage, LiveTabAction};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn crdt_roundtrip() {
        let msg = CrdtMessage::SyncRequest {
            since_vector_clock: HashMap::new(),
            enabled_categories: HashSet::new(),
        };
        let enc = encode_crdt(&msg);
        let (dec, consumed) = decode(&enc).unwrap();
        assert_eq!(consumed, enc.len());
        match dec {
            Decoded::Crdt(CrdtMessage::SyncRequest { .. }) => {}
            Decoded::Crdt(_) => panic!("wrong variant"),
        }
    }

    #[test]
    fn live_tab_roundtrip() {
        let msg = CrdtMessage::PushLiveTab {
            action: LiveTabAction::PushTab {
                url: "https://a.com".to_owned(),
                source_device: "A".to_owned(),
                target_device: "B".to_owned(),
                timestamp: 1,
            },
        };
        let enc = encode_crdt(&msg);
        let (dec, _) = decode(&enc).unwrap();
        assert!(matches!(
            dec,
            Decoded::Crdt(CrdtMessage::PushLiveTab { .. })
        ));
    }

    #[test]
    fn oversize_frame_rejected_without_alloc() {
        let mut hdr = vec![MessageType::Crdt as u8];
        hdr.extend_from_slice(&u32::MAX.to_be_bytes());
        hdr.extend_from_slice(&[0u8; 16]);
        assert!(decode(&hdr).is_none());
    }

    #[test]
    fn multiplex_no_framing_error() {
        // Two back-to-back CRDT frames decode cleanly.
        let first = CrdtMessage::SyncResponse { deltas: vec![] };
        let second = CrdtMessage::SyncRequest {
            since_vector_clock: HashMap::new(),
            enabled_categories: HashSet::new(),
        };
        let mut buf = encode_crdt(&first);
        buf.extend_from_slice(&encode_crdt(&second));
        let (one, n) = decode(&buf).unwrap();
        assert!(matches!(one, Decoded::Crdt(_)));
        let (two, _) = decode(&buf[n..]).unwrap();
        assert!(matches!(two, Decoded::Crdt(_)));
    }
}
