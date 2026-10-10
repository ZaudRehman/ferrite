//! Typed errors for the content layer (`ferrite-tabs`).

use thiserror::Error;
use uuid::Uuid;

pub type TabResult<T> = Result<T, TabError>;

#[derive(Debug, Error)]
pub enum TabError {
    #[error("tab not found: {id}")]
    TabNotFound { id: Uuid },

    #[error("tab is pinned, unpin or force close: {id}")]
    PinnedTab { id: Uuid },

    #[error("tab index out of bounds: {index} (len={len})")]
    IndexOutOfBounds { index: usize, len: usize },

    #[error("tab limit exceeded: {current} >= {limit}")]
    TabLimitExceeded { current: usize, limit: usize },

    #[error("invalid url '{url}': {source}")]
    InvalidUrl {
        url: String,
        #[source]
        source: url::ParseError,
    },

    #[error("webview creation failed: {source}")]
    WebViewCreationFailed {
        #[source]
        source: wry::Error,
    },

    #[error("webview operation failed: {source}")]
    WebViewOperationFailed {
        #[source]
        source: wry::Error,
    },

    #[error("no active tab")]
    NoActiveTab,

    #[error("tab group not found: {id}")]
    GroupNotFound { id: Uuid },

    #[error("workspace not found: {id}")]
    WorkspaceNotFound { id: Uuid },

    #[error("invalid group name (empty or over 64 chars)")]
    InvalidGroupName,

    #[error("invalid group color index: {color}")]
    InvalidGroupColor { color: u8 },

    #[error("session persistence: {message}")]
    SessionIo { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn tab_not_found_displays_id() {
        let id = Uuid::nil();
        let err = TabError::TabNotFound { id };
        assert!(err.to_string().contains(&id.to_string()));
    }

    #[test]
    fn index_out_of_bounds_has_len() {
        let err = TabError::IndexOutOfBounds { index: 5, len: 2 };
        assert!(err.to_string().contains("5"));
        assert!(err.to_string().contains("2"));
    }

    #[test]
    fn tab_limit_displays_both() {
        let err = TabError::TabLimitExceeded {
            current: 50,
            limit: 50,
        };
        assert!(err.to_string().contains("50"));
    }

    #[test]
    fn invalid_url_wraps_parse_error() {
        let url = "http://[invalid".to_string();
        let parse = url::ParseError::InvalidIpv6Address;
        let err = TabError::InvalidUrl {
            url: url.clone(),
            source: parse,
        };
        assert!(err.to_string().contains(&url));
        assert!(err.source().is_some());
    }
}
