use crate::types::VectorId;
use tej_db_core::{DbError, KeyRange, Result, Storage};
use tej_db_txn::{ReadOnlyTx, WriteTx};

/// Prefix for the ACID Copy-on-Write B+ tree delta tables.
pub const VECTOR_DELTA_PREFIX: &str = "__vec_delta_";

/// Returns the internal delta table name for a user table.
#[must_use]
pub fn delta_table_name(table_name: &str) -> String {
    format!("{VECTOR_DELTA_PREFIX}{table_name}__")
}

/// Delta mutation entry for a vector.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorDeltaItem {
    pub vector_id: VectorId,
    pub vector: Option<Vec<f32>>,
}

/// Appends a new vector to the transaction's delta buffer table.
pub fn insert_vector_delta<S: Storage>(
    tx: &mut WriteTx<'_, S>,
    table_name: &str,
    id: VectorId,
    vector: &[f32],
) -> Result<()> {
    let delta_name = delta_table_name(table_name);
    let mut table = tx.open_table_mut(&delta_name)?;

    let key = id.0.to_be_bytes();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "vector dimensions fit in u16"
    )]
    let dim = vector.len() as u16;

    let mut val = Vec::with_capacity(3 + vector.len() * 4);
    val.push(0x00); // 0x00: Active insert (not a tombstone)
    val.extend_from_slice(&dim.to_le_bytes());
    for &f in vector {
        val.extend_from_slice(&f.to_le_bytes());
    }

    table.put(&key, &val)
}

/// Records a tombstone deletion for a vector in the delta buffer table.
pub fn delete_vector_delta<S: Storage>(
    tx: &mut WriteTx<'_, S>,
    table_name: &str,
    id: VectorId,
) -> Result<()> {
    let delta_name = delta_table_name(table_name);
    let mut table = tx.open_table_mut(&delta_name)?;
    let key = id.0.to_be_bytes();
    let val = [0x01u8]; // 0x01: Tombstone delete
    table.put(&key, &val)
}

/// Scans all active vector mutations and deletions from the delta buffer table.
pub fn scan_vector_delta<S: Storage>(
    rx: &ReadOnlyTx<S>,
    table_name: &str,
) -> Result<Vec<VectorDeltaItem>> {
    let delta_name = delta_table_name(table_name);
    let table = match rx.open_table(&delta_name) {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };

    let kvs = table.scan_range(&KeyRange::all())?;
    let mut items = Vec::with_capacity(kvs.len());

    for kv in kvs {
        if kv.key.len() != 8 {
            return Err(DbError::Corruption(
                "Invalid VectorId key length in delta".into(),
            ));
        }
        let id =
            VectorId(u64::from_be_bytes(kv.key[0..8].try_into().map_err(
                |_| DbError::Corruption("VectorId conversion failed".into()),
            )?));

        if kv.value.is_empty() {
            continue;
        }

        let is_tombstone = kv.value[0] == 0x01;
        if is_tombstone {
            items.push(VectorDeltaItem {
                vector_id: id,
                vector: None,
            });
        } else {
            if kv.value.len() < 3 {
                return Err(DbError::Corruption("Truncated delta vector payload".into()));
            }
            let dim = u16::from_le_bytes(
                kv.value[1..3]
                    .try_into()
                    .map_err(|_| DbError::Corruption("Dimension conversion failed".into()))?,
            ) as usize;
            if kv.value.len() != 3 + dim * 4 {
                return Err(DbError::Corruption("Delta vector length mismatch".into()));
            }
            let mut vec = Vec::with_capacity(dim);
            let mut offset = 3;
            for _ in 0..dim {
                let f = f32::from_le_bytes(
                    kv.value[offset..offset + 4]
                        .try_into()
                        .map_err(|_| DbError::Corruption("Float conversion failed".into()))?,
                );
                vec.push(f);
                offset += 4;
            }
            items.push(VectorDeltaItem {
                vector_id: id,
                vector: Some(vec),
            });
        }
    }

    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tej_db_pager::{MemStorage, PageCache};
    use tej_db_txn::TxManager;

    #[test]
    fn test_delta_insert_and_scan_roundtrip() {
        let storage = MemStorage::new();
        let cache = Arc::new(PageCache::new(storage, 10));
        let manager = TxManager::open(cache).unwrap();

        // 1. Insert vector into delta buffer
        {
            let mut tx = manager.begin_write().unwrap();
            insert_vector_delta(&mut tx, "items", VectorId(1), &[0.1, 0.2, 0.3]).unwrap();
            delete_vector_delta(&mut tx, "items", VectorId(2)).unwrap();
            tx.commit().unwrap();
        }

        // 2. Scan delta buffer
        {
            let rx = manager.begin_read().unwrap();
            let items = scan_vector_delta(&rx, "items").unwrap();
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].vector_id, VectorId(1));
            assert_eq!(items[0].vector, Some(vec![0.1, 0.2, 0.3]));
            assert_eq!(items[1].vector_id, VectorId(2));
            assert_eq!(items[1].vector, None);
        }
    }
}
