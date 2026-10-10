# Contributing to Ferrite

## Ground rules

- Typed errors in libraries (`thiserror`), `anyhow` only in the binary.
- `tracing` only: no `println!`, no `unwrap`/`expect` in production paths.
- Every behavior change ships with a test that fails without the fix.
- Docs change with code: `docs/LIMITATIONS.md` stays honest, or the PR
  does not merge.

## Workflow

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

All three must be green. CI runs the same plus `cargo deny` and relay
`go build/vet/test`.

## PRs

- One concern per PR. Fill out the PR template (what / why / gates).
- Touching crypto, proxy verdicts, IPC, mTLS, or updater signing? Mark it
  `[security]` in the title so it gets reviewed against the threat model
  line by line, not skimmed.
- Do not weaken fail-closed behavior to make a test pass. If a gate is
  red, the code is wrong, not the gate.

## Design tokens

Chrome tokens live in `crates/ferrite-chrome/src/tokens.rs`, ported from
`DESIGN/*.md` frontmatter by hand (no codegen on purpose). Porting a pack:
add the enum variant, its `*_CSS` const, and the `name()`/`css()` arms,
extend `ALL`, and add a contract test.
