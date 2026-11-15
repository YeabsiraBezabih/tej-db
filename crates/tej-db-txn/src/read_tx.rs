use crate::catalog::TableRootsSnapshot;
use crate::table::TableRef;
use std::fmt;
use std::sync::Arc;
use tej_db_btree::BTreeIndex;
use tej_db_core::{DbError, Index, KV, KeyRange, Result, Storage, TxId};
use tej_db_pager::PageCache;

/// A snapshot-isolated read-only transaction.
pub struct ReadOnlyTx<S: Storage> {
    pub tx_id: TxId,
    pub(crate) btree: BTreeIndex<S>,
    pub(crate) cache: Arc<PageCache<S>>,
    pub(crate) catalog_snapshot: Arc<TableRootsSnapshot>,
}

impl<S: Storage> fmt::Debug for ReadOnlyTx<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadOnlyTx")
            .field("tx_id", &self.tx_id)
            .finish_non_exhaustive()
    }
}

impl<S: Storage> ReadOnlyTx<S> {
    pub(crate) fn new(
        tx_id: TxId,
        btree: BTreeIndex<S>,
        cache: Arc<PageCache<S>>,
        catalog_snapshot: Arc<TableRootsSnapshot>,
    ) -> Self {
        Self {
            tx_id,
            btree,
            cache,
            catalog_snapshot,
        }
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.btree.get(key)
    }

    /// Zero-allocation, high-throughput value visitor executing against in-page memory.
    pub fn with_value<R>(
        &self,
        key: &[u8],
        f: impl FnOnce(&[u8]) -> Result<R>,
    ) -> Result<Option<R>> {
        self.btree.with_value(key, f)
    }

    pub fn scan_range(&self, range: &KeyRange) -> Result<Vec<KV>> {
        self.btree.scan_range(range)
    }

    /// Zero-allocation, high-throughput streaming cursor scan visiting page references directly.
    pub fn scan_range_visitor(
        &self,
        range: &KeyRange,
        visitor: impl FnMut(&[u8], &[u8]) -> Result<bool>,
    ) -> Result<()> {
        self.btree.scan_range_visitor(range, visitor)
    }

    pub fn scan_all(&self) -> Result<Vec<KV>> {
        self.btree.scan_range(&KeyRange::all())
    }

    pub fn list_tables(&self) -> Result<Vec<String>> {
        let mut tables: Vec<String> = self
            .catalog_snapshot
            .iter()
            .map(|(n, _)| n.clone())
            .filter(|n| n != crate::sharding::SHARDS_META_TABLE && !n.contains("$shard$"))
            .collect();
        if let Ok(meta_tbl) = self.open_table(crate::sharding::SHARDS_META_TABLE) {
            if let Ok(kvs) = meta_tbl.scan_all() {
                for kv in kvs {
                    if let Ok(name) = String::from_utf8(kv.key) {
                        tables.push(name);
                    }
                }
            }
        }
        tables.sort();
        tables.dedup();
        Ok(tables)
    }

    pub fn open_table(&self, name: &str) -> Result<TableRef<'_, S>> {
        if name == "default" {
            return Ok(TableRef::new(name, self.btree.clone(), self));
        }
        if let Some((_, root_id)) = self.catalog_snapshot.iter().find(|(n, _)| n == name) {
            let btree = BTreeIndex::new(Arc::clone(&self.cache), Some(*root_id))?;
            Ok(TableRef::new(name, btree, self))
        } else {
            Err(DbError::TableNotFound(name.to_string()))
        }
    }
}

impl<S: Storage> Drop for ReadOnlyTx<S> {
    fn drop(&mut self) {
        self.cache.release_reader();
    }
}
