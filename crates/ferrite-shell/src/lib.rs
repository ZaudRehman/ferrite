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

pub(crate) mod app_context;
mod error;
mod fonts;
mod renderer;
mod shell;
mod theme;

/// The root viewport managed by the shell.
pub(crate) const ROOT_VIEWPORT: ViewportId = ViewportId::ROOT;
