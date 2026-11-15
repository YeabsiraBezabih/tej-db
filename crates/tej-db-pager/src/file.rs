use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use tej_db_core::{DbError, PAGE_SIZE, PageId, Result, Storage};

/// High-performance file-backed storage backend using pread64/pwrite64 and fdatasync.
#[derive(Debug)]
pub struct FileStorage {
    path: PathBuf,
    file: File,
    page_count: u64,
}

impl FileStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        let len = file.seek(SeekFrom::End(0))?;
        if len > 0 && len % PAGE_SIZE as u64 != 0 {
            return Err(DbError::Corruption(format!(
                "Database file length ({len} bytes) is not a multiple of PAGE_SIZE ({PAGE_SIZE} bytes)"
            )));
        }
        let page_count = len / PAGE_SIZE as u64;

        Ok(Self {
            path,
            file,
            page_count,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Storage for FileStorage {
    fn read_page(&self, id: PageId, buf: &mut [u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }
        if id.0 >= self.page_count {
            return Err(DbError::PageNotFound(id));
        }

        let offset = id.0 * PAGE_SIZE as u64;
        let file_len = self.file.metadata().map_or(0, |m| m.len());
        if offset >= file_len {
            buf.fill(0);
            return Ok(());
        }
        if offset + PAGE_SIZE as u64 > file_len {
            let to_read = (file_len - offset) as usize;
            self.file.read_exact_at(&mut buf[..to_read], offset)?;
            buf[to_read..].fill(0);
            return Ok(());
        }
        self.file.read_exact_at(buf, offset)?;
        Ok(())
    }

    fn read_pages(&self, start_id: PageId, pages: &mut [u8]) -> Result<()> {
        if pages.len() % PAGE_SIZE != 0 {
            return Err(DbError::Custom(
                "Buffer size must be a multiple of PAGE_SIZE".into(),
            ));
        }
        let count = (pages.len() / PAGE_SIZE) as u64;
        if start_id.0 + count > self.page_count {
            return Err(DbError::PageNotFound(PageId(start_id.0 + count)));
        }
        let offset = start_id.0 * PAGE_SIZE as u64;
        self.file.read_exact_at(pages, offset)?;
        Ok(())
    }

    fn read_overflow_payload(
        &self,
        start_id: PageId,
        total_len: usize,
        out: &mut [u8],
    ) -> Result<bool> {
        let offset = start_id.0 * PAGE_SIZE as u64;
        let file_len = self.file.metadata().map_or(0, |m| m.len());
        if offset + total_len as u64 > file_len {
            return Ok(false);
        }

        self.file.read_exact_at(out, offset)?;
        Ok(true)
    }

    fn with_overflow_payload<R>(
        &self,
        start_id: PageId,
        total_len: usize,
        f: impl FnOnce(&[u8]) -> Result<R>,
    ) -> Result<Option<R>> {
        thread_local! {
            static SCRATCH_BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }

        let offset = start_id.0 * PAGE_SIZE as u64;
        let file_len = self.file.metadata().map_or(0, |m| m.len());
        if offset + total_len as u64 > file_len {
            return Ok(None);
        }

        SCRATCH_BUF.with(|cell| {
            let mut buf = cell.borrow_mut();
            if buf.len() < total_len {
                buf.resize(total_len, 0);
            }
            self.file.read_exact_at(&mut buf[..total_len], offset)?;
            let res = f(&buf[..total_len])?;
            Ok(Some(res))
        })
    }

    fn write_overflow_payload(&mut self, start_id: PageId, data: &[u8]) -> Result<()> {
        let offset = start_id.0 * PAGE_SIZE as u64;
        self.file.write_all_at(data, offset)?;
        let num_pages = if data.is_empty() {
            1
        } else {
            data.len().div_ceil(PAGE_SIZE) as u64
        };
        let end_page = start_id.0 + num_pages;
        if end_page > self.page_count {
            self.page_count = end_page;
        }
        Ok(())
    }

    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }

        let offset = id.0 * PAGE_SIZE as u64;
        self.file.write_all_at(buf, offset)?;

        if id.0 >= self.page_count {
            self.page_count = id.0 + 1;
        }
        Ok(())
    }

    fn write_pages(&mut self, start_id: PageId, pages: &[u8]) -> Result<()> {
        if pages.len() % PAGE_SIZE != 0 {
            return Err(DbError::Custom(
                "Buffer size must be a multiple of PAGE_SIZE".into(),
            ));
        }
        let count = (pages.len() / PAGE_SIZE) as u64;
        let offset = start_id.0 * PAGE_SIZE as u64;
        self.file.write_all_at(pages, offset)?;

        let end_page = start_id.0 + count;
        if end_page > self.page_count {
            self.page_count = end_page;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        self.file.sync_data()?;
        Ok(())
    }

    fn page_count(&self) -> u64 {
        self.page_count
    }

    fn allocate_page(&mut self) -> Result<PageId> {
        let id = PageId(self.page_count);
        self.page_count += 1;
        let new_len = self.page_count * PAGE_SIZE as u64;
        let _ = self.file.set_len(new_len);
        Ok(id)
    }

    fn allocate_pages(&mut self, count: usize) -> Result<Vec<PageId>> {
        let start = self.page_count;
        self.page_count += count as u64;
        let mut ids = Vec::with_capacity(count);
        for i in 0..count {
            ids.push(PageId(start + i as u64));
        }
        Ok(ids)
    }

    fn allocate_contiguous_extent(&mut self, count: usize) -> Result<PageId> {
        let start = self.page_count;
        self.page_count += count as u64;
        Ok(PageId(start))
    }
}
