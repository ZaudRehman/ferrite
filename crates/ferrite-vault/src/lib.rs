//! Encrypted vault and telemetry pipeline.

pub mod bookmarks;
pub mod error;
pub mod import;
pub mod keyring;
pub mod recovery;
pub mod vault;

pub use bookmarks::{BookmarkEvent, BookmarkEventKind};
pub use error::{VaultError, VaultResult};
pub use vault::{Vault, VaultConfig};
