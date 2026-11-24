use crate::cache::{CacheShardState, PageCache};
use std::sync::atomic::Ordering;
use tej_db_core::{PAGE_SIZE, Page, PageId, Result, Storage};

impl<S: Storage> PageCache<S> {
    pub fn flush_to_storage(&self) -> Result<()> {
        let mask = self.dirty_shard_mask.swap(0, Ordering::AcqRel);
        if mask == 0 {
            return Ok(());
        }

        let mut dirty_pages: Vec<(PageId, Page)> = Vec::new();
        for (s_idx, shard) in self.shards.iter().enumerate() {
            if (mask & (1 << s_idx)) != 0 {
                let mut state = shard.state.write();
                let dirty_ids = std::mem::take(&mut state.dirty_pages);
                for id in dirty_ids {
                    if let Some(entry) = state.entries.get_mut(&id) {
                        if entry.is_dirty {
                            entry.page.update_checksum();
                            dirty_pages.push((id, entry.page.clone()));
                            entry.is_dirty = false;
                        }
                    }
                }
            }
        }

        if dirty_pages.is_empty() {
            return Ok(());
        }

        dirty_pages.sort_unstable_by_key(|(id, _)| *id);
        dirty_pages.dedup_by_key(|(id, _)| *id);

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

        Ok(())
    }

    /// Flush only data pages (PageId >= 2) to storage, leaving superblock pages intact.
    /// This establishes Phase 1 of the two-phase commit flush barrier.
    pub fn flush_data_pages(&self) -> Result<()> {
        let mask = self.dirty_shard_mask.load(Ordering::Acquire);
        if mask == 0 {
            return Ok(());
        }

        let mut dirty_pages: Vec<(PageId, Page)> = Vec::new();
        for (s_idx, shard) in self.shards.iter().enumerate() {
            if (mask & (1 << s_idx)) != 0 {
                let mut state = shard.state.write();
                let dirty_ids = std::mem::take(&mut state.dirty_pages);
                for id in dirty_ids {
                    if id.0 < 2 {
                        state.dirty_pages.push(id);
                        continue;
                    }
                    if let Some(entry) = state.entries.get_mut(&id) {
                        if entry.is_dirty {
                            entry.page.update_checksum();
                            dirty_pages.push((id, entry.page.clone()));
                            entry.is_dirty = false;
                        }
                    }
                }
            }
        }

        if dirty_pages.is_empty() {
            return Ok(());
        }

        dirty_pages.sort_unstable_by_key(|(id, _)| *id);
        dirty_pages.dedup_by_key(|(id, _)| *id);

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

        Ok(())
    }

    /// Flush a single page (e.g. active superblock) directly to storage.
    /// This establishes Phase 2 of the two-phase commit flush barrier.
    pub fn flush_page(&self, id: PageId) -> Result<()> {
        let shard_idx = self.shard_index(id);
        let shard = &self.shards[shard_idx];
        let mut state = shard.state.write();

        state.dirty_pages.retain(|&pid| pid != id);
        if let Some(entry) = state.entries.get_mut(&id) {
            if entry.is_dirty {
                entry.page.update_checksum();
                entry.is_dirty = false;
                let mut storage = self.storage.write();
                storage.write_page(id, entry.page.as_slice())?;
            }
        }
        Ok(())
    }

    pub fn flush(&self) -> Result<()> {
        self.flush_to_storage()?;
        self.sync()
    }

    pub fn sync(&self) -> Result<()> {
        let mut storage = self.storage.write();
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
