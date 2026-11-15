use tej_db_core::{DbError, PAGE_SIZE, PageHeader, PageId, Result};

/// Zero-copy page view providing direct borrowed slice access to a 4 KiB page
/// without copying or heap allocations.
#[derive(Debug, Clone, Copy)]
pub struct ZeroCopyPageView<'a> {
    data: &'a [u8; PAGE_SIZE],
}

impl<'a> ZeroCopyPageView<'a> {
    /// Creates a new zero-copy page view over a borrowed 4 KiB memory slice.
    #[must_use]
    pub const fn new(data: &'a [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    /// Borrows the raw 4 KiB byte slice.
    #[must_use]
    pub const fn as_slice(&self) -> &'a [u8; PAGE_SIZE] {
        self.data
    }

    /// Reads and parses the page header from the mapped slice.
    pub fn read_header(&self) -> Result<PageHeader> {
        let dummy = tej_db_core::Page::from_bytes(*self.data);
        dummy.read_header()
    }

    /// Verifies the checksum of the mapped page in-place.
    pub fn verify_checksum(&self) -> Result<()> {
        let dummy = tej_db_core::Page::from_bytes(*self.data);
        dummy.verify_checksum()
    }

    /// Borrows the payload region of the mapped page (bytes 24..4096).
    #[must_use]
    pub fn payload(&self) -> &'a [u8] {
        &self.data[24..PAGE_SIZE]
    }
}

/// Helper function to validate and slice a memory-mapped byte buffer into a page view.
pub fn view_page_at_offset(mmap_slice: &[u8], page_id: PageId) -> Result<ZeroCopyPageView<'_>> {
    let offset = (page_id.0 as usize) * PAGE_SIZE;
    if offset + PAGE_SIZE > mmap_slice.len() {
        return Err(DbError::PageNotFound(page_id));
    }

    let page_ref: &[u8; PAGE_SIZE] = match mmap_slice[offset..offset + PAGE_SIZE].try_into() {
        Ok(arr) => arr,
        Err(_) => return Err(DbError::Corruption("Failed to slice mmap page".into())),
    };

    Ok(ZeroCopyPageView::new(page_ref))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tej_db_core::{Page, PageType};

    #[test]
    fn test_zero_copy_page_view() {
        let pid = PageId(42);
        let page = Page::new(pid, PageType::BTreeLeaf);
        let view = ZeroCopyPageView::new(&page.data);

        let header = view.read_header().unwrap();
        assert_eq!(header.page_id, pid);
        assert_eq!(header.page_type, PageType::BTreeLeaf);
        assert!(view.verify_checksum().is_ok());
    }
}
