use super::*;
use tej_db_core::{PAGE_SIZE, Page, PageId, PageType, Storage};

#[test]
fn test_mem_storage() {
    let mut storage = MemStorage::new();
    let page_id = storage.allocate_page().unwrap();
    assert_eq!(page_id, PageId(0));

    let page = Page::new(page_id, PageType::BTreeLeaf);
    storage.write_page(page_id, page.as_slice()).unwrap();

    let mut buf = [0u8; PAGE_SIZE];
    storage.read_page(page_id, &mut buf).unwrap();
    assert_eq!(&buf[0..4], b"MYPG");
}

#[test]
fn test_file_storage() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("test.db");

    {
        let mut storage = FileStorage::open(&path).unwrap();
        let pid = storage.allocate_page().unwrap();
        let page = Page::new(pid, PageType::BTreeLeaf);
        storage.write_page(pid, page.as_slice()).unwrap();
        storage.sync().unwrap();
    }

    {
        let storage = FileStorage::open(&path).unwrap();
        assert_eq!(storage.page_count(), 1);
        let mut buf = [0u8; PAGE_SIZE];
        storage.read_page(PageId(0), &mut buf).unwrap();
        let loaded = Page::from_bytes(buf);
        assert!(loaded.verify_checksum().is_ok());
    }
}

#[test]
fn test_page_cache_lru() {
    let storage = MemStorage::new();
    let cache = PageCache::new(storage, 2); // capacity 2

    let p1 = cache.allocate_page().unwrap();
    let p2 = cache.allocate_page().unwrap();
    let p3 = cache.allocate_page().unwrap();

    let mut page1 = Page::new(p1, PageType::BTreeLeaf);
    page1.payload_mut()[0] = 10;
    cache.write_page(page1).unwrap();

    let mut page2 = Page::new(p2, PageType::BTreeLeaf);
    page2.payload_mut()[0] = 20;
    cache.write_page(page2).unwrap();

    // Access p1 to make p2 the oldest
    let _ = cache.get_page(p1).unwrap();

    // Write p3 -> should evict p2
    let page3 = Page::new(p3, PageType::BTreeLeaf);
    cache.write_page(page3).unwrap();

    let (_, _, count) = cache.stats();
    assert_eq!(count, 2);

    // Fetching p1 should hit cache
    let (hits_before, _, _) = cache.stats();
    let fetched_p1 = cache.get_page(p1).unwrap();
    let (hits_after, _, _) = cache.stats();
    assert_eq!(hits_after, hits_before + 1);
    assert_eq!(fetched_p1.payload()[0], 10);
}

#[test]
fn test_free_list_recycling() {
    let storage = MemStorage::new();
    let cache = PageCache::new(storage, 10);

    let p1 = cache.allocate_page().unwrap();
    let p2 = cache.allocate_page().unwrap();
    assert_eq!(p1, PageId(0));
    assert_eq!(p2, PageId(1));

    // Free p1
    cache.free_page(p1);
    assert_eq!(cache.free_list_len(), 1);

    // Next allocation should reuse p1 instead of allocating p3
    let p3 = cache.allocate_page().unwrap();
    assert_eq!(p3, p1);
    assert_eq!(cache.free_list_len(), 0);
}

#[cfg(feature = "mmap")]
#[test]
fn test_mmap_storage() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("mmap_test.db");

    let mut storage = MmapStorage::open(&path).unwrap();
    let p0 = PageId(0);
    let page = Page::new(p0, PageType::BTreeLeaf);
    storage.write_page(p0, page.as_slice()).unwrap();
    storage.sync().unwrap();

    let mut buf = [0u8; PAGE_SIZE];
    storage.read_page(p0, &mut buf).unwrap();
    let loaded = Page::from_bytes(buf);
    assert!(loaded.verify_checksum().is_ok());
}

#[cfg(feature = "compression")]
#[test]
fn test_lz4_compression_roundtrip() {
    let payload =
        b"Hello, world! This is a test for high efficiency LZ4 page compression in TejDb.";
    let compressed = compress_page(payload);
    assert_ne!(compressed.len(), 0);
    let decompressed = decompress_page(&compressed).unwrap();
    assert_eq!(payload.as_slice(), decompressed.as_slice());
}

#[cfg(feature = "compression")]
#[test]
fn test_lz4_decompression_bomb_rejection() {
    // 0xFFFF_FFFF prepended length -> 4 GiB allocation attempt
    let mut bomb = vec![0xFF, 0xFF, 0xFF, 0xFF];
    bomb.extend_from_slice(&[0x12, 0x34, 0x56]);
    let res = decompress_page(&bomb);
    assert!(res.is_err(), "Must reject decompression bomb without OOM");
}

#[cfg(feature = "compression")]
#[test]
fn test_compressed_storage_full_lifecycle() {
    let mem = MemStorage::new();
    let mut storage = CompressedStorage::new(mem);

    let pid0 = storage.allocate_page().unwrap();
    let mut page0 = Page::new(pid0, PageType::BTreeLeaf);
    // Fill with compressible data
    page0.payload_mut()[..200].fill(0xAA);
    page0.update_checksum();

    storage.write_page(pid0, page0.as_slice()).unwrap();
    storage.sync().unwrap();

    let mut read_buf = [0u8; PAGE_SIZE];
    storage.read_page(pid0, &mut read_buf).unwrap();
    let loaded = Page::from_bytes(read_buf);
    assert_eq!(loaded.as_slice(), page0.as_slice());
    assert!(loaded.verify_checksum().is_ok());

    // Test incompressible / high-entropy fallback
    let pid1 = storage.allocate_page().unwrap();
    let mut page1 = Page::new(pid1, PageType::BTreeLeaf);
    for (i, b) in page1.payload_mut().iter_mut().enumerate() {
        *b = (i * 179 + 37) as u8;
    }
    page1.update_checksum();

    storage.write_page(pid1, page1.as_slice()).unwrap();
    let mut read_buf1 = [0u8; PAGE_SIZE];
    storage.read_page(pid1, &mut read_buf1).unwrap();
    let loaded1 = Page::from_bytes(read_buf1);
    assert_eq!(loaded1.as_slice(), page1.as_slice());
    assert!(loaded1.verify_checksum().is_ok());
}

#[cfg(feature = "encryption")]
#[test]
fn test_chacha20_encryption_roundtrip() {
    let key = [0x42u8; 32];
    let nonce = [0x17u8; 12];
    let aad = b"test_page_aad";
    let secret_data = b"ACID transaction confidential payload";
    let ciphertext = encrypt_page_data(secret_data, &key, &nonce, aad).unwrap();
    assert_ne!(ciphertext.as_slice(), secret_data.as_slice());
    let plaintext = decrypt_page_data(&ciphertext, &key, &nonce, aad).unwrap();
    assert_eq!(plaintext.as_slice(), secret_data.as_slice());

    // Tampered AAD fails decryption
    let tampered_aad = b"wrong_page_aad";
    assert!(decrypt_page_data(&ciphertext, &key, &nonce, tampered_aad).is_err());
}

#[cfg(feature = "encryption")]
#[test]
fn test_encrypted_storage_full_lifecycle() {
    let key = [0x5Au8; 32];
    let mem = MemStorage::new();
    let mut storage = EncryptedStorage::new(mem, key);

    let pid0 = storage.allocate_page().unwrap();
    let mut page0 = Page::new(pid0, PageType::BTreeLeaf);
    page0.payload_mut()[..100].copy_from_slice(&[0x42; 100]);
    page0.update_checksum();

    // Write encrypted page
    storage.write_page(pid0, page0.as_slice()).unwrap();
    storage.sync().unwrap();

    // Read decrypted page
    let mut read_buf = [0u8; PAGE_SIZE];
    storage.read_page(pid0, &mut read_buf).unwrap();
    let loaded = Page::from_bytes(read_buf);
    assert_eq!(loaded.as_slice(), page0.as_slice());
    assert!(loaded.verify_checksum().is_ok());

    // Verify underlying storage is actually encrypted
    let raw_mem = storage.into_inner();
    let mut raw_buf = [0u8; PAGE_SIZE];
    // Physical data page for logical page 0 is page 1
    raw_mem.read_page(PageId(1), &mut raw_buf).unwrap();
    assert_ne!(raw_buf, *page0.as_slice());
}

#[test]
fn test_fast_page_id_hasher() {
    use std::hash::{BuildHasher, Hasher};
    let build = FastPageIdBuildHasher;
    let mut hasher1 = build.build_hasher();
    let mut hasher2 = build.build_hasher();

    hasher1.write_u64(42);
    hasher2.write_u64(43);

    let h1 = hasher1.finish();
    let h2 = hasher2.finish();
    assert_ne!(h1, h2);
}

#[test]
fn test_invalid_page_read_error() {
    let storage = MemStorage::new();
    let mut buf = [0u8; PAGE_SIZE];
    // Read out of bounds page
    let res = storage.read_page(PageId(9999), &mut buf);
    assert!(res.is_err());
}

#[cfg(feature = "encryption")]
#[test]
fn test_encrypted_storage_fresh_nonces_and_debug_redacted() {
    let key = [0x7Bu8; 32];
    let mem = MemStorage::new();
    let mut storage = EncryptedStorage::new(mem, key);

    // Verify debug formatting redacts key
    let debug_str = format!("{storage:?}");
    assert!(debug_str.contains("[REDACTED]"));
    assert!(!debug_str.contains("123")); // 0x7B = 123

    let pid = storage.allocate_page().unwrap();
    let mut page = Page::new(pid, PageType::BTreeLeaf);
    page.payload_mut()[..10].copy_from_slice(b"version_01");
    page.update_checksum();

    // First write
    storage.write_page(pid, page.as_slice()).unwrap();
    storage.sync().unwrap();

    let mut raw_buf1 = [0u8; PAGE_SIZE];
    // Read raw physical data page (physical page 1)
    let raw_inner1 = storage.into_inner();
    raw_inner1.read_page(PageId(1), &mut raw_buf1).unwrap();

    // Reconstruct storage and write version 2 to same logical page
    let mut storage = EncryptedStorage::new(raw_inner1, key);
    page.payload_mut()[..10].copy_from_slice(b"version_02");
    page.update_checksum();
    storage.write_page(pid, page.as_slice()).unwrap();
    storage.sync().unwrap();

    let mut raw_buf2 = [0u8; PAGE_SIZE];
    let raw_inner2 = storage.into_inner();
    raw_inner2.read_page(PageId(1), &mut raw_buf2).unwrap();

    // Ciphertexts of subsequent writes must be distinct due to fresh CSPRNG nonce
    assert_ne!(raw_buf1, raw_buf2);
}

#[cfg(feature = "encryption")]
#[test]
fn test_encrypted_storage_superblock_isolation() {
    let key = [0x5A; 32];
    let raw = MemStorage::new();
    let mut storage = EncryptedStorage::new(raw, key);

    // Allocate SB0 (logical 0), SB1 (logical 1), Data 0 (logical 2)
    let sb0 = storage.allocate_page().unwrap();
    let sb1 = storage.allocate_page().unwrap();
    let data0 = storage.allocate_page().unwrap();

    assert_eq!(sb0, PageId(0));
    assert_eq!(sb1, PageId(1));
    assert_eq!(data0, PageId(2));

    let mut p0 = Page::new(sb0, PageType::BTreeLeaf);
    p0.payload_mut()[..8].copy_from_slice(b"SB0_DATA");
    storage.write_page(sb0, p0.as_slice()).unwrap();

    let mut p1 = Page::new(sb1, PageType::BTreeLeaf);
    p1.payload_mut()[..8].copy_from_slice(b"SB1_DATA");
    storage.write_page(sb1, p1.as_slice()).unwrap();

    // Verify roundtrip read
    let mut read_buf = [0u8; PAGE_SIZE];
    storage.read_page(sb0, &mut read_buf).unwrap();
    assert_eq!(&Page::from_bytes(read_buf).payload()[..8], b"SB0_DATA");

    storage.read_page(sb1, &mut read_buf).unwrap();
    assert_eq!(&Page::from_bytes(read_buf).payload()[..8], b"SB1_DATA");

    // Inspect underlying physical storage:
    // Physical page 0: SB0 tag page
    // Physical page 1: SB0 ciphertext page
    // Physical page 2: SB1 tag page
    // Physical page 3: SB1 ciphertext page
    let raw_inner = storage.into_inner();
    assert!(raw_inner.page_count() >= 4);

    // Corrupting SB1 tag page (physical page 2) must NOT affect SB0 decryption
    let mut modified_raw = raw_inner;
    let corrupt_tag = [0xFF; PAGE_SIZE];
    modified_raw.write_page(PageId(2), &corrupt_tag).unwrap();

    let reloaded = EncryptedStorage::new(modified_raw, key);
    let mut p0_check = [0u8; PAGE_SIZE];
    assert!(
        reloaded.read_page(sb0, &mut p0_check).is_ok(),
        "SB0 must decrypt cleanly despite SB1 tag page corruption"
    );
    assert_eq!(&Page::from_bytes(p0_check).payload()[..8], b"SB0_DATA");
}

#[test]
fn test_file_lock_exclusivity() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("exclusive.db");

    let f1 = FileStorage::open(&file_path).unwrap();
    // Second handle to the same file should fail with DbError::Locked
    let f2_res = FileStorage::open(&file_path);
    assert!(f2_res.is_err(), "Second handle must fail advisory lock");
    match f2_res.err().unwrap() {
        tej_db_core::DbError::Locked(_) => {}
        other => panic!("Expected DbError::Locked, got: {other:?}"),
    }
    drop(f1);

    // After drop, opening succeeds
    let f3 = FileStorage::open(&file_path);
    assert!(f3.is_ok());
}
