//! Release gate: a release build without the `sqlcipher` feature would
//! persist page snapshots in plaintext SQLite. That is not shippable, so
//! it is not buildable. See `docs/STORAGE_CONTRACT.md`.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_SQLCIPHER");
    let release = std::env::var("PROFILE")
        .map(|p| p == "release")
        .unwrap_or(false);
    let has_sqlcipher = std::env::var("CARGO_FEATURE_SQLCIPHER").is_ok();
    if release && !has_sqlcipher {
        panic!(
            "release builds require the `sqlcipher` feature for per-app encrypted storage \
             (docs/STORAGE_CONTRACT.md). Rebuild with `--features <crate>/sqlcipher`; \
             the SQLCipher native build needs perl plus a C toolchain (docs/SETUP.md)."
        );
    }
}
