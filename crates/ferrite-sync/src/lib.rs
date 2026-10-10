//! Device sync: discovery, crypto, CRDT merge, and release updates.

pub mod backup;
pub mod crdt;
pub mod crdt_engine;
pub mod crypto;
pub mod discovery;
pub mod hlc;
pub mod mtls;
pub mod multiplex;
pub mod pairing;
pub mod relay_auth;
pub mod relay_client;
pub mod selective;
pub mod session;
pub mod sync_actions;
pub mod transport;
pub mod trusted;
pub mod updater;
pub mod vector_clock;

pub use backup::{
    BackupConfig, DEFAULT_RELAYS, PairingChannel, decrypt_vault_snapshot, derive_backup_key,
    encrypt_oauth_token, encrypt_vault_snapshot,
};
pub use crdt::{CrdtError, LwwMap, SyncRecord};
pub use crdt_engine::CrdtEngine;
pub use crypto::{CryptoError, DeviceIdentity};
pub use discovery::{DiscoveredDevice, DiscoveryConfig, DiscoveryService};
pub use hlc::Hlc;
pub use mtls::{MtlsConfig, MtlsTransport};
pub use multiplex::{Decoded, MessageType, decode, encode_crdt};
pub use pairing::{PairingPayload, PairingRateLimiter, PairingService};
pub use relay_auth::{MAX_AUTH_SKEW_SECS, RelayAuthPayload, build_auth, verify_auth};
pub use relay_client::{
    RELAY_BACKOFF, RELAY_CONNECT_TIMEOUT, RELAY_IO_TIMEOUT, RELAY_MAX_MESSAGE_BYTES,
    RELAY_PING_INTERVAL, RelayConnection, RelayError, RelayLink, RelaySignal, RelayStatus,
    ServeOutcome, crdt_payload, parse_crdt_payload,
};
pub use selective::SyncSettings;
pub use session::{RoundReport, SyncSession};
pub use sync_actions::{CrdtMessage, LiveTabAction, SyncAction, SyncCategory};
pub use transport::{P2pConfig, P2pTransport};
pub use trusted::{TrustedDevice, TrustedStore};
pub use vector_clock::VectorClock;
