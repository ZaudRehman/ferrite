//! Ferrite — binary entry point.
//!
//! Initializes tracing and panic handling, then starts `ferrite-shell`.
//! Shell errors are surfaced as `anyhow::Error` for top-level reporting.

use anyhow::Context;
use ferrite_shell::{ShellConfig, run};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const DEFAULT_LOG_FILTER: &str = "ferrite=debug,info";
const CRASH_LOG: &str = "ferrite-crash.log";
/// Crash log cap (64KB): rotated by truncation so a crash loop cannot fill disks.
const CRASH_LOG_CAP: u64 = 64 * 1024;

fn main() -> anyhow::Result<()> {
    install_panic_hook();
    init_tracing()?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("ferrite — privacy-first desktop browser");
        println!();
        println!("USAGE: ferrite [--url <url>] [--help]");
        println!();
        println!("  --url <url>   Open this URL in the initial tab");
        println!("  --help, -h    Show this message");
        return Ok(());
    }
    let mut initial_url: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--url" => {
                i += 1;
                if let Some(url) = args.get(i) {
                    initial_url = Some(url.clone());
                } else {
                    anyhow::bail!("--url requires a value");
                }
            }
            other => {
                // Bare positional argument treated as a URL (browser UX).
                if initial_url.is_none() && !other.starts_with('-') {
                    initial_url = Some(other.to_owned());
                } else {
                    anyhow::bail!("unknown argument: {other} (see --help)");
                }
            }
        }
        i += 1;
    }

    let config = ShellConfig {
        initial_url,
        ..ShellConfig::default()
    };
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

/// Forward panics to `tracing` plus an on-disk crash log. Supplements the
/// per-frame `catch_unwind` in `ferrite-shell`.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "uncaught panic in ferrite-browser");
        append_crash_log(&format!("{}: {}", chrono_stamp(), panic_message(info)));
        default_hook(info);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else if let Some(loc) = info.location() {
        format!("panic at {}:{}", loc.file(), loc.line())
    } else {
        "unknown panic".to_owned()
    }
}

fn chrono_stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}", d.as_secs()))
        .unwrap_or_else(|_| "0".to_owned())
}

fn append_crash_log(line: &str) {
    // Cap the log so a crash loop cannot fill the disk.
    if let Ok(meta) = std::fs::metadata(CRASH_LOG)
        && meta.len() > CRASH_LOG_CAP
        && std::fs::write(CRASH_LOG, line).is_ok()
    {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(CRASH_LOG)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}
