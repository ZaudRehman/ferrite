//! End-to-end guard for the console-free build.
//!
//! The browser is GUI-subsystem on Windows so starting it never opens a console
//! window. That must not cost the CLI its output: the updater's rollback health
//! gate runs `ferrite --version` with piped stdio and parses the first stdout
//! line, so this pins the exact path an over-eager console attach would hijack
//! (attaching to the parent console and replacing the pipe with a console
//! handle makes stdout come back empty). Runs on every platform.

use std::process::Command;

fn ferrite() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ferrite"))
}

#[test]
fn version_reaches_a_pipe_not_a_console() {
    let out = ferrite().arg("--version").output().expect("spawn ferrite");
    assert!(out.status.success(), "non-zero exit: {:?}", out.status);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        env!("CARGO_PKG_VERSION"),
        "the health gate parses this line, so it must arrive on the pipe"
    );
}

#[test]
fn help_reaches_a_pipe_not_a_console() {
    let out = ferrite().arg("--help").output().expect("spawn ferrite");
    assert!(out.status.success(), "non-zero exit: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("privacy-first desktop browser"),
        "usage banner missing from piped stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("--version"),
        "usage must list the flags it documents: {stdout:?}"
    );
}
