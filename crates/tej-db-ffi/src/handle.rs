use std::ffi::{CStr, c_char};
#[cfg(not(target_arch = "wasm32"))]
use tej_db::FileStorage;
use tej_db::{Database, MemStorage, Storage};
use tej_db_core::Result as DbResult;
use tej_db_sql::SqlEngine;
use tej_db_txn::{ReadOnlyTx, WriteTx};

use crate::error::FfiError;

/// Storage backend instance for a database connection.
pub enum DbInstance {
    #[cfg(not(target_arch = "wasm32"))]
    File {
        db: Database<FileStorage>,
        sql: SqlEngine<FileStorage>,
    },
    Mem {
        db: Database<MemStorage>,
        sql: SqlEngine<MemStorage>,
    },
}

impl std::fmt::Debug for DbInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File { .. } => f.debug_struct("DbInstance::File").finish_non_exhaustive(),
            Self::Mem { .. } => f.debug_struct("DbInstance::Mem").finish_non_exhaustive(),
        }
    }
}

/// Opaque database handle exposed to C ABI.
#[derive(Debug)]
pub struct TejDbHandle(pub(crate) DbInstance);

/// Active snapshot read transaction instance.
pub enum ReadTxInstance {
    #[cfg(not(target_arch = "wasm32"))]
    File(ReadOnlyTx<FileStorage>),
    Mem(ReadOnlyTx<MemStorage>),
}

impl std::fmt::Debug for ReadTxInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(_) => f
                .debug_struct("ReadTxInstance::File")
                .finish_non_exhaustive(),
            Self::Mem(_) => f
                .debug_struct("ReadTxInstance::Mem")
                .finish_non_exhaustive(),
        }
    }
}

/// Opaque read transaction handle exposed to C ABI.
#[derive(Debug)]
pub struct TejReadTxHandle(pub(crate) ReadTxInstance);

/// Self-contained owned write transaction wrapper holding its parent database instance.
pub struct OwnedWriteTx<S: Storage> {
    db: Database<S>,
    tx: Option<WriteTx<'static, S>>,
}

impl<S: Storage> std::fmt::Debug for OwnedWriteTx<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedWriteTx")
            .field("has_tx", &self.tx.is_some())
            .finish_non_exhaustive()
    }
}

impl<S: Storage> OwnedWriteTx<S> {
    /// Creates a new owned write transaction bound to `db`.
    pub fn new(db: Database<S>) -> DbResult<Self> {
        let mut owned = Self { db, tx: None };
        let tx = owned.db.begin_write()?;
        // SAFETY: The parent `Database<S>` is stored within `db` and will remain alive
        // for the entire lifetime of `tx`. The handle is pinned on the heap when exposed to C.
        let tx_static = unsafe { std::mem::transmute::<WriteTx<'_, S>, WriteTx<'static, S>>(tx) };
        owned.tx = Some(tx_static);
        Ok(owned)
    }

    /// Borrows the active write transaction mutably.
    pub fn as_mut(&mut self) -> Result<&mut WriteTx<'static, S>, FfiError> {
        self.tx
            .as_mut()
            .ok_or_else(|| FfiError::InvalidArg("Write transaction already finalized".into()))
    }

    /// Commits the active write transaction.
    pub fn commit(mut self) -> DbResult<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit()?;
        }
        Ok(())
    }

    /// Aborts the active write transaction without committing mutations.
    pub fn abort(mut self) {
        if let Some(tx) = self.tx.take() {
            drop(tx);
        }
    }
}

/// Active write transaction instance.
pub enum WriteTxInstance {
    #[cfg(not(target_arch = "wasm32"))]
    File(OwnedWriteTx<FileStorage>),
    Mem(OwnedWriteTx<MemStorage>),
}

impl std::fmt::Debug for WriteTxInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(_) => f
                .debug_struct("WriteTxInstance::File")
                .finish_non_exhaustive(),
            Self::Mem(_) => f
                .debug_struct("WriteTxInstance::Mem")
                .finish_non_exhaustive(),
        }
    }
}

/// Opaque write transaction handle exposed to C ABI.
#[derive(Debug)]
pub struct TejWriteTxHandle(pub(crate) WriteTxInstance);

/// Validates that a raw pointer argument is non-null.
pub fn check_not_null<T>(ptr: *const T, name: &'static str) -> Result<(), FfiError> {
    if ptr.is_null() {
        Err(FfiError::NullPtr(name))
    } else {
        Ok(())
    }
}

/// Validates that a raw mutable pointer argument is non-null.
pub fn check_not_null_mut<T>(ptr: *mut T, name: &'static str) -> Result<(), FfiError> {
    if ptr.is_null() {
        Err(FfiError::NullPtr(name))
    } else {
        Ok(())
    }
}

/// Converts a null-terminated C string into a Rust string slice.
///
/// # Safety
/// `c_ptr` must point to a valid null-terminated C string.
pub unsafe fn c_str_to_str<'a>(
    c_ptr: *const c_char,
    name: &'static str,
) -> Result<&'a str, FfiError> {
    check_not_null(c_ptr, name)?;
    // SAFETY: Validated non-null; caller guarantees pointer points to a valid null-terminated C string.
    let raw_cstr = unsafe { CStr::from_ptr(c_ptr) };
    raw_cstr.to_str().map_err(|_| FfiError::InvalidUtf8(name))
}
