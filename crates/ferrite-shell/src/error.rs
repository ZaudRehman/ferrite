//! Shell errors.

use thiserror::Error;
use winit::error::OsError;

/// Result from shell operations.
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

    #[error("render failure: {0}")]
    Render(#[from] egui_wgpu::WgpuError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_creation_carries_os_error_source() {
        // OsError has no public constructor, so assert the static contract that
        // drives this variant: the `source` field must implement std::error::Error.
        fn assert_source_error<T: std::error::Error>() {}
        assert_source_error::<OsError>();
        assert_source_error::<ShellError>();
    }

    #[test]
    fn render_from_wgpu_error_satisfies_source() {
        fn assert_source_error<T: std::error::Error>() {}
        assert_source_error::<egui_wgpu::WgpuError>();
    }
}
