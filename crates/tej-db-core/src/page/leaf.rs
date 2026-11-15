use crate::error::{DbError, Result};
use crate::page::{
    MAX_INLINE_VAL_SIZE, OVERFLOW_FLAG, PAGE_SIZE, Page, PageType, SLOTS_START_OFFSET,
};
use crate::types::PageId;
use alloc::vec::Vec;

impl Page {
    #[must_use]
    pub fn new_leaf(page_id: PageId, next_leaf: PageId) -> Self {
        let mut page = Self::new(page_id, PageType::BTreeLeaf);
        page.set_aux_ptr(next_leaf);
        page.update_checksum();
        page
    }

    // --- LEAF NODE ZERO-COPY OPERATIONS ---

    #[must_use]
    pub fn leaf_can_fit(&self, key_len: usize, val_len: usize) -> bool {
        let stored_v_len = if val_len > MAX_INLINE_VAL_SIZE {
            16
        } else {
            val_len
        };
        let cell_size = 4 + key_len + stored_v_len;
        let needed = cell_size + 2; // cell data + 2-byte slot
        self.free_space_remaining() >= needed
    }

    pub fn leaf_get_key(&self, index: usize) -> Result<&[u8]> {
        let offset = self.slot_offset(index)?;
        let k_len = u16::from_le_bytes([self.data[offset], self.data[offset + 1]]) as usize;
        let start = offset + 4;
        let end = start + k_len;
        if end > PAGE_SIZE {
            return Err(DbError::Corruption("Leaf cell key exceeds page".into()));
        }
        Ok(&self.data[start..end])
    }

    pub fn leaf_get_val(&self, index: usize) -> Result<&[u8]> {
        let offset = self.slot_offset(index)?;
        let k_len = u16::from_le_bytes([self.data[offset], self.data[offset + 1]]) as usize;
        let v_raw = u16::from_le_bytes([self.data[offset + 2], self.data[offset + 3]]);
        let v_len = (v_raw & !OVERFLOW_FLAG) as usize;
        let start = offset + 4 + k_len;
        let end = start + v_len;
        if end > PAGE_SIZE {
            return Err(DbError::Corruption("Leaf cell value exceeds page".into()));
        }
        Ok(&self.data[start..end])
    }

    pub fn leaf_is_overflow(&self, index: usize) -> Result<bool> {
        let offset = self.slot_offset(index)?;
        let v_raw = u16::from_le_bytes([self.data[offset + 2], self.data[offset + 3]]);
        Ok((v_raw & OVERFLOW_FLAG) != 0)
    }

    pub fn leaf_get_overflow_ref(&self, index: usize) -> Result<(u64, PageId)> {
        let offset = self.slot_offset(index)?;
        let k_len = u16::from_le_bytes([self.data[offset], self.data[offset + 1]]) as usize;
        let v_start = offset + 4 + k_len;
        if v_start + 16 > PAGE_SIZE {
            return Err(DbError::Corruption("Leaf overflow ref exceeds page".into()));
        }
        let total_len = u64::from_le_bytes(
            self.data[v_start..v_start + 8]
                .try_into()
                .map_err(|_| DbError::Corruption("Invalid overflow total_len".into()))?,
        );
        let pid = u64::from_le_bytes(
            self.data[v_start + 8..v_start + 16]
                .try_into()
                .map_err(|_| DbError::Corruption("Invalid overflow page_id".into()))?,
        );
        Ok((total_len, PageId(pid)))
    }

    pub fn leaf_get_kv_raw(&self, index: usize) -> Result<(&[u8], &[u8], u16)> {
        let offset = self.slot_offset(index)?;
        let k_len = u16::from_le_bytes([self.data[offset], self.data[offset + 1]]) as usize;
        let v_raw = u16::from_le_bytes([self.data[offset + 2], self.data[offset + 3]]);
        let v_len = (v_raw & !OVERFLOW_FLAG) as usize;
        let k_start = offset + 4;
        let k_end = k_start + k_len;
        let v_end = k_end + v_len;
        if v_end > PAGE_SIZE {
            return Err(DbError::Corruption("Leaf cell exceeds page".into()));
        }
        Ok((&self.data[k_start..k_end], &self.data[k_end..v_end], v_raw))
    }

    /// Fast sequential in-page leaf scan over slots from `start_idx`.
    #[inline]
    pub fn leaf_scan_range<F>(&self, start_idx: usize, mut visitor: F) -> Result<bool>
    where
        F: FnMut(usize, &[u8], &[u8], u16) -> Result<bool>,
    {
        let count = self.item_count();
        if start_idx >= count {
            return Ok(true);
        }
        let mut pos = SLOTS_START_OFFSET + start_idx * 2;
        for idx in start_idx..count {
            let offset = u16::from_le_bytes([self.data[pos], self.data[pos + 1]]) as usize;
            pos += 2;
            let k_len = u16::from_le_bytes([self.data[offset], self.data[offset + 1]]) as usize;
            let v_raw = u16::from_le_bytes([self.data[offset + 2], self.data[offset + 3]]);
            let v_len = (v_raw & !OVERFLOW_FLAG) as usize;
            let k_start = offset + 4;
            let k_end = k_start + k_len;
            let v_end = k_end + v_len;
            if v_end > PAGE_SIZE {
                return Err(DbError::Corruption("Leaf cell exceeds page".into()));
            }
            let key = &self.data[k_start..k_end];
            let val = &self.data[k_end..v_end];
            if !visitor(idx, key, val, v_raw)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn leaf_get_kv(&self, index: usize) -> Result<(&[u8], &[u8])> {
        let (k, v, _) = self.leaf_get_kv_raw(index)?;
        Ok((k, v))
    }

    pub fn leaf_search(&self, target_key: &[u8]) -> Result<core::result::Result<usize, usize>> {
        let count = self.item_count();
        let mut low = 0;
        let mut high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            let key = self.leaf_get_key(mid)?;
            match key.cmp(target_key) {
                core::cmp::Ordering::Less => low = mid + 1,
                core::cmp::Ordering::Greater => high = mid,
                core::cmp::Ordering::Equal => return Ok(Ok(mid)),
            }
        }
        Ok(Err(low))
    }

    /// Construct a new leaf page with `new_page_id` by inserting or replacing `(key, val)`.
    pub fn leaf_create_with_raw_insert(
        &self,
        new_page_id: PageId,
        key: &[u8],
        val_bytes: &[u8],
        v_raw: u16,
    ) -> Result<Page> {
        let mut new_page = Page::new_leaf(new_page_id, self.aux_ptr());
        let count = self.item_count();
        let search_res = self.leaf_search(key)?;

        let (replace_idx, insert_idx) = match search_res {
            Ok(idx) => (Some(idx), idx),
            Err(idx) => (None, idx),
        };

        let new_count = if replace_idx.is_some() {
            count
        } else {
            count + 1
        };
        let mut free_end = PAGE_SIZE;

        // Build sorted items into new_page
        for i in 0..new_count {
            let (k, v, cur_raw) = if i == insert_idx {
                (key, val_bytes, v_raw)
            } else {
                let src_idx = if replace_idx.is_none() && i > insert_idx {
                    i - 1
                } else {
                    i
                };
                self.leaf_get_kv_raw(src_idx)?
            };

            let cell_size = 4 + k.len() + v.len();
            free_end -= cell_size;
            let cell_offset = free_end;

            // Write cell header
            #[allow(clippy::cast_possible_truncation)]
            new_page.data[cell_offset..cell_offset + 2]
                .copy_from_slice(&(k.len() as u16).to_le_bytes());
            new_page.data[cell_offset + 2..cell_offset + 4].copy_from_slice(&cur_raw.to_le_bytes());
            // Write key & value
            new_page.data[cell_offset + 4..cell_offset + 4 + k.len()].copy_from_slice(k);
            new_page.data[cell_offset + 4 + k.len()..cell_offset + 4 + k.len() + v.len()]
                .copy_from_slice(v);

            // Write slot
            let slot_pos = SLOTS_START_OFFSET + i * 2;
            #[allow(clippy::cast_possible_truncation)]
            new_page.data[slot_pos..slot_pos + 2]
                .copy_from_slice(&(cell_offset as u16).to_le_bytes());
        }

        // Update header
        #[allow(clippy::cast_possible_truncation)]
        new_page.data[18..20].copy_from_slice(&(new_count as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        new_page.data[20..22].copy_from_slice(&(free_end as u16).to_le_bytes());
        new_page.update_checksum();
        Ok(new_page)
    }

    /// Construct a new leaf page with `new_page_id` by inserting or replacing `(key, val)`.
    pub fn leaf_create_with_insert(
        &self,
        new_page_id: PageId,
        key: &[u8],
        val: &[u8],
    ) -> Result<Page> {
        #[allow(clippy::cast_possible_truncation)]
        self.leaf_create_with_raw_insert(new_page_id, key, val, val.len() as u16)
    }

    /// Construct a new leaf page with an overflow reference cell.
    pub fn leaf_create_with_overflow_insert(
        &self,
        new_page_id: PageId,
        key: &[u8],
        total_len: u64,
        first_page: PageId,
    ) -> Result<Page> {
        let mut ref_bytes = [0u8; 16];
        ref_bytes[0..8].copy_from_slice(&total_len.to_le_bytes());
        ref_bytes[8..16].copy_from_slice(&first_page.0.to_le_bytes());
        self.leaf_create_with_raw_insert(new_page_id, key, &ref_bytes, 16 | OVERFLOW_FLAG)
    }

    /// Insert or replace `(key, val)` in-place in this leaf page with raw `v_raw`.
    pub fn leaf_insert_raw_inplace(&mut self, key: &[u8], val: &[u8], v_raw: u16) -> Result<bool> {
        let count = self.item_count();
        let search_res = self.leaf_search(key)?;
        let cell_size = 4 + key.len() + val.len();

        match search_res {
            Ok(idx) => {
                let (_, old_v, _) = self.leaf_get_kv_raw(idx)?;
                if old_v.len() == val.len() {
                    let slot_pos = SLOTS_START_OFFSET + idx * 2;
                    let cell_offset =
                        u16::from_le_bytes([self.data[slot_pos], self.data[slot_pos + 1]]) as usize;
                    self.data[cell_offset + 2..cell_offset + 4]
                        .copy_from_slice(&v_raw.to_le_bytes());
                    let k_len =
                        u16::from_le_bytes([self.data[cell_offset], self.data[cell_offset + 1]])
                            as usize;
                    let val_start = cell_offset + 4 + k_len;
                    self.data[val_start..val_start + val.len()].copy_from_slice(val);
                    self.update_checksum();
                    return Ok(true);
                }

                let free_space = self.free_space_offset();
                let slots_end = SLOTS_START_OFFSET + count * 2;
                if free_space < slots_end + cell_size {
                    return Ok(false);
                }

                let new_offset = free_space - cell_size;
                #[allow(clippy::cast_possible_truncation)]
                self.data[new_offset..new_offset + 2]
                    .copy_from_slice(&(key.len() as u16).to_le_bytes());
                self.data[new_offset + 2..new_offset + 4].copy_from_slice(&v_raw.to_le_bytes());
                self.data[new_offset + 4..new_offset + 4 + key.len()].copy_from_slice(key);
                self.data[new_offset + 4 + key.len()..new_offset + 4 + key.len() + val.len()]
                    .copy_from_slice(val);

                let slot_pos = SLOTS_START_OFFSET + idx * 2;
                #[allow(clippy::cast_possible_truncation)]
                self.data[slot_pos..slot_pos + 2]
                    .copy_from_slice(&(new_offset as u16).to_le_bytes());
                #[allow(clippy::cast_possible_truncation)]
                self.data[20..22].copy_from_slice(&(new_offset as u16).to_le_bytes());
                self.update_checksum();
                Ok(true)
            }
            Err(idx) => {
                let free_space = self.free_space_offset();
                let slots_end = SLOTS_START_OFFSET + (count + 1) * 2;
                if free_space < slots_end + cell_size {
                    return Ok(false);
                }

                let new_offset = free_space - cell_size;
                #[allow(clippy::cast_possible_truncation)]
                self.data[new_offset..new_offset + 2]
                    .copy_from_slice(&(key.len() as u16).to_le_bytes());
                self.data[new_offset + 2..new_offset + 4].copy_from_slice(&v_raw.to_le_bytes());
                self.data[new_offset + 4..new_offset + 4 + key.len()].copy_from_slice(key);
                self.data[new_offset + 4 + key.len()..new_offset + 4 + key.len() + val.len()]
                    .copy_from_slice(val);

                let slot_pos = SLOTS_START_OFFSET + idx * 2;
                let cur_slots_end = SLOTS_START_OFFSET + count * 2;
                if idx < count {
                    self.data.copy_within(slot_pos..cur_slots_end, slot_pos + 2);
                }

                #[allow(clippy::cast_possible_truncation)]
                self.data[slot_pos..slot_pos + 2]
                    .copy_from_slice(&(new_offset as u16).to_le_bytes());

                let new_count = count + 1;
                #[allow(clippy::cast_possible_truncation)]
                self.data[18..20].copy_from_slice(&(new_count as u16).to_le_bytes());
                #[allow(clippy::cast_possible_truncation)]
                self.data[20..22].copy_from_slice(&(new_offset as u16).to_le_bytes());
                self.update_checksum();
                Ok(true)
            }
        }
    }

    /// Insert or replace `(key, val)` in-place in this leaf page.
    /// Returns `Ok(true)` if inserted, or `Ok(false)` if there was not enough space.
    pub fn leaf_insert_inplace(&mut self, key: &[u8], val: &[u8]) -> Result<bool> {
        #[allow(clippy::cast_possible_truncation)]
        self.leaf_insert_raw_inplace(key, val, val.len() as u16)
    }

    /// Insert or replace an overflow reference in-place in this leaf page.
    pub fn leaf_insert_overflow_inplace(
        &mut self,
        key: &[u8],
        total_len: u64,
        first_page: PageId,
    ) -> Result<bool> {
        let mut ref_bytes = [0u8; 16];
        ref_bytes[0..8].copy_from_slice(&total_len.to_le_bytes());
        ref_bytes[8..16].copy_from_slice(&first_page.0.to_le_bytes());
        self.leaf_insert_raw_inplace(key, &ref_bytes, 16 | OVERFLOW_FLAG)
    }

    /// Fast-path append for keys strictly greater than all existing keys in the page with raw `v_raw`.
    pub fn leaf_append_raw_inplace(&mut self, key: &[u8], val: &[u8], v_raw: u16) -> Result<bool> {
        let count = self.item_count();
        if count > 0 {
            let last_key = self.leaf_get_key(count - 1)?;
            if key <= last_key {
                return Ok(false);
            }
        }
        let cell_size = 4 + key.len() + val.len();
        let free_space = self.free_space_offset();
        let slots_end = SLOTS_START_OFFSET + (count + 1) * 2;
        if free_space < slots_end + cell_size {
            return Ok(false);
        }

        let new_offset = free_space - cell_size;
        #[allow(clippy::cast_possible_truncation)]
        self.data[new_offset..new_offset + 2].copy_from_slice(&(key.len() as u16).to_le_bytes());
        self.data[new_offset + 2..new_offset + 4].copy_from_slice(&v_raw.to_le_bytes());
        self.data[new_offset + 4..new_offset + 4 + key.len()].copy_from_slice(key);
        self.data[new_offset + 4 + key.len()..new_offset + 4 + key.len() + val.len()]
            .copy_from_slice(val);

        let slot_pos = SLOTS_START_OFFSET + count * 2;
        #[allow(clippy::cast_possible_truncation)]
        self.data[slot_pos..slot_pos + 2].copy_from_slice(&(new_offset as u16).to_le_bytes());

        let new_count = count + 1;
        #[allow(clippy::cast_possible_truncation)]
        self.data[18..20].copy_from_slice(&(new_count as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        self.data[20..22].copy_from_slice(&(new_offset as u16).to_le_bytes());
        Ok(true)
    }

    /// Fast-path append for keys strictly greater than all existing keys in the page.
    /// Operates in O(1) time (~2ns) without binary searching or slot shifting.
    pub fn leaf_append_inplace(&mut self, key: &[u8], val: &[u8]) -> Result<bool> {
        #[allow(clippy::cast_possible_truncation)]
        self.leaf_append_raw_inplace(key, val, val.len() as u16)
    }

    /// Fast-path append for overflow reference keys strictly greater than all existing keys.
    pub fn leaf_append_overflow_inplace(
        &mut self,
        key: &[u8],
        total_len: u64,
        first_page: PageId,
    ) -> Result<bool> {
        let mut ref_bytes = [0u8; 16];
        ref_bytes[0..8].copy_from_slice(&total_len.to_le_bytes());
        ref_bytes[8..16].copy_from_slice(&first_page.0.to_le_bytes());
        self.leaf_append_raw_inplace(key, &ref_bytes, 16 | OVERFLOW_FLAG)
    }

    /// Split leaf into two halves: `left` and `right`.
    /// Returns `(left_page, right_page, middle_key)`.
    /// Split leaf into two halves: `left` and `right` using standard 50/50 midpoint.
    pub fn leaf_split_into(
        &self,
        left_id: PageId,
        right_id: PageId,
    ) -> Result<(Page, Page, Vec<u8>)> {
        self.leaf_split_into_at(left_id, right_id, self.item_count() / 2)
    }

    /// Split leaf into two halves at arbitrary split index `mid`: `0..mid` in left, `mid..count` in right.
    /// Used for right-edge append optimization where `mid = count - 1`, achieving ~95-100% fill factor on sequential appends.
    pub fn leaf_split_into_at(
        &self,
        left_id: PageId,
        right_id: PageId,
        mid: usize,
    ) -> Result<(Page, Page, Vec<u8>)> {
        let count = self.item_count();
        if count < 2 || mid == 0 || mid >= count {
            return Err(DbError::Internal(
                "Cannot split leaf with invalid split boundary".into(),
            ));
        }
        let mid_key = self.leaf_get_key(mid)?.to_vec();

        let mut left = Page::new_leaf(left_id, right_id);
        let mut right = Page::new_leaf(right_id, self.aux_ptr());

        // Left items: 0..mid
        let mut left_free_end = PAGE_SIZE;
        for i in 0..mid {
            let (k, v, v_raw) = self.leaf_get_kv_raw(i)?;
            let cell_size = 4 + k.len() + v.len();
            left_free_end -= cell_size;
            let offset = left_free_end;

            #[allow(clippy::cast_possible_truncation)]
            left.data[offset..offset + 2].copy_from_slice(&(k.len() as u16).to_le_bytes());
            left.data[offset + 2..offset + 4].copy_from_slice(&v_raw.to_le_bytes());
            left.data[offset + 4..offset + 4 + k.len()].copy_from_slice(k);
            left.data[offset + 4 + k.len()..offset + 4 + k.len() + v.len()].copy_from_slice(v);

            let slot_pos = SLOTS_START_OFFSET + i * 2;
            #[allow(clippy::cast_possible_truncation)]
            left.data[slot_pos..slot_pos + 2].copy_from_slice(&(offset as u16).to_le_bytes());
        }
        #[allow(clippy::cast_possible_truncation)]
        left.data[18..20].copy_from_slice(&(mid as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        left.data[20..22].copy_from_slice(&(left_free_end as u16).to_le_bytes());
        left.update_checksum();

        // Right items: mid..count
        let right_count = count - mid;
        let mut right_free_end = PAGE_SIZE;
        for i in 0..right_count {
            let (k, v, v_raw) = self.leaf_get_kv_raw(mid + i)?;
            let cell_size = 4 + k.len() + v.len();
            right_free_end -= cell_size;
            let offset = right_free_end;

            #[allow(clippy::cast_possible_truncation)]
            right.data[offset..offset + 2].copy_from_slice(&(k.len() as u16).to_le_bytes());
            right.data[offset + 2..offset + 4].copy_from_slice(&v_raw.to_le_bytes());
            right.data[offset + 4..offset + 4 + k.len()].copy_from_slice(k);
            right.data[offset + 4 + k.len()..offset + 4 + k.len() + v.len()].copy_from_slice(v);

            let slot_pos = SLOTS_START_OFFSET + i * 2;
            #[allow(clippy::cast_possible_truncation)]
            right.data[slot_pos..slot_pos + 2].copy_from_slice(&(offset as u16).to_le_bytes());
        }
        #[allow(clippy::cast_possible_truncation)]
        right.data[18..20].copy_from_slice(&(right_count as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        right.data[20..22].copy_from_slice(&(right_free_end as u16).to_le_bytes());
        right.update_checksum();

        Ok((left, right, mid_key))
    }
}
