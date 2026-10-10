//! Archive errors.

use thiserror::Error;

pub type ArchiveResult<T> = Result<T, ArchiveError>;

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("storage error: {0}")]
    Storage(String),

    #[error("crypto error: {0}")]
    Crypto(String),

    #[error("compression error: {0}")]
    Compression(String),

    #[error("sanitization error: {0}")]
    Sanitization(String),

    #[error("not found: {url}")]
    NotFound { url: String },

    #[error("pdf error: {0}")]
    Pdf(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
