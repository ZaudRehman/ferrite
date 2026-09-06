//! Ferrite shell: window, rendering, and theme.
//!
//! Owns the OS window and `egui` render loop. See [`ShellConfig`] and
//! [`run`](shell::run). Panics in UI code are caught per-frame so a bad
//! widget does not take down the process.

use egui::ViewportId;

pub use self::{
    app_context::AppContext,
    error::{ShellError, ShellResult},
    shell::{RepaintPolicy, ShellConfig, run},
    theme::{AccentContext, PaletteTokens, ThemeConfig},
};
pub use ferrite_tabs::{
    DownloadEvent, DownloadManager, DownloadStateKind, default_download_dir, sanitize_filename,
};

pub(crate) mod app_context;
mod error;
mod fonts;
pub mod icons;
pub mod inspector;
mod renderer;
mod shell;
mod theme;
mod titlebar;

/// The root viewport managed by the shell.
pub(crate) const ROOT_VIEWPORT: ViewportId = ViewportId::ROOT;
