use crate::meta::TableMetaEntry;
use std::sync::Arc;
use tej_db_btree::{BTreeIndex, TxPageSet};
use tej_db_core::{DbError, Index, KeyRange, Page, PageId, Result, Storage};
use tej_db_pager::PageCache;

/// System table name for the internal table catalog B+ tree.
pub const CATALOG_TREE_TABLE: &str = "__sys_catalog__";

/// Reads all table metadata entries from the catalog B+ tree root.
pub fn load_catalog_entries<S: Storage>(
    cache: &Arc<PageCache<S>>,
    catalog_root: PageId,
) -> Result<Vec<TableMetaEntry>> {
    if !catalog_root.is_valid() {
        return Ok(Vec::new());
    }

    let btree = BTreeIndex::new(Arc::clone(cache), Some(catalog_root))?;
    let kvs = btree.scan_range(&KeyRange::all())?;
    let mut entries = Vec::with_capacity(kvs.len());

    for kv in kvs {
        let name = String::from_utf8(kv.key).map_err(|e| {
            DbError::Corruption(format!("Invalid UTF-8 in catalog table name: {e}"))
        })?;
        if kv.value.len() != 8 {
            return Err(DbError::Corruption(
                "Invalid PageId byte length in catalog table value".into(),
            ));
        }
        let root_u64 = u64::from_le_bytes(kv.value[0..8].try_into().map_err(|_| {
            DbError::Corruption("Slice to array conversion failed for catalog root".into())
        })?);
        entries.push(TableMetaEntry {
            name,
            root_id: PageId(root_u64),
        });
    }

    Ok(entries)
}

/// Applies table creations, root updates, and drops to the catalog B+ tree.
///
/// Returns the updated catalog root `PageId`.
pub fn sync_catalog_tree<S: Storage>(
    cache: &Arc<PageCache<S>>,
    catalog_root: PageId,
    active_tables: &[TableMetaEntry],
    tx_pages: &mut TxPageSet,
) -> Result<PageId> {
    let root = if catalog_root.is_valid() {
        catalog_root
    } else {
        let new_root = cache.allocate_page()?;
        let leaf = Page::new_leaf(new_root, PageId::INVALID);
        cache.write_page(leaf)?;
        tx_pages.insert(new_root);
        new_root
    };

    let btree = BTreeIndex::new(Arc::clone(cache), Some(root))?;
    for entry in active_tables {
        let key = entry.name.as_bytes();
        let val = entry.root_id.0.to_le_bytes();
        btree.insert_in_tx(key, &val, tx_pages)?;
    }

    Ok(btree.root_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tej_db_pager::MemStorage;

    #[test]
    fn test_catalog_tree_sync_and_load_roundtrip() {
        let storage = MemStorage::new();
        let cache = Arc::new(PageCache::new(storage, 10));

        let mut tx_pages =
            TxPageSet::with_capacity_and_hasher(16, tej_db_pager::FastPageIdBuildHasher);
        let tables = vec![
            TableMetaEntry {
                name: "users".into(),
                root_id: PageId(10),
            },
            TableMetaEntry {
                name: "orders".into(),
                root_id: PageId(20),
            },
            TableMetaEntry {
                name: "items".into(),
                root_id: PageId(30),
            },
        ];

        let catalog_root =
            sync_catalog_tree(&cache, PageId::INVALID, &tables, &mut tx_pages).unwrap();
        assert!(catalog_root.is_valid());

        let loaded = load_catalog_entries(&cache, catalog_root).unwrap();
        assert_eq!(loaded.len(), 3);
        assert!(
            loaded
                .iter()
                .any(|t| t.name == "users" && t.root_id == PageId(10))
        );
        assert!(
            loaded
                .iter()
                .any(|t| t.name == "orders" && t.root_id == PageId(20))
        );
        assert!(
            loaded
                .iter()
                .any(|t| t.name == "items" && t.root_id == PageId(30))
        );
    }
}
