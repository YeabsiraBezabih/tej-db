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

        #[cfg(not(unix))]
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

        #[cfg(not(unix))]
        {
            let _ = file;
            Ok(())
        }
    }
}
