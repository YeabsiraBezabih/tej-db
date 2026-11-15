use crate::page::{PAGE_MAGIC, PAGE_SIZE, Page, PageType, SLOTS_START_OFFSET};
use crate::types::PageId;
use crc32fast::Hasher;

impl Page {
    #[must_use]
    pub fn overflow_payload(&self) -> &[u8] {
        &self.data[SLOTS_START_OFFSET..]
    }

    pub fn overflow_payload_mut(&mut self) -> &mut [u8] {
        &mut self.data[SLOTS_START_OFFSET..]
    }

    #[must_use]
    pub fn new_overflow(page_id: PageId, next_page: PageId, chunk: &[u8]) -> Self {
        let mut page = Self::new(page_id, PageType::Overflow);
        page.set_aux_ptr(next_page);
        let payload = page.overflow_payload_mut();
        let copy_len = chunk.len().min(payload.len());
        payload[..copy_len].copy_from_slice(&chunk[..copy_len]);
        // Defer checksum calculation until dirty page flush to storage
        page
    }

    /// Fast in-place overflow page formatting directly into a pre-allocated extent buffer.
    pub fn format_overflow_slice(page_buf: &mut [u8], pid: PageId, next_pid: PageId, chunk: &[u8]) {
        page_buf[0..4].copy_from_slice(&PAGE_MAGIC);
        page_buf[4] = PageType::Overflow as u8;
        page_buf[5] = 1;
        page_buf[6..14].copy_from_slice(&pid.0.to_le_bytes());
        page_buf[14..18].copy_from_slice(&[0u8; 4]);
        page_buf[18..20].copy_from_slice(&[0u8; 2]);
        #[allow(clippy::cast_possible_truncation)]
        page_buf[20..22]
            .copy_from_slice(&((SLOTS_START_OFFSET + chunk.len()) as u16).to_le_bytes());
        page_buf[22..24].copy_from_slice(&[0u8; 2]);
        page_buf[24..32].copy_from_slice(&next_pid.0.to_le_bytes());
        page_buf[SLOTS_START_OFFSET..SLOTS_START_OFFSET + chunk.len()].copy_from_slice(chunk);

        let mut hasher = Hasher::new();
        hasher.update(&page_buf[0..14]);
        hasher.update(&page_buf[18..PAGE_SIZE]);
        let cs = hasher.finalize();
        page_buf[14..18].copy_from_slice(&cs.to_le_bytes());
    }
}
