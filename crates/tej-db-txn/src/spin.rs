use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// An adaptive spinlock with bounded spin loops, thread yielding, and brief sleep backoff.
#[derive(Default)]
pub struct AdaptiveSpinLock {
    locked: AtomicBool,
}

impl fmt::Debug for AdaptiveSpinLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdaptiveSpinLock")
            .field("locked", &self.locked.load(Ordering::Relaxed))
            .finish()
    }
}

impl AdaptiveSpinLock {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    #[inline]
    #[must_use]
    pub fn try_lock(&self) -> Option<AdaptiveSpinGuard<'_>> {
        if self.locked.swap(true, Ordering::Acquire) {
            None
        } else {
            Some(AdaptiveSpinGuard { lock: &self.locked })
        }
    }

    #[inline]
    pub fn lock(&self) -> AdaptiveSpinGuard<'_> {
        let mut spins = 0u32;
        while self.locked.swap(true, Ordering::Acquire) {
            spins = spins.wrapping_add(1);
            if spins < 64 {
                std::hint::spin_loop();
            } else if spins < 256 {
                std::thread::yield_now();
            } else {
                std::thread::sleep(Duration::from_micros(20));
            }
        }
        AdaptiveSpinGuard { lock: &self.locked }
    }
}

/// Scoped RAII guard for `AdaptiveSpinLock`.
#[derive(Debug)]
pub struct AdaptiveSpinGuard<'a> {
    lock: &'a AtomicBool,
}

impl Drop for AdaptiveSpinGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.lock.store(false, Ordering::Release);
    }
}
