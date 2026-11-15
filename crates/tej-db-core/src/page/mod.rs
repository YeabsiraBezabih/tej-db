pub mod internal;
pub mod leaf;
pub mod overflow;
pub mod simd_crc;

use crate::error::{DbError, Result};
use crate::types::PageId;

/// Standard page size in bytes (4 KiB).
pub const PAGE_SIZE: usize = 4096;

/// Fixed page header size in bytes.
/// Layout:
/// - 0..4: Magic bytes (`MYPG`)
/// - 4: Page type (`u8`)
/// - 5: Flags (`u8`)
/// - 6..14: `PageId` (`u64` little-endian)
/// - 14..18: Checksum (`u32` little-endian)
/// - 18..20: `item_count` (`u16` little-endian)
/// - 20..22: `free_space_offset` (`u16` little-endian, points to lowest cell offset)
/// - 22..24: Reserved (`[u8; 2]`)
pub const PAGE_HEADER_SIZE: usize = 24;

/// Auxiliary pointer offset (byte 24..32).
/// Stores `next_leaf` for leaves, `first_child` for internal nodes, or `next_free` for free pages.
pub const AUX_PTR_OFFSET: usize = 24;
pub const AUX_PTR_SIZE: usize = 8;

/// Start of the slot array (byte 32).
pub const SLOTS_START_OFFSET: usize = AUX_PTR_OFFSET + AUX_PTR_SIZE;

/// Database page magic bytes: "MYPG".
pub const PAGE_MAGIC: [u8; 4] = *b"MYPG";

/// Bit flag indicating that a leaf slot's value is stored on an overflow chain.
pub const OVERFLOW_FLAG: u16 = 0x8000;

/// Maximum value payload size (in bytes) stored inline inside a 4 KiB slotted page.
/// Values exceeding this size are transparently chained onto dedicated overflow pages.
/// Aligned with SQLite (1002B) and standard 4-way B-Tree leaf fanout guarantees.
pub const MAX_INLINE_VAL_SIZE: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageType {
    Free = 0,
    BTreeLeaf = 1,
    BTreeInternal = 2,
    Meta = 3,
    Overflow = 4,
    VectorNode = 5,
    MeshletNode = 6,
    GaussianSplatNode = 7,
}

impl TryFrom<u8> for PageType {
    type Error = DbError;

    fn try_from(val: u8) -> Result<Self> {
        match val {
            0 => Ok(Self::Free),
            1 => Ok(Self::BTreeLeaf),
            2 => Ok(Self::BTreeInternal),
            3 => Ok(Self::Meta),
            4 => Ok(Self::Overflow),
            5 => Ok(Self::VectorNode),
            6 => Ok(Self::MeshletNode),
            7 => Ok(Self::GaussianSplatNode),
            other => Err(DbError::InvalidPageType(other)),
        }
    }
}

/// Parsed header of a database page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageHeader {
    pub page_type: PageType,
    pub flags: u8,
    pub page_id: PageId,
    pub checksum: u32,
    pub item_count: u16,
    pub free_space_offset: u16,
}

/// Fixed-size in-memory representation of a disk page.
#[derive(Clone, PartialEq, Eq)]
pub struct Page {
    pub data: [u8; PAGE_SIZE],
}

impl core::fmt::Debug for Page {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Page")
            .field("header", &self.read_header().ok())
            .field("aux_ptr", &self.aux_ptr())
            .finish_non_exhaustive()
    }
}

impl Default for Page {
    fn default() -> Self {
        Self {
            data: [0u8; PAGE_SIZE],
        }
    }
}

impl Page {
    #[must_use]
    pub fn new(page_id: PageId, page_type: PageType) -> Self {
        let mut page = Self::default();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "PAGE_SIZE is 4096 which safely fits in u16"
        )]
        let header = PageHeader {
            page_type,
            flags: 0,
            page_id,
            checksum: 0,
            item_count: 0,
            free_space_offset: PAGE_SIZE as u16,
        };
        page.write_header(&header);
        page.set_aux_ptr(PageId::INVALID);
        page.update_checksum();
        page
    }

    #[must_use]
    pub fn from_bytes(bytes: [u8; PAGE_SIZE]) -> Self {
        Self { data: bytes }
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.data[PAGE_HEADER_SIZE..]
    }

    pub fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.data[PAGE_HEADER_SIZE..]
    }

    #[must_use]
    pub fn aux_ptr(&self) -> PageId {
        let val = u64::from_le_bytes(
            self.data[AUX_PTR_OFFSET..AUX_PTR_OFFSET + AUX_PTR_SIZE]
                .try_into()
                .unwrap_or([0u8; 8]),
        );
        PageId(val)
    }

    pub fn set_aux_ptr(&mut self, ptr: PageId) {
        self.data[AUX_PTR_OFFSET..AUX_PTR_OFFSET + AUX_PTR_SIZE]
            .copy_from_slice(&ptr.0.to_le_bytes());
    }

    pub fn set_page_id(&mut self, pid: PageId) {
        self.data[6..14].copy_from_slice(&pid.0.to_le_bytes());
    }

    pub fn read_header(&self) -> Result<PageHeader> {
        if self.data[0..4] != PAGE_MAGIC {
            return Err(DbError::InvalidPageHeader(
                "Magic bytes do not match 'MYPG'".into(),
            ));
        }

        let page_type = PageType::try_from(self.data[4])?;
        let flags = self.data[5];
        let page_id_val = u64::from_le_bytes(
            self.data[6..14]
                .try_into()
                .map_err(|_| DbError::InvalidPageHeader("Invalid page_id length".into()))?,
        );
        let checksum = u32::from_le_bytes(
            self.data[14..18]
                .try_into()
                .map_err(|_| DbError::InvalidPageHeader("Invalid checksum length".into()))?,
        );
        let item_count = u16::from_le_bytes(
            self.data[18..20]
                .try_into()
                .map_err(|_| DbError::InvalidPageHeader("Invalid item_count length".into()))?,
        );
        let free_space_offset = u16::from_le_bytes(
            self.data[20..22]
                .try_into()
                .map_err(|_| DbError::InvalidPageHeader("Invalid offset length".into()))?,
        );

        Ok(PageHeader {
            page_type,
            flags,
            page_id: PageId(page_id_val),
            checksum,
            item_count,
            free_space_offset,
        })
    }

    pub fn write_header(&mut self, header: &PageHeader) {
        self.data[0..4].copy_from_slice(&PAGE_MAGIC);
        self.data[4] = header.page_type as u8;
        self.data[5] = header.flags;
        self.data[6..14].copy_from_slice(&header.page_id.0.to_le_bytes());
        self.data[14..18].copy_from_slice(&header.checksum.to_le_bytes());
        self.data[18..20].copy_from_slice(&header.item_count.to_le_bytes());
        self.data[20..22].copy_from_slice(&header.free_space_offset.to_le_bytes());
        self.data[22..24].copy_from_slice(&[0u8, 0u8]);
    }

    #[must_use]
    pub fn compute_checksum(&self) -> u32 {
        simd_crc::compute_page_checksum(&self.data)
    }

    pub fn update_checksum(&mut self) {
        let cs = self.compute_checksum();
        self.data[14..18].copy_from_slice(&cs.to_le_bytes());
    }

    pub fn verify_checksum(&self) -> Result<()> {
        let header = self.read_header()?;
        let computed = self.compute_checksum();
        if computed != header.checksum {
            return Err(DbError::ChecksumMismatch {
                page_id: header.page_id,
                expected: header.checksum,
                found: computed,
            });
        }
        Ok(())
    }

    // =========================================================================
    // SLOTTED PAGE ZERO-COPY PRIMITIVES
    // =========================================================================

    #[must_use]
    pub fn item_count(&self) -> usize {
        u16::from_le_bytes([self.data[18], self.data[19]]) as usize
    }

    #[must_use]
    pub fn free_space_offset(&self) -> usize {
        u16::from_le_bytes([self.data[20], self.data[21]]) as usize
    }

    #[must_use]
    pub fn free_space_remaining(&self) -> usize {
        let free_end = u16::from_le_bytes([self.data[20], self.data[21]]) as usize;
        let count = self.item_count();
        let slots_end = SLOTS_START_OFFSET + count * 2;
        free_end.saturating_sub(slots_end)
    }

    pub fn slot_offset(&self, index: usize) -> Result<usize> {
        let count = self.item_count();
        if index >= count {
            return Err(DbError::Corruption("Slot index out of bounds".into()));
        }
        let pos = SLOTS_START_OFFSET + index * 2;
        let offset = u16::from_le_bytes([self.data[pos], self.data[pos + 1]]) as usize;
        if !(SLOTS_START_OFFSET..PAGE_SIZE).contains(&offset) {
            return Err(DbError::Corruption("Corrupt slot offset in page".into()));
        }
        Ok(offset)
    }
}
