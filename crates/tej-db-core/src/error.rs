use crate::types::PageId;
use alloc::string::String;
use alloc::vec::Vec;
use thiserror::Error;

/// Database error types across the engine.
#[derive(Debug, Error)]
pub enum DbError {
    #[cfg(feature = "std")]
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Checksum mismatch for page {page_id}: expected {expected:#010x}, found {found:#010x}")]
    ChecksumMismatch {
        page_id: PageId,
        expected: u32,
        found: u32,
    },

    #[error("Page not found: {0}")]
    PageNotFound(PageId),

    #[error("Invalid page header: {0}")]
    InvalidPageHeader(String),

    #[error("Invalid page type: {0}")]
    InvalidPageType(u8),

    #[error("Key not found")]
    KeyNotFound,

    #[error("Transaction aborted")]
    TxAborted,

    #[error("Cannot write in read-only transaction")]
    ReadOnlyTx,

    #[error("Storage capacity exceeded: {0}")]
    CapacityExceeded(String),

    #[error("Database corruption detected: {0}")]
    Corruption(String),

    #[error("Table not found: {0}")]
    TableNotFound(String),

    #[error("Table already exists: {0}")]
    TableAlreadyExists(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Lock contention or concurrency error: {0}")]
    Concurrency(String),

    #[error(
        "Write-write conflict on key {key:?}: modified by concurrent transaction {committed_tx}"
    )]
    WriteConflict { key: Vec<u8>, committed_tx: u64 },

    #[error("Internal engine error: {0}")]
    Internal(String),

    #[error("{0}")]
    Custom(String),
}

pub type Result<T> = core::result::Result<T, DbError>;
