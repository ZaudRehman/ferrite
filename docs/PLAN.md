# Ferrite — Plan to Go Head to Head

**Goal:** One window that browses `https://example.com` reliably, then parity with Chrome and Firefox on speed and privacy, then advantages they cannot copy.

## Phase A — Make It Browse (2 weeks, fixes the nothing works feeling)

**Problem:** `wry` child never gets a correct `Rect`, `egui` `max_rect` goes `NaN`, omnibox does not drive navigation, tabs do not show loading.

**Work:**
- `ferrite-shell/src/shell.rs:500` `on_window_resized` already guards `NonZeroU32`, add same guard in `build_shell_ui` before `available_width().min(520.0)`: `if !rect.is_positive() || !scale.is_finite() { return None }` in `spatial.rs:23` already, but `egui::Frame` `inner_margin` + `ScrollArea` in `shell.rs:1185` and PDF `ScrollArea` need `max_width.is_finite()` check before `min`. Fix `NaN` at `layout.rs:402` by wrapping `available_width` with `if w.is_finite() { w.min(520.0) } else { 520.0 }`.
- `ferrite-shell/src/shell.rs:500` `update_active_bounds` already called, but `wry` `set_bounds` is called only after `central_resp.response.rect` is set. Ensure `content_rect_cell` is `Some` before `update_active_bounds`, and call `tab.set_bounds` on every `RedrawRequested`, not only when `central_resp` changes.
- `ferrite-omnibox/src/omnibox.rs:103` `classify` already correct, but `build_shell_ui` `pick_suggestion` must call `vault.insert_history` and `tab.load_url` and `app.omnibox.clear` and `window.request_redraw` — already done, add `tab.set_loaded(false)` + `is_loading` spinner in tab bar (`shell.rs:730` already has `ui.spinner()` when `is_loading`).
- `ferrite-tabs/src/tab.rs:90` `navigate` sets `is_loading true`, `webview.rs:48` `audio_listener_js` already, add `on_navigation_completed` via `wry` `with_navigation_handler` to call `tab.set_loaded(false)` and `vault.insert_history` from shell IPC `title` message.

**Validation:** `cargo run` `https://example.com` loads in `wry` child, `Ctrl+L` focuses omnibox, `Enter` navigates, back/forward work, `cargo test --workspace` still 177.

## Phase B — Make It Fast (2 weeks)

**Work:**
- `ferrite-proxy/src/server.rs:1008` `ConnPool` already LRU 64x2, add `SO_KEEPALIVE` and `TCP_NODELAY` already, add `read_timeout` 5s for pooled `try_pop`.
- `ferrite-vault/src/vault.rs:226` `spawn_batch_writer` 50 or 1s `BEGIN IMMEDIATE` already, ensure `history_cache` read is `O(1)` `dashmap` and `autocomplete.query` `<16ms` verified (`autocomplete.rs:198` `latency_under_16ms`).
- `ferrite-archive/src/storage.rs:1` `web_snapshots_fts` already `porter`, add `VACUUM` after `evict_lru` 10, and `ferrite-shell/src/shell.rs:1285` PDF virtualization already `visible +/-1` + `free_texture`, ensure `pdf_cache` `Mutex<HashMap>` is sharded per tab not global.

**Validation:** `Wireshark` 0 extra, `cargo test` `pipeline_throughput_no_block` 1000 `try_send` <2s, `heaptrack` flat 1000-page PDF.

## Phase C — Make It Private (1 week, hardens the choke)

**Work:**
- `ferrite-proxy/src/server.rs:532` `Sec-GPC` gate already, add `Permissions-Policy` and `Referrer-Policy` stripping for trackers.
- `ferrite-privacy/src/filters.rs:14` `adblock::Engine` already, add `csp` injection via `Engine::get_csp_directives`.
- `ferrite-vault/src/vault.rs:1` file xor now, switch to `rusqlite bundled-sqlcipher-vendored-openssl` once `perl` is available in CI (one feature change), `PRAGMA key` already handled.

**Validation:** `https://coveryourtracks.eff.org` shows `GPC` and `tracker` blocked, `ferrite.db` not openable via `sqlite3` without key.

## Phase D — Make It Permanent (1 week, archive advantage)

**Work:**
- `ferrite-archive/src/archive.rs:1` `zstd lvl10` + `sanitize` already, keep `web_snapshots` integer vs `vault` UUID both (migration note in `CHECKSUM/PART6`).
- `ferrite-shell/src/shell.rs:1085` `From your archive` FTS and `Time Machine` already inserted after `autocomplete.query`, polish dedicated `ferrite://archive/search?q=` pagination and `dashmap` 50 per URL bound (already `if entry.len()>50 drain`).
- `ferrite-tabs/src/webview.rs:269` `ferrite://archive/view?url=&timestamp=` already serves `arch.get_latest` with banner, ensure `try_serve_archive` in `proxy` already sanitized-only.

**Validation:** Disconnect, `404` on `github.com/this-does-not-exist` serves sanitized snapshot with banner, `Wireshark` 0.

## Phase E — Make It Yours (2 weeks, vault + sync prep)

**Work:**
- `ferrite-vault/src/vault.rs:1` `history`/`bookmarks` UUID `hlc_timestamp` + `is_deleted` tombstone already, add `AppContext` multi-vault for work/personal (spec 8) as `HashMap<ProfileId, Arc<Vault>>`.
- `ferrite-vault/src/import.rs:1` `import_netscape_bookmarks` regex already, add `rfd::FileDialog` picker in `shell.rs:484` `import` IPC (currently `bookmarks.html` CWD), and `export` writes `ferrite-export-bookmarks.html` with warning.
- `crates/ferrite-pdf-renderer` + `pdf.rs:1` `IsolatedPdfRenderer` `shm` `memmap2` handle over pipe already, add `form` per `PdfFormFieldType` overlay `egui` `TextEdit`/`Checkbox` and `Printer` `print` via `rframe` `Printer::print_file`.

**Validation:** Import Chrome `History` 10k rows without `SQLITE_BUSY`, `cargo test -p ferrite-vault` 8 passed -> 12 with `rfd` picker test.

## Phase F — Make It Competitive (ongoing, head to head)

**What giants cannot copy without re-architecting:**
- **Choke before TLS:** Every tab forced through `127.0.0.1` `ProxyConfig.archive` provider, `DecisionHook` before any byte. Chrome extensions cannot do this.
- **Archive as feature:** `FTS5` `porter` inside same encrypted file, `Time Machine` vs Chrome `History` only URL/title. This is the moat.
- **Vault zero-knowledge by default:** `keyring` + `bip39` + `Argon2` `recovery_key_wrap` singleton, no `SQLCipher` plaintext fallback, `ferrite.db` xor until `sqlcipher` vendored.

**Go to market order:** Fix `NaN` and `wry` sizing first, then `cargo run` `example.com` end to end, then `cargo clippy` 0 and `cargo test` 185, then `CHECKSUM/PART7` close `I-56` downloads as separate `crates/ferrite-download` and `WRY_WAYLAND` `build_gtk`.

Start with Phase A `NaN` guard and `wry` bounds, 2 days, then re-run `cargo run` and confirm navigation.
