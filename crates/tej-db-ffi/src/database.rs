use std::ffi::c_char;
use tej_db::Database;
use tej_db_sql::SqlEngine;

use crate::error::{FfiError, TEJ_OK, catch_ffi};
use crate::handle::{DbInstance, TejDbHandle, c_str_to_str, check_not_null, check_not_null_mut};

/// Opens or creates a file-backed database at `path`.
///
/// # Safety
/// `path` must point to a valid null-terminated UTF-8 C string.
/// `db_out` must point to valid writable pointer memory.
#[cfg(not(target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_open(path: *const c_char, db_out: *mut *mut TejDbHandle) -> i32 {
    catch_ffi(|| {
        check_not_null_mut(db_out, "db_out")?;
        // SAFETY: Pointer validated non-null; caller guarantees path is null-terminated.
        let path_str = unsafe { c_str_to_str(path, "path")? };
        let db = Database::open_file(path_str)?;
        let sql = SqlEngine::new(&db);
        let handle = Box::into_raw(Box::new(TejDbHandle(DbInstance::File { db, sql })));
        // SAFETY: db_out validated non-null and writable.
        unsafe {
            *db_out = handle;
        }
        Ok(TEJ_OK)
    })
}

/// Fallback for `tej_db_open` on Web/WASM where local filesystem storage is unavailable.
///
/// # Safety
/// Pointers are unused.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_open(_path: *const c_char, _db_out: *mut *mut TejDbHandle) -> i32 {
    crate::error::set_last_error(
        "File-backed storage is not supported on wasm32; use tej_db_open_memory",
    );
    crate::error::TEJ_ERR_DATABASE
}

/// Opens an ephemeral in-memory database instance.
///
/// # Safety
/// `db_out` must point to valid writable pointer memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_open_memory(db_out: *mut *mut TejDbHandle) -> i32 {
    catch_ffi(|| {
        check_not_null_mut(db_out, "db_out")?;
        let db = Database::open_memory()?;
        let sql = SqlEngine::new(&db);
        let handle = Box::into_raw(Box::new(TejDbHandle(DbInstance::Mem { db, sql })));
        // SAFETY: db_out validated non-null and writable.
        unsafe {
            *db_out = handle;
        }
        Ok(TEJ_OK)
    })
}

/// Closes a database handle and releases associated resources.
///
/// # Safety
/// `db` must be a valid handle returned by `tej_db_open` or `tej_db_open_memory`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_close(db: *mut TejDbHandle) -> i32 {
    catch_ffi(|| {
        check_not_null_mut(db, "db")?;
        // SAFETY: Validated non-null; reclaimed by Box to drop cleanly.
        let _ = unsafe { Box::from_raw(db) };
        Ok(TEJ_OK)
    })
}

/// Fetches a value by key directly using an auto-commit snapshot read.
///
/// # Safety
/// All pointers must be valid and non-null as per their respective invariants.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_get(
    db: *mut TejDbHandle,
    table: *const c_char,
    key_ptr: *const u8,
    key_len: usize,
    val_out: *mut *mut u8,
    val_len_out: *mut usize,
) -> i32 {
    catch_ffi(|| {
        check_not_null(db, "db")?;
        check_not_null(key_ptr, "key_ptr")?;
        check_not_null_mut(val_out, "val_out")?;
        check_not_null_mut(val_len_out, "val_len_out")?;

        // SAFETY: table is validated non-null; key_ptr is validated for slice access of key_len bytes.
        let table_str = unsafe { c_str_to_str(table, "table")? };
        let key = unsafe { std::slice::from_raw_parts(key_ptr, key_len) };
        let handle = unsafe { &*db };

        let opt_val: Option<Vec<u8>> = match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            DbInstance::File { db, .. } => {
                let rx = db.begin_read()?;
                let tbl = rx.open_table(table_str)?;
                tbl.get(key)?
            }
            DbInstance::Mem { db, .. } => {
                let rx = db.begin_read()?;
                let tbl = rx.open_table(table_str)?;
                tbl.get(key)?
            }
        };

        if let Some(val) = opt_val {
            let len = val.len();
            let mut boxed = val.into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            // SAFETY: Output pointers are validated non-null and writable.
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

/// Puts a key-value pair directly using an auto-commit write transaction.
///
/// # Safety
/// All pointers must be valid and non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_put(
    db: *mut TejDbHandle,
    table: *const c_char,
    key_ptr: *const u8,
    key_len: usize,
    val_ptr: *const u8,
    val_len: usize,
) -> i32 {
    catch_ffi(|| {
        check_not_null(db, "db")?;
        check_not_null(key_ptr, "key_ptr")?;
        if val_len > 0 {
            check_not_null(val_ptr, "val_ptr")?;
        }

        // SAFETY: Validated non-null; pointers valid for slice reads.
        let table_str = unsafe { c_str_to_str(table, "table")? };
        let key = unsafe { std::slice::from_raw_parts(key_ptr, key_len) };
        let val = if val_len == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(val_ptr, val_len) }
        };
        let handle = unsafe { &*db };

        match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            DbInstance::File { db, .. } => {
                let mut tx = db.begin_write()?;
                let _ = tx.create_table(table_str);
                {
                    let mut tbl = tx.open_table_mut(table_str)?;
                    tbl.put(key, val)?;
                }
                tx.commit()?;
            }
            DbInstance::Mem { db, .. } => {
                let mut tx = db.begin_write()?;
                let _ = tx.create_table(table_str);
                {
                    let mut tbl = tx.open_table_mut(table_str)?;
                    tbl.put(key, val)?;
                }
                tx.commit()?;
            }
        }

        Ok(TEJ_OK)
    })
}

/// Deletes a key directly using an auto-commit write transaction.
///
/// # Safety
/// All pointers must be valid and non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_delete(
    db: *mut TejDbHandle,
    table: *const c_char,
    key_ptr: *const u8,
    key_len: usize,
) -> i32 {
    catch_ffi(|| {
        check_not_null(db, "db")?;
        check_not_null(key_ptr, "key_ptr")?;

        // SAFETY: Validated non-null; pointer valid for key_len bytes.
        let table_str = unsafe { c_str_to_str(table, "table")? };
        let key = unsafe { std::slice::from_raw_parts(key_ptr, key_len) };
        let handle = unsafe { &*db };

        match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            DbInstance::File { db, .. } => {
                let mut tx = db.begin_write()?;
                {
                    let mut tbl = tx.open_table_mut(table_str)?;
                    tbl.delete(key)?;
                }
                tx.commit()?;
            }
            DbInstance::Mem { db, .. } => {
                let mut tx = db.begin_write()?;
                {
                    let mut tbl = tx.open_table_mut(table_str)?;
                    tbl.delete(key)?;
                }
                tx.commit()?;
            }
        }

        Ok(TEJ_OK)
    })
}

/// Frees a byte buffer allocated by `tej-db-ffi`.
///
/// # Safety
/// `buf_ptr` must have been allocated by `tej_db_get` with length `buf_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_free_buffer(buf_ptr: *mut u8, buf_len: usize) -> i32 {
    if !buf_ptr.is_null() && buf_len > 0 {
        // SAFETY: Caller guarantees buf_ptr and buf_len originated from a tej-db-ffi buffer.
        unsafe {
            let slice = std::ptr::slice_from_raw_parts_mut(buf_ptr, buf_len);
            drop(Box::from_raw(slice));
        }
    }
    TEJ_OK
}
