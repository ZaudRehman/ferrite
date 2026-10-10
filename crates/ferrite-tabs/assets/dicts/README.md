# Bundled spellcheck dictionaries

Windows and Linux Chromium fetches Hunspell `.bdic` dictionaries on demand
from a Google CDN the first time a language is needed. Ferrite blocks those
fetches at the proxy choke (`ferrite-privacy::spellcheck::{is_dict_host,
is_dict_url}`, enforced as `BlockTracker`), so dictionaries must arrive
another way: this directory.

## Sources (either is the same upstream bits)

- Chromium: `https://redirector.gvt1.com/edgedl/chrome/dict/<lang>.bdic`
  (canonical, but fetching it is exactly what the block exists to prevent
  at runtime, fetch once at build/provision time, verify, then ship).
- Electron's `hunspell_dictionaries.zip` (same Hunspell sources, repackaged).

## Install

Place one `<lang>.bdic` per active language (e.g. `en-US.bdic`, `de.bdic`)
in the **runtime** directory `ferrite-profile/Dictionaries/` (next to the
engine profile, created on first run). Files here in the repo are _not_
auto-copied; this directory documents the contract.

## Verification

Each file must be pinned by SHA-256 in the release notes before it ships.
The proxy block holds whether or not files are present, a missing
dictionary degrades to fewer suggestions, never to a network fetch. The
shell logs the installed count at boot (`bundled spellcheck dictionaries`).

## License

Hunspell dictionaries are LGPL-2.1 OR GPL-2.0 OR MPL-1.1 (Kevin Hendricks
and contributors). They are a bundled asset, not a Cargo crate, so they
carry a manual entry on `ferrite://credits` (see `MANUAL_CREDITS` in
`crates/ferrite-tabs/src/protocol.rs`) rather than a `cargo-about` row.
