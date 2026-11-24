use std::collections::HashSet;
use tej_db_core::PageId;
use tej_db_pager::FastPageIdBuildHasher;

/// Tracks pages allocated and retired during a single write transaction.
/// Newly allocated pages (`allocated`) can be discarded if the transaction aborts.
/// Pages replaced by copy-on-write (`retired`) are only freed to the allocator after
/// the transaction successfully commits and syncs its new superblock to disk.
#[derive(Debug, Clone, Default)]
pub struct TxPageSet {
    pub allocated: HashSet<PageId, FastPageIdBuildHasher>,
    pub retired: Vec<PageId>,
}

impl TxPageSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_capacity_and_hasher(capacity: usize, hasher: FastPageIdBuildHasher) -> Self {
        Self {
            allocated: HashSet::with_capacity_and_hasher(capacity, hasher),
            retired: Vec::new(),
        }
    }

    #[inline]
    pub fn insert(&mut self, id: PageId) -> bool {
        self.allocated.insert(id)
    }

    #[inline]
    #[must_use]
    pub fn contains(&self, id: &PageId) -> bool {
        self.allocated.contains(id)
    }

    #[inline]
    pub fn retire(&mut self, id: PageId) {
        if id.is_valid() && !self.allocated.contains(&id) && !self.retired.contains(&id) {
            self.retired.push(id);
        }
    }

    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.allocated.len()
    }

    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allocated.is_empty()
    }

    pub fn clear(&mut self) {
        self.allocated.clear();
        self.retired.clear();
    }

    pub fn extend(&mut self, other: Self) {
        self.allocated.extend(other.allocated);
        for id in other.retired {
            if !self.retired.contains(&id) {
                self.retired.push(id);
            }
        }
    }

    pub fn iter(&self) -> std::collections::hash_set::Iter<'_, PageId> {
        self.allocated.iter()
    }
}

impl<'a> IntoIterator for &'a TxPageSet {
    type Item = &'a PageId;
    type IntoIter = std::collections::hash_set::Iter<'a, PageId>;

    fn into_iter(self) -> Self::IntoIter {
        self.allocated.iter()
    }
}

impl IntoIterator for TxPageSet {
    type Item = PageId;
    type IntoIter = std::collections::hash_set::IntoIter<PageId>;

    fn into_iter(self) -> Self::IntoIter {
        self.allocated.into_iter()
    }
}
