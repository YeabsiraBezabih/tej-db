use super::{PAGE_SIZE, Page, SLOTS_START_OFFSET};
use crate::error::{DbError, Result};
use crate::types::PageId;

impl Page {
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
