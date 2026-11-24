use crate::{BTreeIndex, InlineKey, InlineVal, RawBatchItem, TxPageSet};
use tej_db_core::{PageId, Result, Storage};

impl<S: Storage> BTreeIndex<S> {
    pub fn insert_sorted_batch_in_tx(
        &mut self,
        batch: &[(Vec<u8>, Option<Vec<u8>>)],
        tx_pages: &mut TxPageSet,
    ) -> Result<()> {
        let converted: Vec<RawBatchItem> = batch
            .iter()
            .map(|(k, v)| {
                (
                    InlineKey::from_slice(k),
                    v.as_ref().map(|val| InlineVal::from_slice(val)),
                    false,
                )
            })
            .collect();
        self.insert_sorted_batch_raw_in_tx(&converted, tx_pages)
    }

    /// High-throughput cursor-accelerated sorted batch insertion with leaf pinning and pre-streamed overflow references.
    pub fn insert_sorted_batch_raw_in_tx(
        &mut self,
        batch: &[RawBatchItem],
        tx_pages: &mut TxPageSet,
    ) -> Result<()> {
        let mut cursor_path: Option<Vec<PageId>> = None;
        let mut cursor_upper_bound: Option<InlineKey> = None;
        let mut last_key: Option<InlineKey> = None;
        let mut idx = 0;

        while idx < batch.len() {
            let (key, val_opt, is_overflow_ref) = &batch[idx];
            let Some(val) = val_opt else {
                cursor_path = None;
                cursor_upper_bound = None;
                last_key = None;
                let _ = self.remove_in_tx(key, tx_pages)?;
                idx += 1;
                continue;
            };

            let mut can_append = false;
            let mut can_insert_in_leaf = false;
            if cursor_path.is_some() {
                if let Some(ref lk) = last_key {
                    if key.as_slice() > lk.as_slice() {
                        match cursor_upper_bound {
                            None => can_append = true,
                            Some(ref ub) => {
                                if key.as_slice() < ub.as_slice() {
                                    can_append = true;
                                }
                            }
                        }
                    } else if key.as_slice() == lk.as_slice() {
                        can_insert_in_leaf = true;
                    }
                }
            }

            let (ref_bytes, v_raw) = if *is_overflow_ref {
                (None, 16 | tej_db_core::OVERFLOW_FLAG)
            } else if val.len() > tej_db_core::MAX_INLINE_VAL_SIZE {
                let pid = self.write_overflow_chain(val, tx_pages)?;
                let mut rb = [0u8; 16];
                rb[0..8].copy_from_slice(&(val.len() as u64).to_le_bytes());
                rb[8..16].copy_from_slice(&pid.0.to_le_bytes());
                (Some(rb), 16 | tej_db_core::OVERFLOW_FLAG)
            } else {
                #[allow(clippy::cast_possible_truncation)]
                (None, val.len() as u16)
            };
            let v_bytes = match ref_bytes.as_ref() {
                Some(rb) => &rb[..],
                None => val.as_slice(),
            };

            let active_leaf_opt = cursor_path.as_ref().and_then(|p| p.last().copied());
            if let Some(leaf_id) = active_leaf_opt {
                if can_append && tx_pages.contains(&leaf_id) {
                    let mut chunk_appended = 0;
                    self.cache.with_page_mut(leaf_id, |p| {
                        while idx + chunk_appended < batch.len() {
                            let (k, v_opt, is_ov) = &batch[idx + chunk_appended];
                            let Some(v) = v_opt else {
                                break;
                            };
                            if let Some(ref lk) = last_key {
                                if k.as_slice() <= lk.as_slice() {
                                    break;
                                }
                            }
                            if let Some(ref ub) = cursor_upper_bound {
                                if k.as_slice() >= ub.as_slice() {
                                    break;
                                }
                            }
                            if *is_ov || v.len() > tej_db_core::MAX_INLINE_VAL_SIZE {
                                break;
                            }
                            #[allow(clippy::cast_possible_truncation)]
                            let v_len = v.len() as u16;
                            if !p.leaf_append_raw_inplace(k.as_slice(), v.as_slice(), v_len)? {
                                break;
                            }
                            if let Some(ref mut lk) = last_key {
                                lk.clear();
                                lk.extend_from_slice(k.as_slice());
                            } else {
                                last_key = Some(InlineKey::from_slice(k.as_slice()));
                            }
                            chunk_appended += 1;
                        }
                        Ok(())
                    })?;
                    if chunk_appended > 0 {
                        idx += chunk_appended;
                        continue;
                    }
                } else if can_insert_in_leaf && tx_pages.contains(&leaf_id) {
                    let inserted = self.cache.with_page_mut(leaf_id, |p| {
                        p.leaf_insert_raw_inplace(key, v_bytes, v_raw)
                    })?;
                    if inserted {
                        if let Some(ref mut lk) = last_key {
                            lk.clear();
                            lk.extend_from_slice(key);
                        } else {
                            last_key = Some(InlineKey::from_slice(key.as_slice()));
                        }
                        idx += 1;
                        continue;
                    }
                }
            }

            let (path, bound) = self.insert_in_tx_prepared(key, v_bytes, v_raw, tx_pages)?;
            cursor_path = Some(path);
            cursor_upper_bound = bound.map(|b| InlineKey::from_slice(&b));
            if let Some(ref mut lk) = last_key {
                lk.clear();
                lk.extend_from_slice(key);
            } else {
                last_key = Some(InlineKey::from_slice(key.as_slice()));
            }
            idx += 1;
        }
        Ok(())
    }
}
