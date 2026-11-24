//! Foreign Function Interface (FFI) bindings for `tej-db`.
//!
//! This crate exports a C-compatible ABI designed for:
//! - Mobile applications (Flutter on iOS and Android via `dart:ffi`).
//! - Desktop platforms (macOS, Linux, Windows).
//! - Any language runtime supporting C ABI bindings (Python, Node.js, C++).
//!
//! Every exported entry point catches panics via `catch_unwind`, recording thread-local
//! error diagnostics and returning non-zero error codes to prevent crashes in host processes.

pub mod database;
pub mod error;
pub mod handle;
pub mod sql;
pub mod transaction;
pub mod vector;

#[cfg(test)]
mod tests;

pub use database::{
    tej_db_close, tej_db_delete, tej_db_free_buffer, tej_db_get, tej_db_open, tej_db_open_memory,
    tej_db_put,
};
pub use error::{
    TEJ_ERR_CONFLICT, TEJ_ERR_DATABASE, TEJ_ERR_INVALID_ARG, TEJ_ERR_INVALID_UTF8,
    TEJ_ERR_NOT_FOUND, TEJ_ERR_NULL_PTR, TEJ_ERR_PANIC, TEJ_OK, catch_ffi, tej_db_clear_last_error,
    tej_db_last_error,
};
pub use handle::{TejDbHandle, TejReadTxHandle, TejWriteTxHandle};
pub use sql::{tej_db_free_string, tej_db_sql_execute};
pub use transaction::{
    tej_db_begin_read, tej_db_begin_write, tej_db_read_tx_close, tej_db_read_tx_get,
    tej_db_write_tx_abort, tej_db_write_tx_commit, tej_db_write_tx_delete, tej_db_write_tx_put,
};
pub use vector::{tej_db_vector_compact, tej_db_vector_insert, tej_db_vector_search};
