# Changelog

All notable changes to **TejDb** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Community & governance files (`CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `CHANGELOG.md`, `rust-toolchain.toml`).
- GitHub issue/PR templates, `CODEOWNERS`, Dependabot configuration, multi-OS CI matrix, and `v*` release workflow.

## [0.1.1] - 2026-11-27

### Added
- **Multi-Platform & Bindings**: Added `tej-db-ffi` C ABI bindings (for Flutter/Dart, iOS, Android, and desktop) and `examples/axum-api` HTTP/SQL server example.
- **Database Operations**: Added `Database::verify()` (`fsck` B+ tree ordering verifier), `Database::close()`, and `Database::recover_file()`.
- **SQL Features**: Added `ORDER BY` (with `NULLS FIRST`/`LAST`), `OFFSET`, `DISTINCT`, `LIMIT 0`, `NOT NULL` enforcement, negative PK bounds (`i64::MIN` safe), and non-INT/TEXT PK literal coercion (e.g. `UUID`).
- **Benchmarking & CI**: Added `bench_comprehensive` suite and `.github/workflows/ci.yml`.
- **Licensing**: Added `LICENSE-MIT` and `LICENSE-APACHE` at repository root.

### Fixed
- **Encryption & Storage**: Migrated `EncryptedStorage` to 1:1 tag page mapping (eliminating shared tag page RMW torn-write races), added `flush_lock` serialization in `PageCache`, made `with_page_mut` rollback-safe on error, added Windows `FileLock` via `LockFileEx`/`UnlockFileEx`, and updated `FileStorage` `set_len` / overflow bounds validation.
- **ACID & Transactions**: Fixed Phase 2 durability barrier ordering, evicted uncommitted superblocks on Phase 2 failure (`discard_page`), serialized `table_commit.rs` default table mutations under `commit_lock`, fixed mixed default/named-table OCC validation, fixed `OwnedWriteTx` drop-order UAF in `tej-db-ffi`, and added atomic temp-file + rename for `Database::backup()`.
- **Slotted Page & B-Tree Safety**: Added strict offset/cell bounds checks in `Page` slotted-page readers and deferred B-Tree page frees until post-commit.
- **SQL Optimizer & Executor**: Fixed `WHERE id > NULL` 3VL bounds, short-circuit `AND`/`OR` evaluation, `num_cols` OOM guard on corrupt rows, PK-less `UPDATE` rowid preservation, and explicit rejection of unsupported `WITH`/`FETCH`/multi-table `FROM` clauses.
- **Vector, Vectorized & Spatial**: Fixed 2-bit RaBitQ decoding (boosting recall to 95.8%–100%), preserved Vamana reachability across deletion tombstones, inverted `DotProduct` sort order, fixed `MorselScheduler` `rows_per_rg` division-by-zero in `join.rs` and `parallel.rs`, and validated meshlet payloads.
- **FFI & Axum API**: Wrapped `TejWriteTxHandle` in a mutex, enforced pointer alignment on vector FFI calls, fixed UTF-8 truncation in `tej_db_last_error`, and made `/api/sql` fail closed when `TEJ_API_KEY` is unset with constant-time comparison and body size limits.
- **WASM32 & Feature Gates**: Fixed `cargo check -p tej-db --target wasm32-unknown-unknown --no-default-features` and `cargo check -p tej-db-pager --no-default-features`.

## [0.1.0] - 2026-11-15

### Added
- Initial workspace release across `tej-db-core`, `tej-db-pager`, `tej-db-btree`, `tej-db-txn`, `tej-db-vector`, `tej-db-columnar`, `tej-db-vectorized`, `tej-db-sql`, `tej-db`, and `tej-db-cli`.
- Copy-on-Write B+ tree with S3-FIFO page cache, dual ping-pong superblocks, OCC + group commit, DiskANN (Vamana) + RaBitQ vector search, columnar row-groups, vectorized morsel execution, SQLite-parity SQL engine + CBO, and Ratatui TUI/CLI.

[Unreleased]: https://github.com/YeabsiraBezabih/tej-db/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/YeabsiraBezabih/tej-db/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/YeabsiraBezabih/tej-db/releases/tag/v0.1.0
