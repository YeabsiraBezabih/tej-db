use crate::{BTreeIndex, TxPageSet};
use tej_db_core::{DbError, Page, PageId, Result, Storage};

impl<S: Storage> BTreeIndex<S> {
    pub fn insert_in_tx(
        &self,
        key: &[u8],
        val: &[u8],
        tx_pages: &mut TxPageSet,
    ) -> Result<(Vec<PageId>, Option<Vec<u8>>)> {
        let is_overflow = val.len() > tej_db_core::MAX_INLINE_VAL_SIZE;
        let (_first_overflow_pid, ref_bytes) = if is_overflow {
            let pid = self.write_overflow_chain(val, tx_pages)?;
            let mut rb = [0u8; 16];
            rb[0..8].copy_from_slice(&(val.len() as u64).to_le_bytes());
            rb[8..16].copy_from_slice(&pid.0.to_le_bytes());
            (Some(pid), Some(rb))
        } else {
            (None, None)
        };
        let v_raw = if is_overflow {
            16 | tej_db_core::OVERFLOW_FLAG
        } else {
            #[allow(clippy::cast_possible_truncation)]
            {
                val.len() as u16
            }
        };
        let v_bytes = match ref_bytes.as_ref() {
            Some(rb) => &rb[..],
            None => val,
        };
        self.insert_in_tx_prepared(key, v_bytes, v_raw, tx_pages)
    }

    pub fn insert_in_tx_prepared(
        &self,
        key: &[u8],
        v_bytes: &[u8],
        v_raw: u16,
        tx_pages: &mut TxPageSet,
    ) -> Result<(Vec<PageId>, Option<Vec<u8>>)> {
        let (mut path, bound) = self.find_leaf_path_with_bound(key)?;
        let leaf_id = path
            .pop()
            .ok_or_else(|| DbError::Corruption("Empty path".into()))?;

        let free_old_overflow = |p: &Page| -> Result<Option<(PageId, u64)>> {
            if let Ok(idx) = p.leaf_search(key)? {
                if p.leaf_is_overflow(idx).unwrap_or(false) {
                    if let Ok((total_len, old_first)) = p.leaf_get_overflow_ref(idx) {
                        return Ok(Some((old_first, total_len)));
                    }
                }
            }
            Ok(None)
        };

        if tx_pages.contains(&leaf_id) {
            let old_overflow = self.cache.with_page(leaf_id, free_old_overflow)?;
            if let Some((old_first, total_len)) = old_overflow {
                self.free_overflow_extent(old_first, total_len);
            }

            let inserted = self.cache.with_page_mut(leaf_id, |page| {
                page.leaf_insert_raw_inplace(key, v_bytes, v_raw)
            })?;

            if inserted {
                path.push(leaf_id);
                return Ok((path, bound));
            }

            let new_right_id = self.cache.allocate_page()?;
            tx_pages.insert(new_right_id);

            let old_leaf = self.cache.get_page(leaf_id)?;
            let count = old_leaf.item_count();
            let is_append = if count >= 2 {
                if let Ok(last_k) = old_leaf.leaf_get_key(count - 1) {
                    key > last_k
                } else {
                    false
                }
            } else {
                false
            };

            if is_append {
                let mut right = Page::new_leaf(new_right_id, old_leaf.aux_ptr());
                right.leaf_insert_raw_inplace(key, v_bytes, v_raw)?;
                let mid_key = key.to_vec();

                let mut left = old_leaf;
                left.set_aux_ptr(new_right_id);
                left.update_checksum();

                self.cache.write_page(left)?;
                self.cache.write_page(right)?;

                let new_root = self.propagate_up_in_tx(
                    path,
                    leaf_id,
                    leaf_id,
                    Some((mid_key, new_right_id)),
                    tx_pages,
                )?;
                self.set_root_id(new_root);
                return self.find_leaf_path_with_bound(key);
            }

            let split_mid = count / 2;
            let (mut left, mut right, mid_key) =
                old_leaf.leaf_split_into_at(leaf_id, new_right_id, split_mid)?;

            if key < mid_key.as_slice() {
                left.leaf_insert_raw_inplace(key, v_bytes, v_raw)?;
            } else {
                right.leaf_insert_raw_inplace(key, v_bytes, v_raw)?;
            }

            self.cache.write_page(left)?;
            self.cache.write_page(right)?;

            let new_root = self.propagate_up_in_tx(
                path,
                leaf_id,
                leaf_id,
                Some((mid_key, new_right_id)),
                tx_pages,
            )?;
            self.set_root_id(new_root);
            return self.find_leaf_path_with_bound(key);
        }

        let old_leaf = self.cache.get_page(leaf_id)?;
        if let Some((old_first, total_len)) = free_old_overflow(&old_leaf)? {
            self.free_overflow_extent(old_first, total_len);
        }

        if old_leaf.leaf_can_fit(key.len(), v_bytes.len()) {
            let new_leaf_id = self.cache.allocate_page()?;
            tx_pages.insert(new_leaf_id);

            let new_leaf =
                old_leaf.leaf_create_with_raw_insert(new_leaf_id, key, v_bytes, v_raw)?;
            self.cache.write_page(new_leaf)?;
            self.cache.free_page(leaf_id);

            let new_root = self.propagate_up_in_tx(path, leaf_id, new_leaf_id, None, tx_pages)?;
            self.set_root_id(new_root);
            return self.find_leaf_path_with_bound(key);
        }

        let count = old_leaf.item_count();
        let is_append = if count >= 2 {
            if let Ok(last_k) = old_leaf.leaf_get_key(count - 1) {
                key > last_k
            } else {
                false
            }
        } else {
            false
        };

        let new_left_id = self.cache.allocate_page()?;
        let new_right_id = self.cache.allocate_page()?;
        tx_pages.insert(new_left_id);
        tx_pages.insert(new_right_id);

        if is_append {
            let mut left = old_leaf.clone();
            left.set_page_id(new_left_id);
            left.set_aux_ptr(new_right_id);
            left.update_checksum();

            let mut right = Page::new_leaf(new_right_id, old_leaf.aux_ptr());
            right.leaf_insert_raw_inplace(key, v_bytes, v_raw)?;
            let mid_key = key.to_vec();

            self.cache.write_page(left)?;
            self.cache.write_page(right)?;
            self.cache.free_page(leaf_id);

            let new_root = self.propagate_up_in_tx(
                path,
                leaf_id,
                new_left_id,
                Some((mid_key, new_right_id)),
                tx_pages,
            )?;
            self.set_root_id(new_root);
            return self.find_leaf_path_with_bound(key);
        }

        let split_mid = count / 2;
        let (mut left, mut right, mid_key) =
            old_leaf.leaf_split_into_at(new_left_id, new_right_id, split_mid)?;

        if key < mid_key.as_slice() {
            left.leaf_insert_raw_inplace(key, v_bytes, v_raw)?;
        } else {
            right.leaf_insert_raw_inplace(key, v_bytes, v_raw)?;
        }

        self.cache.write_page(left)?;
        self.cache.write_page(right)?;
        self.cache.free_page(leaf_id);

        let new_root = self.propagate_up_in_tx(
            path,
            leaf_id,
            new_left_id,
            Some((mid_key, new_right_id)),
            tx_pages,
        )?;
        self.set_root_id(new_root);
        self.find_leaf_path_with_bound(key)
    }

    /// Propagate child split/replacement up the ancestor chain, reusing transaction pages in-place.
    pub(crate) fn propagate_up_in_tx(
        &self,
        mut path: Vec<PageId>,
        old_child: PageId,
        left_child: PageId,
        split_opt: Option<(Vec<u8>, PageId)>,
        tx_pages: &mut TxPageSet,
    ) -> Result<PageId> {
        let parent_id = if let Some(pid) = path.pop() {
            pid
        } else {
            if let Some((mid_key, right_child)) = split_opt {
                let new_root_id = self.cache.allocate_page()?;
                tx_pages.insert(new_root_id);
                let new_root = Page::new_internal(new_root_id, left_child);
                let new_root =
                    new_root.internal_create_with_insert(new_root_id, &mid_key, right_child)?;
                self.cache.write_page(new_root)?;
                return Ok(new_root_id);
            }
            return Ok(left_child);
        };

        if tx_pages.contains(&parent_id) {
            if old_child != left_child {
                self.cache.with_page_mut(parent_id, |page| {
                    page.internal_replace_child(old_child, left_child)?;
                    Ok(())
                })?;
            }

            if let Some((mid_key, right_child)) = split_opt {
                let inserted = self.cache.with_page_mut(parent_id, |page| {
                    page.internal_insert_inplace(&mid_key, right_child)
                })?;

                if inserted {
                    if let Some(&root) = path.first() {
                        return Ok(root);
                    }
                    return Ok(parent_id);
                }

                let p_right_id = self.cache.allocate_page()?;
                tx_pages.insert(p_right_id);

                let old_parent = self.cache.get_page(parent_id)?;
                let (mut p_left, mut p_right, p_mid_key) =
                    old_parent.internal_split_into(parent_id, p_right_id)?;

                if mid_key.as_slice() < p_mid_key.as_slice() {
                    p_left.internal_insert_inplace(&mid_key, right_child)?;
                } else {
                    p_right.internal_insert_inplace(&mid_key, right_child)?;
                }

                self.cache.write_page(p_left)?;
                self.cache.write_page(p_right)?;

                return self.propagate_up_in_tx(
                    path,
                    parent_id,
                    parent_id,
                    Some((p_mid_key, p_right_id)),
                    tx_pages,
                );
            }

            if let Some(&root) = path.first() {
                return Ok(root);
            }
            return Ok(parent_id);
        }

        let parent = self.cache.get_page(parent_id)?;
        let count = parent.item_count();

        let new_parent_id = self.cache.allocate_page()?;
        tx_pages.insert(new_parent_id);
        let mut new_parent = Page::new_internal(
            new_parent_id,
            if parent.aux_ptr() == old_child {
                left_child
            } else {
                parent.aux_ptr()
            },
        );

        let mut free_end = tej_db_core::PAGE_SIZE;
        for i in 0..count {
            let k = parent.internal_get_key(i)?;
            let c = parent.internal_get_child(i)?;
            let new_c = if c == old_child { left_child } else { c };

            let cell_size = 10 + k.len();
            free_end -= cell_size;
            let offset = free_end;

            new_parent.data[offset..offset + 8].copy_from_slice(&new_c.0.to_le_bytes());
            #[allow(clippy::cast_possible_truncation)]
            new_parent.data[offset + 8..offset + 10]
                .copy_from_slice(&(k.len() as u16).to_le_bytes());
            new_parent.data[offset + 10..offset + 10 + k.len()].copy_from_slice(k);

            let slot_pos = tej_db_core::page::SLOTS_START_OFFSET + i * 2;
            #[allow(clippy::cast_possible_truncation)]
            new_parent.data[slot_pos..slot_pos + 2].copy_from_slice(&(offset as u16).to_le_bytes());
        }
        #[allow(clippy::cast_possible_truncation)]
        new_parent.data[18..20].copy_from_slice(&(count as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        new_parent.data[20..22].copy_from_slice(&(free_end as u16).to_le_bytes());
        new_parent.update_checksum();

        if let Some((mid_key, right_child)) = split_opt {
            if new_parent.internal_can_fit(mid_key.len()) {
                let updated_parent =
                    new_parent.internal_create_with_insert(new_parent_id, &mid_key, right_child)?;
                self.cache.write_page(updated_parent)?;
                self.cache.free_page(parent_id);
                let mut single_tx = TxPageSet::default();
                return self.propagate_up_in_tx(
                    path,
                    parent_id,
                    new_parent_id,
                    None,
                    &mut single_tx,
                );
            }

            let p_left_id = self.cache.allocate_page()?;
            let p_right_id = self.cache.allocate_page()?;
            tx_pages.insert(p_left_id);
            tx_pages.insert(p_right_id);
            let (mut p_left, mut p_right, p_mid_key) =
                new_parent.internal_split_into(p_left_id, p_right_id)?;

            if mid_key.as_slice() < p_mid_key.as_slice() {
                p_left = p_left.internal_create_with_insert(p_left_id, &mid_key, right_child)?;
            } else {
                p_right = p_right.internal_create_with_insert(p_right_id, &mid_key, right_child)?;
            }

            self.cache.write_page(p_left)?;
            self.cache.write_page(p_right)?;
            self.cache.free_page(parent_id);
            self.cache.free_page(new_parent_id);

            return self.propagate_up_in_tx(
                path,
                parent_id,
                p_left_id,
                Some((p_mid_key, p_right_id)),
                tx_pages,
            );
        }

        self.cache.write_page(new_parent)?;
        self.cache.free_page(parent_id);
        self.propagate_up_in_tx(path, parent_id, new_parent_id, None, tx_pages)
    }

    /// Propagate a child page replacement and/or split up the parent ancestor chain under COW.
    pub(crate) fn propagate_up(
        &self,
        path: Vec<PageId>,
        old_child: PageId,
        left_child: PageId,
        split_opt: Option<(Vec<u8>, PageId)>,
    ) -> Result<PageId> {
        let mut tx_pages = TxPageSet::default();
        self.propagate_up_in_tx(path, old_child, left_child, split_opt, &mut tx_pages)
    }
}
