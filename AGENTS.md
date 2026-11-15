# Agent Development Rules & Engineering Standards

These rules govern all AI agent and developer contributions to the `tej-db` codebase. They ensure high maintainability, strict ACID safety, zero-cost performance, and complete verification across the workspace.

---

## 1. Modularity & File Size Limits

- **Maximum 500 Lines Per File**:
  - No source file (`.rs`) should exceed **500 lines** unless absolutely necessary (e.g., massive generated lookup tables or unified benchmark scenarios).
  - Any file that approaches 400 lines is a prime candidate for modular decomposition.

- **Dedicated Files for Independent Concepts**:
  - Whenever a new concept is introduced (e.g., a new eviction strategy, indexing structure, conflict validator, custom lock, or query execution node), it **must be given its own dedicated file and module**.
  - When an existing concept or structure expands in scope or responsibility, extract it immediately into a separate file. Do not accumulate disparate responsibilities in a single file.

- **Slim Root Facades (`lib.rs` / `mod.rs`)**:
  - `lib.rs` and `mod.rs` files should primarily serve as public facades: declaring modules, re-exporting key types, and documenting crate-level architecture.
  - Avoid putting extensive implementation details or long `impl` blocks directly inside `lib.rs` or `mod.rs`.

- **Unit Test Placement**:
  - When unit test suites grow beyond 80–100 lines, extract them into a separate `tests.rs` submodule or into dedicated integration test files under `tests/` to keep production code focused and readable.

---

## 2. Mandatory Verification & Quality Gates

On **every code change**, all three gates must be executed and confirmed passing before concluding work:

1. **Formatting**:
   ```bash
   cargo fmt --all -- --check
   ```
   No formatting deviations are allowed. Use `cargo fmt --all` to resolve styling issues.

2. **Clippy (Zero Warnings)**:
   ```bash
   cargo clippy --all-targets --all-features
   ```
   Must pass with **zero warnings**. Workspace clippy lints are configured strictly (`missing_debug_implementations`, `unsafe_op_in_unsafe_fn`, `max-fn-params-bools = 0`). Do not silence lints with `#[allow(...)]` without documented justification.

3. **Test Suite**:
   ```bash
   cargo test -j 2
   ```
   All tests (unit tests, integration tests, proptests, doc tests) must pass.
   *(Note: Use `-j 2` in memory-constrained environments to avoid linker memory spikes when compiling multiple integration test binaries simultaneously).*

---

## 3. Test Discipline & Regression Prevention

- **Test Accompaniment**:
  - Every bug fix, feature addition, or refactoring **must** be accompanied by updated or new tests.
  - Bug fixes must include a regression test reproducing the original issue.
  - New features must include positive cases, boundary/edge conditions, and error paths.
- **Never Weaken Tests**:
  - Never delete, ignore (`#[ignore]`), or weaken existing test assertions to bypass failures. Test failures indicate either a bug in the code change or a documented, intentional change in semantics that requires updating the test's expectations.

---

## 4. Database & ACID Invariants

- **Atomicity**:
  - Incomplete transactions must leave zero observable trace on persisted storage or in the active page cache.
  - Uncommitted root page IDs must never be published to the meta page.
- **Consistency**:
  - Slotted page structure (`item_count`, `free_space_offset`, slot directory, key-value cells) must maintain valid checksums and byte alignment.
  - Keys within B-Tree internal nodes and leaf nodes must remain strictly sorted.
- **Isolation**:
  - Snapshot isolation readers must never observe uncommitted mutations from concurrent write transactions.
  - OCC conflict validation must catch read-write and write-write hazards before committing a transaction.
- **Durability**:
  - Honor the requested `DurabilityMode`:
    - `ImmediateSync`: Call `fsync`/storage sync before returning commit success.
    - `Eventual`: Flush background buffer promptly.
    - `None`: Ephemeral in-memory speed with no persistence guarantees.

---

## 5. Performance & Systems Idioms

- **Zero-Copy Hot Paths**:
  - Avoid copying 4KB page buffers when slice references (`&[u8]`, `&mut [u8]`) suffice.
  - Use in-place slotted page modifications (`leaf_insert_raw_inplace`) to minimize allocations.
- **Allocation Awareness**:
  - Use `SmallVec` for small, short-lived collections (e.g. table lists, conflict cohorts).
  - Pre-allocate collection capacities with `Vec::with_capacity` when sizes are known or bounded.
- **Concurrency & Memory Ordering**:
  - Use fine-grained atomics with explicit ordering (`Ordering::Acquire`, `Ordering::Release`, `Ordering::AcqRel`).
  - Document all lock ordering hierarchies to guarantee absence of deadlocks.
- **Explicit Error Handling**:
  - Production crates (`tej-db-*`) must never call `.unwrap()`, `.expect()`, or `panic!()` in runtime paths.
  - Use `thiserror`-derived `DbError` and propagate failures with `?`.
- **Unsafe Code Guidelines**:
  - Restrict `unsafe` strictly to necessary low-level primitives (e.g. mmap or SIMD operations).
  - Every `unsafe` block **must** be preceded by a `// SAFETY:` comment explaining why all invariants are satisfied.
