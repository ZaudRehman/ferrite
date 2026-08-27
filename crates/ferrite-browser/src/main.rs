//! Ferrite — binary entry point.
//!
//! Initializes tracing and panic handling, then starts `ferrite-shell`.
//! Shell errors are surfaced as `anyhow::Error` for top-level reporting.

use anyhow::Context;
use ferrite_shell::{ShellConfig, run};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const DEFAULT_LOG_FILTER: &str = "ferrite=debug,info";

fn main() -> anyhow::Result<()> {
    install_panic_hook();
    init_tracing()?;

    let config = ShellConfig::default();
    tracing::info!("starting");
    run(config).context("shell terminated with an error")?;
    tracing::info!("shutdown");
    Ok(())
}

/// Initialize tracing. Uses `RUST_LOG` when set, otherwise a sensible default.
fn init_tracing() -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    let fmt_layer = fmt::layer()
        .with_target(false)
        .with_timer(fmt::time::uptime());
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .try_init()
        .context("failed to initialise tracing subscriber")?;
    Ok(())
}

/// Forward panics to `tracing`. Supplements the per-frame `catch_unwind`
/// in `ferrite-shell`.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "uncaught panic in ferrite-browser");
        default_hook(info);
    }));
}
