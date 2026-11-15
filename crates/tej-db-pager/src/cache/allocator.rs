use crate::cache::PageCache;
use std::sync::atomic::Ordering;
use tej_db_core::{PageId, Result, Storage};

impl<S: Storage> PageCache<S> {
    /// Allocate a page ID, reusing from the Free-List before allocating new disk pages.
    /// Batches underlying storage page allocations to minimize storage lock contention.
    pub fn allocate_page(&self) -> Result<PageId> {
        let mut free_list = self.free_list.lock();
        if let Some(recycled) = free_list.pop() {
            return Ok(recycled);
        }
        drop(free_list);

        let mut prealloc = self.preallocated_pages.lock();
        if let Some(recycled) = prealloc.pop() {
            return Ok(recycled);
        }

        let mut storage = self.storage.write();
        let chunk = storage.allocate_pages(32)?;
        if let Some((first, rest)) = chunk.split_first() {
            prealloc.extend(rest.iter().rev().copied());
            Ok(*first)
        } else {
            storage.allocate_page()
        }
    }

    /// Allocate multiple page IDs in batch, draining from the Free-List first.
    pub fn allocate_pages_batch(&self, count: usize) -> Result<Vec<PageId>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut allocated = Vec::with_capacity(count);
        let mut free_list = self.free_list.lock();
        while allocated.len() < count {
            if let Some(recycled) = free_list.pop() {
                allocated.push(recycled);
            } else {
                break;
            }
        }
        drop(free_list);

        if allocated.len() < count {
            let mut prealloc = self.preallocated_pages.lock();
            while allocated.len() < count {
                if let Some(recycled) = prealloc.pop() {
                    allocated.push(recycled);
                } else {
                    break;
                }
            }

            let needed = count - allocated.len();
            if needed > 0 {
                let alloc_count = needed.max(32);
                let mut storage = self.storage.write();
                let new_ids = storage.allocate_pages(alloc_count)?;
                if alloc_count > needed {
                    let (take, surplus) = new_ids.split_at(needed);
                    allocated.extend_from_slice(take);
                    prealloc.extend(surplus.iter().rev().copied());
                } else {
                    allocated.extend(new_ids);
                }
            }
        }
        Ok(allocated)
    }

    /// Allocate contiguous page IDs directly from storage for sequential streaming extents.
    pub fn allocate_overflow_extent(&self, count: usize) -> Result<PageId> {
        if count == 0 {
            return Ok(PageId::INVALID);
        }
        let mut storage = self.storage.write();
        storage.allocate_contiguous_extent(count)
    }

    /// Write contiguous pages directly to storage, bypassing cache hash table overhead for multi-MB streaming BLOBs.
    pub fn write_storage_extent(&self, start_id: PageId, pages: &[u8]) -> Result<()> {
        let mut storage = self.storage.write();
        storage.write_pages(start_id, pages)
    }

    /// Format and write overflow pages directly to storage.
    pub fn write_storage_overflow(&self, start_id: PageId, data: &[u8]) -> Result<()> {
        let mut storage = self.storage.write();
        storage.write_overflow_payload(start_id, data)
    }

    /// Read a contiguous overflow extent directly from storage in a single pass.
    pub fn read_overflow_extent(
        &self,
        start_id: PageId,
        total_len: usize,
    ) -> Result<Option<Vec<u8>>> {
        let mut out = Vec::with_capacity(total_len);
        #[allow(clippy::uninit_vec)]
        unsafe {
            out.set_len(total_len);
        }
        let storage = self.storage.read();
        if storage.read_overflow_payload(start_id, total_len, &mut out)? {
            Ok(Some(out))
        } else {
            Ok(None)
        }
    }

    /// Execute a closure against a contiguous overflow payload directly from storage without allocations.
    pub fn with_overflow_payload<R>(
        &self,
        start_id: PageId,
        total_len: usize,
        f: impl FnOnce(&[u8]) -> Result<R>,
    ) -> Result<Option<R>> {
        let storage = self.storage.read();
        storage.with_overflow_payload(start_id, total_len, f)
    }

    /// Return a retired page ID to the Free-List or Pending-Free List (if snapshot readers are active).
    pub fn free_page(&self, id: PageId) {
        if !id.is_valid() {
            return;
        }
        if self.active_readers.load(Ordering::SeqCst) > 0 {
            let mut pending = self.pending_free.lock();
            if self.active_readers.load(Ordering::SeqCst) > 0 {
                pending.push(id);
                return;
            }
        }
        self.free_list.lock().push(id);
    }

    /// Register a new active snapshot reader.
    pub fn acquire_reader(&self) {
        self.active_readers.fetch_add(1, Ordering::SeqCst);
    }

    /// Release an active snapshot reader and reclaim pending free pages if no readers remain.
    pub fn release_reader(&self) {
        if self.active_readers.fetch_sub(1, Ordering::SeqCst) == 1 {
            let mut pending = self.pending_free.lock();
            if !pending.is_empty() {
                let mut free = self.free_list.lock();
                free.append(&mut pending);
            }
        }
    }

    /// Active snapshot readers count.
    #[must_use]
    pub fn active_readers(&self) -> usize {
        self.active_readers.load(Ordering::SeqCst)
    }

    /// Number of recyclable pages in the Free-List.
    #[must_use]
    pub fn free_list_len(&self) -> usize {
        self.free_list.lock().len()
    }

    /// Number of pages waiting in the Pending-Free List.
    #[must_use]
    pub fn pending_free_len(&self) -> usize {
        self.pending_free.lock().len()
    }

    /// Total number of pages allocated in the underlying physical storage.
    #[must_use]
    pub fn storage_page_count(&self) -> u64 {
        self.storage.read().page_count()
    }

    pub fn stats(&self) -> (u64, u64, usize) {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let mut count = 0;
        for shard in &self.shards {
            count += shard.state.read().entries.len();
        }
        (hits, misses, count)
    }
}
