# Contributing to TejDb

Thank you for your interest in contributing to **TejDb**! We welcome bug reports, feature proposals, documentation improvements, and pull requests.

## Getting Started

### Prerequisites

- **Rust**: Stable toolchain (`1.85.0` or newer; see [`rust-toolchain.toml`](rust-toolchain.toml))
- **Components**: `rustfmt`, `clippy`
- **Optional Targets**: `wasm32-unknown-unknown` (for verifying no-std/WASM compilation)

```bash
git clone https://github.com/YeabsiraBezabih/tej-db.git
cd tej-db
```

## Engineering Standards (`AGENTS.md`)

All contributions must adhere to the codebase standards defined in [`AGENTS.md`](AGENTS.md):

1. **Modularity & File Size Limits**:
   - No `.rs` source file under `crates/`, `examples/`, or `tests/` may exceed **500 lines**.
   - Keep `lib.rs` and `mod.rs` files as slim public facades.
2. **Error Handling & Safety**:
   - Never use `.unwrap()`, `.expect()`, or `panic!()` in production runtime paths (`crates/tej-db*`). Propagate errors via `DbError` and `?`.
   - Avoid `unsafe` outside low-level primitives (e.g., mmap/SIMD/FFI) and precede every `unsafe` block with a `// SAFETY:` comment.
3. **Test Discipline**:
   - Every bug fix must include a regression test.
   - New features must include positive, boundary/edge, and error-path tests.
   - Never weaken or `#[ignore]` existing assertions.

## Local Verification Gates

Before opening a pull request, verify all quality gates pass locally:

```bash
# 1. Formatting
cargo fmt --all -- --check

# 2. Clippy (zero warnings allowed)
cargo clippy --workspace --exclude tej-db-benches --all-targets --all-features -- -D warnings

# 3. Full test suite (use -j 2 in memory-constrained environments)
cargo test --workspace --exclude tej-db-benches -j 2

# 4. Feature-gated & cross-target checks
cargo check -p tej-db-pager --no-default-features
cargo check -p tej-db --target wasm32-unknown-unknown --no-default-features

# 5. Repository modularity & AGENTS.md rules
./scripts/check_agents_rules.sh
```

## Pull Request Process

1. Fork the repository and create a branch from `main` (e.g., `fix/pager-flush-race` or `feat/sql-window-lag`).
2. Keep commits focused and use [Conventional Commits](https://www.conventionalcommits.org/) (e.g., `feat(sql): ...`, `fix(pager): ...`, `docs: ...`).
3. Ensure all CI checks and the PR template checklist pass.
4. Update relevant crate `README.md` files and [`CHANGELOG.md`](CHANGELOG.md) when adding user-facing features or fixes.

## License

By contributing, you agree that your contributions will be dual-licensed under **MIT OR Apache-2.0** (see [`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE)).
