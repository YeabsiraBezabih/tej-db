use tej_db_core::PageId;
use std::collections::{HashSet, VecDeque};

/// Scan-resistant S3-FIFO eviction state for a single cache shard.
///
/// Implements the S3-FIFO algorithm (SOSP 2023):
/// - `Small` queue (10% capacity): filters one-hit-wonders from table range scans and vector traversals.
/// - `Main` queue (90% capacity): preserves popular pages using second-chance reference bits.
/// - `Ghost` queue: memorizes recent eviction fingerprints to immediately promote repeated accesses to `Main`.
#[derive(Debug)]
pub struct S3FifoQueue {
    pub small: VecDeque<PageId>,
    pub main: VecDeque<PageId>,
    pub ghost: HashSet<PageId>,
    pub ghost_queue: VecDeque<PageId>,
    pub small_capacity: usize,
    pub main_capacity: usize,
    pub ghost_capacity: usize,
}

impl S3FifoQueue {
    #[must_use]
    pub fn new(total_capacity: usize) -> Self {
        let small_cap = (total_capacity / 10).max(1);
        let main_cap = total_capacity.saturating_sub(small_cap).max(1);
        let ghost_cap = total_capacity.max(16);

        Self {
            small: VecDeque::with_capacity(small_cap),
            main: VecDeque::with_capacity(main_cap),
            ghost: HashSet::with_capacity(ghost_cap),
            ghost_queue: VecDeque::with_capacity(ghost_cap),
            small_capacity: small_cap,
            main_capacity: main_cap,
            ghost_capacity: ghost_cap,
        }
    }

    /// Records insertion of a newly fetched page into the S3-FIFO queue.
    pub fn insert_page(&mut self, id: PageId) {
        if self.ghost.remove(&id) {
            // Page was recently evicted from small; promote directly to main
            self.main.push_back(id);
        } else {
            // New page enters small queue
            self.small.push_back(id);
        }
    }

    /// Selects a candidate victim PageId for eviction.
    ///
    /// The caller supplies `is_referenced`, which checks and resets the atomic reference bit on the cache entry.
    pub fn select_victim<F>(&mut self, mut is_referenced: F) -> Option<PageId>
    where
        F: FnMut(PageId) -> bool,
    {
        // 1. If small queue is beyond its target capacity, evict from small
        if self.small.len() >= self.small_capacity {
            while let Some(candidate) = self.small.pop_front() {
                if is_referenced(candidate) {
                    // Accessed while in small: promote to main
                    self.main.push_back(candidate);
                } else {
                    // Evict from small and record in ghost
                    self.record_ghost(candidate);
                    return Some(candidate);
                }
            }
        }

        // 2. Otherwise evict from main queue using second chance
        let main_len = self.main.len();
        for _ in 0..(main_len * 2) {
            if let Some(candidate) = self.main.pop_front() {
                if is_referenced(candidate) {
                    self.main.push_back(candidate);
                } else {
                    return Some(candidate);
                }
            } else {
                break;
            }
        }

        // 3. Fallback: pop from small if main was empty
        if let Some(candidate) = self.small.pop_front() {
            self.record_ghost(candidate);
            return Some(candidate);
        }

        self.main.pop_front()
    }

    /// Removes a page from queues if explicitly evicted or deleted.
    pub fn remove(&mut self, id: PageId) {
        self.small.retain(|&x| x != id);
        self.main.retain(|&x| x != id);
        self.ghost.remove(&id);
    }

    fn record_ghost(&mut self, id: PageId) {
        if self.ghost.len() >= self.ghost_capacity {
            if let Some(old) = self.ghost_queue.pop_front() {
                self.ghost.remove(&old);
            }
        }
        self.ghost.insert(id);
        self.ghost_queue.push_back(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s3_fifo_scan_resistance() {
        let mut fifo = S3FifoQueue::new(10);
        assert_eq!(fifo.small_capacity, 1);
        assert_eq!(fifo.main_capacity, 9);

        // Insert hot page that is referenced
        fifo.insert_page(PageId(1));

        // When small queue is probed, if referenced it promotes to main
        let victim = fifo.select_victim(|pid| pid == PageId(1));
        assert!(victim.is_none());
        assert!(fifo.main.contains(&PageId(1)));

        // Unreferenced scan pages pass through small and get evicted
        fifo.insert_page(PageId(2));
        let victim2 = fifo.select_victim(|_| false);
        assert_eq!(victim2, Some(PageId(2)));
        assert!(fifo.ghost.contains(&PageId(2)));

        // Re-accessing page in ghost promotes directly to main
        fifo.insert_page(PageId(2));
        assert!(fifo.main.contains(&PageId(2)));
    }
}
