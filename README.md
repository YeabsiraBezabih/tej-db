# TejDb — Modular ACID Embedded DBMS in Rust

[![CI](https://github.com/YeabsiraBezabih/tej-db/actions/workflows/ci.yml/badge.svg)](https://github.com/YeabsiraBezabih/tej-db/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Rust: 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](rust-toolchain.toml)

A modular, crash-safe, multi-model embedded database engine written in Rust (`edition = "2024"`). It combines full ACID transactions (Atomicity, Consistency, Isolation, Durability), Copy-on-Write B+ trees, DiskANN/RaBitQ vector search, columnar row-groups, vectorized SIMD execution, a SQLite-parity SQL engine, 3D spatial meshlet indexing, C ABI FFI bindings, and WASM32 support behind clean Rust traits.

---

## Workspace Architecture

The engine is organized as a modular Cargo workspace enforcing strict lints and per-file modularity (`<= 500` lines per crate source file):

```text
tej-db/
├── Cargo.toml                  # Workspace manifest (v0.1.1, MIT OR Apache-2.0, strict lints)
├── clippy.toml                 # Workspace clippy rules
├── rust-toolchain.toml         # Pinned stable toolchain + rustfmt/clippy/wasm32
├── crates/
│   ├── tej-db-core/            # Core traits (Storage, Index, Durability, Hook), slotted pages & SIMD CRC32C
│   ├── tej-db-pager/           # File, memory & mmap backends, ChaCha20-Poly1305, LZ4, S3-FIFO/Clock PageCache
│   ├── tej-db-btree/           # Copy-On-Write (COW) B+ Tree index, in-place leaf staging & overflow extents
│   ├── tej-db-txn/             # Transaction manager (dual superblocks, group commit, OCC, savepoints)
│   ├── tej-db-vector/          # DiskANN (Vamana) graph, RaBitQ 1/2-bit quantization, FWHT & delta buffer
│   ├── tej-db-columnar/        # Hybrid Columnar Row-Groups, ZoneMaps, FoR/RLE/Dict codecs & Parquet/CSV/Arrow
│   ├── tej-db-vectorized/      # Vectorized SIMD query execution, Morsel-driven scheduler & out-of-core spill
│   ├── tej-db-sql/             # SQL engine (Decimal/UUID/JSON, CHECK/FK/NOT NULL, ORDER BY/DISTINCT, Window, BM25 FTS, CBO)
│   ├── tej-db/                 # Top-level client facade (Database, VectorIndex, SpatialViewportManager, verify, backup)
│   ├── tej-db-cli/             # Interactive CLI shell & Ratatui terminal metrics dashboard
│   └── tej-db-ffi/             # Panic-safe C ABI / FFI bindings (Flutter/Dart, iOS, Android, Desktop)
├── examples/
│   └── axum-api/               # REST & authenticated SQL HTTP API example using Axum
├── tests/                      # ACID, fault-injection, proptest, spatial & v5 audit regression suites
├── benches/                    # Criterion & comparative benchmark binaries (TejDb vs Redb vs SQLite WAL)
├── docs/                       # Architecture documents and design specifications
└── scripts/                    # AGENTS.md gatekeeper, test coverage check & crates.io publish scripts
```

---

## Quickstart

### Building & Checking

```bash
# Build the entire workspace
cargo build --workspace

# Run formatting and strict clippy checks
cargo fmt --all -- --check
cargo clippy --workspace --exclude tej-db-benches --all-targets --all-features -- -D warnings

# Verify minimal pager build and WASM32 target
cargo check -p tej-db-pager --no-default-features
cargo check -p tej-db --target wasm32-unknown-unknown --no-default-features
```

### Running Tests & Quality Gates

The test suite covers unit tests, full ACID property tests, fault injection, randomized property-based tests, vector/spatial persistence, and v5 audit regression tests:

```bash
# Run all workspace unit and integration tests
cargo test --workspace --exclude tej-db-benches -j 2

# Run the full AGENTS.md rule & quality gatekeeper
./scripts/check_agents_rules.sh
```

### Running Benchmarks

```bash
# 1. Embedded HTAP & Vectorized Execution Benchmarks
cargo run --release -p tej-db-benches --bin bench_htap

# 2. Native Vector Engine Benchmarks (RaBitQ 1/2-bit quantization, FWHT, hybrid search)
cargo run --release -p tej-db-benches --bin bench_vector

# 3. Comparative Head-to-Head (TejDb vs Redb vs SQLite WAL)
cargo run --release -p tej-db-benches --bin bench_compare
cargo run --release -p tej-db-benches --bin bench_compare -- --keys 50000 --val-size 128 --threads 8 --duration 5

# 4. Multi-Writer Concurrency Matrix (1, 2, 4, 8 threads)
cargo run --release -p tej-db-benches --bin bench_writer_matrix

# 5. Real-World Workloads (Zipfian Skew, Cache Pressure, Deletion Churn)
cargo run --release -p tej-db-benches --bin bench_realworld

# 6. Production Hardening & Power-Loss Scenarios
cargo run --release -p tej-db-benches --bin bench_production

# 7. Extreme Edge Cases (Overflow BLOBs, Cold-Disk Paging, MVCC GC, Crypto/Compression Matrix)
cargo run --release -p tej-db-benches --bin bench_extreme

# 8. Comprehensive Cross-Subsystem Suite
cargo run --release -p tej-db-benches --bin bench_comprehensive
```

### Interactive Terminal User Interface (TUI) & CLI

```bash
# Launch the interactive Ratatui TUI dashboard
cargo run --release -p tej-db-cli -- --db tej.db

# Insert or update a key
cargo run --release -p tej-db-cli -- --db test.db put "user:1001" '{"name":"Alice","role":"Admin"}'

# Read a key
cargo run --release -p tej-db-cli -- --db test.db get "user:1001"

# Create and list named tables
cargo run --release -p tej-db-cli -- --db test.db create-table "orders"
cargo run --release -p tej-db-cli -- --db test.db list-tables

# Export an atomic online backup or compact
cargo run --release -p tej-db-cli -- --db test.db backup "backup.db"
cargo run --release -p tej-db-cli -- --db test.db compact "compacted.db"

# Inspect engine statistics
cargo run --release -p tej-db-cli -- --db test.db stats
```

---

## Documentation & Community

- [`docs/architecture.md`](docs/architecture.md) — Detailed layer architecture and ACID specification
- [`CHANGELOG.md`](CHANGELOG.md) — Release notes (`v0.1.0`, `v0.1.1`)
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — Development setup, `AGENTS.md` standards, and PR guidelines
- [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) — Community standards (Contributor Covenant v2.1)
- [`SECURITY.md`](SECURITY.md) — Vulnerability disclosure policy

---

## License

Dual-licensed under either of:

- **MIT License** ([`LICENSE-MIT`](LICENSE-MIT))
- **Apache License, Version 2.0** ([`LICENSE-APACHE`](LICENSE-APACHE))

at your option.
