//! Typed errors for the omnibox layer.

use thiserror::Error;

pub type OmniboxResult<T> = Result<T, OmniboxError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OmniboxError {
    #[error("unknown search engine: {id}")]
    UnknownEngine { id: String },

    #[error("empty input")]
    EmptyInput,

    #[error("invalid url '{input}': {reason}")]
    InvalidUrl { input: String, reason: String },
}
