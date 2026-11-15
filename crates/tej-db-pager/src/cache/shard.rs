use crate::cache::hasher::FastPageIdBuildHasher;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use tej_db_core::{Page, PageId};

pub(crate) struct CacheEntry {
    pub(crate) page: Page,
    pub(crate) is_dirty: bool,
    pub(crate) referenced: AtomicBool,
}

pub(crate) struct CacheShardState {
    pub(crate) entries: HashMap<PageId, CacheEntry, FastPageIdBuildHasher>,
    pub(crate) clock_ring: Vec<PageId>,
    pub(crate) clock_hand: usize,
    pub(crate) dirty_pages: Vec<PageId>,
}

pub(crate) struct CacheShard {
    pub(crate) state: RwLock<CacheShardState>,
    pub(crate) capacity: usize,
}
