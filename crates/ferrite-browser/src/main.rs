//! Binary entry. Init tracing and panic hook, then run `ferrite-shell`.

// Windows is GUI-subsystem in every profile, so starting Ferrite never opens a
// console window beside the browser — not even in a debug build. Debug logs
// still have somewhere to go: the standard handles a launching terminal handed
// us, its console when we were given none, or a capped file when there is no
// console at all (Explorer, a shortcut). Subsystem does not affect pipes, so
// the updater's `--version` health gate keeps working headless.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use anyhow::Context;
use ferrite_shell::{ShellConfig, run};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const DEFAULT_LOG_FILTER: &str = "ferrite=debug,info";
/// Crash-log file name (under the OS data root, never the launch dir).
const CRASH_LOG_NAME: &str = "ferrite-crash.log";
/// Debug-build log, used only when there is no console to print to. Debug
/// builds are never shipped, so this adds no release privacy surface (the
/// shipped GUI build keeps logging to the OS-provided streams and nothing else).
#[cfg(all(target_os = "windows", debug_assertions))]
const DEBUG_LOG_NAME: &str = "ferrite-debug.log";
/// Truncate rather than grow unbounded (matches the crash-log cap intent).
#[cfg(all(target_os = "windows", debug_assertions))]
const DEBUG_LOG_CAP: u64 = 512 * 1024;
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
    #[cfg(all(target_os = "windows", debug_assertions))]
    {
        // The process is GUI-subsystem: it owns no console. Join the console of
        // a terminal that launched us so logs (and `--help`/`--version`) still
        // print there; when there is none, keep the trail in a capped file
        // beside the crash log instead of losing it.
        if !attach_console_if_any() {
            let path = ferrite_shell::profiles::data_root().join(DEBUG_LOG_NAME);
            return tracing_subscriber::registry()
                .with(filter)
                .with(
                    fmt::layer()
                        .with_target(false)
                        .with_timer(fmt::time::uptime())
                        .with_writer(DebugLogWriter { path }),
                )
                .try_init()
                .context("failed to initialise tracing subscriber");
        }
    }
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_target(false)
                .with_timer(fmt::time::uptime()),
        )
        .try_init()
        .context("failed to initialise tracing subscriber")?;
    Ok(())
}

/// Give this GUI-subsystem process somewhere to write its logs, and return
/// whether it found one.
///
/// If a standard handle is already set (a pipe or a terminal that launched us),
/// leave it alone. Otherwise join the console of the terminal that launched us
/// and point the standard handles at it. Returns `false` when there is no
/// console to join (Explorer, a shortcut) — the file-log fallback case.
#[cfg(all(target_os = "windows", debug_assertions))]
fn attach_console_if_any() -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };
    // SAFETY: plain Win32 calls, no memory of ours is borrowed. The console
    // handles are leaked on purpose (`mem::forget`) so they stay valid for the
    // whole process lifetime, exactly as OS-provided standard handles would.
    unsafe {
        // A standard handle already in place is a real sink: a pipe (the
        // updater's `--version` health gate) or a terminal that launched us and
        // handed its console down. Leave it untouched — replacing it here would
        // send output to the console instead of the pipe reading it.
        if let Ok(stdout) = GetStdHandle(STD_OUTPUT_HANDLE)
            && !stdout.is_invalid()
        {
            return true;
        }
        // Attach only when we truly have nowhere to write. Fails when the
        // creator had no console (the normal double-click case).
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return false;
        }
        // AttachConsole alone does not refresh std's cached handles; reopen the
        // console devices and install them so `println!`/tracing reach the
        // terminal rather than a stale (null) handle.
        if let Ok(out) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            let handle = HANDLE(out.as_raw_handle());
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
            let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
            std::mem::forget(out);
        }
        if let Ok(inp) = std::fs::OpenOptions::new().read(true).open("CONIN$") {
            let _ = SetStdHandle(STD_INPUT_HANDLE, HANDLE(inp.as_raw_handle()));
            std::mem::forget(inp);
        }
        true
    }
}

/// Append-only, size-capped tracing sink for a debug build that has no console
/// (see `init_tracing`). Reopened per event: debug-only, and it keeps lines
/// from several threads in file order via the OS append guarantee.
#[cfg(all(target_os = "windows", debug_assertions))]
#[derive(Clone)]
struct DebugLogWriter {
    path: std::path::PathBuf,
}

#[cfg(all(target_os = "windows", debug_assertions))]
impl<'a> fmt::MakeWriter<'a> for DebugLogWriter {
    type Writer = Box<dyn std::io::Write + 'a>;

    fn make_writer(&'a self) -> Self::Writer {
        // Truncate past the cap so a log storm or crash loop stays bounded.
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > DEBUG_LOG_CAP) {
            let _ = std::fs::write(&self.path, b"");
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(file) => Box::new(file),
            // A failed open must never take the browser down: drop the line.
            Err(_) => Box::new(std::io::sink()),
        }
    }
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

/// The console-free Windows build has no stdout to lose: when it also has no
/// console to attach to, these pin that the fallback log file still records
/// output and stays capped.
#[cfg(all(test, target_os = "windows", debug_assertions))]
mod debug_log_tests {
    use super::*;
    use tracing_subscriber::fmt::MakeWriter;

    #[test]
    fn writer_appends_then_truncates_past_the_cap() {
        let dir = std::env::temp_dir().join(format!("ferrite-debug-log-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(DEBUG_LOG_NAME);
        let _ = std::fs::remove_file(&path);
        let writer = DebugLogWriter { path: path.clone() };

        {
            use std::io::Write;
            let mut w = writer.make_writer();
            writeln!(w, "first").unwrap();
        }
        {
            use std::io::Write;
            let mut w = writer.make_writer();
            writeln!(w, "second").unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first\nsecond\n",
            "later events append rather than clobber"
        );

        std::fs::write(&path, vec![b'x'; (DEBUG_LOG_CAP + 1) as usize]).unwrap();
        {
            use std::io::Write;
            let mut w = writer.make_writer();
            writeln!(w, "after-cap").unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "after-cap\n",
            "past the cap the file restarts instead of growing without bound"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
