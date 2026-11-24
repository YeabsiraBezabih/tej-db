use crate::cache::{CacheShardState, PageCache};
use std::sync::atomic::Ordering;
use tej_db_core::{PAGE_SIZE, Page, PageId, Result, Storage};

impl<S: Storage> PageCache<S> {
    pub fn flush_to_storage(&self) -> Result<()> {
        let _flush_guard = self.flush_lock.lock();
        let mask = self.dirty_shard_mask.swap(0, Ordering::AcqRel);
        if mask == 0 {
            return Ok(());
        }

        let mut dirty_pages: Vec<(PageId, Page, u64)> = Vec::new();
        for (s_idx, shard) in self.shards.iter().enumerate() {
            if (mask & (1 << s_idx)) != 0 {
                let mut state = shard.state.write();
                let CacheShardState {
                    dirty_pages: shard_dirty,
                    entries,
                    ..
                } = &mut *state;
                for &id in shard_dirty.iter() {
                    if let Some(entry) = entries.get_mut(&id) {
                        if entry.is_dirty {
                            entry.page.update_checksum();
                            dirty_pages.push((id, entry.page.clone(), entry.dirty_seq));
                        }
                    }
                }
            }
        }

        if dirty_pages.is_empty() {
            return Ok(());
        }

        dirty_pages.sort_unstable_by_key(|(id, _, _)| *id);
        dirty_pages.dedup_by_key(|(id, _, _)| *id);

        let write_res = (|| -> Result<()> {
            let mut storage = self.storage.write();
            let mut i = 0;
            let n = dirty_pages.len();
            let mut extent_buf = Vec::with_capacity(64 * PAGE_SIZE);

            while i < n {
                let start_id = dirty_pages[i].0;
                let mut run_len = 1;
                while i + run_len < n && dirty_pages[i + run_len].0.0 == start_id.0 + run_len as u64
                {
                    run_len += 1;
                }

                if run_len == 1 {
                    storage.write_page(start_id, dirty_pages[i].1.as_slice())?;
                } else {
                    extent_buf.clear();
                    extent_buf.reserve(run_len * PAGE_SIZE);
                    for j in 0..run_len {
                        extent_buf.extend_from_slice(dirty_pages[i + j].1.as_slice());
                    }
                    storage.write_pages(start_id, &extent_buf)?;
                }
                i += run_len;
            }
            Ok(())
        })();

        if let Err(e) = write_res {
            self.dirty_shard_mask.fetch_or(mask, Ordering::Release);
            return Err(e);
        }

        // Only clear dirty state for pages whose dirty_seq has not advanced during I/O
        for (s_idx, shard) in self.shards.iter().enumerate() {
            if (mask & (1 << s_idx)) != 0 {
                let mut state = shard.state.write();
                let CacheShardState {
                    dirty_pages: shard_dirty,
                    entries,
                    ..
                } = &mut *state;
                for (id, _, flushed_seq) in &dirty_pages {
                    if self.shard_index(*id) == s_idx {
                        if let Some(entry) = entries.get_mut(id) {
                            if entry.dirty_seq == *flushed_seq {
                                entry.is_dirty = false;
                            }
                        }
                    }
                }
                shard_dirty.retain(|id| entries.get(id).is_some_and(|e| e.is_dirty));
                if !shard_dirty.is_empty() {
                    self.dirty_shard_mask
                        .fetch_or(1 << s_idx, Ordering::Release);
                }
            }
        }

        Ok(())
    }

    /// Flush only data pages (PageId >= 2) to storage, leaving superblock pages intact.
    /// This establishes Phase 1 of the two-phase commit flush barrier.
    pub fn flush_data_pages(&self) -> Result<()> {
        let _flush_guard = self.flush_lock.lock();
        let mask = self.dirty_shard_mask.load(Ordering::Acquire);
        if mask == 0 {
            return Ok(());
        }

        let mut dirty_pages: Vec<(PageId, Page, u64)> = Vec::new();
        for (s_idx, shard) in self.shards.iter().enumerate() {
            if (mask & (1 << s_idx)) != 0 {
                let mut state = shard.state.write();
                let CacheShardState {
                    dirty_pages: shard_dirty,
                    entries,
                    ..
                } = &mut *state;
                for &id in shard_dirty.iter() {
                    if id.0 < 2 {
                        continue;
                    }
                    if let Some(entry) = entries.get_mut(&id) {
                        if entry.is_dirty {
                            entry.page.update_checksum();
                            dirty_pages.push((id, entry.page.clone(), entry.dirty_seq));
                        }
                    }
                }
            }
        }

        if dirty_pages.is_empty() {
            return Ok(());
        }

        dirty_pages.sort_unstable_by_key(|(id, _, _)| *id);
        dirty_pages.dedup_by_key(|(id, _, _)| *id);

        let mut storage = self.storage.write();
        let mut i = 0;
        let n = dirty_pages.len();
        let mut extent_buf = Vec::with_capacity(64 * PAGE_SIZE);

        while i < n {
            let start_id = dirty_pages[i].0;
            let mut run_len = 1;
            while i + run_len < n && dirty_pages[i + run_len].0.0 == start_id.0 + run_len as u64 {
                run_len += 1;
            }

            if run_len == 1 {
                storage.write_page(start_id, dirty_pages[i].1.as_slice())?;
            } else {
                extent_buf.clear();
                extent_buf.reserve(run_len * PAGE_SIZE);
                for j in 0..run_len {
                    extent_buf.extend_from_slice(dirty_pages[i + j].1.as_slice());
                }
                storage.write_pages(start_id, &extent_buf)?;
            }
            i += run_len;
        }
        drop(storage);

        // Only clear data pages' dirty state if their dirty_seq matches flushed_seq
        for (s_idx, shard) in self.shards.iter().enumerate() {
            if (mask & (1 << s_idx)) != 0 {
                let mut state = shard.state.write();
                let CacheShardState {
                    dirty_pages: shard_dirty,
                    entries,
                    ..
                } = &mut *state;
                for (id, _, flushed_seq) in &dirty_pages {
                    if self.shard_index(*id) == s_idx {
                        if let Some(entry) = entries.get_mut(id) {
                            if entry.dirty_seq == *flushed_seq {
                                entry.is_dirty = false;
                            }
                        }
                    }
                }
                shard_dirty.retain(|id| entries.get(id).is_some_and(|e| e.is_dirty));
                if !shard_dirty.is_empty() {
                    self.dirty_shard_mask
                        .fetch_or(1 << s_idx, Ordering::Release);
                }
            }
        }

        Ok(())
    }

    /// Flush a single page (e.g. active superblock) directly to storage.
    /// This establishes Phase 2 of the two-phase commit flush barrier.
    pub fn flush_page(&self, id: PageId) -> Result<()> {
        let _flush_guard = self.flush_lock.lock();
        let shard_idx = self.shard_index(id);
        let shard = &self.shards[shard_idx];
        let mut state = shard.state.write();

        if let Some(entry) = state.entries.get_mut(&id) {
            if entry.is_dirty {
                entry.page.update_checksum();
                let mut storage = self.storage.write();
                storage.write_page(id, entry.page.as_slice())?;
                entry.is_dirty = false;
                state.dirty_pages.retain(|&pid| pid != id);
            }
        }
        Ok(())
    }

    pub fn flush(&self) -> Result<()> {
        self.flush_to_storage()?;
        self.sync()
    }

    pub fn sync(&self) -> Result<()> {
        let storage = self.storage.read();
        storage.sync()
    }

    pub fn discard_dirty(&self) {
        self.dirty_shard_mask.store(0, Ordering::Release);
        for shard in &self.shards {
            let mut state = shard.state.write();
            let dirty = std::mem::take(&mut state.dirty_pages);
            for id in dirty {
                state.entries.remove(&id);
            }
            let CacheShardState {
                entries,
                clock_ring,
                ..
            } = &mut *state;
            clock_ring.retain(|id| entries.contains_key(id));
        }
    }
}
