//! Typed errors for the content layer (`ferrite-tabs`).

use thiserror::Error;
use uuid::Uuid;

/// All fallible outcomes from the tab/content layer.
pub type TabResult<T> = Result<T, TabError>;

/// Failures produced by `TabManager` and `TabState` operations.
#[derive(Debug, Error)]
pub enum TabError {
    /// No tab exists for the given identifier.
    #[error("tab not found: {id}")]
    TabNotFound { id: Uuid },

    /// Index is outside the current `Vec<TabState>` bounds.
    #[error("tab index out of bounds: {index} (len={len})")]
    IndexOutOfBounds { index: usize, len: usize },

    /// Tab limit exceeded (spec §5.1 memory guard, default 50).
    #[error("tab limit exceeded: {current} >= {limit}")]
    TabLimitExceeded { current: usize, limit: usize },

    /// The URL is not valid per RFC 3986 (`url` crate).
    #[error("invalid url '{url}': {source}")]
    InvalidUrl {
        url: String,
        #[source]
        source: url::ParseError,
    },

    /// `wry` failed to create the underlying OS webview.
    #[error("webview creation failed: {source}")]
    WebViewCreationFailed {
        #[source]
        source: wry::Error,
    },

    /// A webview operation (`set_bounds`, `set_visible`, `load_url`, `evaluate_script`) failed.
    #[error("webview operation failed: {source}")]
    WebViewOperationFailed {
        #[source]
        source: wry::Error,
    },

    /// The IPC channel is closed or the receiver was dropped.
    #[error("ipc channel closed")]
    IpcChannelClosed,

    /// No active tab is selected (`active_tab_index` is `None`).
    #[error("no active tab")]
    NoActiveTab,
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
