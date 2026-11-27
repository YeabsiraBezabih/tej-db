use parking_lot::{Mutex, MutexGuard};
use std::fmt;

/// Fair mutex-backed lock (preserving the `AdaptiveSpinLock` public API).
#[derive(Default)]
pub struct AdaptiveSpinLock {
    inner: Mutex<()>,
}

impl fmt::Debug for AdaptiveSpinLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdaptiveSpinLock")
            .field("locked", &self.inner.is_locked())
            .finish()
    }
}

impl AdaptiveSpinLock {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(()),
        }
    }

    #[inline]
    #[must_use]
    pub fn try_lock(&self) -> Option<AdaptiveSpinGuard<'_>> {
        self.inner
            .try_lock()
            .map(|guard| AdaptiveSpinGuard { _guard: guard })
    }

    #[inline]
    pub fn lock(&self) -> AdaptiveSpinGuard<'_> {
        AdaptiveSpinGuard {
            _guard: self.inner.lock(),
        }
    }
}

/// Scoped RAII guard for `AdaptiveSpinLock`.
#[derive(Debug)]
pub struct AdaptiveSpinGuard<'a> {
    _guard: MutexGuard<'a, ()>,
}
