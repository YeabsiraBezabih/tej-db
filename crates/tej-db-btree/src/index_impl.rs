use crate::{BTreeIndex, TxPageSet};
use tej_db_core::{DbError, Index, KV, KeyRange, Page, PageId, Result, Storage};

impl<S: Storage> BTreeIndex<S> {
    /// Safe transaction-aware key removal honoring CoW and deferring page reclamation into `tx_pages`.
    pub fn remove_in_tx(&mut self, key: &[u8], tx_pages: &mut TxPageSet) -> Result<bool> {
        let mut path = self.find_leaf_path(key)?;
        let leaf_id = path
            .pop()
            .ok_or_else(|| DbError::Corruption("Empty path".into()))?;
        let old_leaf = self.cache.get_page(leaf_id)?;

        let search_res = old_leaf.leaf_search(key)?;
        let del_idx = match search_res {
            Ok(idx) => idx,
            Err(_) => return Ok(false),
        };

        if old_leaf.leaf_is_overflow(del_idx)? {
            let (total_len, old_first) = old_leaf.leaf_get_overflow_ref(del_idx)?;
            let num_pages = if total_len == 0 {
                1
            } else {
                total_len.div_ceil(tej_db_core::PAGE_SIZE as u64)
            };
            for p in 0..num_pages {
                tx_pages.retire(PageId(old_first.0 + p));
            }
        }

        let count = old_leaf.item_count();
        let new_leaf_id = self.cache.allocate_page()?;
        tx_pages.insert(new_leaf_id);
        let mut new_leaf = Page::new_leaf(new_leaf_id, old_leaf.aux_ptr());
        let new_count = count - 1;

        let mut free_end = tej_db_core::PAGE_SIZE;
        let mut dest_idx = 0;
        for i in 0..count {
            if i == del_idx {
                continue;
            }
            let (k, v, v_raw) = old_leaf.leaf_get_kv_raw(i)?;
            let cell_size = 4 + k.len() + v.len();
            free_end -= cell_size;
            let offset = free_end;

            #[allow(clippy::cast_possible_truncation)]
            new_leaf.data[offset..offset + 2].copy_from_slice(&(k.len() as u16).to_le_bytes());
            new_leaf.data[offset + 2..offset + 4].copy_from_slice(&v_raw.to_le_bytes());
            new_leaf.data[offset + 4..offset + 4 + k.len()].copy_from_slice(k);
            new_leaf.data[offset + 4 + k.len()..offset + 4 + k.len() + v.len()].copy_from_slice(v);

            let slot_pos = tej_db_core::page::SLOTS_START_OFFSET + dest_idx * 2;
            #[allow(clippy::cast_possible_truncation)]
            new_leaf.data[slot_pos..slot_pos + 2].copy_from_slice(&(offset as u16).to_le_bytes());
            dest_idx += 1;
        }

        #[allow(clippy::cast_possible_truncation)]
        new_leaf.data[18..20].copy_from_slice(&(new_count as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        new_leaf.data[20..22].copy_from_slice(&(free_end as u16).to_le_bytes());
        new_leaf.update_checksum();

        self.cache.write_page(new_leaf)?;
        tx_pages.retire(leaf_id);

        let new_root = self.propagate_up_in_tx(path, leaf_id, new_leaf_id, None, tx_pages)?;
        self.set_root_id(new_root);
        Ok(true)
    }
}

impl<S: Storage> Index for BTreeIndex<S> {
    /// Zero-copy point lookup. Single-lock traversal directly over in-page slices with zero 4KB copies and zero path allocations.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.with_value(key, |val| Ok(val.to_vec()))
    }

    /// Copy-On-Write insert with slotted pages and automatic free-list page recycling.
    fn insert(&mut self, key: &[u8], val: &[u8]) -> Result<()> {
        let mut tx_pages = TxPageSet::default();
        let _ = self.insert_in_tx(key, val, &mut tx_pages)?;
        for pid in tx_pages.retired {
            self.cache.free_page(pid);
        }
        Ok(())
    }

    fn remove(&mut self, key: &[u8]) -> Result<bool> {
        let mut tx_pages = TxPageSet::default();
        let res = self.remove_in_tx(key, &mut tx_pages)?;
        for pid in tx_pages.retired {
            self.cache.free_page(pid);
        }
        Ok(res)
    }

    /// Fast sequential cursor scan along leaf `aux_ptr` (`next_leaf`) links.
    fn scan_range(&self, range: &KeyRange) -> Result<Vec<KV>> {
        let mut results = Vec::new();
        self.scan_range_visitor(range, |k, v| {
            results.push(KV::new(k.to_vec(), v.to_vec()));
            Ok(true)
        })?;
        Ok(results)
    }
}
