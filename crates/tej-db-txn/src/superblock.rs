use crate::meta::{DB_META_MAGIC, MetaPage};
use std::sync::Arc;
use tej_db_core::{DbError, Page, PageId, PageType, Result, Storage, TxId};
use tej_db_pager::PageCache;

/// Fixed PageId for Superblock A (Ping slot).
pub const SUPERBLOCK_A: PageId = PageId(0);

/// Fixed PageId for Superblock B (Pong slot).
pub const SUPERBLOCK_B: PageId = PageId(1);

/// Selects the alternating ping-pong superblock slot for the next transaction commit.
///
/// If `current_slot` is known, alternates to the opposite slot. If unknown or invalid,
/// uses parity of `next_tx_id` to distribute across alternating slots.
#[must_use]
pub fn choose_commit_superblock(current_slot: PageId, next_tx_id: TxId) -> PageId {
    match current_slot {
        SUPERBLOCK_A => SUPERBLOCK_B,
        SUPERBLOCK_B => SUPERBLOCK_A,
        _ => {
            if next_tx_id.0 % 2 == 0 {
                SUPERBLOCK_A
            } else {
                SUPERBLOCK_B
            }
        }
    }
}

/// Reads and validates a candidate superblock page.
///
/// Returns `Ok(Some(meta))` if the page exists, has valid checksum, type `Meta`, and magic bytes.
/// Returns `Ok(None)` if the page does not exist or has an uninitialized/unallocated state.
/// Returns `Err(DbError)` if corrupted.
pub fn read_candidate_superblock<S: Storage>(
    cache: &Arc<PageCache<S>>,
    id: PageId,
) -> Result<Option<MetaPage>> {
    let page = match cache.get_page(id) {
        Ok(p) => p,
        Err(
            DbError::PageNotFound(_)
            | DbError::InvalidPageHeader(_)
            | DbError::ChecksumMismatch { .. }
            | DbError::Corruption(_),
        ) => return Ok(None),
        Err(e) => return Err(e),
    };

    if let Ok(header) = page.read_header() {
        if header.page_type != PageType::Meta {
            return Ok(None);
        }
    } else {
        return Ok(None);
    }

    match MetaPage::from_page(&page) {
        Ok(meta) => Ok(Some(meta)),
        Err(DbError::Corruption(_) | DbError::ChecksumMismatch { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Loads the active superblock by inspecting both Page 0 (Superblock A) and Page 1 (Superblock B).
///
/// Recovery logic:
/// 1. If both superblocks are valid, selects the one with the higher `next_tx_id`.
/// 2. If only one is valid, adopts the valid one (safely recovering from power loss mid-write of the other).
/// 3. If neither is valid:
///    - If storage is empty (zero pages), returns `Ok(None)` to signal initialization is required.
///    - If storage contains existing pages, returns a fatal `DbError::Corruption` to prevent data loss.
pub fn load_active_superblock<S: Storage>(
    cache: &Arc<PageCache<S>>,
) -> Result<Option<(MetaPage, PageId)>> {
    let sb_a = read_candidate_superblock(cache, SUPERBLOCK_A)?;
    let sb_b = read_candidate_superblock(cache, SUPERBLOCK_B)?;

    match (sb_a, sb_b) {
        (Some(meta_a), Some(meta_b)) => {
            if meta_b.next_tx_id.0 > meta_a.next_tx_id.0 {
                Ok(Some((meta_b, SUPERBLOCK_B)))
            } else {
                Ok(Some((meta_a, SUPERBLOCK_A)))
            }
        }
        (Some(meta_a), None) => Ok(Some((meta_a, SUPERBLOCK_A))),
        (None, Some(meta_b)) => Ok(Some((meta_b, SUPERBLOCK_B))),
        (None, None) => {
            let total_pages = cache.storage_page_count();
            if total_pages == 0 {
                Ok(None)
            } else {
                let page_a = cache.get_page(SUPERBLOCK_A);
                let is_unformatted = match page_a {
                    Ok(p) => p.data.iter().all(|&b| b == 0),
                    Err(_) => false,
                };
                if is_unformatted {
                    Ok(None)
                } else {
                    Err(DbError::Corruption(
                        "Database corruption: Both superblocks (Page 0 and Page 1) are invalid or missing"
                            .into(),
                    ))
                }
            }
        }
    }
}

/// Initializes both Superblock A and Superblock B for a new database.
pub fn init_superblocks<S: Storage>(
    cache: &Arc<PageCache<S>>,
    initial_root: PageId,
) -> Result<(MetaPage, PageId)> {
    let slot_a = cache.allocate_page()?; // Page 0
    let slot_b = cache.allocate_page()?; // Page 1

    let mut initial_meta = MetaPage::new(initial_root);
    initial_meta.next_tx_id = TxId(1);

    // Write Superblock A
    let mut page_a = Page::new(slot_a, PageType::Meta);
    initial_meta.write_to_page(&mut page_a)?;
    cache.write_page(page_a)?;

    // Write Superblock B as backup with identical state
    let mut page_b = Page::new(slot_b, PageType::Meta);
    initial_meta.write_to_page(&mut page_b)?;
    cache.write_page(page_b)?;

    cache.flush()?;
    Ok((initial_meta, SUPERBLOCK_A))
}

/// Atomically writes a new commit metadata state into the target superblock slot.
pub fn write_superblock<S: Storage>(
    cache: &Arc<PageCache<S>>,
    slot: PageId,
    meta: &MetaPage,
) -> Result<()> {
    cache.with_page_mut(slot, |page| {
        let payload = page.payload_mut();
        payload[0..4].copy_from_slice(&DB_META_MAGIC);
        payload[4..8].copy_from_slice(&meta.version.to_le_bytes());
        payload[8..16].copy_from_slice(&meta.active_root.0.to_le_bytes());
        payload[16..24].copy_from_slice(&meta.catalog_root.0.to_le_bytes());
        payload[24..32].copy_from_slice(&meta.next_tx_id.0.to_le_bytes());
        let table_count = (meta.tables.len() as u16).to_le_bytes();
        payload[32..34].copy_from_slice(&table_count);

        let mut offset = 34;
        for entry in &meta.tables {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tej_db_pager::MemStorage;

    #[test]
    fn test_superblock_ping_pong_selection() {
        assert_eq!(
            choose_commit_superblock(SUPERBLOCK_A, TxId(2)),
            SUPERBLOCK_B
        );
        assert_eq!(
            choose_commit_superblock(SUPERBLOCK_B, TxId(3)),
            SUPERBLOCK_A
        );
        assert_eq!(
            choose_commit_superblock(PageId::INVALID, TxId(2)),
            SUPERBLOCK_A
        );
        assert_eq!(
            choose_commit_superblock(PageId::INVALID, TxId(3)),
            SUPERBLOCK_B
        );
    }

    #[test]
    fn test_superblock_torn_write_recovery() {
        let storage = MemStorage::new();
        let cache = Arc::new(PageCache::new(storage, 10));

        // 1. Initialize superblocks
        let (mut meta, _) = init_superblocks(&cache, PageId(2)).unwrap();
        assert_eq!(meta.next_tx_id, TxId(1));

        // 2. Commit Tx 2 to Superblock B
        meta.next_tx_id = TxId(2);
        write_superblock(&cache, SUPERBLOCK_B, &meta).unwrap();

        // Active superblock should now be Superblock B (Tx 2 > Tx 1)
        let (loaded, active_id) = load_active_superblock(&cache).unwrap().unwrap();
        assert_eq!(active_id, SUPERBLOCK_B);
        assert_eq!(loaded.next_tx_id, TxId(2));

        // 3. Simulate torn write / corruption on Superblock B (e.g. power failure during write)
        cache
            .with_page_mut(SUPERBLOCK_B, |page| {
                page.data[20] ^= 0xFF; // corrupt byte
                // Do NOT call update_checksum(), simulating torn write
                Ok(())
            })
            .unwrap();

        // 4. Recover: should gracefully fall back to Superblock A (Tx 1)
        let (recovered, recovered_id) = load_active_superblock(&cache).unwrap().unwrap();
        assert_eq!(recovered_id, SUPERBLOCK_A);
        assert_eq!(recovered.next_tx_id, TxId(1));
    }
}
