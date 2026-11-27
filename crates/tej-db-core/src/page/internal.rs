use crate::error::{DbError, Result};
use crate::page::{PAGE_SIZE, Page, PageType, SLOTS_START_OFFSET};
use crate::types::PageId;
use alloc::vec::Vec;

impl Page {
    #[must_use]
    pub fn new_internal(page_id: PageId, first_child: PageId) -> Self {
        let mut page = Self::new(page_id, PageType::BTreeInternal);
        page.set_aux_ptr(first_child);
        page.update_checksum();
        page
    }

    #[must_use]
    pub fn internal_can_fit(&self, key_len: usize) -> bool {
        let cell_size = 10 + key_len;
        let needed = cell_size + 2;
        self.free_space_remaining() >= needed
    }

    pub fn internal_get_key(&self, index: usize) -> Result<&[u8]> {
        let offset = self.slot_offset(index)?;
        if offset + 10 > PAGE_SIZE {
            return Err(DbError::Corruption(
                "Internal cell header exceeds page".into(),
            ));
        }
        let k_len = u16::from_le_bytes([self.data[offset + 8], self.data[offset + 9]]) as usize;
        let start = offset + 10;
        let end = start + k_len;
        if end > PAGE_SIZE {
            return Err(DbError::Corruption("Internal cell key exceeds page".into()));
        }
        Ok(&self.data[start..end])
    }

    pub fn internal_get_child(&self, index: usize) -> Result<PageId> {
        let offset = self.slot_offset(index)?;
        if offset + 8 > PAGE_SIZE {
            return Err(DbError::Corruption(
                "Internal cell child offset exceeds page".into(),
            ));
        }
        let child_val = u64::from_le_bytes(
            self.data[offset..offset + 8]
                .try_into()
                .map_err(|_| DbError::Corruption("Invalid child id bytes".into()))?,
        );
        Ok(PageId(child_val))
    }

    #[inline]
    #[must_use]
    pub fn internal_child_count(&self) -> usize {
        self.item_count() + 1
    }

    pub fn internal_child_at(&self, idx: usize) -> Result<PageId> {
        if idx == 0 {
            Ok(self.aux_ptr())
        } else {
            self.internal_get_child(idx - 1)
        }
    }

    pub fn internal_search_index(&self, target_key: &[u8]) -> Result<(usize, PageId)> {
        let count = self.item_count();
        if count == 0 {
            return Ok((0, self.aux_ptr()));
        }
        let first_key = self.internal_get_key(0)?;
        if target_key < first_key {
            return Ok((0, self.aux_ptr()));
        }
        let mut low = 0;
        let mut high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            let k = self.internal_get_key(mid)?;
            if k <= target_key {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let child = self.internal_get_child(low - 1)?;
        Ok((low, child))
    }

    pub fn internal_search(&self, target_key: &[u8]) -> Result<PageId> {
        let count = self.item_count();
        if count == 0 {
            return Ok(self.aux_ptr());
        }
        let first_key = self.internal_get_key(0)?;
        if target_key < first_key {
            return Ok(self.aux_ptr());
        }
        let mut low = 0;
        let mut high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            let k = self.internal_get_key(mid)?;
            if k <= target_key {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        self.internal_get_child(low - 1)
    }

    pub fn internal_search_with_bound(&self, target_key: &[u8]) -> Result<(PageId, Option<&[u8]>)> {
        let count = self.item_count();
        if count == 0 {
            return Ok((self.aux_ptr(), None));
        }
        let first_key = self.internal_get_key(0)?;
        if target_key < first_key {
            return Ok((self.aux_ptr(), Some(first_key)));
        }
        let mut low = 0;
        let mut high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            let k = self.internal_get_key(mid)?;
            if k <= target_key {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let child = self.internal_get_child(low - 1)?;
        let upper_bound = if low < count {
            Some(self.internal_get_key(low)?)
        } else {
            None
        };
        Ok((child, upper_bound))
    }

    /// Construct a new internal page with `new_page_id` by inserting `(key, child_id)`.
    pub fn internal_create_with_insert(
        &self,
        new_page_id: PageId,
        key: &[u8],
        child_id: PageId,
    ) -> Result<Page> {
        let mut new_page = Page::new_internal(new_page_id, self.aux_ptr());
        let count = self.item_count();

        // Find insert position
        let mut insert_idx = count;
        for i in 0..count {
            let k = self.internal_get_key(i)?;
            if key < k {
                insert_idx = i;
                break;
            }
        }

        let new_count = count + 1;
        let mut free_end = PAGE_SIZE;

        for i in 0..new_count {
            let (k, c) = if i == insert_idx {
                (key, child_id)
            } else {
                let src_idx = if i > insert_idx { i - 1 } else { i };
                (
                    self.internal_get_key(src_idx)?,
                    self.internal_get_child(src_idx)?,
                )
            };

            let cell_size = 10 + k.len();
            free_end -= cell_size;
            let offset = free_end;

            new_page.data[offset..offset + 8].copy_from_slice(&c.0.to_le_bytes());
            #[allow(clippy::cast_possible_truncation)]
            new_page.data[offset + 8..offset + 10].copy_from_slice(&(k.len() as u16).to_le_bytes());
            new_page.data[offset + 10..offset + 10 + k.len()].copy_from_slice(k);

            let slot_pos = SLOTS_START_OFFSET + i * 2;
            #[allow(clippy::cast_possible_truncation)]
            new_page.data[slot_pos..slot_pos + 2].copy_from_slice(&(offset as u16).to_le_bytes());
        }

        #[allow(clippy::cast_possible_truncation)]
        new_page.data[18..20].copy_from_slice(&(new_count as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        new_page.data[20..22].copy_from_slice(&(free_end as u16).to_le_bytes());
        new_page.update_checksum();
        Ok(new_page)
    }

    /// Insert `(key, child_id)` in-place in this internal page.
    /// Returns `Ok(true)` if inserted, or `Ok(false)` if there was not enough space.
    pub fn internal_insert_inplace(&mut self, key: &[u8], child_id: PageId) -> Result<bool> {
        let count = self.item_count();
        let cell_size = 10 + key.len();

        let free_space = self.free_space_offset();
        let slots_end = SLOTS_START_OFFSET + (count + 1) * 2;
        if free_space < slots_end + cell_size {
            return Ok(false);
        }

        let mut insert_idx = count;
        for i in 0..count {
            let k = self.internal_get_key(i)?;
            if key < k {
                insert_idx = i;
                break;
            }
        }

        let new_offset = free_space - cell_size;
        self.data[new_offset..new_offset + 8].copy_from_slice(&child_id.0.to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        self.data[new_offset + 8..new_offset + 10]
            .copy_from_slice(&(key.len() as u16).to_le_bytes());
        self.data[new_offset + 10..new_offset + 10 + key.len()].copy_from_slice(key);

        let slot_pos = SLOTS_START_OFFSET + insert_idx * 2;
        let cur_slots_end = SLOTS_START_OFFSET + count * 2;
        if insert_idx < count {
            self.data.copy_within(slot_pos..cur_slots_end, slot_pos + 2);
        }

        #[allow(clippy::cast_possible_truncation)]
        self.data[slot_pos..slot_pos + 2].copy_from_slice(&(new_offset as u16).to_le_bytes());

        let new_count = count + 1;
        #[allow(clippy::cast_possible_truncation)]
        self.data[18..20].copy_from_slice(&(new_count as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        self.data[20..22].copy_from_slice(&(new_offset as u16).to_le_bytes());
        self.update_checksum();
        Ok(true)
    }

    /// Replace child pointer in-place without copying the internal page.
    pub fn internal_replace_child(&mut self, old_child: PageId, new_child: PageId) -> Result<bool> {
        if self.aux_ptr() == old_child {
            self.set_aux_ptr(new_child);
            self.update_checksum();
            return Ok(true);
        }
        let count = self.item_count();
        for i in 0..count {
            let c = self.internal_get_child(i)?;
            if c == old_child {
                let slot_pos = SLOTS_START_OFFSET + i * 2;
                let offset =
                    u16::from_le_bytes([self.data[slot_pos], self.data[slot_pos + 1]]) as usize;
                self.data[offset..offset + 8].copy_from_slice(&new_child.0.to_le_bytes());
                self.update_checksum();
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Split internal node into two halves: `left` and `right`, promoting `middle_key`.
    pub fn internal_split_into(
        &self,
        left_id: PageId,
        right_id: PageId,
    ) -> Result<(Page, Page, Vec<u8>)> {
        let count = self.item_count();
        if count < 2 {
            return Err(DbError::Internal(
                "Cannot split internal node with < 2 items".into(),
            ));
        }
        let mid = count / 2;
        let mid_key = self.internal_get_key(mid)?.to_vec();
        let mid_child = self.internal_get_child(mid)?;

        let mut left = Page::new_internal(left_id, self.aux_ptr());
        let mut right = Page::new_internal(right_id, mid_child);

        // Left items: 0..mid
        let mut left_free_end = PAGE_SIZE;
        for i in 0..mid {
            let k = self.internal_get_key(i)?;
            let c = self.internal_get_child(i)?;
            let cell_size = 10 + k.len();
            left_free_end -= cell_size;
            let offset = left_free_end;

            left.data[offset..offset + 8].copy_from_slice(&c.0.to_le_bytes());
            #[allow(clippy::cast_possible_truncation)]
            left.data[offset + 8..offset + 10].copy_from_slice(&(k.len() as u16).to_le_bytes());
            left.data[offset + 10..offset + 10 + k.len()].copy_from_slice(k);

            let slot_pos = SLOTS_START_OFFSET + i * 2;
            #[allow(clippy::cast_possible_truncation)]
            left.data[slot_pos..slot_pos + 2].copy_from_slice(&(offset as u16).to_le_bytes());
        }
        #[allow(clippy::cast_possible_truncation)]
        left.data[18..20].copy_from_slice(&(mid as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        left.data[20..22].copy_from_slice(&(left_free_end as u16).to_le_bytes());
        left.update_checksum();

        // Right items: mid+1..count
        let right_count = count - (mid + 1);
        let mut right_free_end = PAGE_SIZE;
        for i in 0..right_count {
            let src_idx = mid + 1 + i;
            let k = self.internal_get_key(src_idx)?;
            let c = self.internal_get_child(src_idx)?;
            let cell_size = 10 + k.len();
            right_free_end -= cell_size;
            let offset = right_free_end;

            right.data[offset..offset + 8].copy_from_slice(&c.0.to_le_bytes());
            #[allow(clippy::cast_possible_truncation)]
            right.data[offset + 8..offset + 10].copy_from_slice(&(k.len() as u16).to_le_bytes());
            right.data[offset + 10..offset + 10 + k.len()].copy_from_slice(k);

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
