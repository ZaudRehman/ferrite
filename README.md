# Ferrite

Privacy-first desktop browser. Rust, `wry` + `egui` + `winit`, local proxy for all network.

## Overview

Ferrite routes every tab through a local forward proxy on `127.0.0.1`. Filtering, threat checks, and header policy run before any upstream byte. No extension API to bypass.

## Features

- **Network choke**: blind `CONNECT` tunnels, `204`/`302` blocking, pooled upstream, `16KB` head cap, 30s timeout.
- **Filtering**: `adblock-rust` trie with `Sec-Fetch-Dest` mapping, cosmetic selectors, `Sec-GPC: 1` injection, `Set-Cookie` stripping for trackers.
- **Threat intel**: file-backed SQLite (`Mutex<Connection>` WAL), atomic snapshot swap, 300ms fail-open `k-anon` prefix for top-level navigations.
- **Content scripts**: param stripping, De-AMP, fingerprint spoof (`Canvas`/`WebGL`/`Audio`) at `document_start`.
- **Shell**: `wgpu` + `egui` with DPI-aware `wry` child webviews, sharp/pill chrome, `Ctrl+Shift+A` tab search, `ferrite://` internal pages.

## Architecture

```
ferrite-browser (binary) -> ferrite-shell (window, theme, egui)
  -> ferrite-tabs (TabManager, WebViewHandle, spatial, IPC)
  -> ferrite-omnibox (classification, engines, autocomplete, hints)
  -> ferrite-proxy (tokio + httparse, DecisionHook, telemetry, fork)
  -> ferrite-privacy (PrivacyEngine, FilterEngine, ThreatDb, JS)
```

The proxy owns its `tokio` runtime on a dedicated thread. The shell holds `ProxyHandle` and an `Arc<PrivacyEngine>` as `DecisionHook`.

## Getting Started

### Requirements

- Rust 1.96+, Windows 10/11 (Linux X11/Wayland via `WRY_WAYLAND` stub)
- `cargo`

### Run

```powershell
cargo run -p ferrite-browser
# fixed proxy port for debugging
$env:FERRITE_PORT=18944; cargo run -p ferrite-browser
# with log filter
$env:RUST_LOG="ferrite=debug"; cargo run -p ferrite-browser
```

### Build

```powershell
cargo build
cargo build --release
```

## Testing and Quality

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Current: `fmt` clean, `clippy` 0 warnings, 157 tests passed.

## Configuration

- `FERRITE_PORT`: fixed proxy port (random if unset)
- `RUST_LOG`: tracing filter (default `ferrite=debug,info`)
- `WRY_WAYLAND=1`: log Wayland fallback (build still uses `build_as_child`)
- `ferrite-settings.json`: privacy toggles (adblock, GPC, HTTPS upgrade, real-time)
- `ferrite-threat.db`: threat hashes (SQLite, created in CWD)
- `easylist.txt` / `threats.json`: hourly updater file fallback (or `FERRITE_FILTER_URL` / `FERRITE_THREAT_URL`)

## Project Status

Parts 1-5 implemented and production grade for the choke. Not yet a full consumer browser.

- Done: window/shell, tabs with suspend/discard, omnibox with 4 engines and autocomplete, proxy with telemetry/fork, privacy with trie and SQLite.
- Next: Archive pipeline (fork consumer), Vault, Sync, and CDN fetch for hourly updates.

## License

TBD

## Contributing

Issues and PRs welcome. Keep changes typed (`thiserror` in libs, `anyhow` in binary), `tracing` only, no `unwrap`/`expect` in production paths.
