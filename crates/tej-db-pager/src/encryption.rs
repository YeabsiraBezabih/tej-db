#[cfg(feature = "encryption")]
use alloc::format;
use alloc::vec::Vec;
use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit},
};
use parking_lot::Mutex;
use tej_db_core::{DbError, PAGE_SIZE, PageId, Result, Storage};

const TAG_SIZE: usize = 16;
const TAGS_PER_PAGE: u64 = (PAGE_SIZE / TAG_SIZE) as u64; // 256 tags per 4096-byte page
const STRIDE: u64 = TAGS_PER_PAGE + 1; // 1 tag page + 256 data pages = 257 physical pages

/// Encrypt raw bytes with ChaCha20-Poly1305 using a 256-bit key and 96-bit nonce.
#[cfg(feature = "encryption")]
pub fn encrypt_page_data(data: &[u8], key: &[u8; 32], nonce: &[u8; 12]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .encrypt(Nonce::from_slice(nonce), data)
        .map_err(|e| DbError::Corruption(format!("Page encryption failed: {e}")))
}

/// Decrypt raw bytes with ChaCha20-Poly1305 using a 256-bit key and 96-bit nonce.
#[cfg(feature = "encryption")]
pub fn decrypt_page_data(data: &[u8], key: &[u8; 32], nonce: &[u8; 12]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), data)
        .map_err(|e| DbError::Corruption(format!("Page decryption failed: {e}")))
}

#[inline]
fn logical_to_physical_ids(logical_id: PageId) -> (PageId, PageId, usize) {
    let group = logical_id.0 / TAGS_PER_PAGE;
    let offset_in_group = (logical_id.0 % TAGS_PER_PAGE) as usize;
    let tag_pid = PageId(group * STRIDE);
    let data_pid = PageId(group * STRIDE + 1 + offset_in_group as u64);
    (tag_pid, data_pid, offset_in_group)
}

#[inline]
fn make_nonce(logical_id: PageId) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[0..8].copy_from_slice(&logical_id.0.to_le_bytes());
    nonce[8..12].copy_from_slice(&0xDB4D_B001u32.to_le_bytes());
    nonce
}

/// Transparent authenticated per-page ChaCha20-Poly1305 encryption storage wrapper.
///
/// Stores full 4096-byte page ciphertexts in dedicated data pages and persists their
/// 16-byte Poly1305 authentication tags in interleaved tag catalog pages. Decryption
/// strictly verifies the AEAD authentication tag on every page access.
#[cfg(feature = "encryption")]
#[derive(Debug)]
pub struct EncryptedStorage<S: Storage> {
    inner: S,
    key: [u8; 32],
    tag_cache: Mutex<Option<(PageId, [u8; PAGE_SIZE], bool)>>, // (tag_pid, page_buf, is_dirty)
}

#[cfg(feature = "encryption")]
impl<S: Storage> EncryptedStorage<S> {
    pub fn new(inner: S, key: [u8; 32]) -> Self {
        Self {
            inner,
            key,
            tag_cache: Mutex::new(None),
        }
    }

    pub fn into_inner(mut self) -> S {
        let _ = self.flush_tag_cache();
        self.inner
    }

    fn flush_tag_cache(&mut self) -> Result<()> {
        let mut cache_guard = self.tag_cache.lock();
        if let Some((tag_pid, buf, is_dirty)) = cache_guard.take() {
            if is_dirty {
                self.inner.write_page(tag_pid, &buf)?;
            }
            *cache_guard = Some((tag_pid, buf, false));
        }
        Ok(())
    }

    fn load_tag_page(&self, tag_pid: PageId, out: &mut [u8; PAGE_SIZE]) -> Result<()> {
        let mut cache_guard = self.tag_cache.lock();
        if let Some((cached_pid, buf, _)) = *cache_guard {
            if cached_pid == tag_pid {
                out.copy_from_slice(&buf);
                return Ok(());
            }
        }

        if tag_pid.0 < self.inner.page_count() {
            self.inner.read_page(tag_pid, out)?;
        } else {
            out.fill(0);
        }

        *cache_guard = Some((tag_pid, *out, false));
        Ok(())
    }

    fn write_tag(&mut self, tag_pid: PageId, slot: usize, tag: &[u8]) -> Result<()> {
        let mut page_buf = [0u8; PAGE_SIZE];
        let mut cache_guard = self.tag_cache.lock();

        let mut hit = false;
        if let Some((cached_pid, ref buf, _)) = *cache_guard {
            if cached_pid == tag_pid {
                page_buf.copy_from_slice(buf);
                hit = true;
            }
        }

        if !hit {
            if tag_pid.0 < self.inner.page_count() {
                self.inner.read_page(tag_pid, &mut page_buf)?;
            } else {
                page_buf.fill(0);
            }
        }

        let start = slot * TAG_SIZE;
        page_buf[start..start + TAG_SIZE].copy_from_slice(tag);

        self.inner.write_page(tag_pid, &page_buf)?;
        *cache_guard = Some((tag_pid, page_buf, false));
        Ok(())
    }

    fn read_tag(&self, tag_pid: PageId, slot: usize, out_tag: &mut [u8; TAG_SIZE]) -> Result<()> {
        let mut page_buf = [0u8; PAGE_SIZE];
        self.load_tag_page(tag_pid, &mut page_buf)?;
        let start = slot * TAG_SIZE;
        out_tag.copy_from_slice(&page_buf[start..start + TAG_SIZE]);
        Ok(())
    }
}

#[cfg(feature = "encryption")]
impl<S: Storage> Storage for EncryptedStorage<S> {
    fn read_page(&self, id: PageId, buf: &mut [u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }

        let (tag_pid, data_pid, slot) = logical_to_physical_ids(id);
        let mut tag = [0u8; TAG_SIZE];
        self.read_tag(tag_pid, slot, &mut tag)?;

        let mut cipher_page = [0u8; PAGE_SIZE];
        self.inner.read_page(data_pid, &mut cipher_page)?;

        let mut cipher_with_tag = Vec::with_capacity(PAGE_SIZE + TAG_SIZE);
        cipher_with_tag.extend_from_slice(&cipher_page);
        cipher_with_tag.extend_from_slice(&tag);

        let nonce = make_nonce(id);
        let plain = decrypt_page_data(&cipher_with_tag, &self.key, &nonce)?;
        if plain.len() != PAGE_SIZE {
            return Err(DbError::Corruption(format!(
                "Decrypted page size mismatch: expected {PAGE_SIZE}, got {}",
                plain.len()
            )));
        }

        buf.copy_from_slice(&plain);
        Ok(())
    }

    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }

        let (tag_pid, data_pid, slot) = logical_to_physical_ids(id);
        let nonce = make_nonce(id);

        let cipher_with_tag = encrypt_page_data(buf, &self.key, &nonce)?;
        if cipher_with_tag.len() != PAGE_SIZE + TAG_SIZE {
            return Err(DbError::Corruption(
                "Encrypted ciphertext length mismatch".into(),
            ));
        }

        let cipher_body = &cipher_with_tag[..PAGE_SIZE];
        let tag = &cipher_with_tag[PAGE_SIZE..];

        // Ensure physical capacity exists
        while self.inner.page_count() <= data_pid.0 {
            self.inner.allocate_page()?;
        }

        self.write_tag(tag_pid, slot, tag)?;
        self.inner.write_page(data_pid, cipher_body)
    }

    fn sync(&mut self) -> Result<()> {
        self.flush_tag_cache()?;
        self.inner.sync()
    }

    fn page_count(&self) -> u64 {
        let total_physical = self.inner.page_count();
        if total_physical == 0 {
            return 0;
        }

        let full_groups = total_physical / STRIDE;
        let rem = total_physical % STRIDE;
        let data_in_rem = rem.saturating_sub(1);
        full_groups * TAGS_PER_PAGE + data_in_rem
    }

    fn allocate_page(&mut self) -> Result<PageId> {
        let next_logical = PageId(self.page_count());
        let (tag_pid, data_pid, _) = logical_to_physical_ids(next_logical);

        while self.inner.page_count() <= tag_pid.0 {
            self.inner.allocate_page()?;
        }
        while self.inner.page_count() <= data_pid.0 {
            self.inner.allocate_page()?;
        }

        Ok(next_logical)
    }

    fn allocate_pages(&mut self, count: usize) -> Result<Vec<PageId>> {
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            ids.push(self.allocate_page()?);
        }
        Ok(ids)
    }

    fn allocate_contiguous_extent(&mut self, count: usize) -> Result<PageId> {
        let mut start_id = PageId::INVALID;
        for i in 0..count {
            let pid = self.allocate_page()?;
            if i == 0 {
                start_id = pid;
            }
        }
        Ok(start_id)
    }
}
