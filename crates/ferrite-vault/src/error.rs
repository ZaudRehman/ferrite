//! Vault errors.

use thiserror::Error;

pub type VaultResult<T> = Result<T, VaultError>;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("storage error: {0}")]
    Storage(String),

    #[error("keyring error: {0}")]
    Keyring(String),

    #[error("crypto error: {0}")]
    Crypto(String),

    #[error("import error: {0}")]
    Import(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("not found: {0}")]
    NotFound(String),
}
