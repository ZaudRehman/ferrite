//! Part 10 Sovereign Sync: discovery & cryptography - _ferrite._tcp, Ed25519 mTLS, QR pairing, mesh trust propagation.

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
pub mod selective;
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
pub use selective::SyncSettings;
pub use sync_actions::{CrdtMessage, LiveTabAction, SyncAction, SyncCategory};
pub use transport::{P2pConfig, P2pTransport};
pub use trusted::{TrustedDevice, TrustedStore};
pub use vector_clock::VectorClock;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("crdt: {0}")]
    Crdt(String),
    #[error("vault: {0}")]
    Vault(String),
}
pub type SyncResult<T> = Result<T, SyncError>;
