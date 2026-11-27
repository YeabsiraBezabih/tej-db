## Summary
<!-- Describe what this PR changes and why. -->

## Related Issue
<!-- Closes #... -->

## Checklist
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --workspace --exclude tej-db-benches --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --workspace --exclude tej-db-benches -j 2` passes
- [ ] `./scripts/check_agents_rules.sh` passes (no source file > 500 lines, no `.unwrap()`/`.expect()` in production crates)
- [ ] Added/updated tests (including regression tests for bug fixes)
- [ ] Updated relevant `README.md` / `CHANGELOG.md` if user-facing behavior changed
