use crate::conflict::CommittedTxRecord;
use crate::meta::TableMetaEntry;
use crate::spin::AdaptiveSpinLock;
use parking_lot::RwLock;
use smallvec::SmallVec;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tej_db_core::PageId;

pub type TableRootsSnapshot = SmallVec<[(String, PageId); 16]>;

/// In-memory descriptor for a registered table with atomic root and commit metadata.
#[derive(Debug)]
pub struct TableCatalogEntry {
    pub name: String,
    pub root: AtomicU64,
    pub last_commit_tx: AtomicU64,
    pub history: RwLock<VecDeque<CommittedTxRecord>>,
    pub commit_lock: AdaptiveSpinLock,
}

impl TableCatalogEntry {
    #[must_use]
    pub fn new(name: String, root_id: PageId) -> Self {
        Self {
            name,
            root: AtomicU64::new(root_id.0),
            last_commit_tx: AtomicU64::new(0),
            history: RwLock::new(VecDeque::with_capacity(128)),
            commit_lock: AdaptiveSpinLock::new(),
        }
    }
}

/// Fast in-memory lock-free table catalog maintaining active B+ Tree root PageIds per table.
#[derive(Debug)]
pub struct TableCatalog {
    entries: RwLock<SmallVec<[Arc<TableCatalogEntry>; 16]>>,
}

impl TableCatalog {
    #[must_use]
    pub fn new(initial_tables: &[TableMetaEntry]) -> Self {
        let mut list = SmallVec::new();
        for entry in initial_tables {
            list.push(Arc::new(TableCatalogEntry::new(
                entry.name.clone(),
                entry.root_id,
            )));
        }
        Self {
            entries: RwLock::new(list),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Arc<TableRootsSnapshot> {
        let r = self.entries.read();
        let mut snap = SmallVec::with_capacity(r.len());
        for entry in r.iter() {
            snap.push((
                entry.name.clone(),
                PageId(entry.root.load(Ordering::Acquire)),
            ));
        }
        Arc::new(snap)
    }

    #[must_use]
    pub fn get_table_root(&self, name: &str) -> Option<PageId> {
        let r = self.entries.read();
        r.iter()
            .find(|e| e.name == name)
            .map(|e| PageId(e.root.load(Ordering::Acquire)))
    }

    #[must_use]
    pub fn get_table_entry(&self, name: &str) -> Option<Arc<TableCatalogEntry>> {
        let r = self.entries.read();
        r.iter().find(|e| e.name == name).cloned()
    }

    #[must_use]
    pub fn list_tables(&self) -> Vec<String> {
        let r = self.entries.read();
        let mut names: Vec<String> = r.iter().map(|e| e.name.clone()).collect();
        names.sort();
        names
    }

    /// Update a table root. If the table already exists, this performs a fast
    /// AtomicU64 store with zero write-lock contention and zero allocations!
    pub fn update_table_root(&self, name: &str, new_root: PageId) {
        {
            let r = self.entries.read();
            if let Some(entry) = r.iter().find(|e| e.name == name) {
                entry.root.store(new_root.0, Ordering::Release);
                return;
            }
        }
        let mut w = self.entries.write();
        if let Some(entry) = w.iter().find(|e| e.name == name) {
            entry.root.store(new_root.0, Ordering::Release);
        } else {
            w.push(Arc::new(TableCatalogEntry::new(name.to_string(), new_root)));
        }
    }

    pub fn remove_table(&self, name: &str) -> bool {
        let mut w = self.entries.write();
        if let Some(pos) = w.iter().position(|e| e.name == name) {
            w.swap_remove(pos);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn to_meta_entries(&self) -> SmallVec<[TableMetaEntry; 8]> {
        let r = self.entries.read();
        let mut entries = SmallVec::new();
        for entry in r.iter() {
            entries.push(TableMetaEntry {
                name: entry.name.clone(),
                root_id: PageId(entry.root.load(Ordering::Acquire)),
            });
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_catalog_lifecycle() {
        let catalog = TableCatalog::new(&[]);
        assert_eq!(catalog.list_tables(), Vec::<String>::new());
        assert_eq!(catalog.get_table_root("users"), None);

        catalog.update_table_root("users", PageId(10));
        catalog.update_table_root("orders", PageId(20));

        assert_eq!(catalog.get_table_root("users"), Some(PageId(10)));
        assert_eq!(catalog.get_table_root("orders"), Some(PageId(20)));
        assert_eq!(catalog.list_tables(), vec!["orders", "users"]);

        // Update root
        catalog.update_table_root("users", PageId(15));
        assert_eq!(catalog.get_table_root("users"), Some(PageId(15)));

        // Remove table
        assert!(catalog.remove_table("orders"));
        assert_eq!(catalog.get_table_root("orders"), None);
        assert_eq!(catalog.list_tables(), vec!["users"]);
    }
}
