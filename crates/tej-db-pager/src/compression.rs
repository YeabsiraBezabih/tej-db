use tej_db_core::{DbError, PAGE_SIZE, PageId, Result, Storage};

/// Magic identifier for compressed pages: "LZ4P".
pub const COMPRESSION_MAGIC: [u8; 4] = *b"LZ4P";

/// Minimum size reduction (in bytes) required to store a page compressed.
const MIN_COMPRESSION_SAVINGS: usize = 64;

/// Compress a page payload using LZ4 with a 4-byte uncompressed length prefix.
#[cfg(feature = "compression")]
pub fn compress_page(src: &[u8]) -> Vec<u8> {
    lz4_flex::block::compress_prepend_size(src)
}

/// Decompresses an LZ4-compressed payload safely, strictly bounded by `PAGE_SIZE`.
///
/// Prevents decompression bomb attacks by enforcing `uncomp_size <= PAGE_SIZE` (4096),
/// rejecting any crafted header with oversized or negative lengths before allocation.
#[cfg(feature = "compression")]
pub fn decompress_page(src: &[u8]) -> Result<Vec<u8>> {
    if src.len() < 4 {
        return Err(DbError::Corruption("Truncated compressed page data".into()));
    }
    let uncomp_size = u32::from_le_bytes([src[0], src[1], src[2], src[3]]) as usize;
    if uncomp_size > PAGE_SIZE {
        return Err(DbError::Corruption(format!(
            "Invalid decompressed size prefix: {uncomp_size} exceeds max PAGE_SIZE ({PAGE_SIZE})"
        )));
    }

    let mut dst = vec![0u8; uncomp_size];
    let decomp_len = lz4_flex::block::decompress_into(&src[4..], &mut dst)
        .map_err(|e| DbError::Corruption(format!("LZ4 decompression failed: {e}")))?;

    if decomp_len != uncomp_size {
        return Err(DbError::Corruption(format!(
            "LZ4 decompressed size mismatch: expected {uncomp_size}, got {decomp_len}"
        )));
    }

    Ok(dst)
}

/// Transparent per-page LZ4 compression storage wrapper.
///
/// Compresses pages and stores them with an `LZ4P` identifier when compression
/// yields meaningful size reduction. Incompressible or high-entropy pages automatically
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
            if comp_len + 6 <= PAGE_SIZE {
                if let Ok(decomp) = decompress_page(&raw[6..6 + comp_len]) {
                    if decomp.len() <= PAGE_SIZE {
                        buf[..decomp.len()].copy_from_slice(&decomp);
                        if decomp.len() < PAGE_SIZE {
                            buf[decomp.len()..].fill(0);
                        }
                        return Ok(());
                    }
                }
            }
        }

        // Uncompressed fallback page or raw data starting with LZ4P
        buf.copy_from_slice(&raw);
        Ok(())
    }

    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }

        let comp = compress_page(buf);
        if comp.len() + 6 <= PAGE_SIZE - MIN_COMPRESSION_SAVINGS {
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

    fn sync(&self) -> Result<()> {
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
