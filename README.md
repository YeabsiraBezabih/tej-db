# Modular ACID Embedded DBMS in Rust

Final-year Software Engineering Senior Research Project at Addis Ababa Science and Technology University (AASTU).

## Project Overview

A modular, crash-safe, embedded key-value database engine written in Rust. It provides full ACID transactions (Atomicity, Consistency, Isolation, Durability), a strictly bounded memory footprint, and pluggable layers (pager/VFS, page cache, durability, indexing, and query API) behind clean, zero-cost Rust traits.

---

## Workspace Architecture

The engine is engineered as a clean, idiomatic Cargo workspace enforcing BeautifulRust lint standards and modular layer isolation:

```
tej-db/
├── Cargo.toml                  # Workspace manifest with strict lints & shared dependencies
├── clippy.toml                 # BeautifulRust clippy enforcement
├── crates/
│   ├── tej-db-core/              # Core traits (Storage, Index, Durability, Hook), types & SIMD CRC32C
│   ├── tej-db-pager/             # File, memory & mmap storage backends + S3-FIFO buffer pool
│   ├── tej-db-btree/             # Copy-On-Write (COW) B+ Tree index, in-place leaf staging & range scans
│   ├── tej-db-txn/               # Transaction manager (dual superblocks, group commit, OCC, savepoints)
│   ├── tej-db-vector/            # DiskANN (Vamana) graph, RaBitQ 1/2-bit quantization, FWHT & FreshDiskANN delta buffer
│   ├── tej-db-columnar/          # Hybrid Columnar Row-Groups, SIMD ColumnVectors, ZoneMaps, FoR/RLE/Dict codecs & Parquet/CSV/Arrow interop
│   ├── tej-db-vectorized/        # Vectorized SIMD query execution, Morsel-driven parallel scheduler & out-of-core spill operators
│   ├── tej-db-sql/               # SQLite parity (Decimal/UUID/JSON, CHECK, FK, Window functions, BM25 FTS) + Cost-Based Optimizer (CBO)
│   ├── tej-db/                 # Top-level client facade (Database, VectorIndex, ReadOnlyTx, WriteTx)
│   └── tej-db-cli/               # Interactive CLI shell & Ratatui terminal metrics dashboard
├── tests/                      # Dedicated integration and verification test suite
│   ├── tests/acid_atomicity.rs    # Atomicity & rollback verification
│   ├── tests/acid_consistency.rs  # Invariant & sorted key-order verification
│   ├── tests/acid_isolation.rs    # Concurrent snapshot isolation verification
│   ├── tests/acid_durability.rs   # Persistent storage recovery verification
│   ├── tests/fault_injection.rs   # Corrupt page & checksum mismatch detection
│   └── tests/proptest_btree.rs    # Randomized property-based testing vs BTreeMap oracle
├── benches/                    # Criterion performance benchmarks
├── docs/                       # Architecture documents, diagrams, and research guidelines
│   └── architecture.md         # Detailed layer design and ACID specification
├── .agents/skills/             # Copied BeautifulRust and idiomatic Rust skill guidelines
└── books/                      # Curated textbook reference library for DBMS development
    ├── rust/                   # 5 Major Rust textbooks
    ├── system_architecture/    # 5 System architecture & OS textbooks
    ├── database_building/      # 5 Database engine internals textbooks
    ├── other_relevant/         # 5 Linux systems, concurrency & distributed textbooks
    └── algorithms/             # 5 Algorithm textbooks
```

---

## Quickstart

### Building the Project

```bash
# Build the entire workspace
cargo build --workspace

# Run static analysis and clippy checks
cargo clippy --all-targets --all-features -- -D warnings
```

### Running Tests

The test suite covers unit tests, full ACID property tests, fault injection tests, and randomized property-based tests:

```bash
# Run all workspace unit and integration tests
cargo test --workspace
```

### Running Benchmarks

TejDb features an enterprise-grade benchmark suite covering HTAP vectorized execution, vector similarity search, head-to-head comparisons against SQLite and Redb, multi-writer concurrency, and disaster recovery:

```bash
# 1. Embedded HTAP & Vectorized Execution Benchmarks (DuckDB-grade SIMD, 1.07B rows/s RLE, 28M rows/s scan, SQLite comparison)
cargo run --release -p tej-db-benches --bin bench_htap

# 2. Native Vector Engine Benchmarks (RaBitQ 1/2-bit quantization, 100% Recall@10, FWHT energy dispersal, hybrid search)
cargo run --release -p tej-db-benches --bin bench_vector

# 3. Comparative Head-to-Head (TejDb vs Redb vs SQLite WAL)
cargo run --release -p tej-db-benches --bin bench_compare
# Custom parameters:
cargo run --release -p tej-db-benches --bin bench_compare -- --keys 50000 --val-size 128 --threads 8 --duration 5

# 3. Multi-Writer Concurrency Matrix (1, 2, 4, 8 threads scaling)
cargo run --release -p tej-db-benches --bin bench_writer_matrix

# 4. Real-World Workload Scenarios (Zipfian Skew, Cache Pressure, Deletion Churn)
cargo run --release -p tej-db-benches --bin bench_realworld

# 5. Production Hardening & Hardware Failure Scenarios (Dual Superblocks, Torn-Write Power-Loss, ENOSPC, Aging)
cargo run --release -p tej-db-benches --bin bench_production

# 6. Extreme Edge Cases (Overflow BLOBs, Zero-Copy Cold-Disk Paging, MVCC GC Lag, Range Scans, Crypto/Compression Matrix)
cargo run --release -p tej-db-benches --bin bench_extreme
```

### Interactive Terminal User Interface (TUI) & CLI

`TejDb` provides both a full graphical Ratatui terminal dashboard and scriptable CLI subcommands:

```bash
# Launch the interactive Ratatui TUI dashboard
cargo run --release -p tej-db-cli -- --db tej.db

# Or use scriptable subcommands directly:
# Insert or update a key
cargo run --release -p tej-db-cli -- --db test.db put "user:1001" '{"name":"Alice","role":"Admin"}'

# Read a key
cargo run --release -p tej-db-cli -- --db test.db get "user:1001"

# Create a named table/keyspace
cargo run --release -p tej-db-cli -- --db test.db create-table "orders"

# List all tables
cargo run --release -p tej-db-cli -- --db test.db list-tables

# Export an online, non-blocking snapshot backup
cargo run --release -p tej-db-cli -- --db test.db backup "backup.db"

# Compact database into contiguous pages
cargo run --release -p tej-db-cli -- --db test.db compact "compacted.db"

# Inspect engine statistics (cache hits, misses, active readers)
cargo run --release -p tej-db-cli -- --db test.db stats
```

---

## Development Skills & Standards

The repository includes the BeautifulRust developer skills in `.agents/skills/`:
- `idiomatic-rust` — Best practices for ownership, borrowing, error handling, and edition 2024.
- `rust-best-practices` — Synthesized guidelines from Google, Microsoft, and the Rust API Guidelines.
- `rust-review` — Checklist-driven code review methodology.
- `rust-port` — Safe migration patterns.

---

## Reference Library (`books/`)

Organized into topical subdirectories:
- `books/rust/`:
  - *The Rust Programming Language (2nd Edition)* — Steve Klabnik & Carol Nichols
  - *Programming Rust* — Jim Blandy & Jason Orendorff
  - *Rust in Action* — Tim McNamara
  - *Creative Projects for Rust Programmers* — Carlo Milanesi
  - *Rust Programming Fundamentals*
- `books/system_architecture/`:
  - *Designing Data-Intensive Applications* — Martin Kleppmann
  - *System Design Interview – An Insider's Guide* — Alex Xu
  - *Computer Systems: A Programmer's Perspective (CS:APP 3rd Edition)* — Randal E. Bryant & David R. O'Hallaron
  - *Architecture of a Database System* — Joseph M. Hellerstein, Michael Stonebraker, James Hamilton
  - *Patterns of Enterprise Application Architecture* — Martin Fowler
- `books/database_building/`:
  - *Database Internals: A Deep Dive into How Distributed Data Systems Work* — Alex Petrov
  - *Database Management Systems (3rd Edition)* — Raghu Ramakrishnan & Johannes Gehrke
  - *Database System Concepts (6th Edition)* — Abraham Silberschatz, Henry Korth, S. Sudarshan
  - *Foundations of Databases (The Alice Book)* — Serge Abiteboul, Richard Hull, Victor Vianu
  - *Readings in Database Systems (5th Edition, The Red Book)* — Peter Bailis, Joseph M. Hellerstein, Michael Stonebraker
- `books/other_relevant/`:
  - *Operating Systems: Three Easy Pieces (OSTEP)* — Remzi H. Arpaci-Dusseau & Andrea C. Arpaci-Dusseau
  - *The Art of Multiprocessor Programming* — Maurice Herlihy & Nir Shavit
  - *The Linux Programming Interface (TLPI)* — Michael Kerrisk
  - *Advanced Programming in the UNIX Environment (APUE)* — W. Richard Stevens & Stephen A. Rago
  - *Understanding the Linux Kernel (3rd Edition)* — Daniel P. Bovet & Marco Cesati
- `books/algorithms/`:
  - *Introduction to Algorithms (CLRS 3rd Edition)* — Cormen, Leiserson, Rivest, Stein
  - *Algorithms (4th Edition)* — Robert Sedgewick & Kevin Wayne
  - *The Algorithm Design Manual* — Steven S. Skiena
  - *Algorithm Design* — Jon Kleinberg & Éva Tardos
  - *Algorithms* — Jeff Erickson

---

## Project Proposal & Research Guidelines

- [`dbms-project-plan.md`](dbms-project-plan.md) — Comprehensive project plan covering milestones, engineering trade-offs, and work breakdown.
- [`AASTU_Final_Year_Project_Proposal-2.md`](AASTU_Final_Year_Project_Proposal-2.md) — AASTU Department of Software Engineering Project Proposal.
- [`Senior_Research_Project_Document_Guideline.md`](Senior_Research_Project_Document_Guideline.md) — Documentation guideline and formatting template.
- [`docs/architecture.md`](docs/architecture.md) — Detailed layer architecture specification.
