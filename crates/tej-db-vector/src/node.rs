use crate::types::{MAX_DEGREE_R, VectorId, VectorNodeMetadata};
use smallvec::SmallVec;
use tej_db_core::{DbError, Page, PageId, PageType, Result};

/// Metadata layout offsets within the 4 KiB `PageType::VectorNode` payload (starts at byte 24):
/// - 0..8   : VectorId (u64 LE)
/// - 8..16  : Overflow PageId (u64 LE, PageId::INVALID if none)
/// - 16..20 : Alpha scaling factor (f32 LE)
/// - 20..24 : Beta offset factor (f32 LE)
/// - 24..28 : Norm factor (f32 LE)
/// - 28..30 : Neighbor count R (u16 LE)
/// - 30..32 : Quantized code length in bytes (u16 LE)
/// - 32..33 : Flags (bit 0: is_deleted)
/// - 33..48 : Reserved alignment padding
const NODE_META_SIZE: usize = 48;

/// In-memory view of a deserialized `VectorNode`.
#[derive(Debug, Clone)]
pub struct VectorNode {
    pub metadata: VectorNodeMetadata,
    pub neighbors: SmallVec<[PageId; MAX_DEGREE_R]>,
    pub quantized_code: Vec<u8>,
}

impl VectorNode {
    /// Creates a new `VectorNode`.
    #[must_use]
    pub fn new(
        vector_id: VectorId,
        overflow_page: PageId,
        alpha: f32,
        beta: f32,
        norm: f32,
        quantized_code: Vec<u8>,
    ) -> Self {
        Self {
            metadata: VectorNodeMetadata {
                vector_id,
                overflow_page,
                alpha,
                beta,
                norm,
                neighbor_count: 0,
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "quantized codes are under 512B"
                )]
                quantized_bytes: quantized_code.len() as u16,
                is_deleted: false,
            },
            neighbors: SmallVec::new(),
            quantized_code,
        }
    }

    /// Serializes the node into a dedicated 4 KiB `PageType::VectorNode` page.
    pub fn write_to_page(&self, page_id: PageId, page: &mut Page) -> Result<()> {
        let mut p = Page::new(page_id, PageType::VectorNode);
        let payload = p.payload_mut();

        // 1. Write node metadata
        payload[0..8].copy_from_slice(&self.metadata.vector_id.0.to_le_bytes());
        payload[8..16].copy_from_slice(&self.metadata.overflow_page.0.to_le_bytes());
        payload[16..20].copy_from_slice(&self.metadata.alpha.to_le_bytes());
        payload[20..24].copy_from_slice(&self.metadata.beta.to_le_bytes());
        payload[24..28].copy_from_slice(&self.metadata.norm.to_le_bytes());
        let n_count = (self.neighbors.len() as u16).min(MAX_DEGREE_R as u16);
        payload[28..30].copy_from_slice(&n_count.to_le_bytes());
        let code_len = self.quantized_code.len() as u16;
        payload[30..32].copy_from_slice(&code_len.to_le_bytes());
        payload[32] = u8::from(self.metadata.is_deleted);
        payload[33..NODE_META_SIZE].fill(0);

        // 2. Write adjacency list
        let mut offset = NODE_META_SIZE;
        for &neighbor in self.neighbors.iter().take(MAX_DEGREE_R) {
            payload[offset..offset + 8].copy_from_slice(&neighbor.0.to_le_bytes());
            offset += 8;
        }

        // 3. Write quantized binary code
        if offset + self.quantized_code.len() > payload.len() {
            return Err(DbError::Corruption(
                "Vector node payload exceeds 4 KiB slotted page limit".into(),
            ));
        }
        payload[offset..offset + self.quantized_code.len()].copy_from_slice(&self.quantized_code);

        p.update_checksum();
        *page = p;
        Ok(())
    }

    /// Zero-copy read of node metadata and quantized code slice without allocating `Vec<u8>` or `SmallVec`.
    #[inline]
    pub fn visit_meta_and_code<R>(
        page: &Page,
        f: impl FnOnce(&VectorNodeMetadata, &[u8]) -> R,
    ) -> Result<R> {
        let header = page.read_header()?;
        if header.page_type != PageType::VectorNode {
            return Err(DbError::InvalidPageType(header.page_type as u8));
        }

        let payload = page.payload();
        if payload.len() < NODE_META_SIZE {
            return Err(DbError::Corruption("Truncated VectorNode payload".into()));
        }

        let vector_id =
            VectorId(u64::from_le_bytes(payload[0..8].try_into().map_err(
                |_| DbError::Corruption("VectorId read failed".into()),
            )?));
        let overflow_page =
            PageId(u64::from_le_bytes(payload[8..16].try_into().map_err(
                |_| DbError::Corruption("OverflowPage read failed".into()),
            )?));
        let alpha = f32::from_le_bytes(
            payload[16..20]
                .try_into()
                .map_err(|_| DbError::Corruption("Alpha read failed".into()))?,
        );
        let beta = f32::from_le_bytes(
            payload[20..24]
                .try_into()
                .map_err(|_| DbError::Corruption("Beta read failed".into()))?,
        );
        let norm = f32::from_le_bytes(
            payload[24..28]
                .try_into()
                .map_err(|_| DbError::Corruption("Norm read failed".into()))?,
        );
        let neighbor_count = u16::from_le_bytes(
            payload[28..30]
                .try_into()
                .map_err(|_| DbError::Corruption("Neighbor count read failed".into()))?,
        ) as usize;
        let quantized_bytes = u16::from_le_bytes(
            payload[30..32]
                .try_into()
                .map_err(|_| DbError::Corruption("Quantized bytes read failed".into()))?,
        ) as usize;
        let is_deleted = payload[32] != 0;

        let clamped_neighbors = neighbor_count.min(MAX_DEGREE_R);
        let code_offset = NODE_META_SIZE + clamped_neighbors * 8;
        if code_offset + quantized_bytes > payload.len() {
            return Err(DbError::Corruption(
                "Truncated quantized code payload".into(),
            ));
        }
        let meta = VectorNodeMetadata {
            vector_id,
            overflow_page,
            alpha,
            beta,
            norm,
            neighbor_count: clamped_neighbors as u16,
            quantized_bytes: quantized_bytes as u16,
            is_deleted,
        };
        Ok(f(
            &meta,
            &payload[code_offset..code_offset + quantized_bytes],
        ))
    }

    /// Reads neighbor PageIds into a caller-reused buffer without allocating `Vec<u8>`.
    #[inline]
    pub fn read_neighbors_into(
        page: &Page,
        out: &mut SmallVec<[PageId; MAX_DEGREE_R]>,
    ) -> Result<()> {
        let header = page.read_header()?;
        if header.page_type != PageType::VectorNode {
            return Err(DbError::InvalidPageType(header.page_type as u8));
        }
        let payload = page.payload();
        if payload.len() < NODE_META_SIZE {
            return Err(DbError::Corruption("Truncated VectorNode payload".into()));
        }
        let neighbor_count = u16::from_le_bytes(
            payload[28..30]
                .try_into()
                .map_err(|_| DbError::Corruption("Neighbor count read failed".into()))?,
        ) as usize;
        out.clear();
        let mut offset = NODE_META_SIZE;
        for _ in 0..neighbor_count.min(MAX_DEGREE_R) {
            if offset + 8 > payload.len() {
                break;
            }
            let pid = PageId(u64::from_le_bytes(
                payload[offset..offset + 8]
                    .try_into()
                    .map_err(|_| DbError::Corruption("Neighbor PageId read failed".into()))?,
            ));
            out.push(pid);
            offset += 8;
        }
        Ok(())
    }

    /// Deserializes a `VectorNode` from a 4 KiB page.
    pub fn from_page(page: &Page) -> Result<Self> {
        let header = page.read_header()?;
        if header.page_type != PageType::VectorNode {
            return Err(DbError::InvalidPageType(header.page_type as u8));
        }
        page.verify_checksum()?;

        let payload = page.payload();
        if payload.len() < NODE_META_SIZE {
            return Err(DbError::Corruption("Truncated VectorNode payload".into()));
        }

        let vector_id =
            VectorId(u64::from_le_bytes(payload[0..8].try_into().map_err(
                |_| DbError::Corruption("VectorId read failed".into()),
            )?));
        let overflow_page =
            PageId(u64::from_le_bytes(payload[8..16].try_into().map_err(
                |_| DbError::Corruption("OverflowPage read failed".into()),
            )?));
        let alpha = f32::from_le_bytes(
            payload[16..20]
                .try_into()
                .map_err(|_| DbError::Corruption("Alpha read failed".into()))?,
        );
        let beta = f32::from_le_bytes(
            payload[20..24]
                .try_into()
                .map_err(|_| DbError::Corruption("Beta read failed".into()))?,
        );
        let norm = f32::from_le_bytes(
            payload[24..28]
                .try_into()
                .map_err(|_| DbError::Corruption("Norm read failed".into()))?,
        );
        let neighbor_count = u16::from_le_bytes(
            payload[28..30]
                .try_into()
                .map_err(|_| DbError::Corruption("Neighbor count read failed".into()))?,
        ) as usize;
        let quantized_bytes = u16::from_le_bytes(
            payload[30..32]
                .try_into()
                .map_err(|_| DbError::Corruption("Quantized bytes read failed".into()))?,
        ) as usize;
        let is_deleted = payload[32] != 0;

        let mut offset = NODE_META_SIZE;
        let mut neighbors = SmallVec::with_capacity(neighbor_count.min(MAX_DEGREE_R));
        for _ in 0..neighbor_count.min(MAX_DEGREE_R) {
            if offset + 8 > payload.len() {
                break;
            }
            let pid = PageId(u64::from_le_bytes(
                payload[offset..offset + 8]
                    .try_into()
                    .map_err(|_| DbError::Corruption("Neighbor PageId read failed".into()))?,
            ));
            neighbors.push(pid);
            offset += 8;
        }

        if offset + quantized_bytes > payload.len() {
            return Err(DbError::Corruption(
                "Truncated quantized code payload".into(),
            ));
        }
        let quantized_code = payload[offset..offset + quantized_bytes].to_vec();

        Ok(Self {
            metadata: VectorNodeMetadata {
                vector_id,
                overflow_page,
                alpha,
                beta,
                norm,
                neighbor_count: neighbors.len() as u16,
                quantized_bytes: quantized_code.len() as u16,
                is_deleted,
            },
            neighbors,
            quantized_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_node_page_roundtrip() {
        let node = VectorNode::new(
            VectorId(1001),
            PageId(55),
            1.25,
            0.5,
            3.4,
            vec![0xAA, 0xBB, 0xCC, 0xDD],
        );
        let mut node = node;
        node.neighbors.push(PageId(10));
        node.neighbors.push(PageId(20));

        let mut page = Page::default();
        node.write_to_page(PageId(1), &mut page).unwrap();

        let decoded = VectorNode::from_page(&page).unwrap();
        assert_eq!(decoded.metadata.vector_id, VectorId(1001));
        assert_eq!(decoded.metadata.overflow_page, PageId(55));
        assert_eq!(decoded.neighbors.len(), 2);
        assert_eq!(decoded.neighbors[0], PageId(10));
        assert_eq!(decoded.neighbors[1], PageId(20));
        assert_eq!(decoded.quantized_code, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }
}
