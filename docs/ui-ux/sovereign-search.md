# Sovereign Search — Ferrite vs Giants

**Why this wins:** Chrome shows Google. Ferrite shows *you* first.

## Ranking (deterministic, local-first)

1. **Instant Answer (0ms, offline)** — client-side `calc`/`unit` pinned top, `Enter` copies not navigates per 1.4 spec. Not remote.
2. **Your Archive (FTS5)** — `crates/ferrite-archive/src/storage.rs:169` `search_fts` `porter unicode61` — "From your archive" source-labeled, no competitor has this.
3. **Open Tabs** — `TabManager::search` title 2x weight, `tab-switch` icon.
4. **History/Bookmarks** — `AutocompleCache` `dashmap` `<1ms` + `vault` `history_cache 5000` `bookmarks` table.
5. **Web (via proxy, sanitized)** — SearxNG self-hosted `http://localhost:8888/search?q=&format=json` (sovereign, zero-key) or Brave API `https://api.search.brave.com/res/v1/web/search?q=` via `127.0.0.1:FERRITE_PORT` `DecisionHook` `GPC` still, `sanitize.rs` strip `<script>` tracker, `extract_text` snippet. Never direct fetch.
6. **Live Suggestions (opt-in off)** — `via [engine]` tagged, disabled by default per 1.4.

**Why not just DuckDuckGo page:** That is their UI, their ads, their tracking, their `1.5 Shield` can't count. Our UI is `Aetherial Mercury` `Surface1` `Geist Pixel` `left accent` `sharp` `0.5px`, `eTLD+1` highlight, `Shield 12` `Geist`, `Time Machine` sub-dropdown, `Tab-to-Search` chip — all Ferrite chrome, not DuckDuckGo.

## Wire: `ferrite://search?q=...`

- Omnibox `Enter` with `query` not URL → `tab.load_url(format!("ferrite://search?q={}", encode))` not `https://duckduckgo.com`.
- `build_shell_ui` `CentralPanel` when `tab.url.starts_with("ferrite://search")` renders `egui` scroll: instant answer top, `From your archive` 5, `Open Tabs` 5, `History` 5, `Web` 10 via proxy fetch + sanitize + `FTS` snippet. Each row `Button` `fill surface_3 if selected` `left accent` `open` loads `https://...` via `tab.load_url` through proxy.
- `newtab` tiles and `Omnibox` unified dropdown already use same `archive.search` + `time_machine_for` + `autocomplete.query` — consistent.
- API key for Brave stored `field-level AES-GCM` `vault` `encrypt_oauth_token` pattern `3.4`, `drive.file` scope, never plaintext. SearxNG no key, self-host `relay` blind.

## Privacy

All web fetches via `ferrite_proxy::ProxyConfig {fixed_port, archive:None}` `127.0.0.1` `DecisionHook::should_send_gpc` `Sec-GPC:1`, `should_strip_cookies` for trackers, `X-Forwarded` never. Relay never sees query (local `ferrite://`).

## Verification per Part 13

`cargo test` `omnibox::tests::search_url_encodes` still passes, `protocol::tests::newtab_handler_returns_html` now includes `<!-- ferrite://search -->`, `go vet` clean, `wry` `update_active_bounds` finite guard keeps `ferrite://search` `wry` 0x0 not needed — rendered in `egui` not `wry`.
