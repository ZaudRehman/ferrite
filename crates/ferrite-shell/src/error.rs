//! Shell error type. OS failures keep their source.

use thiserror::Error;
use winit::error::OsError;

pub type ShellResult<T> = Result<T, ShellError>;

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("failed to create OS window: {source}")]
    WindowCreation {
        #[source]
        source: OsError,
    },

    #[error("event loop failure: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),

    #[error("chrome webview failure: {0}")]
    Chrome(String),

    #[error("WebView2 Runtime missing: {0}")]
    WebView2Runtime(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_creation_carries_os_error_source() {
        // OsError has no public constructor, so assert the trait bound.
        fn assert_source_error<T: std::error::Error>() {}
        assert_source_error::<OsError>();
        assert_source_error::<ShellError>();
    }
}
