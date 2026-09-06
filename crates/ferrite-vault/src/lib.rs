//! Encrypted vault and telemetry pipeline.

#![allow(clippy::collapsible_if, clippy::too_many_arguments)]

pub mod error;
pub mod import;
pub mod keyring;
pub mod recovery;
pub mod vault;

pub use error::{VaultError, VaultResult};
pub use vault::{Vault, VaultConfig};
