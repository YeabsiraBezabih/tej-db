#[cfg(feature = "compression")]
use alloc::format;
use alloc::vec::Vec;
use tej_db_core::{DbError, PAGE_SIZE, PageId, Result, Storage};

/// Magic identifier for compressed pages: "LZ4P".
pub const COMPRESSION_MAGIC: [u8; 4] = *b"LZ4P";

/// Compress a page payload using LZ4.
#[cfg(feature = "compression")]
pub fn compress_page(src: &[u8]) -> Vec<u8> {
    lz4_flex::block::compress_prepend_size(src)
}

/// Decompress an LZ4-compressed page payload.
#[cfg(feature = "compression")]
pub fn decompress_page(src: &[u8]) -> Result<Vec<u8>> {
    lz4_flex::block::decompress_size_prepended(src)
        .map_err(|e| DbError::Corruption(format!("LZ4 decompression failed: {e}")))
}

/// Transparent per-page LZ4 compression storage wrapper.
///
/// Compresses pages and stores them with an `LZ4P` identifier when compression
/// succeeds within the page boundary. Incompressible or high-entropy pages automatically
/// fall back to raw uncompressed storage without buffer truncation or data loss.
#[cfg(feature = "compression")]
#[derive(Debug)]
pub struct CompressedStorage<S: Storage> {
    inner: S,
}

#[cfg(feature = "compression")]
impl<S: Storage> CompressedStorage<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

#[cfg(feature = "compression")]
impl<S: Storage> Storage for CompressedStorage<S> {
    fn read_page(&self, id: PageId, buf: &mut [u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }

        let mut raw = [0u8; PAGE_SIZE];
        self.inner.read_page(id, &mut raw)?;

        if raw[0..4] == COMPRESSION_MAGIC {
            let comp_len = u16::from_le_bytes([raw[4], raw[5]]) as usize;
            if comp_len + 6 > PAGE_SIZE {
                return Err(DbError::Corruption(
                    "Invalid compressed page length in header".into(),
                ));
            }
            let decomp = decompress_page(&raw[6..6 + comp_len])?;
            let copy_len = decomp.len().min(buf.len());
            buf[..copy_len].copy_from_slice(&decomp[..copy_len]);
        } else {
            // Uncompressed fallback page
            buf.copy_from_slice(&raw);
        }

        Ok(())
    }

    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }

        let comp = compress_page(buf);
        if comp.len() + 6 <= PAGE_SIZE {
            let mut out = [0u8; PAGE_SIZE];
            out[0..4].copy_from_slice(&COMPRESSION_MAGIC);
            out[4..6].copy_from_slice(&(comp.len() as u16).to_le_bytes());
            out[6..6 + comp.len()].copy_from_slice(&comp);
            self.inner.write_page(id, &out)
        } else {
            // Fall back to writing raw uncompressed page without truncation
            self.inner.write_page(id, buf)
        }
    }

    fn sync(&mut self) -> Result<()> {
        self.inner.sync()
    }

    fn page_count(&self) -> u64 {
        self.inner.page_count()
    }

    fn allocate_page(&mut self) -> Result<PageId> {
        self.inner.allocate_page()
    }

    fn allocate_pages(&mut self, count: usize) -> Result<Vec<PageId>> {
        self.inner.allocate_pages(count)
    }

    fn allocate_contiguous_extent(&mut self, count: usize) -> Result<PageId> {
        self.inner.allocate_contiguous_extent(count)
    }
}
