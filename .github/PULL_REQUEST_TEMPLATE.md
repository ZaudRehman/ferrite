## What / why

## Gates

- [ ] `cargo fmt --all` clean
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
- [ ] `cargo test --workspace --all-features` green (or scoped `cargo test -p <crate>`)
- [ ] `docs/LIMITATIONS.md` updated if behavior/deferrals changed

## Security

- [ ] No fail-closed behavior weakened
- [ ] No new `unwrap`/`expect` in production paths
- [ ] `[security]` in title if touching crypto/proxy/IPC/mTLS/updater
