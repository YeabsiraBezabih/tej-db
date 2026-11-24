use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use core::fmt;
use tej_db_core::{DbError, PAGE_SIZE, PageId, Result, Storage};
use zeroize::Zeroizing;

const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;

/// Encrypt raw bytes with ChaCha20-Poly1305 using a 256-bit key, 96-bit nonce, and optional AAD.
#[cfg(feature = "encryption")]
pub fn encrypt_page_data(
    data: &[u8],
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let payload = Payload { msg: data, aad };
    cipher
        .encrypt(Nonce::from_slice(nonce), payload)
        .map_err(|e| DbError::Corruption(format!("Page encryption failed: {e}")))
}

/// Decrypt raw bytes with ChaCha20-Poly1305 using a 256-bit key, 96-bit nonce, and optional AAD.
#[cfg(feature = "encryption")]
pub fn decrypt_page_data(
    data: &[u8],
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let payload = Payload { msg: data, aad };
    cipher
        .decrypt(Nonce::from_slice(nonce), payload)
        .map_err(|e| DbError::Corruption(format!("Page decryption failed: {e}")))
}

/// 1:1 physical page mapping: Each logical page `k` maps to dedicated physical tag page `2*k`
/// and dedicated physical ciphertext page `2*k + 1`. This completely isolates tag pages across
/// all pages, eliminating torn writes, read-modify-write races, and adjacent page corruption.
#[inline]
fn logical_to_physical_ids(logical_id: PageId) -> (PageId, PageId) {
    (PageId(2 * logical_id.0), PageId(2 * logical_id.0 + 1))
}

/// Generate a fresh cryptographically secure random 96-bit nonce.
fn generate_random_nonce() -> Result<[u8; 12]> {
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce)
        .map_err(|e| DbError::Custom(format!("CSPRNG nonce generation failed: {e}")))?;
    Ok(nonce)
}

/// Transparent authenticated per-page ChaCha20-Poly1305 encryption storage wrapper.
///
/// Stores full 4096-byte page ciphertexts in dedicated data pages and persists their
/// fresh 12-byte CSPRNG nonces and 16-byte Poly1305 authentication tags in dedicated
/// 1:1 physical tag pages. The logical `PageId` is strictly bound into the AEAD Additional
/// Authenticated Data (AAD) to prevent page-swapping and ciphertext relocation attacks.
#[cfg(feature = "encryption")]
pub struct EncryptedStorage<S: Storage> {
    inner: S,
    key: Zeroizing<[u8; 32]>,
}

#[cfg(feature = "encryption")]
impl<S: Storage> fmt::Debug for EncryptedStorage<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncryptedStorage")
            .field("key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "encryption")]
impl<S: Storage> EncryptedStorage<S> {
    pub fn new(inner: S, key: [u8; 32]) -> Self {
        Self {
            inner,
            key: Zeroizing::new(key),
        }
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

#[cfg(feature = "encryption")]
impl<S: Storage> Storage for EncryptedStorage<S> {
    fn read_page(&self, id: PageId, buf: &mut [u8]) -> Result<()> {
        if buf.len() != PAGE_SIZE {
            return Err(DbError::Custom("Buffer size must equal PAGE_SIZE".into()));
        }

        let (tag_pid, data_pid) = logical_to_physical_ids(id);
        let mut tag_page = [0u8; PAGE_SIZE];
        self.inner.read_page(tag_pid, &mut tag_page)?;

        let mut nonce = [0u8; NONCE_SIZE];
        let mut tag = [0u8; TAG_SIZE];
        nonce.copy_from_slice(&tag_page[..NONCE_SIZE]);
        tag.copy_from_slice(&tag_page[NONCE_SIZE..NONCE_SIZE + TAG_SIZE]);

        let mut cipher_page = [0u8; PAGE_SIZE];
        self.inner.read_page(data_pid, &mut cipher_page)?;

        let mut cipher_with_tag = Vec::with_capacity(PAGE_SIZE + TAG_SIZE);
        cipher_with_tag.extend_from_slice(&cipher_page);
        cipher_with_tag.extend_from_slice(&tag);

        let aad = id.0.to_le_bytes();
        let plain = decrypt_page_data(&cipher_with_tag, &self.key, &nonce, &aad)?;
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

        let (tag_pid, data_pid) = logical_to_physical_ids(id);
        let nonce = generate_random_nonce()?;
        let aad = id.0.to_le_bytes();

        let cipher_with_tag = encrypt_page_data(buf, &self.key, &nonce, &aad)?;
        if cipher_with_tag.len() != PAGE_SIZE + TAG_SIZE {
            return Err(DbError::Corruption(
                "Encrypted ciphertext length mismatch".into(),
            ));
        }

        let cipher_body = &cipher_with_tag[..PAGE_SIZE];
        let tag: [u8; TAG_SIZE] = cipher_with_tag[PAGE_SIZE..PAGE_SIZE + TAG_SIZE]
            .try_into()
            .map_err(|_| DbError::Corruption("Invalid tag length".into()))?;

        while self.inner.page_count() <= tag_pid.0 {
            self.inner.allocate_page()?;
        }
        while self.inner.page_count() <= data_pid.0 {
            self.inner.allocate_page()?;
        }

        self.inner.write_page(data_pid, cipher_body)?;

        let mut tag_page = [0u8; PAGE_SIZE];
        tag_page[..NONCE_SIZE].copy_from_slice(&nonce);
        tag_page[NONCE_SIZE..NONCE_SIZE + TAG_SIZE].copy_from_slice(&tag);
        self.inner.write_page(tag_pid, &tag_page)
    }

    fn sync(&self) -> Result<()> {
        self.inner.sync()
    }

    fn page_count(&self) -> u64 {
        self.inner.page_count() / 2
    }

    fn allocate_page(&mut self) -> Result<PageId> {
        let next_logical = PageId(self.page_count());
        let (tag_pid, data_pid) = logical_to_physical_ids(next_logical);

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
