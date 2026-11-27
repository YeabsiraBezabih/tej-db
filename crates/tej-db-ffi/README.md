# tej-db-ffi

C ABI and Foreign Function Interface (FFI) bindings for `tej-db` (`cdylib`, `staticlib`, `rlib`).

Provides panic-safe, thread-safe C functions suitable for:
- **Flutter / Dart** (`dart:ffi`) on iOS, Android, macOS, Linux, and Windows
- **C / C++** native applications
- **Python / Zig / Node.js** or any runtime supporting the C ABI

## Key Safety Guarantees (`v0.1.1`)

- **Panic Isolation**: Every entry point wraps execution in `catch_unwind` via `catch_ffi` and records thread-local diagnostics retrievable via `tej_db_last_error`.
- **UTF-8 Safe Error Truncation**: `tej_db_last_error` respects UTF-8 character boundaries when truncating messages to `max_len - 1`.
- **Thread-Safe Write Transactions**: `TejWriteTxHandle` wraps `OwnedWriteTx<S>` in a `parking_lot::Mutex` (with `tx` declared before `db` to enforce safe drop ordering), preventing concurrent `&mut` aliasing UB across multi-threaded callers.
- **Pointer Alignment Checks**: Vector FFI entry points (`tej_db_vector_insert`, `tej_db_vector_search`) validate pointer alignment (`f32`, `u64`, `usize`) before constructing Rust slices.

## Status Codes

| Constant | Code | Meaning |
| --- | --- | --- |
| `TEJ_OK` | `0` | Operation succeeded |
| `TEJ_ERR_NULL_PTR` | `-1` | Required pointer argument was null |
| `TEJ_ERR_INVALID_ARG` | `-2` | Invalid dimension, metric, alignment, or handle state |
| `TEJ_ERR_DATABASE` | `-3` | Engine / storage / SQL execution error |
| `TEJ_ERR_CONFLICT` | `-4` | Optimistic concurrency (`WriteConflict` / `Concurrency`) |
| `TEJ_ERR_NOT_FOUND` | `-5` | Key not found |
| `TEJ_ERR_INVALID_UTF8` | `-6` | Invalid UTF-8 C string |
| `TEJ_ERR_PANIC` | `-99` | Internal panic caught at FFI boundary |

## Exported C ABI Surface

- **Database Lifecycle & KV**: `tej_db_open`, `tej_db_open_memory`, `tej_db_close`, `tej_db_put`, `tej_db_get`, `tej_db_delete`, `tej_db_free_buffer`
- **Transactions**: `tej_db_begin_read`, `tej_db_read_tx_get`, `tej_db_read_tx_close`, `tej_db_begin_write`, `tej_db_write_tx_put`, `tej_db_write_tx_delete`, `tej_db_write_tx_commit`, `tej_db_write_tx_abort`
- **SQL**: `tej_db_sql_execute`, `tej_db_free_string`
- **Vector Index**: `tej_db_vector_insert`, `tej_db_vector_compact`, `tej_db_vector_search`
- **Diagnostics**: `tej_db_last_error`, `tej_db_clear_last_error`

## Building & Testing

```bash
cargo build --release -p tej-db-ffi
cargo test -p tej-db-ffi
```

## License

Dual-licensed under `MIT OR Apache-2.0` (see [`LICENSE-MIT`](../../LICENSE-MIT) and [`LICENSE-APACHE`](../../LICENSE-APACHE)).
