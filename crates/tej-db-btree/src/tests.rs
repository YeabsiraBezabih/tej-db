use super::*;
use core::ops::Bound;
use tej_db_core::{Index, KeyRange};
use tej_db_pager::MemStorage;

#[test]
fn test_btree_crud() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 100));
    let mut btree = BTreeIndex::new(cache, None).unwrap();

    assert_eq!(btree.get(b"foo").unwrap(), None);

    btree.insert(b"foo", b"bar").unwrap();
    assert_eq!(btree.get(b"foo").unwrap(), Some(b"bar".to_vec()));

    btree.insert(b"baz", b"qux").unwrap();
    assert_eq!(btree.get(b"baz").unwrap(), Some(b"qux".to_vec()));
    assert_eq!(btree.get(b"foo").unwrap(), Some(b"bar".to_vec()));

    btree.insert(b"foo", b"updated").unwrap();
    assert_eq!(btree.get(b"foo").unwrap(), Some(b"updated".to_vec()));

    assert!(btree.remove(b"foo").unwrap());
    assert_eq!(btree.get(b"foo").unwrap(), None);
    assert_eq!(btree.get(b"baz").unwrap(), Some(b"qux".to_vec()));
}

#[test]
fn test_btree_split_and_scan() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 100));
    let mut btree = BTreeIndex::new(cache, None).unwrap();

    // Insert enough items to trigger leaf and internal node splits
    for i in 0..200u32 {
        let key = format!("key_{i:04}").into_bytes();
        let val = format!("val_{i:04}").into_bytes();
        btree.insert(&key, &val).unwrap();
    }

    for i in 0..200u32 {
        let key = format!("key_{i:04}").into_bytes();
        let val = format!("val_{i:04}").into_bytes();
        assert_eq!(btree.get(&key).unwrap(), Some(val));
    }

    // Test range scan
    let range = KeyRange {
        start: Bound::Included(b"key_0050".to_vec()),
        end: Bound::Excluded(b"key_0060".to_vec()),
    };
    let items = btree.scan_range(&range).unwrap();
    assert_eq!(items.len(), 10);
    assert_eq!(items[0].key, b"key_0050");
    assert_eq!(items[9].key, b"key_0059");
}

#[test]
fn test_btree_large_blob_overflow() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 500));
    let mut btree = BTreeIndex::new(cache, None).unwrap();

    // 64 KiB BLOB
    let mut blob_64k = vec![0xABu8; 64 * 1024];
    blob_64k[0..4].copy_from_slice(b"HEAD");
    blob_64k[65532..65536].copy_from_slice(b"TAIL");

    btree.insert(b"blob:64k", &blob_64k).unwrap();
    let retrieved = btree.get(b"blob:64k").unwrap().expect("BLOB found");
    assert_eq!(retrieved.len(), 64 * 1024);
    assert_eq!(&retrieved[0..4], b"HEAD");
    assert_eq!(&retrieved[65532..65536], b"TAIL");
    assert_eq!(retrieved, blob_64k);

    // Update BLOB to 100 KiB
    let mut blob_100k = vec![0xCDu8; 100 * 1024];
    blob_100k[0..4].copy_from_slice(b"NEW!");
    btree.insert(b"blob:64k", &blob_100k).unwrap();
    let retrieved_100k = btree.get(b"blob:64k").unwrap().expect("Updated BLOB found");
    assert_eq!(retrieved_100k.len(), 100 * 1024);
    assert_eq!(&retrieved_100k[0..4], b"NEW!");

    // Remove BLOB
    assert!(btree.remove(b"blob:64k").unwrap());
    assert_eq!(btree.get(b"blob:64k").unwrap(), None);
}

#[test]
fn test_btree_value_scaling_bounds() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 500));
    let mut btree = BTreeIndex::new(cache, None).unwrap();

    // 2x lower bound (512B), nominal (1024B), 2x upper bound (2048B)
    let payload_sizes = [512, 1024, 2048];
    let num_keys_per_size = 50;

    for &size in &payload_sizes {
        let mut val = vec![0x77u8; size];
        val[0..4].copy_from_slice(b"SZ__");
        #[allow(clippy::cast_possible_truncation)]
        val[4..8].copy_from_slice(&(size as u32).to_le_bytes());

        for i in 0..num_keys_per_size {
            let k = format!("bound_{size:04}_{i:03}").into_bytes();
            val[8..12].copy_from_slice(&(i as u32).to_le_bytes());
            btree.insert(&k, &val).unwrap();
        }

        for i in 0..num_keys_per_size {
            let k = format!("bound_{size:04}_{i:03}").into_bytes();
            let retrieved = btree.get(&k).unwrap().unwrap();
            assert_eq!(retrieved.len(), size);
            assert_eq!(&retrieved[0..4], b"SZ__");
            #[allow(clippy::cast_possible_truncation)]
            {
                assert_eq!(&retrieved[4..8], &(size as u32).to_le_bytes());
            }
            assert_eq!(retrieved[8..12], (i as u32).to_le_bytes());
        }
    }

    // Verify range scans across mixed sizes
    let items = btree.scan_range(&KeyRange::all()).unwrap();
    assert_eq!(items.len(), payload_sizes.len() * num_keys_per_size);

    // Delete 50% and verify consistency
    for &size in &payload_sizes {
        for i in (0..num_keys_per_size).step_by(2) {
            let k = format!("bound_{size:04}_{i:03}").into_bytes();
            assert!(btree.remove(&k).unwrap());
        }
    }
    let remaining = btree.scan_range(&KeyRange::all()).unwrap();
    assert_eq!(
        remaining.len(),
        (payload_sizes.len() * num_keys_per_size) / 2
    );
}

#[test]
fn test_btree_sorted_batch_insertion() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 100));
    let mut btree = BTreeIndex::new(cache, None).unwrap();
    let mut tx_pages = TxPageSet::default();

    // Prepare sorted batch with interleaved puts and deletes
    let batch = vec![
        (b"batch_a".to_vec(), Some(b"val_a".to_vec())),
        (b"batch_b".to_vec(), Some(b"val_b".to_vec())),
        (b"batch_c".to_vec(), Some(b"val_c".to_vec())),
        (b"batch_d".to_vec(), Some(b"val_d".to_vec())),
    ];

    btree
        .insert_sorted_batch_in_tx(&batch, &mut tx_pages)
        .unwrap();

    assert_eq!(btree.get(b"batch_a").unwrap(), Some(b"val_a".to_vec()));
    assert_eq!(btree.get(b"batch_b").unwrap(), Some(b"val_b".to_vec()));
    assert_eq!(btree.get(b"batch_c").unwrap(), Some(b"val_c".to_vec()));
    assert_eq!(btree.get(b"batch_d").unwrap(), Some(b"val_d".to_vec()));

    // Now insert a second sorted batch that updates some and deletes batch_c
    let batch2 = vec![
        (b"batch_b".to_vec(), Some(b"val_b_updated".to_vec())),
        (b"batch_c".to_vec(), None),
        (b"batch_e".to_vec(), Some(b"val_e".to_vec())),
    ];

    btree
        .insert_sorted_batch_in_tx(&batch2, &mut tx_pages)
        .unwrap();

    assert_eq!(
        btree.get(b"batch_b").unwrap(),
        Some(b"val_b_updated".to_vec())
    );
    assert_eq!(btree.get(b"batch_c").unwrap(), None);
    assert_eq!(btree.get(b"batch_e").unwrap(), Some(b"val_e".to_vec()));
}

#[test]
fn test_btree_empty_and_unbounded_range_scans() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 100));
    let mut btree = BTreeIndex::new(cache, None).unwrap();

    // Scan completely empty btree
    let empty_scan = btree.scan_range(&KeyRange::all()).unwrap();
    assert_eq!(empty_scan.len(), 0);

    // Single item scan
    btree.insert(b"alpha", b"1").unwrap();
    let single = btree.scan_range(&KeyRange::all()).unwrap();
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].key, b"alpha");

    // Scan outside range
    let outside = btree
        .scan_range(&KeyRange {
            start: Bound::Included(b"beta".to_vec()),
            end: Bound::Unbounded,
        })
        .unwrap();
    assert_eq!(outside.len(), 0);
}
