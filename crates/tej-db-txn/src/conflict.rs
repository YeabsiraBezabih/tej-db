use smallvec::SmallVec;
use std::collections::VecDeque;
use std::sync::Arc;
use tej_db_btree::RawBatchItem;
use tej_db_core::{DbError, Result, TxId};

pub type ApprovedRecord = (u64, SmallVec<[String; 2]>, Arc<[tej_db_btree::InlineKey]>);
pub type ApprovedSlotWrites = (Vec<RawBatchItem>, tej_db_btree::TxPageSet);

/// In-memory commit history record used for optimistic concurrency control (OCC) conflict detection.
#[derive(Debug, Clone)]
pub struct CommittedTxRecord {
    pub tx_id: TxId,
    pub tables: SmallVec<[String; 2]>,
    pub keys: Arc<[tej_db_btree::InlineKey]>,
}

#[inline]
#[must_use]
pub fn tables_overlap(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    a.iter().any(|ta| b.iter().any(|tb| ta == tb))
}

#[inline]
#[must_use]
pub fn raw_and_keys_intersect(
    raw: &[tej_db_btree::RawBatchItem],
    keys: &[tej_db_btree::InlineKey],
) -> bool {
    if raw.is_empty() || keys.is_empty() {
        return false;
    }
    // Fast O(1) boundary check: if slices do not overlap in range, they cannot intersect.
    if let (Some(r_last), Some(k_first), Some(k_last), Some(r_first)) =
        (raw.last(), keys.first(), keys.last(), raw.first())
    {
        if r_last.0.as_slice() < k_first.as_slice() || k_last.as_slice() < r_first.0.as_slice() {
            return false;
        }
    }
    let mut i = 0;
    let mut j = 0;
    while i < raw.len() && j < keys.len() {
        match raw[i].0.as_slice().cmp(keys[j].as_slice()) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

/// Validates whether a transaction with snapshot `snapshot_tx_id` conflicts with committed transactions.
pub fn validate_tx_conflict(
    history: &VecDeque<CommittedTxRecord>,
    snapshot_tx_id: TxId,
    modified_tables: &[String],
    pending: &[tej_db_btree::RawBatchItem],
) -> Result<()> {
    if history.len() >= 1024 {
        if let Some(oldest) = history.front() {
            if snapshot_tx_id.0 + 1 < oldest.tx_id.0 {
                return Err(DbError::WriteConflict {
                    key: Vec::new(),
                    committed_tx: oldest.tx_id.0,
                });
            }
        }
    }

    for record in history.iter().rev() {
        if record.tx_id <= snapshot_tx_id {
            break;
        }

        let table_conflict = if modified_tables.is_empty() || record.tables.is_empty() {
            true
        } else {
            modified_tables
                .iter()
                .any(|t| record.tables.iter().any(|rt| rt == t))
        };

        if table_conflict {
            let committed_keys = &record.keys[..];
            if let (Some(p_last), Some(c_first), Some(c_last), Some(p_first)) = (
                pending.last(),
                committed_keys.first(),
                committed_keys.last(),
                pending.first(),
            ) {
                if p_last.0.as_slice() < c_first.as_slice()
                    || c_last.as_slice() < p_first.0.as_slice()
                {
                    continue;
                }
            }

            let mut p_idx = 0;
            let mut c_idx = 0;
            while p_idx < pending.len() && c_idx < committed_keys.len() {
                match pending[p_idx]
                    .0
                    .as_slice()
                    .cmp(committed_keys[c_idx].as_slice())
                {
                    std::cmp::Ordering::Less => p_idx += 1,
                    std::cmp::Ordering::Greater => c_idx += 1,
                    std::cmp::Ordering::Equal => {
                        return Err(DbError::WriteConflict {
                            key: pending[p_idx].0.to_vec(),
                            committed_tx: record.tx_id.0,
                        });
                    }
                }
            }
        }
    }

    Ok(())
}
