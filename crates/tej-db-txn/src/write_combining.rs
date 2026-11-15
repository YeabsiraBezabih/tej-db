use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tej_db_btree::{BTreeIndex, RawBatchItem, TxPageSet};
use tej_db_core::{DbError, Result, Storage};

/// A pending write batch submitted by a follower thread for write combining.
#[derive(Debug)]
pub struct QueuedBatch {
    pub writes: Vec<RawBatchItem>,
    pub tx_pages: TxPageSet,
    pub completed: Arc<AtomicBool>,
    pub result: Arc<Mutex<Option<Result<()>>>>,
    pub cond: Arc<Condvar>,
}

/// Per-table write-combining queue that allows multiple concurrent writer threads
/// to coalesce their disjoint sorted batch mutations into a single in-place B+ tree pass.
#[derive(Default)]
pub struct TableWriteQueue {
    queue: Mutex<VecDeque<QueuedBatch>>,
}

impl core::fmt::Debug for TableWriteQueue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TableWriteQueue")
            .field("pending_batches", &self.queue.lock().len())
            .finish()
    }
}

impl TableWriteQueue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(32)),
        }
    }

    /// Submits a sorted batch to the queue and waits until the leader thread has drained
    /// and committed it into the table B+ tree.
    pub fn submit_and_wait(&self, writes: Vec<RawBatchItem>, tx_pages: TxPageSet) -> Result<()> {
        let completed = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let cond = Arc::new(Condvar::new());

        let batch = QueuedBatch {
            writes,
            tx_pages,
            completed: Arc::clone(&completed),
            result: Arc::clone(&result),
            cond: Arc::clone(&cond),
        };

        {
            let mut q = self.queue.lock();
            q.push_back(batch);
        }

        // Wait until leader thread signals completion
        let mut r_guard = result.lock();
        while !completed.load(Ordering::Acquire) {
            cond.wait(&mut r_guard);
        }

        r_guard.take().unwrap_or(Ok(()))
    }

    /// Drains all currently queued batches and applies them in a single batch pass.
    ///
    /// Must be invoked by the thread holding the table commit lock.
    pub fn drain_and_apply<S: Storage>(
        &self,
        btree: &mut BTreeIndex<S>,
        leader_tx_pages: &mut TxPageSet,
    ) -> Result<usize> {
        let batches: Vec<QueuedBatch> = {
            let mut q = self.queue.lock();
            if q.is_empty() {
                return Ok(0);
            }
            q.drain(..).collect()
        };

        let count = batches.len();
        if count == 0 {
            return Ok(0);
        }

        // Merge all writes into a single unified list
        let mut total_writes = Vec::new();
        for batch in &batches {
            total_writes.extend(batch.writes.iter().cloned());
            for pid in &batch.tx_pages {
                leader_tx_pages.insert(*pid);
            }
        }

        // Sort and deduplicate keys across all queued batches
        total_writes.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        total_writes.dedup_by(|a, b| a.0 == b.0);

        let res = btree.insert_sorted_batch_raw_in_tx(&total_writes, leader_tx_pages);

        // Notify all waiting followers of the batch result
        for batch in batches {
            let mut r = batch.result.lock();
            *r = Some(match &res {
                Ok(()) => Ok(()),
                Err(e) => Err(DbError::Corruption(format!("Write combining failure: {e}"))),
            });
            batch.completed.store(true, Ordering::Release);
            batch.cond.notify_one();
        }

        res.map(|()| count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tej_db_btree::InlineKey;
    use tej_db_core::{Index, PageId};
    use tej_db_pager::{FastPageIdBuildHasher, MemStorage, PageCache};

    #[test]
    fn test_write_combining_queue_drain_and_apply() {
        let storage = MemStorage::new();
        let cache = Arc::new(PageCache::new(storage, 10));
        let root_id = cache.allocate_page().unwrap();
        let leaf = tej_db_core::Page::new_leaf(root_id, PageId::INVALID);
        cache.write_page(leaf).unwrap();

        let mut btree = BTreeIndex::new(Arc::clone(&cache), Some(root_id)).unwrap();
        let queue = Arc::new(TableWriteQueue::new());
        let mut leader_pages = TxPageSet::with_capacity_and_hasher(16, FastPageIdBuildHasher);

        // Spawn a follower thread that submits writes
        let q_clone = Arc::clone(&queue);
        let handle = std::thread::spawn(move || {
            let writes = vec![
                (
                    InlineKey::from_slice(b"k1"),
                    Some(tej_db_btree::InlineVal::from_slice(b"v1")),
                    false,
                ),
                (
                    InlineKey::from_slice(b"k2"),
                    Some(tej_db_btree::InlineVal::from_slice(b"v2")),
                    false,
                ),
            ];
            let follower_pages = TxPageSet::with_capacity_and_hasher(16, FastPageIdBuildHasher);
            q_clone.submit_and_wait(writes, follower_pages).unwrap();
        });

        // Spin until follower has enqueued
        while queue.queue.lock().is_empty() {
            std::thread::yield_now();
        }

        // Leader drains and applies
        let drained = queue
            .drain_and_apply(&mut btree, &mut leader_pages)
            .unwrap();
        assert_eq!(drained, 1);
        handle.join().unwrap();

        // Verify keys exist in btree
        assert_eq!(btree.get(b"k1").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(btree.get(b"k2").unwrap(), Some(b"v2".to_vec()));
    }
}
