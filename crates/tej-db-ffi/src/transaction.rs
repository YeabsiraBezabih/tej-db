use std::ffi::c_char;

use crate::error::{FfiError, TEJ_OK, catch_ffi};
use crate::handle::{
    DbInstance, OwnedWriteTx, ReadTxInstance, TejDbHandle, TejReadTxHandle, TejWriteTxHandle,
    WriteTxInstance, c_str_to_str, check_not_null, check_not_null_mut,
};

/// Begins a snapshot read transaction.
///
/// # Safety
/// `db` and `tx_out` must be valid non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_begin_read(
    db: *mut TejDbHandle,
    tx_out: *mut *mut TejReadTxHandle,
) -> i32 {
    catch_ffi(|| {
        check_not_null(db, "db")?;
        check_not_null_mut(tx_out, "tx_out")?;

        // SAFETY: db is validated non-null.
        let handle = unsafe { &*db };
        let read_instance = match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            DbInstance::File { db, .. } => ReadTxInstance::File(db.begin_read()?),
            DbInstance::Mem { db, .. } => ReadTxInstance::Mem(db.begin_read()?),
        };

        let tx_handle = Box::into_raw(Box::new(TejReadTxHandle(read_instance)));
        // SAFETY: tx_out is validated non-null and writable.
        unsafe {
            *tx_out = tx_handle;
        }
        Ok(TEJ_OK)
    })
}

/// Fetches a value from a snapshot read transaction.
///
/// # Safety
/// All pointers must be valid and non-null as per their invariants.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_read_tx_get(
    tx: *mut TejReadTxHandle,
    table: *const c_char,
    key_ptr: *const u8,
    key_len: usize,
    val_out: *mut *mut u8,
    val_len_out: *mut usize,
) -> i32 {
    catch_ffi(|| {
        check_not_null(tx, "tx")?;
        check_not_null(key_ptr, "key_ptr")?;
        check_not_null_mut(val_out, "val_out")?;
        check_not_null_mut(val_len_out, "val_len_out")?;

        // SAFETY: Validated non-null; pointers valid for reads.
        let table_str = unsafe { c_str_to_str(table, "table")? };
        let key = unsafe { std::slice::from_raw_parts(key_ptr, key_len) };
        let handle = unsafe { &*tx };

        let opt_val = match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            ReadTxInstance::File(rx) => {
                let tbl = rx.open_table(table_str)?;
                tbl.get(key)?
            }
            ReadTxInstance::Mem(rx) => {
                let tbl = rx.open_table(table_str)?;
                tbl.get(key)?
            }
        };

        if let Some(val) = opt_val {
            let len = val.len();
            let mut boxed = val.into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            // SAFETY: val_out and val_len_out validated non-null and writable.
            unsafe {
                *val_out = ptr;
                *val_len_out = len;
            }
            Ok(TEJ_OK)
        } else {
            Err(FfiError::NotFound)
        }
    })
}

/// Closes and releases a snapshot read transaction.
///
/// # Safety
/// `tx` must be a valid handle returned by `tej_db_begin_read`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_read_tx_close(tx: *mut TejReadTxHandle) -> i32 {
    catch_ffi(|| {
        check_not_null_mut(tx, "tx")?;
        // SAFETY: Validated non-null; reclaimed by Box to drop cleanly.
        let _ = unsafe { Box::from_raw(tx) };
        Ok(TEJ_OK)
    })
}

/// Begins an optimistic concurrency control (OCC) write transaction.
///
/// # Safety
/// `db` and `tx_out` must be valid non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_begin_write(
    db: *mut TejDbHandle,
    tx_out: *mut *mut TejWriteTxHandle,
) -> i32 {
    catch_ffi(|| {
        check_not_null(db, "db")?;
        check_not_null_mut(tx_out, "tx_out")?;

        // SAFETY: db validated non-null.
        let handle = unsafe { &*db };
        let write_instance = match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            DbInstance::File { db, .. } => WriteTxInstance::File(OwnedWriteTx::new(db.clone())?),
            DbInstance::Mem { db, .. } => WriteTxInstance::Mem(OwnedWriteTx::new(db.clone())?),
        };

        let tx_handle = Box::into_raw(Box::new(TejWriteTxHandle(write_instance)));
        // SAFETY: tx_out validated non-null and writable.
        unsafe {
            *tx_out = tx_handle;
        }
        Ok(TEJ_OK)
    })
}

/// Inserts or updates a key-value pair within an active write transaction.
///
/// # Safety
/// All pointers must be valid and non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_write_tx_put(
    tx: *mut TejWriteTxHandle,
    table: *const c_char,
    key_ptr: *const u8,
    key_len: usize,
    val_ptr: *const u8,
    val_len: usize,
) -> i32 {
    catch_ffi(|| {
        check_not_null_mut(tx, "tx")?;
        check_not_null(key_ptr, "key_ptr")?;
        if val_len > 0 {
            check_not_null(val_ptr, "val_ptr")?;
        }

        // SAFETY: Validated non-null; pointers valid for memory slice reads.
        let table_str = unsafe { c_str_to_str(table, "table")? };
        let key = unsafe { std::slice::from_raw_parts(key_ptr, key_len) };
        let val = if val_len == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(val_ptr, val_len) }
        };
        let handle = unsafe { &mut *tx };

        match &mut handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            WriteTxInstance::File(owned) => {
                let write_tx = owned.as_mut()?;
                let _ = write_tx.create_table(table_str);
                let mut tbl = write_tx.open_table_mut(table_str)?;
                tbl.put(key, val)?;
            }
            WriteTxInstance::Mem(owned) => {
                let write_tx = owned.as_mut()?;
                let _ = write_tx.create_table(table_str);
                let mut tbl = write_tx.open_table_mut(table_str)?;
                tbl.put(key, val)?;
            }
        }

        Ok(TEJ_OK)
    })
}

/// Deletes a key within an active write transaction.
///
/// # Safety
/// All pointers must be valid and non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_write_tx_delete(
    tx: *mut TejWriteTxHandle,
    table: *const c_char,
    key_ptr: *const u8,
    key_len: usize,
) -> i32 {
    catch_ffi(|| {
        check_not_null_mut(tx, "tx")?;
        check_not_null(key_ptr, "key_ptr")?;

        // SAFETY: Validated non-null; key pointer valid for key_len bytes.
        let table_str = unsafe { c_str_to_str(table, "table")? };
        let key = unsafe { std::slice::from_raw_parts(key_ptr, key_len) };
        let handle = unsafe { &mut *tx };

        match &mut handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            WriteTxInstance::File(owned) => {
                let write_tx = owned.as_mut()?;
                let mut tbl = write_tx.open_table_mut(table_str)?;
                tbl.delete(key)?;
            }
            WriteTxInstance::Mem(owned) => {
                let write_tx = owned.as_mut()?;
                let mut tbl = write_tx.open_table_mut(table_str)?;
                tbl.delete(key)?;
            }
        }

        Ok(TEJ_OK)
    })
}

/// Commits the active write transaction, persisting changes atomically.
///
/// Automatically frees the transaction handle upon completion.
///
/// # Safety
/// `tx` must be a valid handle returned by `tej_db_begin_write`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_write_tx_commit(tx: *mut TejWriteTxHandle) -> i32 {
    catch_ffi(|| {
        check_not_null_mut(tx, "tx")?;
        // SAFETY: Reclaims boxed handle to commit and drop upon completion.
        let boxed = unsafe { Box::from_raw(tx) };
        match boxed.0 {
            #[cfg(not(target_arch = "wasm32"))]
            WriteTxInstance::File(owned) => owned.commit()?,
            WriteTxInstance::Mem(owned) => owned.commit()?,
        }
        Ok(TEJ_OK)
    })
}

/// Aborts the active write transaction, discarding all uncommitted changes.
///
/// Automatically frees the transaction handle.
///
/// # Safety
/// `tx` must be a valid handle returned by `tej_db_begin_write`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_write_tx_abort(tx: *mut TejWriteTxHandle) -> i32 {
    catch_ffi(|| {
        check_not_null_mut(tx, "tx")?;
        // SAFETY: Reclaims boxed handle to abort and drop cleanly.
        let boxed = unsafe { Box::from_raw(tx) };
        match boxed.0 {
            #[cfg(not(target_arch = "wasm32"))]
            WriteTxInstance::File(owned) => owned.abort(),
            WriteTxInstance::Mem(owned) => owned.abort(),
        }
        Ok(TEJ_OK)
    })
}
