//! Typed errors for the privacy layer.

use thiserror::Error;

/// All fallible outcomes from the privacy layer.
pub type PrivacyResult<T> = Result<T, PrivacyError>;

#[derive(Debug, Error)]
pub enum PrivacyError {
    #[error("adblock engine init failed: {0}")]
    AdblockInit(String),

    #[error("filter list parse failed: {0}")]
    FilterParse(String),

    #[error("threat intel DB error: {0}")]
    ThreatDb(String),

    #[error("integrity check failed for {list}: expected {expected}, got {actual}")]
    IntegrityMismatch {
        list: String,
        expected: String,
        actual: String,
    },

    #[error("invalid url '{input}': {reason}")]
    InvalidUrl { input: String, reason: String },
}
