# Ferrite Limitations, complete internal record (Honest Disclosure)

> Internal companion to `../docs/LIMITATIONS.md`, which is the short
> public excerpt. This file keeps the engineering detail (mechanisms,
> struct names, version-pinned API gaps, P0–P2 history). The public file
> must stay free of all of that so the two never need co-maintenance:
> user-visible behaviors live in both, mechanisms live only here.

This document honestly discloses what is hardened, what is deferred, and
what operators must provision. Updated 2026-09-11 after P0–P2.

## Hardened in P0–P2 (no longer limitations)

- **Proxy choke**: chunked re-framing, `host:port` pooling with TTL, exact
  `ferrite` internal hosts, split timeouts, header hygiene, pipelined-body
  handling. `CONNECT` remains a blind tunnel by design (no MITM).
- **Privacy recall**: real-URL adblock matching, suffix threat intel,
  in-memory hot path, bounded tracker LRU, strict proxy-client fetchers.
- **Storage crypto**: SQLCipher fail-fast (no silent plaintext), strict
  AES-GCM everywhere, exact-origin credential matching, tombstones + HLC
  LWW, atomic settings/session writes.
- **Privileged UI**: escaped `ferrite://` with real 404 + CSP, exact route
  matching, bounded + rate-limited IPC, title-spoof fix.
- **Sync trust**: identity-bound certs, fail-closed pinning with real
  handshake-signature verification, signed relay sessions, signed releases
  (fail-closed without a provisioned root), real failover probing.
- **Durability**: transactional archive/FTS, FTS/LIKE escaping, bounded
  retention everywhere, hardened sanitizer + shm + renderer protocol.
- **Perf**: OnDemand repaint with dirty triggers, virtualized tab paint,
  paged history with DB fallback, SQL-filtered deltas, Raft replaced by
  TTL live-tab presence.

## Still deferred (honest)

### Network
- **DoH / encrypted DNS: optional, off by default.** Set `doh_endpoint` to an
  `https://` JSON endpoint to route proxy DNS through it (invalid values are
  refused at import with a warning, never probed). Defaults stay OS DNS:
  packet capture shows plaintext DNS on port 53 unless you opt in. Even with
  DoH on, the DoH server's own hostname resolves via the OS (bootstrap), and
  `direct.rs` (updaters, breach checks) always uses the OS resolver.
- **Predictive pre-connect**: the pool is reactive only (LRU 64×2).
- **Passive TLS metadata**: `CONNECT` tunnels carry no ClientHello
  inspection; `NetworkEvent` has no TLS version/cipher fields.

### Process isolation
- Proxy + PDF renderer run as threads/child processes without heartbeat
  supervision or hot-restart. A proxy that fails at boot parks the first
  tab on an error page (fail-closed, never silent direct), but a proxy
  that dies mid-session is not re-probed: its tabs keep a dead tunnel
  and restored sessions skip the proxy-down page. The 500ms shell
  heartbeat is the place to add a liveness re-probe.

### Vault / Crypto operations
- SQLCipher builds require `perl` + a C toolchain (see `docs/SETUP.md`).
  Without `--features sqlcipher`, keyed vaults **refuse to open**
  (fail-closed, never plaintext).
- Release-root key provisioning is an operator step: without
  `FERRITE_RELEASE_ROOT` (app boot env) and `FERRITE_RELEASE_SIGNING_SEED`
  (CI), the updater refuses all updates (fail-closed). When provisioned,
  the shell checks hourly plus on About-page demand, downloads delta-first
  with progress, stages verified artifacts, and applies at next boot with
  health-gated rollback; restart prompts escalate for security releases.
  macOS Sparkle is deliberately not integrated (separate native stack).

### Credentials
- Wordlist is synthetic `wordXXXX` entries with correct 77.5-bit entropy
  but not real EFF memorability. TOTP is SHA1/6-digit/30s only.

### Import boundaries (decided, not deferred)
- **Payments and cookies are never imported.** Cards would arrive without
  CVVs (never stored, by design) and cookies are live sessions; both
  would be lossy or misleading copies. Addresses, search engines,
  bookmarks, and history import with explicit one-click consent.
- **Keychains are never read.** Passwords arrive via CSV export only;
  direct reads of Chrome/Firefox/Safari secret stores would put other
  apps' secrets in our process memory.
- **Chrome open tabs are not imported.** The session format is an
  undocumented binary blob with no verifiable fixtures; a hand parser
  would be guesswork. History import covers the URLs.
- **Firefox compressed backups (.jsonlz4) and Safari binary plists are
  refused with guidance**, not silently skipped: the error text says
  exactly which export to produce instead.
- Nothing imports silently or automatically: every import is an explicit
  click with exact counts back, and repeat imports dedupe.

### Sync scale
- **Shell integration: DONE (trust ceremony + LAN live transport + relay
  client).** `ShellApp` holds the trust graph (`TrustedStore` on
  the encrypted vault), this device's long-term identity (vault-sealed
  seed), the pairing ceremony (Settings → Sync: device list, pairing codes,
  approvals, revocations, category toggles, discovered peers, live tabs),
   and a live session: pinned-QUIC accept loop + dial-out rounds every 15s
   (plus one immediate round at boot) over mDNS-discovered LAN peers, CRDT
   apply in one transaction, presence fan-out with TTL inbox. Trust gates
   dialing (unknown devices are never dialed; the handshake enforces the
   key regardless), revocation takes effect on the next round without
   re-pairing, history stays opt-in, ephemeral processes run transport-off.
- **Relay client: DONE (Rust `RelayLink` + persistent serve loop).**
  Off-LAN rounds reuse the LAN message shapes over signal envelopes;
  untrusted senders are dropped before bytes reach the engine. Relay
  URLs are operator input in Settings → Sync (empty = LAN-only).
- **Relay mailbox (TTL blobs): DONE.** The server holds addressed
  ciphertext for offline devices: 100 slots/recipient, 1 GiB total,
  30-day expiry, delivery-delete with ack, unreadable server-side
  (see `docs/STORAGE_CONTRACT.md`). Indefinite store-and-forward stays
  rejected: both devices need never be online *together*, but mail
   still rots on a clock.
- **Relay semantics (decided, docs/STORAGE_CONTRACT.md)**: live
  forwarding plus TTL mailbox: addressed ciphertext held at most
  30 days / 100 slots per recipient / 1 GiB total, delivery-deleted,
  unreadable server-side. Indefinite retention stays rejected.
  Presence is in-memory only.
- Per-device causality is approximated by HLC high-water marks (origin
  columns deferred). Vector-clock counters are diagnostic.
- mDNS/DNS-SD storms beyond ~50 LAN peers untested. Relay is a single
  process (no sharding); presence is in-memory only.
- TURN requires a live WS session per username; no shared-secret rotation.

### Platform
- Wayland: `WRY_WAYLAND` + `gtk` feature path compile-gated in CI
  (`wayland` job) but behaviorally untested; runners have no display, so
  no test clicks through it. Primary target is Windows + X11.
- JS dialogs (`alert`/`confirm`/`prompt`/`beforeunload`): Windows shows the
  Ferrite chrome overlay (origin-attributed, tab-modal, suppressible) via
  WebView2 `ScriptDialogOpening`; macOS/Linux keep engine-default dialogs
  because wry surfaces no interception hook there, same class of gap as
  Wayland, fixed when the toolkit exposes it.

### Push notifications
- **Shipped:** permission requests broker like camera/mic (deny-by-default,
  chip, backoff, per-site grants), plus a Settings → Privacy master switch
  (`notifications_global`, silent deny when off, persisted). In-app toasts
  are a separate, chrome-rendered system and unaffected by the switch.
- **Inherited, not built:** Service Worker execution, VAPID/payload
  encryption, OS-center display, and notification-body attribution are all
  engine-native. Prompt and grant rows show the engine-resolved ASCII
  origin (`origin_of` never decodes punycode, so homographs display as
  `xn--`, the safe direction); the rendered notification's own source line
  is the engine's to get right.
- **Push relay endpoint unverified.** Chromium's Push API has historically
  resolved subscriptions against Google (FCM) infrastructure; whether a
  WebView2-hosted Ferrite tab inherits that wiring unmodified (vs full
  Edge) is unconfirmed. Payloads stay end-to-end encrypted either way, but
  subscription existence and delivery timing may pass through third-party
  infrastructure. Verify before claiming: register a subscription from a
  Ferrite-loaded page and inspect the endpoint domain; until then the
  Settings row states plainly that Ferrite does not operate the relay.
- Single-instance lock: implemented (`fs2` exclusive lock on
  `ferrite.lock` / `ferrite-incognito.lock` in the data root). A second
  launch hands its URL to the live window over a token-authed loopback
  forwarder (same mode only, `http(s)`/`ferrite` URLs, 500ms budget) and
  exits; anything else gets the refuse dialog. No cross-mode forwarding.
- Downloads page/badge: files land safely in Downloads with events in
  diagnostics; no dedicated downloads UI yet.

### Media / DRM / casting
- **Widevine: not provided, by platform structure.** WebView2 supports
  PlayReady natively and exposes no Widevine CDM (open upstream feature
  request, unresolved), and there is no plugin API a CDM could attach
  to; so this cannot be fixed from Ferrite's side. Widevine-only
  services do not play (the Spotify web player is the notable casualty).
  Most premium video also serves PlayReady, which plays normally, as
  does all unencrypted media. Per-service status is unverified, so
  no service list is claimed here. A blocked player gets an explicit
  toast naming the cause, never a silent black box. Even a future WebView2 Widevine would still need Google VMP
  certification per browser build (a licensing process, not an engineering
  sprint), so this is disclosed as structural, not scheduled.
- **macOS DRM unconfirmed, Linux DRM weak.** Whether WKWebView exposes CDM
  support to embeds (vs full Safari) is unverified; WebKitGTK EME support
  is historically limited. Same honest toast path on both.
- **H.264 licensing: needs counsel review.** WebView2 inherits H.264 via
  Microsoft's licensed Edge build, but whether that coverage extends to a
  redistributing third-party app is a legal question, not an engineering
  one. Ferrite's own media (if any ships) must use royalty-free
  unencrypted codecs (AV1/VP9 + Opus) to sidestep this entirely.
- **Casting: no native hook.** wry 0.56.1 exposes no Cast/Presentation/
  RemotePlayback/AirPlay surface (verified in vendored sources), so there
  is no Ferrite cast action; a Cast SDK would be a separate native
  integration. Recheck each wry upgrade.
- **GPU decode: engine-controlled, no toggle.** A hardware-acceleration
  switch cannot ship safely: on Windows, custom browser args replace wry's
  internally built args including the `--proxy-server` default, so a
  `--disable-gpu` flag would silently route tabs around the privacy proxy.
  See `ferrite-tabs/src/media.rs` for the verified analysis.

### Printing
- **Dialog only, no silent printing.** wry exposes just `window.print()`
  (engine preview + OS dialog) and no controller handle to call
  `PrintAsync` on, so there is no print-to-named-printer path, no
  Ferrite-side copies/duplex/color panel (those live on the unreachable
  native settings object), and no branded picker. Shipped around the
  dialog: a per-tab in-flight guard (the engine throws on concurrent jobs
  instead of queueing), a no-printers preflight that still names the
  dialog's Save as PDF, and failure toasts. Guard releases on navigation
  commit or 5-minute stale eviction.
- **macOS print / Linux CUPS parity unconfirmed.** The dialog path is the
  same call everywhere, but silent-print equivalents on those platforms
  are unverified; assumed absent until tested.

### Accessibility / UX
- No high-contrast mode, no font scaling, no screen-reader roles audit. `prefers-reduced-motion` is honored in
  chrome (all strip/popover motion drops to instant) and inherited via
  media queries in content. Diagnostics default off (F3).
- Screen-reader bridge (verified, not assumed): every webview builds as a
  child of the main window so the OS composes the subtrees; exactly one
  tab is OS-visible at a time (`show_only_active`, pinned by
  `ferrite-tabs/src/a11y.rs`), which is what excludes background tabs
  from the AT tab order; overlay pages carry menu/dialog roles with
  focus-first keyboard flow, and dismissing an overlay refocuses the
  originating tab. Reduced-motion and OS a11y prefs are engine-inherited
  via media queries, never polled.
- Still manual (no headless harness can run a real AT stack): crossing
  from chrome into content with an actual screen reader, and chrome
  live-region announcement while focus sits in page content. An axe-core
  CI pass is deferred for the same reason (needs rendered output).

### Chrome refinement boundaries
- **Remote suggestions are DuckDuckGo-only** (opt-in, off by default,
  never from private tabs, only when DDG is the effective engine).
  Per-engine suggestion APIs are not built: a non-DDG default with the
  toggle on simply gets no remote rows, by design.
- **Tab groups, vertical tabs, workspaces, and multi-select are built.**
  Tear-off and multi-window are not: the manager is single-window by
  construction (dragging a tab out of the window has nowhere to land,
  and session restore covers one window). Profiles run side-by-side
  processes instead; that is the multi-surface story.
- **Page screenshots are not built.** wry 0.56.1 exposes no capture
  surface (`CapturePreview`/pixels are unreachable through the pinned
  version), same constraint as live hover thumbnails. Print-to-PDF
  through the OS print dialog covers page export. Recheck each wry
  upgrade alongside the thumbnail item below.
- **Favicons:** engine-reported `link[rel=icon]` URLs, Rust-fetched
  through blocking `ureq` on a worker (64KB cap, raster magic only,
  SVG refused), 200-host session cache. Letter avatars remain for
  privileged pages, newtab tiles, suggestions, and bookmarks (push-size
  bound, by design).
- **Tab previews:** enhanced rich card (icon, title, URL, audio/muted/
  inactive state). Live bitmap thumbnails are not built: wry 0.56.1
  exposes no capture surface (`CapturePreview`/`takeSnapshot` are
  unreachable through the pinned version). Recheck each wry upgrade.
- **OS materials:** Windows tries Mica (`DWMWA_SYSTEMBACKDROP_TYPE`)
  with solid-caption fallback; macOS uses a transparent titlebar with
  full-size content (no vibrancy view inserted, the webview covers the
  content); Linux follows the GNOME accent color when `gsettings`
  answers (else the pack accent). None of this is verified by CI on
  all three targets; visual sign-off is manual per platform.

### Crash reporting
- **WebView2 crash-upload default stands (unreachable).** WebView2 sends
  minidumps to Microsoft on renderer crashes unless
  `IsCustomCrashReportingEnabled` is set, and wry 0.56.1 exposes no hook
  to set it (verified in vendored sources; the COM interface exists in
  webview2-com but wry builds the environment internally). Baseline
  WebView2 diagnostic collection is OS-governed regardless. The About
  page states this boundary where users can find it. Recheck each wry
  upgrade.
- **SmartScreen resolved as unavailable.** wry disables
  `msSmartScreenProtection` in its default args and exposes no
  `IsReputationCheckingRequired` switch; malware/phishing protection is
  Ferrite's own proxy threat stack, not SmartScreen.
- **Native dumps deferred.** Panics are hooked with version-tagged log
  lines; abnormal exits of any cause trip the session marker and the
  version-scoped crash counter. True minidumps for segfault-class crashes
  would need a native crash-handler crate (new dependency, offline
  build); detection is cause-agnostic, only non-panic details are missing.
- **Crash-to-rollback loop wired.** Crash lines and the counter carry the
  exact build version (a fixed bug stops escalating on update), and the
  restore page offers rollback to the updater-retained previous release
  with graceful relaunch when one exists.

### Privacy operations
- Relay logs truncated device IDs only (no IPs at log level); presence and
  TURN credentials are memory-only with 24h credential eviction. Self-erasure
  is a protocol message (`erase_me`: drops the session, the id→key pin, and
  cached TURN credentials immediately). mDNS broadcasts stable device IDs on
  the LAN while discovery runs.
- Local erasure is the Clear browsing data flow (history/downloads/cookies/
  cache by range) plus per-device revocation (Settings → Sync) plus recovery
  sidecar disable. There is no central account to delete: nothing leaves the
  device except the consent-gated signals enumerated in `docs/TELEMETRY.md`.
- No usage collection exists in the shipped binary (see
  `docs/TELEMETRY.md` for the enumerated egress inventory and the
  dependency gate); the trade-off is accepted blindness on prioritization.

### Memory / storage accounting
- **Filter-list "memory" is source text, not heap.** The task manager shows
  per-list source bytes + rule lines; the `adblock` crate exposes no heap
  API, so the page labels the figures as source size. Treat them as a floor
  for the compiled index, never a heap measurement.
- **Forget-site scope is enumerated, not total.** `ferrite://site` deletes
  history, saved logins, permission grants, zoom, cookies, and remembered
  form values for one origin. Bookmarks, download records, and autofill
  address/card records stay: they are user-managed and carry no origin key,
  so deleting them would destroy user data, not site residue. The page and
  its confirm dialog state this scope explicitly.

All above are **deferred honestly**, not hidden via `let _ =` stubs.
Historical `CHECKSUM`/`I-*` tracking references in earlier revisions are
superseded by `../CHECKSUM/PRODUCTION_PLAN.md` (P0–P2 DONE).
