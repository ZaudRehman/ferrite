//! Binary entry. Init tracing and panic hook, then run `ferrite-shell`.

use anyhow::Context;
use ferrite_shell::{ShellConfig, run};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const DEFAULT_LOG_FILTER: &str = "ferrite=debug,info";
/// Crash-log file name (under the OS data root, never the launch dir).
const CRASH_LOG_NAME: &str = "ferrite-crash.log";
// Truncated past 64 KB so a crash loop cannot fill disk.
const CRASH_LOG_CAP: u64 = 64 * 1024;

fn main() -> anyhow::Result<()> {
    install_panic_hook();
    init_tracing()?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("ferrite: privacy-first desktop browser");
        println!();
        println!(
            "USAGE: ferrite [--url <url>] [--profile <name>] [--incognito] [--version] [--help]"
        );
        println!();
        println!("  --url <url>     Open this URL in the initial tab");
        println!("  --profile <id>  Run under a named profile (own vault, session,");
        println!("                  tabs; created on first use, runs beside Default)");
        println!("  --incognito   Open an incognito window (nothing is saved)");
        println!("  --version, -V Show the version and exit");
        println!("  --help, -h    Show this message");
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        // Bare version line. The updater health gate parses this output.
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut initial_url: Option<String> = None;
    let mut incognito = false;
    let mut profile: Option<String> = None;
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
            "--profile" => {
                i += 1;
                match args.get(i) {
                    Some(name) if !name.trim().is_empty() => {
                        profile = Some(name.clone());
                    }
                    _ => anyhow::bail!("--profile requires a name"),
                }
            }
            "--incognito" => {
                incognito = true;
            }
            other => {
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
        incognito,
        profile,
        ..ShellConfig::default()
    };
    tracing::info!("starting");
    run(config).context("shell terminated with an error")?;
    tracing::info!("shutdown");
    Ok(())
}

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

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "uncaught panic in ferrite-browser");
        // Format is `<epoch> [<version>] <message>`, one line per crash.
        append_crash_log(&format!(
            "{} [{}] {}",
            chrono_stamp(),
            env!("CARGO_PKG_VERSION"),
            panic_message(info)
        ));
        default_hook(info);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo) -> String {
    let raw = if let Some(s) = info.payload().downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else if let Some(loc) = info.location() {
        format!("panic at {}:{}", loc.file(), loc.line())
    } else {
        "unknown panic".to_owned()
    };
    // Single line, max 500 chars, URLs scrubbed: panic payloads can carry
    // navigated URLs, and the crash log is plaintext on device (contract
    // allows it only because it carries no user data; keep it that way).
    let flat: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect();
    scrub_urls(flat)
}

/// Redact `http(s)://...` runs (scheme plus everything to the next
/// whitespace or quote). Pure for tests. File paths and `file:line`
/// locations pass through untouched.
fn scrub_urls(s: String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    loop {
        let http = rest.find("http://");
        let https = rest.find("https://");
        let start = match (http, https) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some(i) = start else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        out.push_str("[url]");
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
            .map(|e| i + e)
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out
}

fn chrono_stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}", d.as_secs()))
        .unwrap_or_else(|_| "0".to_owned())
}

fn append_crash_log(line: &str) {
    // Data-root location (migration in the shell moves legacy CWD logs on
    // the next normal boot; a crash before that keeps the old path: the
    // evidence still lands, exactly once, in one of the two places).
    let path = ferrite_shell::profiles::data_root().join(CRASH_LOG_NAME);
    if let Ok(meta) = std::fs::metadata(&path)
        && meta.len() > CRASH_LOG_CAP
        && std::fs::write(&path, format!("{line}\n")).is_ok()
    {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_redacts_urls_keeps_locations() {
        assert_eq!(
            scrub_urls("failed to load https://example.com/a?q=1 done".to_owned()),
            "failed to load [url] done"
        );
        assert_eq!(
            scrub_urls("see http://a.test and https://b.test/x".to_owned()),
            "see [url] and [url]"
        );
        assert_eq!(
            scrub_urls("panic at src/main.rs:12".to_owned()),
            "panic at src/main.rs:12"
        );
        assert_eq!(scrub_urls("no urls here".to_owned()), "no urls here");
        assert_eq!(
            scrub_urls("quoted \"https://e.test/p\" end".to_owned()),
            "quoted \"[url]\" end"
        );
    }
}
