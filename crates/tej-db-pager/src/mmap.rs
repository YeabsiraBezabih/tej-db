#[cfg(feature = "mmap")]
use memmap2::MmapMut;
use parking_lot::RwLock;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tej_db_core::{DbError, PAGE_SIZE, PageId, Result, Storage};

/// Memory-mapped file storage backend implementing the [`Storage`] trait.
#[cfg(feature = "mmap")]
#[derive(Debug)]
pub struct MmapStorage {
    file: File,
    mmap: RwLock<MmapMut>,
    page_count: AtomicU64,
}

#[cfg(feature = "mmap")]
impl MmapStorage {
    /// Open or create a memory-mapped storage at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        crate::lock::FileLock::try_lock_exclusive(&file)?;

        let metadata = file.metadata()?;
        let mut file_len = metadata.len();
        if file_len > 0 && file_len % PAGE_SIZE as u64 != 0 {
            return Err(DbError::Corruption(format!(
                "Memory-mapped file length ({file_len} bytes) is not a multiple of PAGE_SIZE ({PAGE_SIZE} bytes)"
            )));
        }
        if file_len == 0 {
            let init = (PAGE_SIZE as u64) * 4;
            file.set_len(init)?;
            file_len = init;
        }

        // SAFETY: We own the file descriptor exclusively and configure MmapMut over a validated file length.
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        let page_count = AtomicU64::new(file_len / PAGE_SIZE as u64);

        Ok(Self {
            file,
            mmap: RwLock::new(mmap),
            page_count,
        })
    }

    fn ensure_capacity(&self, required_pages: u64) -> Result<()> {
        let required_bytes = required_pages * PAGE_SIZE as u64;
        {
            let mmap_guard = self.mmap.read();
            if mmap_guard.len() as u64 >= required_bytes {
                return Ok(());
            }
        }

        let mut mmap_guard = self.mmap.write();
        let current_len = mmap_guard.len() as u64;
        if required_bytes > current_len {
            let new_bytes = (required_bytes + 512 * PAGE_SIZE as u64).max(current_len * 2);
            self.file.set_len(new_bytes)?;
            // SAFETY: File was expanded, creating a new MmapMut over the updated file size.
            let new_mmap = unsafe { MmapMut::map_mut(&self.file)? };
            *mmap_guard = new_mmap;
        }
        Ok(())
    }
    fn page_offset(id: PageId) -> Result<usize> {
        usize::try_from(id.0)
            .ok()
            .and_then(|i| i.checked_mul(PAGE_SIZE))
            .ok_or_else(|| DbError::CapacityExceeded("PageId offset overflow".into()))
    }
}

#[cfg(feature = "mmap")]
impl Storage for MmapStorage {
    fn read_page(&self, id: PageId, buf: &mut [u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }
        let offset = Self::page_offset(id)?;
        let mmap_guard = self.mmap.read();

        if offset + PAGE_SIZE > mmap_guard.len() {
            return Err(DbError::PageNotFound(id));
        }

        buf.copy_from_slice(&mmap_guard[offset..offset + PAGE_SIZE]);
        Ok(())
    }

    fn read_pages(&self, start_id: PageId, pages: &mut [u8]) -> Result<()> {
        if pages.len() % PAGE_SIZE != 0 {
            return Err(DbError::Custom(
                "Buffer size must be a multiple of PAGE_SIZE".into(),
            ));
        }
        let offset = Self::page_offset(start_id)?;
        let mmap_guard = self.mmap.read();

        if offset + pages.len() > mmap_guard.len() {
            return Err(DbError::PageNotFound(start_id));
        }

        pages.copy_from_slice(&mmap_guard[offset..offset + pages.len()]);
        Ok(())
    }

    fn read_overflow_payload(
        &self,
        start_id: PageId,
        total_len: usize,
        out: &mut [u8],
    ) -> Result<bool> {
        let offset = Self::page_offset(start_id)?;
        let mmap_guard = self.mmap.read();

        if offset + total_len > mmap_guard.len() {
            return Ok(false);
        }

        out.copy_from_slice(&mmap_guard[offset..offset + total_len]);
        Ok(true)
    }

    fn with_overflow_payload<R>(
        &self,
        start_id: PageId,
        total_len: usize,
        f: impl FnOnce(&[u8]) -> Result<R>,
    ) -> Result<Option<R>> {
        let offset = Self::page_offset(start_id)?;
        let mmap_guard = self.mmap.read();

        if offset + total_len > mmap_guard.len() {
            return Ok(None);
        }

        let slice = &mmap_guard[offset..offset + total_len];
        let res = f(slice)?;
        Ok(Some(res))
    }

    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }
        self.ensure_capacity(id.0 + 1)?;
        let offset = Self::page_offset(id)?;
        let mut mmap_guard = self.mmap.write();

        mmap_guard[offset..offset + PAGE_SIZE].copy_from_slice(buf);
        self.page_count.fetch_max(id.0 + 1, Ordering::Release);
        Ok(())
    }

    fn write_pages(&mut self, start_id: PageId, pages: &[u8]) -> Result<()> {
        if pages.len() % PAGE_SIZE != 0 {
            return Err(DbError::Custom(
                "Buffer size must be a multiple of PAGE_SIZE".into(),
            ));
        }
        let count = pages.len() / PAGE_SIZE;
        self.ensure_capacity(start_id.0 + count as u64)?;

        let offset = Self::page_offset(start_id)?;
        let mut mmap_guard = self.mmap.write();
        mmap_guard[offset..offset + pages.len()].copy_from_slice(pages);
        self.page_count
            .fetch_max(start_id.0 + count as u64, Ordering::Release);
        Ok(())
    }

    fn write_overflow_payload(&mut self, start_id: PageId, data: &[u8]) -> Result<()> {
        let num_pages = if data.is_empty() {
            1
        } else {
            data.len().div_ceil(PAGE_SIZE)
        };
        self.ensure_capacity(start_id.0 + num_pages as u64)?;

        let offset = Self::page_offset(start_id)?;
        let target_len = num_pages * PAGE_SIZE;
        let mut mmap_guard = self.mmap.write();
        mmap_guard[offset..offset + data.len()].copy_from_slice(data);
        if data.len() < target_len {
            mmap_guard[offset + data.len()..offset + target_len].fill(0);
        }
        self.page_count
            .fetch_max(start_id.0 + num_pages as u64, Ordering::Release);
        Ok(())
    }

    fn allocate_page(&mut self) -> Result<PageId> {
        let current = self.page_count.fetch_add(1, Ordering::SeqCst);
        self.ensure_capacity(current + 1)?;
        Ok(PageId(current))
    }

    fn allocate_pages(&mut self, count: usize) -> Result<Vec<PageId>> {
        let current = self.page_count.fetch_add(count as u64, Ordering::SeqCst);
        self.ensure_capacity(current + count as u64)?;
        let mut ids = Vec::with_capacity(count);
        for i in 0..count {
            ids.push(PageId(current + i as u64));
        }
        Ok(ids)
    }

    fn allocate_contiguous_extent(&mut self, count: usize) -> Result<PageId> {
        let current = self.page_count.fetch_add(count as u64, Ordering::SeqCst);
        self.ensure_capacity(current + count as u64)?;
        Ok(PageId(current))
    }

    fn sync(&mut self) -> Result<()> {
        let mmap_guard = self.mmap.read();
        mmap_guard.flush()?;
        Ok(())
    }

    fn page_count(&self) -> u64 {
        self.page_count.load(Ordering::SeqCst)
    }
}
