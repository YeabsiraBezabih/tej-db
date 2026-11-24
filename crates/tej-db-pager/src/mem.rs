use std::collections::HashMap;
use tej_db_core::{DbError, PAGE_SIZE, PageId, Result, Storage};

/// In-memory storage backend implementing the `Storage` trait.
#[derive(Debug, Default, Clone)]
pub struct MemStorage {
    pages: HashMap<PageId, [u8; PAGE_SIZE]>,
    next_page_id: u64,
}

impl MemStorage {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pages: HashMap::new(),
            next_page_id: 0,
        }
    }
}

impl Storage for MemStorage {
    fn read_page(&self, id: PageId, buf: &mut [u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }
        if let Some(page_data) = self.pages.get(&id) {
            buf.copy_from_slice(page_data);
            Ok(())
        } else {
            Err(DbError::PageNotFound(id))
        }
    }

    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }
        let mut page_data = [0u8; PAGE_SIZE];
        page_data.copy_from_slice(buf);
        self.pages.insert(id, page_data);
        if id.0 >= self.next_page_id {
            self.next_page_id = id.0 + 1;
        }
        Ok(())
    }

    fn sync(&self) -> Result<()> {
        // In-memory writes are immediately in memory; no fsync needed.
        Ok(())
    }

    fn page_count(&self) -> u64 {
        self.next_page_id
    }

    fn allocate_page(&mut self) -> Result<PageId> {
        let id = PageId(self.next_page_id);
        self.next_page_id += 1;
        self.pages.insert(id, [0u8; PAGE_SIZE]);
        Ok(id)
    }

    fn allocate_pages(&mut self, count: usize) -> Result<Vec<PageId>> {
        let start = self.next_page_id;
        self.next_page_id += count as u64;
        let mut ids = Vec::with_capacity(count);
        for i in 0..count {
            let id = PageId(start + i as u64);
            self.pages.insert(id, [0u8; PAGE_SIZE]);
            ids.push(id);
        }
        Ok(ids)
    }

    fn allocate_contiguous_extent(&mut self, count: usize) -> Result<PageId> {
        let start = self.next_page_id;
        self.next_page_id += count as u64;
        for i in 0..count {
            let id = PageId(start + i as u64);
            self.pages.insert(id, [0u8; PAGE_SIZE]);
        }
        Ok(PageId(start))
    }
}
