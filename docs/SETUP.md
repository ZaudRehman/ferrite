# Ferrite Build & Provisioning Setup

## Toolchains

- Rust `1.96.0` (see `rust-toolchain.toml`), `rustfmt` + `clippy`.
- Go `1.27.0` for `relay/` (`go build/vet/test ./...` inside `relay/`).
- Node/tooling not required.

## Standard build (no SQLCipher)

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Without the `sqlcipher` feature, keyed vaults **refuse to open**
(fail-closed). Unkeyed dev flows (history/bookmarks UI, tests) work.

## SQLCipher build (production at-rest encryption)

Needs `perl` + a C toolchain + `pkg-config` (Linux):

```powershell
# Windows: Strawberry Perl (perl on PATH) + MSVC toolchain
# Linux:
sudo apt-get install -y perl pkg-config libssl-dev
cargo build -p ferrite-vault --features sqlcipher
```

Verify: opening a keyed vault prints no `SQLite format` header on disk,
and a wrong key fails with `NOTADB` instead of decoding garbage.

## Optional features

- `ferrite-archive/pdfium-render`: real PDF engine. Without it, page
  rendering fails closed with a named error (never a blank page) and
  encrypted PDFs refuse to open. Metadata search, byte-level form
  listing, and save-a-copy still work.
  Set `FERRITE_PDFIUM_PATH` to override the system library lookup.
- `ferrite-tabs/gtk`: Wayland `build_gtk` path (Linux, `WRY_WAYLAND=1`).

## Runtime configuration

| Variable | Effect |
|---|---|
| `FERRITE_PORT` | Fixed proxy port (random if unset) |
| `RUST_LOG` | Tracing filter (default `ferrite=debug,info`) |
| `WRY_WAYLAND=1` | Log Wayland fallback (needs `gtk` feature to render) |
| `FERRITE_FILTER_URL` / `FERRITE_THREAT_URL` | Hourly updater file fallback |
| `FERRITE_RELAY_ALLOWED_ORIGINS` | Browser origins allowed on the relay WS (comma list; empty Origin always allowed) |
| `FERRITE_RELAY_PUBLIC_IP` | Public IP advertised in TURN relay candidates (required in production) |
| `FERRITE_RELAY_ALLOW_LOOPBACK=1` | Dev-only: permit loopback TURN candidates without `FERRITE_RELAY_PUBLIC_IP` (relay refuses to start otherwise) |
| `FERRITE_PDFIUM_PATH` | Override pdfium library lookup |
| `FERRITE_DATA_DIR` | Override the OS data dir (portable installs, tests) |

Files live under the OS data dir, resolved by
`ferrite-shell::profiles::data_root()` (`%APPDATA%/Ferrite` on Windows,
`~/Library/Application Support/Ferrite` on macOS, `$XDG_DATA_HOME/ferrite` or
`~/.local/share/ferrite` on Linux; `FERRITE_DATA_DIR` overrides everything for
portable installs): `ferrite.db` (vault), `ferrite-archive.db`,
`ferrite-settings.enc`, `ferrite-session.enc`, `ferrite-threat.db`,
`ferrite-crash.log`, `ferrite-session.lock`, `ferrite.lock`,
`ferrite-crash-count`, `ferrite-profile/` (engine data), `ferrite-profiles/`
 (non-default profiles). Debug builds only, and only when no console is
 available (launched from Explorer rather than a terminal), also write
 `ferrite-debug.log` (capped at 512KB); shipped builds never create it.
 Pre-data-root launch-directory files migrate once at boot (byte-verified
 copy, originals renamed to `*.migrated-<millis>`, never deleted).

## Managed installs (master preferences)

 Drop a `master-preferences.json` beside the data root before first run to
 seed homepage, startup behavior, and search engine:

 ```json
 {
   "homepage": "https://intranet.example/",
   "startup_mode": "urls",
   "startup_urls": ["https://intranet.example/", "https://tickets.example/"],
   "search_engine": "duckduckgo"
 }
 ```

 Applies once, only when no settings file exists yet (fresh profile).
 Unknown keys are ignored; invalid values fall back to defaults
  (`restore` mode, default engine). User settings always win afterwards;
  nothing is ever overlaid onto an existing profile.

## Release signing (updates refuse to install without this)

1. Generate an Ed25519 seed once, offline:
   `py -3 -c "import os; print(os.urandom(32).hex())"`.
2. Derive the pubkey and provision it to the app via the
   `FERRITE_RELEASE_ROOT` env var (64 hex chars, read at boot; optional
   `FERRITE_UPDATE_URL` manifest override and `FERRITE_CHANNEL`
   stable/beta/dev selector for testing). Release builds may alternatively
   bake the stable root at compile time via `FERRITE_RELEASE_ROOT_BAKED`
   (explicit env still wins per boot, for rotation and channel testing).
3. Store the seed as the `FERRITE_RELEASE_SIGNING_SEED` CI secret; the
   release workflow signs `latest.json` per channel automatically.
4. Without the secret, manifests ship unsigned and clients fail closed.
5. The manifest carries one artifact per Rust target triple under each
   channel's `targets` map; the client selects its own triple and refuses
   when absent (never a foreign binary). Release tarballs are named
   `ferrite-<triple>.tar.gz` per channel tag.
