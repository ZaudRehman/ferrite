# Security Policy

## Supported versions

| Version | Supported |
| ------- | --------- |
| `main` (pre-1.0) | Best effort, security fixes land on `main` first |
| Tagged `v*` releases | Critical fixes backported to the latest tag only |

Pre-1.0 means APIs and storage formats may still change; the honest
baseline for auditing is `docs/LIMITATIONS.md` plus `docs/STORAGE_CONTRACT.md`
and `docs/TELEMETRY.md`. Read all three before auditing.

## Reporting a vulnerability

- **Do not open a public issue.** Use GitHub's private vulnerability
  reporting (the repository's Security tab → Report a vulnerability)
  with: affected version/commit, steps to reproduce, and impact.
- Expect acknowledgment within **72 hours** and a fix timeline within
  **14 days** for critical issues (RCE, credential disclosure, sandbox
  escape, update-signature bypass).
- Coordinated disclosure: we will credit reporters in release notes unless
  you ask to stay anonymous.

## Scope

In scope: `crates/*` (proxy choke, privacy engine, vault/credentials crypto,
tab IPC, shell, sync/mTLS, updater signature verification), `relay/`
(auth, isolation, TURN), supply chain (`Cargo.lock`, `deny.toml`).

Out of scope (known-deferred, see `docs/LIMITATIONS.md`): DoH off by
default, relay sharding, Wayland-specific compositor bugs.

## Hardening already in place

- Updater refuses unsigned manifests (fail-closed without provisioned root).
- Vault refuses keyed open without SQLCipher (fail-closed, never plaintext).
- `cargo-deny` gates advisories/licenses in CI; SBOM ships with releases.
