use std::fs::File;
use tej_db_core::{DbError, Result};

/// OS file advisory locking helper.
///
/// Ensures exclusive single-writer access to persistent database files across processes.
#[derive(Debug)]
pub struct FileLock;

impl FileLock {
    /// Attempts to acquire an exclusive non-blocking advisory lock on `file`.
    ///
    /// Returns `Err(DbError::Locked)` if another process or handle holds the lock.
    pub fn try_lock_exclusive(file: &File) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            // SAFETY: `fd` is a valid file descriptor from an open `File` handle.
            let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    return Err(DbError::Locked(
                        "Database file is currently held by another process or handle".into(),
                    ));
                }
                return Err(DbError::Io(err));
            }
            Ok(())
        }

        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            #[repr(C)]
            struct Overlapped {
                internal: usize,
                internal_high: usize,
                offset: u32,
                offset_high: u32,
                h_event: *mut core::ffi::c_void,
            }
            unsafe extern "system" {
                fn LockFileEx(
                    h_file: *mut core::ffi::c_void,
                    dw_flags: u32,
                    dw_reserved: u32,
                    n_bytes_low: u32,
                    n_bytes_high: u32,
                    lp_overlapped: *mut Overlapped,
                ) -> i32;
            }
            const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
            const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
            let mut overlapped = Overlapped {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                h_event: std::ptr::null_mut(),
            };
            // SAFETY: Valid Windows file handle and zeroed Overlapped struct.
            let ok = unsafe {
                LockFileEx(
                    file.as_raw_handle(),
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    u32::MAX,
                    u32::MAX,
                    &mut overlapped,
                )
            };
            if ok == 0 {
                return Err(DbError::Locked(
                    "Database file is currently held by another process or handle".into(),
                ));
            }
            Ok(())
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = file;
            Ok(())
        }
    }

    /// Releases an advisory lock on `file`.
    pub fn unlock(file: &File) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            // SAFETY: `fd` is a valid file descriptor from an open `File` handle.
            let ret = unsafe { libc::flock(fd, libc::LOCK_UN) };
            if ret != 0 {
                return Err(DbError::Io(std::io::Error::last_os_error()));
            }
            Ok(())
        }

        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            #[repr(C)]
            struct Overlapped {
                internal: usize,
                internal_high: usize,
                offset: u32,
                offset_high: u32,
                h_event: *mut core::ffi::c_void,
            }
            unsafe extern "system" {
                fn UnlockFileEx(
                    h_file: *mut core::ffi::c_void,
                    dw_reserved: u32,
                    n_bytes_low: u32,
                    n_bytes_high: u32,
                    lp_overlapped: *mut Overlapped,
                ) -> i32;
            }
            let mut overlapped = Overlapped {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                h_event: std::ptr::null_mut(),
            };
            // SAFETY: Valid Windows file handle and zeroed Overlapped struct.
            let ok = unsafe {
                UnlockFileEx(file.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut overlapped)
            };
            if ok == 0 {
                return Err(DbError::Io(std::io::Error::last_os_error()));
            }
            Ok(())
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = file;
            Ok(())
        }
    }
}
