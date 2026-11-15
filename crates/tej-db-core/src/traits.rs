use crate::error::Result;
use crate::types::{ChangeSet, KV, KeyRange, PageId, TxId};
use alloc::vec::Vec;

/// Storage abstraction (Pager / VFS).
///
/// Implemented by file-backed storage (`FileStorage`), in-memory storage (`MemStorage`),
/// and testing fault-injection harnesses.
pub trait Storage: Send + Sync + 'static {
    /// Read raw page bytes from storage into `buf`.
    fn read_page(&self, id: PageId, buf: &mut [u8]) -> Result<()>;

    /// Read multiple contiguous pages in one operation. Defaults to loop over `read_page`.
    fn read_pages(&self, start_id: PageId, pages: &mut [u8]) -> Result<()> {
        let count = pages.len() / crate::PAGE_SIZE;
        for i in 0..count {
            let pid = PageId(start_id.0 + i as u64);
            let offset = i * crate::PAGE_SIZE;
            self.read_page(pid, &mut pages[offset..offset + crate::PAGE_SIZE])?;
        }
        Ok(())
    }

    /// Write raw page bytes to storage.
    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()>;

    /// Write multiple contiguous pages in one operation. Defaults to loop over `write_page`.
    fn write_pages(&mut self, start_id: PageId, pages: &[u8]) -> Result<()> {
        let count = pages.len() / crate::PAGE_SIZE;
        for i in 0..count {
            let pid = PageId(start_id.0 + i as u64);
            let offset = i * crate::PAGE_SIZE;
            self.write_page(pid, &pages[offset..offset + crate::PAGE_SIZE])?;
        }
        Ok(())
    }

    /// Flush all pending writes and sync to physical media (fsync).
    fn sync(&mut self) -> Result<()>;

    /// Return the total number of allocated pages in storage.
    fn page_count(&self) -> u64;

    /// Allocate a new page ID.
    fn allocate_page(&mut self) -> Result<PageId>;

    /// Allocate multiple new page IDs in batch. Defaults to loop over `allocate_page`.
    fn allocate_pages(&mut self, count: usize) -> Result<Vec<PageId>> {
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            ids.push(self.allocate_page()?);
        }
        Ok(ids)
    }

    /// Allocate `count` contiguous page IDs in one operation, returning the starting PageId.
    fn allocate_contiguous_extent(&mut self, count: usize) -> Result<PageId> {
        let pages = self.allocate_pages(count)?;
        pages
            .first()
            .copied()
            .ok_or_else(|| crate::error::DbError::Corruption("Zero pages allocated".into()))
    }

    /// Read contiguous overflow payload directly from storage into `out`.
    /// Returns Ok(true) if valid and read, Ok(false) if validation fails.
    fn read_overflow_payload(
        &self,
        start_id: PageId,
        total_len: usize,
        out: &mut [u8],
    ) -> Result<bool> {
        let num_pages = total_len.div_ceil(crate::PAGE_SIZE);
        let mut dest_offset = 0;
        let mut page_buf = [0u8; crate::PAGE_SIZE];
        for i in 0..num_pages {
            let pid = PageId(start_id.0 + i as u64);
            if self.read_page(pid, &mut page_buf).is_err() {
                return Ok(false);
            }
            let take_len = (total_len - dest_offset).min(crate::PAGE_SIZE);
            out[dest_offset..dest_offset + take_len].copy_from_slice(&page_buf[..take_len]);
            dest_offset += take_len;
        }
        Ok(true)
    }

    /// Visit contiguous overflow payload directly from storage with a closure.
    fn with_overflow_payload<R>(
        &self,
        start_id: PageId,
        total_len: usize,
        f: impl FnOnce(&[u8]) -> Result<R>,
    ) -> Result<Option<R>> {
        let mut buf = vec![0u8; total_len];
        if self.read_overflow_payload(start_id, total_len, &mut buf)? {
            Ok(Some(f(&buf)?))
        } else {
            Ok(None)
        }
    }

    /// Write contiguous overflow payload directly to storage.
    fn write_overflow_payload(&mut self, start_id: PageId, data: &[u8]) -> Result<()> {
        let num_pages = if data.is_empty() {
            1
        } else {
            data.len().div_ceil(crate::PAGE_SIZE)
        };

        if num_pages == 1 {
            let mut stack_buf = [0u8; crate::PAGE_SIZE];
            if !data.is_empty() {
                stack_buf[..data.len()].copy_from_slice(data);
            }
            return self.write_pages(start_id, &stack_buf);
        }

        let extent_len = num_pages * crate::PAGE_SIZE;
        let mut extent_buf = Vec::with_capacity(extent_len);
        #[allow(clippy::uninit_vec)]
        unsafe {
            extent_buf.set_len(extent_len);
        }

        for (i, chunk) in data.chunks(crate::PAGE_SIZE).enumerate() {
            let page_offset = i * crate::PAGE_SIZE;
            let page_buf = &mut extent_buf[page_offset..page_offset + crate::PAGE_SIZE];
            page_buf[..chunk.len()].copy_from_slice(chunk);
            if chunk.len() < crate::PAGE_SIZE {
                page_buf[chunk.len()..].fill(0);
            }
        }
        if data.is_empty() {
            let page_buf = &mut extent_buf[0..crate::PAGE_SIZE];
            page_buf.fill(0);
        }

        self.write_pages(start_id, &extent_buf)
    }
}

/// Index abstraction.
///
/// Pluggable key-value index implementation (e.g. B-Tree or LSM-tree).
pub trait Index: Send + Sync + 'static {
    /// Look up a key and return the associated value if found.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Insert or update a key-value pair.
    fn insert(&mut self, key: &[u8], val: &[u8]) -> Result<()>;

    /// Remove a key, returning true if the key existed.
    fn remove(&mut self, key: &[u8]) -> Result<bool>;

    /// Scan all entries or within a range.
    fn scan_range(&self, range: &KeyRange) -> Result<Vec<KV>>;
}

/// Durability manager abstraction.
///
/// Coordinates transaction commit and recovery (e.g. Copy-on-Write root swapping or WAL).
pub trait Durability: Send + Sync + 'static {
    /// Begin a new transaction and return its ID.
    fn begin(&mut self) -> Result<TxId>;

    /// Commit the given transaction.
    fn commit(&mut self, tx: TxId) -> Result<()>;

    /// Perform crash recovery on database startup.
    fn recover(&mut self) -> Result<()>;
}

/// Transaction lifecycle hook.
pub trait Hook: Send + Sync + 'static {
    /// Called before a transaction commits. If an error is returned, commit is aborted.
    fn before_commit(&self, _changes: &ChangeSet) -> Result<()> {
        Ok(())
    }

    /// Called after a transaction is committed and durable.
    fn after_commit(&self, _changes: &ChangeSet) {}
}
