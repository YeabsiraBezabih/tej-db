pub mod allocator;
pub mod eviction;
pub mod flusher;
pub mod hasher;
pub mod shard;

pub use hasher::{FastPageIdBuildHasher, FastPageIdHasher};
pub(crate) use shard::{CacheEntry, CacheShard, CacheShardState};

use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};
use tej_db_core::{PAGE_SIZE, Page, PageId, Result, Storage};

pub const CACHE_SHARDS: usize = 16;

/// Bounded, concurrent, 16-way sharded Clock-eviction page cache with epoch-based Free-List recycling.
pub struct PageCache<S: Storage> {
    pub(crate) storage: Arc<RwLock<S>>,
    pub(crate) capacity: usize,
    active_shards: usize,
    pub(crate) shards: [CacheShard; CACHE_SHARDS],
    pub(crate) dirty_shard_mask: AtomicU16,
    pub(crate) free_list: Mutex<Vec<PageId>>,
    pub(crate) preallocated_pages: Mutex<Vec<PageId>>,
    pub(crate) pending_free: Mutex<Vec<PageId>>,
    pub(crate) active_readers: AtomicUsize,
    pub(crate) hits: AtomicU64,
    pub(crate) misses: AtomicU64,
}

impl<S: Storage> fmt::Debug for PageCache<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (hits, misses, count) = self.stats();
        let free_count = self.free_list_len();
        let pending_count = self.pending_free_len();
        f.debug_struct("PageCache")
            .field("capacity", &self.capacity)
            .field("cached_entries", &count)
            .field("free_list_pages", &free_count)
            .field("pending_free_pages", &pending_count)
            .field(
                "active_readers",
                &self.active_readers.load(Ordering::Relaxed),
            )
            .field("hits", &hits)
            .field("misses", &misses)
            .finish_non_exhaustive()
    }
}

impl<S: Storage> PageCache<S> {
    pub fn new(storage: S, capacity: usize) -> Self {
        assert!(capacity > 0, "Cache capacity must be greater than zero");
        let (active_shards, shard_caps) = if capacity < CACHE_SHARDS * 4 {
            let mut caps = [0usize; CACHE_SHARDS];
            caps[0] = capacity;
            (1, caps)
        } else {
            let base = capacity / CACHE_SHARDS;
            let rem = capacity % CACHE_SHARDS;
            let mut caps = [base; CACHE_SHARDS];
            for cap in caps.iter_mut().take(rem) {
                *cap += 1;
            }
            (CACHE_SHARDS, caps)
        };

        let shards = std::array::from_fn(|i| {
            let cap = shard_caps[i].max(1);
            CacheShard {
                state: RwLock::new(CacheShardState {
                    entries: HashMap::with_capacity_and_hasher(cap, FastPageIdBuildHasher),
                    clock_ring: Vec::with_capacity(cap),
                    clock_hand: 0,
                    dirty_pages: Vec::with_capacity(32),
                }),
                capacity: cap,
            }
        });

        Self {
            storage: Arc::new(RwLock::new(storage)),
            capacity,
            active_shards,
            shards,
            dirty_shard_mask: AtomicU16::new(0),
            free_list: Mutex::new(Vec::new()),
            preallocated_pages: Mutex::new(Vec::new()),
            pending_free: Mutex::new(Vec::new()),
            active_readers: AtomicUsize::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    #[inline]
    pub(crate) fn shard_index(&self, id: PageId) -> usize {
        if self.active_shards == 1 {
            0
        } else {
            let mut x = id.0;
            x ^= x >> 16;
            x = x.wrapping_mul(0x45d9f3b);
            x ^= x >> 16;
            (x as usize) % CACHE_SHARDS
        }
    }

    /// Read a page by `PageId`. Read hits only acquire a shared lock on the specific shard.
    pub fn get_page(&self, id: PageId) -> Result<Page> {
        let s_idx = self.shard_index(id);
        // Fast path: shared read lock on this shard only
        {
            let state = self.shards[s_idx].state.read();
            if let Some(entry) = state.entries.get(&id) {
                self.hits.fetch_add(1, Ordering::Relaxed);
                entry.referenced.store(true, Ordering::Relaxed);
                return Ok(entry.page.clone());
            }
        }

        // Cache miss: read from underlying storage
        self.misses.fetch_add(1, Ordering::Relaxed);
        let mut raw = [0u8; PAGE_SIZE];
        {
            let storage = self.storage.read();
            storage.read_page(id, &mut raw)?;
        }

        let page = Page::from_bytes(raw);
        // Verify page checksum on load
        if let Ok(header) = page.read_header() {
            if header.page_id.is_valid() && header.page_id == id {
                page.verify_checksum()?;
            }
        }

        // Insert into shard under write lock
        let mut state = self.shards[s_idx].state.write();

        // Double check if loaded concurrently
        if let Some(entry) = state.entries.get(&id) {
            entry.referenced.store(true, Ordering::Relaxed);
            return Ok(entry.page.clone());
        }

        // Evict if at capacity
        self.evict_if_needed(s_idx, &mut state)?;

        state.entries.insert(
            id,
            CacheEntry {
                page: page.clone(),
                is_dirty: false,
                referenced: AtomicBool::new(false),
            },
        );
        state.clock_ring.push(id);

        Ok(page)
    }

    /// Zero-copy read-only access to a page directly in the cache.
    #[inline]
    pub fn with_page<R>(&self, id: PageId, f: impl FnOnce(&Page) -> Result<R>) -> Result<R> {
        let s_idx = self.shard_index(id);
        {
            let state = self.shards[s_idx].state.read();
            if let Some(entry) = state.entries.get(&id) {
                self.hits.fetch_add(1, Ordering::Relaxed);
                entry.referenced.store(true, Ordering::Relaxed);
                return f(&entry.page);
            }
        }

        // Cache miss: read directly from storage without extra heap allocations
        self.misses.fetch_add(1, Ordering::Relaxed);
        let mut raw = [0u8; PAGE_SIZE];
        {
            let storage = self.storage.read();
            storage.read_page(id, &mut raw)?;
        }

        let page = Page::from_bytes(raw);
        if let Ok(header) = page.read_header() {
            if header.page_id.is_valid() && header.page_id == id {
                page.verify_checksum()?;
            }
        }

        let mut state = self.shards[s_idx].state.write();
        if let Some(entry) = state.entries.get(&id) {
            entry.referenced.store(true, Ordering::Relaxed);
            return f(&entry.page);
        }

        self.evict_if_needed(s_idx, &mut state)?;
        let res = f(&page);
        state.entries.insert(
            id,
            CacheEntry {
                page,
                is_dirty: false,
                referenced: AtomicBool::new(false),
            },
        );
        state.clock_ring.push(id);
        res
    }

    /// In-place mutation of a page directly in the cache without cloning.
    pub fn with_page_mut<R>(
        &self,
        id: PageId,
        f: impl FnOnce(&mut Page) -> Result<R>,
    ) -> Result<R> {
        let s_idx = self.shard_index(id);
        let mut state = self.shards[s_idx].state.write();
        let CacheShardState {
            entries,
            dirty_pages,
            ..
        } = &mut *state;
        if let Some(entry) = entries.get_mut(&id) {
            if !entry.is_dirty {
                entry.is_dirty = true;
                dirty_pages.push(id);
                self.dirty_shard_mask
                    .fetch_or(1 << s_idx, Ordering::Relaxed);
            }
            entry.referenced.store(true, Ordering::Relaxed);
            return f(&mut entry.page);
        }

        drop(state);
        let mut page = self.get_page(id)?;
        let res = f(&mut page);
        self.write_page(page)?;
        res
    }

    pub fn write_page(&self, page: Page) -> Result<()> {
        let header = page.read_header()?;
        let id = header.page_id;
        let s_idx = self.shard_index(id);

        let mut state = self.shards[s_idx].state.write();

        {
            let CacheShardState {
                entries,
                dirty_pages,
                ..
            } = &mut *state;
            if let Some(entry) = entries.get_mut(&id) {
                entry.page = page;
                if !entry.is_dirty {
                    entry.is_dirty = true;
                    dirty_pages.push(id);
                    self.dirty_shard_mask
                        .fetch_or(1 << s_idx, Ordering::Relaxed);
                }
                entry.referenced.store(true, Ordering::Relaxed);
                return Ok(());
            }
        }

        self.evict_if_needed(s_idx, &mut state)?;

        state.entries.insert(
            id,
            CacheEntry {
                page,
                is_dirty: true,
                referenced: AtomicBool::new(false),
            },
        );
        state.clock_ring.push(id);
        state.dirty_pages.push(id);
        self.dirty_shard_mask
            .fetch_or(1 << s_idx, Ordering::Relaxed);

        Ok(())
    }

    /// Batch write multiple pages into their respective shards with minimal lock acquisitions.
    pub fn write_pages_batch(&self, pages: Vec<Page>) -> Result<()> {
        if pages.is_empty() {
            return Ok(());
        }

        let mut buckets: [Vec<Page>; CACHE_SHARDS] = Default::default();
        for page in pages {
            if let Ok(header) = page.read_header() {
                let idx = self.shard_index(header.page_id);
                buckets[idx].push(page);
            }
        }

        for (s_idx, bucket) in buckets.into_iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            let mut state = self.shards[s_idx].state.write();
            self.dirty_shard_mask
                .fetch_or(1 << s_idx, Ordering::Relaxed);
            for page in bucket {
                let id = page.read_header()?.page_id;
                self.evict_if_needed(s_idx, &mut state)?;

                let CacheShardState {
                    entries,
                    clock_ring,
                    dirty_pages,
                    ..
                } = &mut *state;

                if let Some(entry) = entries.get_mut(&id) {
                    entry.page = page;
                    if !entry.is_dirty {
                        entry.is_dirty = true;
                        dirty_pages.push(id);
                    }
                    entry.referenced.store(true, Ordering::Relaxed);
                } else {
                    entries.insert(
                        id,
                        CacheEntry {
                            page,
                            is_dirty: true,
                            referenced: AtomicBool::new(false),
                        },
                    );
                    clock_ring.push(id);
                    dirty_pages.push(id);
                }
            }
        }

        Ok(())
    }
}
