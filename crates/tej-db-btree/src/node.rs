use tej_db_core::{DbError, Page, PageId, PageType, Result};

/// B-Tree node representation in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BTreeNode {
    Leaf {
        keys: Vec<Vec<u8>>,
        values: Vec<Vec<u8>>,
        next_leaf: PageId,
    },
    Internal {
        keys: Vec<Vec<u8>>,
        children: Vec<PageId>,
    },
}

impl BTreeNode {
    pub const MAX_KEYS: usize = 32;

    #[must_use]
    pub fn new_leaf() -> Self {
        Self::Leaf {
            keys: Vec::new(),
            values: Vec::new(),
            next_leaf: PageId::INVALID,
        }
    }

    #[must_use]
    pub fn new_internal() -> Self {
        Self::Internal {
            keys: Vec::new(),
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_leaf(&self) -> bool {
        matches!(self, Self::Leaf { .. })
    }

    #[must_use]
    pub fn is_full(&self) -> bool {
        match self {
            Self::Leaf { keys, .. } | Self::Internal { keys, .. } => keys.len() >= Self::MAX_KEYS,
        }
    }

    pub fn serialize_to_page(&self, page_id: PageId) -> Result<Page> {
        let page_type = match self {
            Self::Leaf { .. } => PageType::BTreeLeaf,
            Self::Internal { .. } => PageType::BTreeInternal,
        };

        let mut page = Page::new(page_id, page_type);
        let payload = page.payload_mut();

        let mut offset = 0;
        match self {
            Self::Leaf {
                keys,
                values,
                next_leaf,
            } => {
                // Next leaf PageId (8 bytes)
                payload[offset..offset + 8].copy_from_slice(&next_leaf.0.to_le_bytes());
                offset += 8;

                // Entry count (2 bytes)
                let count = keys.len() as u16;
                payload[offset..offset + 2].copy_from_slice(&count.to_le_bytes());
                offset += 2;

                for (k, v) in keys.iter().zip(values.iter()) {
                    let k_len = k.len() as u16;
                    let v_len = v.len() as u16;

                    if offset + 4 + k.len() + v.len() > payload.len() {
                        return Err(DbError::CapacityExceeded(
                            "Leaf node entries exceed page payload size".into(),
                        ));
                    }

                    payload[offset..offset + 2].copy_from_slice(&k_len.to_le_bytes());
                    offset += 2;
                    payload[offset..offset + k.len()].copy_from_slice(k);
                    offset += k.len();

                    payload[offset..offset + 2].copy_from_slice(&v_len.to_le_bytes());
                    offset += 2;
                    payload[offset..offset + v.len()].copy_from_slice(v);
                    offset += v.len();
                }
            }
            Self::Internal { keys, children } => {
                let count = keys.len() as u16;
                payload[offset..offset + 2].copy_from_slice(&count.to_le_bytes());
                offset += 2;

                // First child
                if !children.is_empty() {
                    payload[offset..offset + 8].copy_from_slice(&children[0].0.to_le_bytes());
                    offset += 8;
                }

                for (i, k) in keys.iter().enumerate() {
                    let k_len = k.len() as u16;
                    let child_id = children.get(i + 1).copied().unwrap_or(PageId::INVALID);

                    if offset + 2 + k.len() + 8 > payload.len() {
                        return Err(DbError::CapacityExceeded(
                            "Internal node entries exceed page payload size".into(),
                        ));
                    }

                    payload[offset..offset + 2].copy_from_slice(&k_len.to_le_bytes());
                    offset += 2;
                    payload[offset..offset + k.len()].copy_from_slice(k);
                    offset += k.len();

                    payload[offset..offset + 8].copy_from_slice(&child_id.0.to_le_bytes());
                    offset += 8;
                }
            }
        }

        page.update_checksum();
        Ok(page)
    }

    pub fn deserialize_from_page(page: &Page) -> Result<Self> {
        let header = page.read_header()?;
        let payload = page.payload();
        let mut offset = 0;

        match header.page_type {
            PageType::BTreeLeaf => {
                let next_leaf_id = u64::from_le_bytes(
                    payload[offset..offset + 8]
                        .try_into()
                        .map_err(|_| DbError::InvalidPageHeader("Invalid next_leaf".into()))?,
                );
                offset += 8;

                let count = u16::from_le_bytes(
                    payload[offset..offset + 2]
                        .try_into()
                        .map_err(|_| DbError::InvalidPageHeader("Invalid count".into()))?,
                ) as usize;
                offset += 2;

                let mut keys = Vec::with_capacity(count);
                let mut values = Vec::with_capacity(count);

                for _ in 0..count {
                    let k_len = u16::from_le_bytes(
                        payload[offset..offset + 2]
                            .try_into()
                            .map_err(|_| DbError::InvalidPageHeader("Invalid k_len".into()))?,
                    ) as usize;
                    offset += 2;

                    let key = payload[offset..offset + k_len].to_vec();
                    offset += k_len;

                    let v_len = u16::from_le_bytes(
                        payload[offset..offset + 2]
                            .try_into()
                            .map_err(|_| DbError::InvalidPageHeader("Invalid v_len".into()))?,
                    ) as usize;
                    offset += 2;

                    let val = payload[offset..offset + v_len].to_vec();
                    offset += v_len;

                    keys.push(key);
                    values.push(val);
                }

                Ok(Self::Leaf {
                    keys,
                    values,
                    next_leaf: PageId(next_leaf_id),
                })
            }
            PageType::BTreeInternal => {
                let count = u16::from_le_bytes(
                    payload[offset..offset + 2]
                        .try_into()
                        .map_err(|_| DbError::InvalidPageHeader("Invalid count".into()))?,
                ) as usize;
                offset += 2;

                let mut keys = Vec::with_capacity(count);
                let mut children = Vec::with_capacity(count + 1);

                if count > 0 {
                    let first_child = u64::from_le_bytes(
                        payload[offset..offset + 8]
                            .try_into()
                            .map_err(|_| DbError::InvalidPageHeader("Invalid child".into()))?,
                    );
                    offset += 8;
                    children.push(PageId(first_child));
                }

                for _ in 0..count {
                    let k_len = u16::from_le_bytes(
                        payload[offset..offset + 2]
                            .try_into()
                            .map_err(|_| DbError::InvalidPageHeader("Invalid k_len".into()))?,
                    ) as usize;
                    offset += 2;

                    let key = payload[offset..offset + k_len].to_vec();
                    offset += k_len;

                    let child_id = u64::from_le_bytes(
                        payload[offset..offset + 8]
                            .try_into()
                            .map_err(|_| DbError::InvalidPageHeader("Invalid child".into()))?,
                    );
                    offset += 8;

                    keys.push(key);
                    children.push(PageId(child_id));
                }

                Ok(Self::Internal { keys, children })
            }
            other => Err(DbError::InvalidPageType(other as u8)),
        }
    }
}
