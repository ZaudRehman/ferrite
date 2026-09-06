# Ferrite Build & Provisioning Setup

## Toolchains

- Rust `1.96.0` (see `rust-toolchain.toml`), `rustfmt` + `clippy`.
- Go `1.27.0` for `relay/` (`go build/vet/test ./...` inside `relay/`).
- Node/tooling not required.

## Standard build (no SQLCipher)

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
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

- `ferrite-archive/pdfium-render`: real PDF engine. Without it, PDFs
  render a 1×1 placeholder and encrypted PDFs fail closed.
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
| `FERRITE_RELAY_PUBLIC_IP` | Public IP advertised in TURN relay candidates |
| `FERRITE_PDFIUM_PATH` | Override pdfium library lookup |

Files in the working directory: `ferrite-settings.json` (privacy +
search engine), `ferrite-session.json` (crash-recovery tabs),
`ferrite-threat.db`, `ferrite-crash.log`, `ferrite-rollout-bucket`.

## Release signing (updates refuse to install without this)

1. Generate an Ed25519 seed once, offline:
   `python3 -c "import os; print(os.urandom(32).hex())"`.
2. Derive the pubkey and provision it to the app via
   `provision_release_root(pubkey_hex)` (wired at startup).
3. Store the seed as the `FERRITE_RELEASE_SIGNING_SEED` CI secret; the
   release workflow signs `latest.json` per channel automatically.
4. Without the secret, manifests ship unsigned and clients fail closed.
