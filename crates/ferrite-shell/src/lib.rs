//! Shell: winit window plus wry webviews. Chrome is a bundled
//! `ferrite://chrome` webview driven by state pushes. Content lives in
//! child webviews below the chrome bars.

pub use self::{
    app_context::AppContext,
    error::{ShellError, ShellResult},
    profiles::{
        DATA_DIR_ENV, Profile, Session, archive_path, crash_log_path, create as create_profile,
        data_root, destroy as destroy_profile, engine_dir, guest as guest_profile,
        migrate_cwd_to_data_root, private_downloads_dir, sanitize as sanitize_profile_name,
        settings_path, sweep_stale_private, threat_db_path, vault_path,
    },
    shell::ShellConfig,
    shell::run,
    theme::{AccentContext, PaletteTokens, ThemeConfig},
};
pub use ferrite_tabs::{
    DownloadEvent, DownloadManager, DownloadStateKind, decide_destination, default_download_dir,
    sanitize_filename,
};

pub(crate) mod app_context;
mod cert;
pub(crate) mod clipboard;
mod dicts;
mod error;
pub(crate) mod export;
mod favicon;
mod filedlg;
mod fonts;
pub(crate) mod forward;
#[cfg(test)]
mod js_syntax;
pub(crate) mod pdf;
pub mod profiles;
mod recovery;
mod rotation;
mod session;
mod settings_file;
mod share;
mod shell;
mod suggest;
mod theme;
pub(crate) mod toast;
mod update;
mod viewsource;
