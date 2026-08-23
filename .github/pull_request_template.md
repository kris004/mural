## Summary

- What changed?
- Why is this the smallest suitable change?

## Validation

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test --workspace --all-targets --locked`
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- [ ] Documentation and man pages were updated for user-visible changes.
- [ ] Logs, examples, and fixtures contain no private or machine-specific data.

For renderer, transition, output, or service changes, include the compositor,
GPU/driver, exercised commands, and live result. Note compatibility or rollback
considerations for protocol, state, cache, and installed-service changes.
