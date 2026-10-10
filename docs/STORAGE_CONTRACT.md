# Ferrite Storage Contract

**Rule: user data exists in exactly two states: encrypted on this device, or in RAM. Nothing else.**

Adopted 2026-09-17. This document is normative: new storage must comply,
and the gates below exist to catch regressions at build time, not in review.

## Covered stores (all encrypted, single device key from the OS keyring)

| Store | Mechanism | Key |
|---|---|---|
| Vault (history, bookmarks, passwords, autofill, TOTP, permissions) | SQLCipher (`sqlcipher` feature) | Master key |
| Archive (page snapshots, FTS) | SQLCipher (`sqlcipher` feature) | Master key |
| Credentials tables | Inside the vault connection | Master key |
| Session (open tabs) | AES-GCM file, master key | Master key |
| Settings | AES-GCM file, master key | Master key |

Field-level AES-GCM inside credentials/autofill is defense in depth, not
the boundary: the database file itself is the boundary.

## Named exceptions (the only plaintext allowed)

1. **Threat-intel cache** (`ferrite-threat.db`): downloaded public
   SHA-256 hashes plus sync metadata, verified by the
   `schema_holds_no_user_data` test to contain no URLs, titles,
   passwords, history, or bookmarks. The test pins the exact table set;
   any new table or user-data-shaped column fails the build's test suite
   and forces a contract discussion first. Its in-memory fallback loses
   nothing private (lookups fail open by design).
2. **Engine-owned profile** (WebView2/HTTP cache, cookies, local storage).
   Chromium's native formats, outside Ferrite's code. Disclosed in-product
   on the privacy page; private tabs run the non-persistent store and never
   touch it. Recheck every wry/engine bump.
3. **Spellcheck dictionary bundles** (`ferrite-profile/Dictionaries`,
   `.bdic` files): downloaded public word lists. No user content by
   construction; the fetcher validates bytes before the atomic write
   (`store_dict_bytes`), and lookup never writes back. Same class as
   exception 1: public data, plaintext by design.

There is no flags-file exception: no such file exists in this tree. Lock
files (`ferrite-session.lock`), the crash counter, and update-staging
metadata carry no user content (presence/counts only) and stay outside
the encrypted stores by design, like process pidfiles. A future exception
must be added here with its justification before it ships.

## Structural enforcement

- **Build:** release profile without the `sqlcipher` feature fails the
  build (`build.rs` gate in every SQLCipher crate). Unencrypted at-rest
  storage is not shippable, so it is not buildable.
- **Boot:** every store is opened and verified before first paint. Any
  failure (including a non-durably-stored key) stops boot on an explicit
  decision screen: Retry, explicit Incognito mode, or Quit. Silent
  in-memory fallback does not exist in release paths.
- **Ephemeral by choice only.** Incognito mode runs memory-only stores behind
  an explicit user decision plus a persistent banner. Nothing else in the
  tree may call `open_in_memory` for a persistent store's job.

## Out of scope by decision (recorded, not deferred)

- **Sync** is live relay plus TTL-bound mailbox: the server forwards
  in-flight encrypted blobs between currently-connected devices, and
holds addressed ciphertext for offline devices under strict bounds:
100 slots per recipient, 1 GiB total, 30-day expiry, delivery-delete,
  unreadable server-side (`relay/blob.go`). Pure store-and-forward
  (indefinite retention) stays rejected. Verify by: one directory
  (`FERRITE_RELAY_DATA_DIR`, default `./ferrite-blobs`), no other file
  writes in `relay/`, expiry sweeper hourly + on every store/drain.
- **Minidumps:** no dump redirection exists in-tree (wry 0.56 exposes no
  hook), so there is no local dump folder to watch. The WebView2
  baseline collection stands as disclosed on the About page. Recheck
  every wry bump; if a hook appears, dumps redirect locally and enter
  this contract (encrypt on arrival, delete plaintext).
- **Crash log and diagnostics** obey the contract by containing no user
  data: panic messages are URL-scrubbed at write time (the crash hint
  quotes those same scrubbed lines), and both copy-diagnostics actions
  emit version/engine/OS only.
