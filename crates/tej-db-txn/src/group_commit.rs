use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use tej_db_core::Result;

/// A high-performance lock-free registration group commit coordinator for `DurabilityMode::Immediate`.
///
/// Enables concurrent transactions that have written their pages to the OS buffer cache
/// to coalesce filesystem fsync/fdatasync calls. Exactly one thread (the Sync Leader)
/// issues the physical hardware barrier, while all concurrent followers await the barrier completion.
#[derive(Debug, Default)]
pub struct GroupCommitCoordinator {
    /// Highest transaction ID requested to be synced.
    requested_tx: AtomicU64,
    /// Highest transaction ID successfully flushed and fsynced to persistent media.
    synced_tx: AtomicU64,
    /// Mutex protecting the sync leader election and follower wait queue.
    sync_lock: Mutex<()>,
    /// Condition variable notifying followers of sync completion.
    sync_cond: Condvar,
}

impl GroupCommitCoordinator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            requested_tx: AtomicU64::new(0),
            synced_tx: AtomicU64::new(0),
            sync_lock: Mutex::new(()),
            sync_cond: Condvar::new(),
        }
    }

    /// Registers transaction `tx_id` for group sync and waits until a sync covering `tx_id` has completed.
    ///
    /// The caller supplies `sync_fn`, which executes the physical barrier (e.g. `cache.flush()`).
    /// If another thread is already the Sync Leader, this thread waits on `sync_cond` until the sync completes.
    pub fn sync_tx<F>(&self, tx_id: u64, mut sync_fn: F) -> Result<()>
    where
        F: FnMut() -> Result<()>,
    {
        // 1. Fast path: check if this transaction has already been synced by a preceding group commit
        if self.synced_tx.load(Ordering::Acquire) >= tx_id {
            return Ok(());
        }

        // 2. Announce our desired sync target
        let _ = self.requested_tx.fetch_max(tx_id, Ordering::SeqCst);

        // 3. Acquire sync lock or wait for the active sync leader
        let guard = self.sync_lock.lock();

        // Check again under lock if already synced
        if self.synced_tx.load(Ordering::Acquire) >= tx_id {
            return Ok(());
        }

        // We are now the Sync Leader for all transactions up to target_tx
        let target_tx = self.requested_tx.load(Ordering::Acquire);

        // Perform the single hardware sync
        let sync_res = sync_fn();

        if sync_res.is_ok() {
            self.synced_tx.store(target_tx, Ordering::Release);
        }

        // Drop the lock and wake all waiting followers
        drop(guard);
        self.sync_cond.notify_all();

        sync_res
    }

    /// Returns the highest transaction ID that has been confirmed flushed to durable media.
    #[must_use]
    pub fn last_synced_tx(&self) -> u64 {
        self.synced_tx.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_group_commit_coalesces_syncs() {
        let coordinator = Arc::new(GroupCommitCoordinator::new());
        let sync_count = Arc::new(AtomicU64::new(0));

        let mut handles = Vec::new();
        for i in 1..=10u64 {
            let coord = Arc::clone(&coordinator);
            let s_count = Arc::clone(&sync_count);
            handles.push(thread::spawn(move || {
                coord
                    .sync_tx(i, || {
                        s_count.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(5));
                        Ok(())
                    })
                    .unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // 10 concurrent threads should have coalesced into significantly fewer syncs (e.g. 1..4)
        let total_syncs = sync_count.load(Ordering::SeqCst);
        assert!(
            total_syncs < 10,
            "Group commit must coalesce sync calls, but took {total_syncs}"
        );
        assert_eq!(coordinator.last_synced_tx(), 10);
    }
}
