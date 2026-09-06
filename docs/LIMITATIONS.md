# Ferrite Limitations (Honest Disclosure) — Production Browser for Millions

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
- **DoH / encrypted DNS**: plain OS DNS via `TcpStream::connect`. Packet
  capture shows plaintext DNS on port 53.
- **Predictive pre-connect**: the pool is reactive only (LRU 64×2).
- **Passive TLS metadata**: `CONNECT` tunnels carry no ClientHello
  inspection; `NetworkEvent` has no TLS version/cipher fields.

### Process isolation
- Proxy + PDF renderer run as threads/child processes without heartbeat
  supervision or hot-restart. A dead proxy degrades to direct traffic with
  only a log line (see `shell.rs` proxy-spawn fallback).

### Vault / Crypto operations
- SQLCipher builds require `perl` + a C toolchain (see `docs/SETUP.md`).
  Without `--features sqlcipher`, keyed vaults **refuse to open**
  (fail-closed, never plaintext).
- Release-root key provisioning is an operator step: without
  `provision_release_root` (app) and `FERRITE_RELEASE_SIGNING_SEED` (CI),
  the updater refuses all updates (fail-closed).

### Credentials
- Wordlist is synthetic `wordXXXX` entries with correct 77.5-bit entropy
  but not real EFF memorability. TOTP is SHA1/6-digit/30s only.

### Sync scale
- **Shell integration**: `ferrite-sync` has no entry point into `ShellApp`
  (no `Arc` held, no background tasks spawned, no pairing UI), so
  cross-device sync is inert no matter how solid the crate is in isolation.
  Wiring it is shell lifecycle + pairing UI work, not yet scheduled.
- Per-device causality is approximated by HLC high-water marks (origin
  columns deferred). Vector-clock counters are diagnostic.
- mDNS/DNS-SD storms beyond ~50 LAN peers untested. Relay is a single
  process (no sharding); presence is in-memory only.
- TURN requires a live WS session per username; no shared-secret rotation.

### Platform
- Wayland: `WRY_WAYLAND` + `gtk` feature path exists but is CI-untested;
  primary target is Windows + X11.
- Single-instance lock: not implemented (two processes share DB files via
  SQLite locking; settings/session last-writer-wins).
- Downloads page/badge: files land safely in Downloads with events in
  diagnostics; no dedicated downloads UI yet.

### Accessibility / UX
- Dark-only theme, no light/high-contrast mode, no font scaling, no
  screen-reader roles audit, no `prefers-reduced-motion` handling beyond
  the 2Hz OnDemand heartbeat. Diagnostics default off (F3).

### Privacy operations
- Relay logs truncated device IDs + IPs; no log redaction pipeline or
  GDPR erase API yet. mDNS broadcasts stable device IDs on the LAN.

All above are **deferred honestly**, not hidden via `let _ =` stubs.
Historical `CHECKSUM`/`I-*` tracking references in earlier revisions are
superseded by `docs/PRODUCTION_PLAN.md` (P0–P2 DONE).
