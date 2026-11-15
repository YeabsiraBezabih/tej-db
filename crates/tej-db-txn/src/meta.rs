use smallvec::SmallVec;
use tej_db_core::{DbError, Page, PageId, PageType, Result, TxId};

pub const DB_META_MAGIC: [u8; 4] = *b"TEJD";
pub const CURRENT_DB_VERSION: u32 = 2;
pub const META_PAGE_ID: PageId = PageId(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMetaEntry {
    pub name: String,
    pub root_id: PageId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaPage {
    pub version: u32,
    pub active_root: PageId,
    pub catalog_root: PageId,
    pub next_tx_id: TxId,
    pub tables: SmallVec<[TableMetaEntry; 8]>,
}

impl MetaPage {
    #[must_use]
    pub fn new(active_root: PageId) -> Self {
        Self {
            version: CURRENT_DB_VERSION,
            active_root,
            catalog_root: PageId::INVALID,
            next_tx_id: TxId(1),
            tables: SmallVec::new(),
        }
    }

    #[must_use]
    pub fn with_tables(
        active_root: PageId,
        catalog_root: PageId,
        next_tx_id: TxId,
        tables: SmallVec<[TableMetaEntry; 8]>,
    ) -> Self {
        Self {
            version: CURRENT_DB_VERSION,
            active_root,
            catalog_root,
            next_tx_id,
            tables,
        }
    }

    pub fn to_page(&self) -> Result<Page> {
        let mut page = Page::new(META_PAGE_ID, PageType::Meta);
        self.write_to_page(&mut page)?;
        Ok(page)
    }

    pub fn write_to_page(&self, page: &mut Page) -> Result<()> {
        let payload = page.payload_mut();
        payload[0..4].copy_from_slice(&DB_META_MAGIC);
        payload[4..8].copy_from_slice(&self.version.to_le_bytes());
        payload[8..16].copy_from_slice(&self.active_root.0.to_le_bytes());
        payload[16..24].copy_from_slice(&self.catalog_root.0.to_le_bytes());
        payload[24..32].copy_from_slice(&self.next_tx_id.0.to_le_bytes());
        let table_count = (self.tables.len() as u16).to_le_bytes();
        payload[32..34].copy_from_slice(&table_count);

        let mut offset = 34;
        for entry in &self.tables {
            let name_bytes = entry.name.as_bytes();
            if name_bytes.len() > 255 || offset + 1 + name_bytes.len() + 8 > payload.len() {
                break;
            }
            payload[offset] = name_bytes.len() as u8;
            offset += 1;
            payload[offset..offset + name_bytes.len()].copy_from_slice(name_bytes);
            offset += name_bytes.len();
            payload[offset..offset + 8].copy_from_slice(&entry.root_id.0.to_le_bytes());
            offset += 8;
        }

        page.update_checksum();
        Ok(())
    }

    pub fn from_page(page: &Page) -> Result<Self> {
        page.verify_checksum()?;
        let header = page.read_header()?;
        if header.page_type != PageType::Meta {
            return Err(DbError::Corruption(
                "Page 0 is not a valid Meta page".into(),
            ));
        }

        let payload = page.payload();
        if payload[0..4] != DB_META_MAGIC {
            return Err(DbError::Corruption(
                "Database meta magic does not match 'TEJD'".into(),
            ));
        }

        let version = u32::from_le_bytes(
            payload[4..8]
                .try_into()
                .map_err(|_| DbError::Corruption("Invalid meta version".into()))?,
        );
        let active_root =
            PageId(u64::from_le_bytes(payload[8..16].try_into().map_err(
                |_| DbError::Corruption("Invalid root PageId".into()),
            )?));

        if version == 1 {
            let next_tx_id =
                TxId(u64::from_le_bytes(payload[16..24].try_into().map_err(
                    |_| DbError::Corruption("Invalid next TxId".into()),
                )?));
            return Ok(Self {
                version,
                active_root,
                catalog_root: PageId::INVALID,
                next_tx_id,
                tables: SmallVec::new(),
            });
        }

        let catalog_root =
            PageId(u64::from_le_bytes(payload[16..24].try_into().map_err(
                |_| DbError::Corruption("Invalid catalog root PageId".into()),
            )?));
        let next_tx_id =
            TxId(u64::from_le_bytes(payload[24..32].try_into().map_err(
                |_| DbError::Corruption("Invalid next TxId".into()),
            )?));

        let mut tables = SmallVec::new();
        if payload.len() >= 34 {
            let table_count =
                u16::from_le_bytes(payload[32..34].try_into().unwrap_or([0, 0])) as usize;
            let mut offset = 34;
            for _ in 0..table_count {
                if offset >= payload.len() {
                    break;
                }
                let name_len = payload[offset] as usize;
                offset += 1;
                if offset + name_len + 8 > payload.len() {
                    break;
                }
                if let Ok(name) = std::str::from_utf8(&payload[offset..offset + name_len]) {
                    offset += name_len;
                    let root_id = PageId(u64::from_le_bytes(
                        payload[offset..offset + 8].try_into().unwrap_or([0; 8]),
                    ));
                    offset += 8;
                    tables.push(TableMetaEntry {
                        name: name.to_string(),
                        root_id,
                    });
                } else {
                    break;
                }
            }
        }

        Ok(Self {
            version,
            active_root,
            catalog_root,
            next_tx_id,
            tables,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meta_page_v2_roundtrip_with_tables() {
        let mut tables = SmallVec::new();
        tables.push(TableMetaEntry {
            name: "users".into(),
            root_id: PageId(14),
        });
        tables.push(TableMetaEntry {
            name: "orders".into(),
            root_id: PageId(29),
        });

        let meta = MetaPage::with_tables(PageId(1), PageId(2), TxId(105), tables);
        let page = meta.to_page().expect("serialization should succeed");
        let decoded = MetaPage::from_page(&page).expect("deserialization should succeed");

        assert_eq!(meta, decoded);
    }

    #[test]
    fn test_meta_page_v1_backward_compatibility() {
        let mut page = Page::new(META_PAGE_ID, PageType::Meta);
        let payload = page.payload_mut();
        payload[0..4].copy_from_slice(&DB_META_MAGIC);
        payload[4..8].copy_from_slice(&1u32.to_le_bytes()); // version 1
        payload[8..16].copy_from_slice(&42u64.to_le_bytes()); // active_root
        payload[16..24].copy_from_slice(&99u64.to_le_bytes()); // next_tx_id
        page.update_checksum();

        let decoded = MetaPage::from_page(&page).expect("v1 page should decode");
        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.active_root, PageId(42));
        assert_eq!(decoded.catalog_root, PageId::INVALID);
        assert_eq!(decoded.next_tx_id, TxId(99));
        assert!(decoded.tables.is_empty());
    }
}
