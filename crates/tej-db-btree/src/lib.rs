pub mod batch;
pub mod index_impl;
pub mod mutate;
pub mod node;
pub mod overflow;
pub mod search;
pub mod tx_pages;

#[cfg(test)]
mod tests;

pub use node::BTreeNode;
pub use tx_pages::TxPageSet;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tej_db_core::{Page, PageId, Result, Storage};
use tej_db_pager::PageCache;

pub type InlineKey = smallvec::SmallVec<[u8; 32]>;
pub type InlineVal = smallvec::SmallVec<[u8; 64]>;
pub type RawBatchItem = (InlineKey, Option<InlineVal>, bool);

/// A high-performance crash-safe Copy-On-Write (COW) B-Tree index backed by zero-copy slotted pages.
pub struct BTreeIndex<S: Storage> {
    pub(crate) cache: Arc<PageCache<S>>,
    pub(crate) root_id: AtomicU64,
}

impl<S: Storage> fmt::Debug for BTreeIndex<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BTreeIndex")
            .field("root_id", &self.root_id())
            .finish_non_exhaustive()
    }
}

impl<S: Storage> Clone for BTreeIndex<S> {
    fn clone(&self) -> Self {
        Self {
            cache: Arc::clone(&self.cache),
            root_id: AtomicU64::new(self.root_id().0),
        }
    }
}

impl<S: Storage> BTreeIndex<S> {
    pub fn new(cache: Arc<PageCache<S>>, root_id: Option<PageId>) -> Result<Self> {
        let root = if let Some(id) = root_id {
            id
        } else {
            let new_root = cache.allocate_page()?;
            let page = Page::new_leaf(new_root, PageId::INVALID);
            cache.write_page(page)?;
            new_root
        };

        Ok(Self {
            cache,
            root_id: AtomicU64::new(root.0),
        })
    }

    #[must_use]
    pub fn root_id(&self) -> PageId {
        PageId(self.root_id.load(Ordering::Acquire))
    }

    pub fn set_root_id(&self, root_id: PageId) {
        self.root_id.store(root_id.0, Ordering::Release);
    }
}
