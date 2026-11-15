use crate::types::VectorId;
use smallvec::SmallVec;
use tej_db_core::{DbError, Page, PageId, PageType, Result};

/// Maximum degree of neighbors stored per vector node.
pub const PACKED_NODE_MAX_DEGREE: usize = 64;

/// A packed vector node representation storing neighbors as dense node indices.
#[derive(Debug, Clone, PartialEq)]
pub struct PackedVectorNode {
    pub node_idx: u32,
    pub vector_id: VectorId,
    pub overflow_page: PageId,
    pub alpha: f32,
    pub beta: f32,
    pub norm: f32,
    pub neighbors: SmallVec<[u32; PACKED_NODE_MAX_DEGREE]>,
    pub quantized_code: Vec<u8>,
}

impl PackedVectorNode {
    /// Computes the serialized binary size of this node in bytes.
    #[must_use]
    pub fn serialized_size(&self) -> usize {
        // Header: node_idx(4) + vector_id(8) + overflow_page(8) + alpha(4) + beta(4) + norm(4)
        //         + neighbor_count(2) + code_len(2) = 36 bytes
        36 + (self.neighbors.len() * 4) + self.quantized_code.len()
    }

    /// Serializes this node into a byte buffer.
    pub fn serialize_into(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.node_idx.to_le_bytes());
        buf.extend_from_slice(&self.vector_id.0.to_le_bytes());
        buf.extend_from_slice(&self.overflow_page.0.to_le_bytes());
        buf.extend_from_slice(&self.alpha.to_le_bytes());
        buf.extend_from_slice(&self.beta.to_le_bytes());
        buf.extend_from_slice(&self.norm.to_le_bytes());
        #[expect(clippy::cast_possible_truncation, reason = "neighbors fit in u16")]
        let n_len = self.neighbors.len() as u16;
        buf.extend_from_slice(&n_len.to_le_bytes());
        #[expect(clippy::cast_possible_truncation, reason = "code fits in u16")]
        let c_len = self.quantized_code.len() as u16;
        buf.extend_from_slice(&c_len.to_le_bytes());

        for &nb in &self.neighbors {
            buf.extend_from_slice(&nb.to_le_bytes());
        }
        buf.extend_from_slice(&self.quantized_code);
    }

    /// Deserializes a node from a byte slice.
    pub fn deserialize_from(buf: &[u8]) -> Result<Self> {
        if buf.len() < 36 {
            return Err(DbError::Corruption("Truncated PackedVectorNode".into()));
        }

        let node_idx = u32::from_le_bytes(
            buf[0..4]
                .try_into()
                .map_err(|_| DbError::Corruption("node_idx read failed".into()))?,
        );
        let vector_id =
            VectorId(u64::from_le_bytes(buf[4..12].try_into().map_err(|_| {
                DbError::Corruption("vector_id read failed".into())
            })?));
        let overflow_page =
            PageId(u64::from_le_bytes(buf[12..20].try_into().map_err(
                |_| DbError::Corruption("overflow_page read failed".into()),
            )?));
        let alpha = f32::from_le_bytes(
            buf[20..24]
                .try_into()
                .map_err(|_| DbError::Corruption("alpha read failed".into()))?,
        );
        let beta = f32::from_le_bytes(
            buf[24..28]
                .try_into()
                .map_err(|_| DbError::Corruption("beta read failed".into()))?,
        );
        let norm = f32::from_le_bytes(
            buf[28..32]
                .try_into()
                .map_err(|_| DbError::Corruption("norm read failed".into()))?,
        );
        let n_len = u16::from_le_bytes(
            buf[32..34]
                .try_into()
                .map_err(|_| DbError::Corruption("n_len read failed".into()))?,
        ) as usize;
        let c_len = u16::from_le_bytes(
            buf[34..36]
                .try_into()
                .map_err(|_| DbError::Corruption("c_len read failed".into()))?,
        ) as usize;

        let expected_size = 36 + (n_len * 4) + c_len;
        if buf.len() < expected_size {
            return Err(DbError::Corruption(
                "PackedVectorNode payload shorter than declared lengths".into(),
            ));
        }

        let mut offset = 36;
        let mut neighbors = SmallVec::with_capacity(n_len);
        for _ in 0..n_len {
            let nb = u32::from_le_bytes(
                buf[offset..offset + 4]
                    .try_into()
                    .map_err(|_| DbError::Corruption("neighbor read failed".into()))?,
            );
            neighbors.push(nb);
            offset += 4;
        }

        let quantized_code = buf[offset..offset + c_len].to_vec();

        Ok(Self {
            node_idx,
            vector_id,
            overflow_page,
            alpha,
            beta,
            norm,
            neighbors,
            quantized_code,
        })
    }
}

/// A 4 KiB slotted page holding multiple `PackedVectorNode` entries.
#[derive(Debug, Clone)]
pub struct PackedVectorPage {
    pub nodes: Vec<PackedVectorNode>,
}

impl Default for PackedVectorPage {
    fn default() -> Self {
        Self::new()
    }
}

impl PackedVectorPage {
    /// Creates a new empty packed page.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(16),
        }
    }

    /// Checks if adding a node with `additional_bytes` will fit in the 4 KiB page.
    #[must_use]
    pub fn can_fit(&self, additional_bytes: usize) -> bool {
        // Page payload is 4096 - 64 = 4032 bytes
        // Header in payload: node_count (2B) + slots (node_count * 4B)
        let current_slots_size = 2 + (self.nodes.len() * 4);
        let current_payload_size: usize = self
            .nodes
            .iter()
            .map(PackedVectorNode::serialized_size)
            .sum();

        let new_slots_size = current_slots_size + 4;
        let new_payload_size = current_payload_size + additional_bytes;

        new_slots_size + new_payload_size <= 4032
    }

    /// Appends a node to the packed page.
    pub fn push_node(&mut self, node: PackedVectorNode) -> Result<()> {
        if !self.can_fit(node.serialized_size()) {
            return Err(DbError::CapacityExceeded("PackedVectorPage full".into()));
        }
        self.nodes.push(node);
        Ok(())
    }

    /// Serializes this packed page into a 4 KiB `Page`.
    pub fn write_to_page(&self, page_id: PageId, page: &mut Page) -> Result<()> {
        let mut p = Page::new(page_id, PageType::VectorNode);
        let payload = p.payload_mut();

        #[expect(clippy::cast_possible_truncation, reason = "nodes per page < 256")]
        let node_count = self.nodes.len() as u16;
        payload[0..2].copy_from_slice(&node_count.to_le_bytes());

        let mut slot_dir_offset = 2;
        let mut cell_write_offset = payload.len();

        for node in &self.nodes {
            let mut node_bytes = Vec::with_capacity(node.serialized_size());
            node.serialize_into(&mut node_bytes);

            cell_write_offset = cell_write_offset.saturating_sub(node_bytes.len());
            payload[cell_write_offset..cell_write_offset + node_bytes.len()]
                .copy_from_slice(&node_bytes);

            #[expect(clippy::cast_possible_truncation, reason = "page offset fits in u16")]
            let offset_u16 = cell_write_offset as u16;
            #[expect(clippy::cast_possible_truncation, reason = "node len fits in u16")]
            let len_u16 = node_bytes.len() as u16;

            payload[slot_dir_offset..slot_dir_offset + 2]
                .copy_from_slice(&offset_u16.to_le_bytes());
            payload[slot_dir_offset + 2..slot_dir_offset + 4]
                .copy_from_slice(&len_u16.to_le_bytes());
            slot_dir_offset += 4;
        }

        p.update_checksum();
        *page = p;
        Ok(())
    }

    /// Deserializes a `PackedVectorPage` from a 4 KiB `Page`.
    pub fn from_page(page: &Page) -> Result<Self> {
        let header = page.read_header()?;
        if header.page_type != PageType::VectorNode {
            return Err(DbError::InvalidPageType(header.page_type as u8));
        }
        page.verify_checksum()?;

        let payload = page.payload();
        if payload.len() < 2 {
            return Err(DbError::Corruption("Truncated PackedVectorPage".into()));
        }

        let node_count = u16::from_le_bytes(
            payload[0..2]
                .try_into()
                .map_err(|_| DbError::Corruption("node_count read failed".into()))?,
        ) as usize;

        let mut nodes = Vec::with_capacity(node_count);
        let mut slot_offset = 2;

        for _ in 0..node_count {
            if slot_offset + 4 > payload.len() {
                return Err(DbError::Corruption("Slot directory exceeds payload".into()));
            }
            let offset = u16::from_le_bytes(
                payload[slot_offset..slot_offset + 2]
                    .try_into()
                    .map_err(|_| DbError::Corruption("offset read failed".into()))?,
            ) as usize;
            let len = u16::from_le_bytes(
                payload[slot_offset + 2..slot_offset + 4]
                    .try_into()
                    .map_err(|_| DbError::Corruption("len read failed".into()))?,
            ) as usize;
            slot_offset += 4;

            if offset + len > payload.len() {
                return Err(DbError::Corruption("Cell exceeds payload bounds".into()));
            }

            let node = PackedVectorNode::deserialize_from(&payload[offset..offset + len])?;
            nodes.push(node);
        }

        Ok(Self { nodes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packed_vector_page_roundtrip() {
        let mut packed = PackedVectorPage::new();

        let node1 = PackedVectorNode {
            node_idx: 0,
            vector_id: VectorId(10),
            overflow_page: PageId::INVALID,
            alpha: 1.0,
            beta: 0.1,
            norm: 3.5,
            neighbors: smallvec::smallvec![1, 2, 3],
            quantized_code: vec![0xAA; 16],
        };
        let node2 = PackedVectorNode {
            node_idx: 1,
            vector_id: VectorId(20),
            overflow_page: PageId::INVALID,
            alpha: 2.0,
            beta: 0.2,
            norm: 4.5,
            neighbors: smallvec::smallvec![0],
            quantized_code: vec![0xBB; 16],
        };

        assert!(packed.can_fit(node1.serialized_size()));
        packed.push_node(node1.clone()).unwrap();
        assert!(packed.can_fit(node2.serialized_size()));
        packed.push_node(node2.clone()).unwrap();

        let mut page = Page::default();
        packed.write_to_page(PageId(5), &mut page).unwrap();

        let loaded = PackedVectorPage::from_page(&page).unwrap();
        assert_eq!(loaded.nodes.len(), 2);
        assert_eq!(loaded.nodes[0], node1);
        assert_eq!(loaded.nodes[1], node2);
    }
}
