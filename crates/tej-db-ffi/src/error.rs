use std::cell::RefCell;
use std::ffi::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};
use tej_db_core::DbError;

/// Successful operation.
pub const TEJ_OK: i32 = 0;
/// Null pointer argument encountered.
pub const TEJ_ERR_NULL_PTR: i32 = -1;
/// Invalid UTF-8 string encountered.
pub const TEJ_ERR_INVALID_UTF8: i32 = -2;
/// Underlying database or storage engine error.
pub const TEJ_ERR_DATABASE: i32 = -3;
/// Optimistic concurrency control (OCC) or write-conflict error.
pub const TEJ_ERR_CONFLICT: i32 = -4;
/// Key or requested resource not found.
pub const TEJ_ERR_NOT_FOUND: i32 = -5;
/// Unhandled panic caught within FFI boundary.
pub const TEJ_ERR_PANIC: i32 = -6;
/// Invalid argument or parameter passed to function.
pub const TEJ_ERR_INVALID_ARG: i32 = -7;

thread_local! {
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Sets the thread-local last error message.
pub fn set_last_error(msg: impl Into<String>) {
    LAST_ERROR.with(|err| {
        *err.borrow_mut() = Some(msg.into());
    });
}

/// Clears the thread-local last error message.
pub fn clear_last_error() {
    LAST_ERROR.with(|err| {
        *err.borrow_mut() = None;
    });
}

/// Internal FFI error type.
#[derive(Debug)]
pub enum FfiError {
    NullPtr(&'static str),
    InvalidUtf8(&'static str),
    Database(DbError),
    Conflict(String),
    NotFound,
    InvalidArg(String),
}

impl FfiError {
    #[must_use]
    pub const fn code(&self) -> i32 {
        match self {
            Self::NullPtr(_) => TEJ_ERR_NULL_PTR,
            Self::InvalidUtf8(_) => TEJ_ERR_INVALID_UTF8,
            Self::Database(_) => TEJ_ERR_DATABASE,
            Self::Conflict(_) => TEJ_ERR_CONFLICT,
            Self::NotFound => TEJ_ERR_NOT_FOUND,
            Self::InvalidArg(_) => TEJ_ERR_INVALID_ARG,
        }
    }
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NullPtr(arg) => write!(f, "Null pointer argument: {arg}"),
            Self::InvalidUtf8(arg) => write!(f, "Invalid UTF-8 sequence in: {arg}"),
            Self::Database(err) => write!(f, "Database error: {err}"),
            Self::Conflict(msg) => write!(f, "Transaction conflict: {msg}"),
            Self::NotFound => write!(f, "Item not found"),
            Self::InvalidArg(msg) => write!(f, "Invalid argument: {msg}"),
        }
    }
}

impl From<DbError> for FfiError {
    fn from(err: DbError) -> Self {
        Self::Database(err)
    }
}

/// Executes a closure within a `catch_unwind` barrier to prevent host application crashes.
pub fn catch_ffi<F>(f: F) -> i32
where
    F: FnOnce() -> Result<i32, FfiError>,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => {
            set_last_error(err.to_string());
            err.code()
        }
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "Unknown panic inside tej-db-ffi".to_string()
            };
            set_last_error(format!("PANIC: {msg}"));
            TEJ_ERR_PANIC
        }
    }
}

/// Copies the last error message into a caller-supplied C string buffer.
///
/// # Safety
/// `buf` must point to valid writable memory of at least `max_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_last_error(buf: *mut c_char, max_len: usize) -> i32 {
    if buf.is_null() || max_len == 0 {
        return TEJ_ERR_NULL_PTR;
    }

    LAST_ERROR.with(|err| {
        let msg = err.borrow();
        let bytes = msg.as_deref().unwrap_or("").as_bytes();
        let copy_len = bytes.len().min(max_len.saturating_sub(1));

        // SAFETY: Caller guarantees buf is valid for writes up to max_len.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), buf, copy_len);
            *buf.add(copy_len) = 0;
        }
        i32::try_from(copy_len).unwrap_or(0)
    })
}

/// Clears the last error recorded for the current thread.
#[unsafe(no_mangle)]
pub extern "C" fn tej_db_clear_last_error() -> i32 {
    clear_last_error();
    TEJ_OK
}
