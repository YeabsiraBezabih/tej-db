use crate::{BTreeIndex, TxPageSet};
use tej_db_core::{DbError, PageId, PageType, Result, Storage};

impl<S: Storage> BTreeIndex<S> {
    /// Read the full contents of a linked overflow page chain with direct extent fast-path.
    pub fn read_overflow_chain(&self, mut curr_id: PageId, total_len: u64) -> Result<Vec<u8>> {
        #[allow(clippy::cast_possible_truncation)]
        let total = total_len as usize;

        // Fast path: read contiguous extent directly from storage in a single syscall/copy
        if let Ok(Some(data)) = self.cache.read_overflow_extent(curr_id, total) {
            return Ok(data);
        }

        // Fallback: page-by-page traversal
        let mut buf = vec![0u8; total];
        let mut dest_offset = 0;
        while curr_id.is_valid() && dest_offset < total {
            curr_id = self.cache.with_page(curr_id, |p| {
                let header = p.read_header()?;
                if header.page_type != PageType::Overflow {
                    return Err(DbError::Corruption("Expected overflow page type".into()));
                }
                let next = p.aux_ptr();
                let payload = p.overflow_payload();
                let take_len = (total - dest_offset).min(payload.len());
                buf[dest_offset..dest_offset + take_len].copy_from_slice(&payload[..take_len]);
                dest_offset += take_len;
                Ok(next)
            })?;
        }
        Ok(buf)
    }

    /// Allocate and write an overflow payload directly to storage extent in a single operation.
    pub fn write_overflow_chain(&self, data: &[u8], tx_pages: &mut TxPageSet) -> Result<PageId> {
        let num_pages = if data.is_empty() {
            1
        } else {
            data.len().div_ceil(tej_db_core::PAGE_SIZE)
        };
        let first_pid = self.cache.allocate_overflow_extent(num_pages)?;
        tx_pages.insert(first_pid);

        self.cache.write_storage_overflow(first_pid, data)?;
        Ok(first_pid)
    }

    /// Free all pages belonging to an overflow extent back to the page allocator.
    pub fn free_overflow_extent(&self, first_id: PageId, total_len: u64) {
        if !first_id.is_valid() {
            return;
        }
        #[allow(clippy::cast_possible_truncation)]
        let num_pages = (total_len as usize).div_ceil(tej_db_core::PAGE_SIZE);
        for i in 0..num_pages {
            self.cache.free_page(PageId(first_id.0 + i as u64));
        }
    }

    /// Free all pages belonging to a legacy linked overflow page chain.
    pub fn free_overflow_chain(&self, mut curr_id: PageId) -> Result<()> {
        while curr_id.is_valid() {
            let next = self.cache.with_page(curr_id, |p| Ok(p.aux_ptr()))?;
            self.cache.free_page(curr_id);
            curr_id = next;
        }
        Ok(())
    }
}
