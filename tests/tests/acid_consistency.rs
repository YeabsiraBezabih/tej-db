use std::collections::BTreeSet;
use tej_db::{Database, KeyRange};

#[test]
fn test_consistency_sorted_order_and_invariants() {
    let db = Database::open_memory().unwrap();

    let keys = vec![
        b"zeta".to_vec(),
        b"alpha".to_vec(),
        b"gamma".to_vec(),
        b"beta".to_vec(),
        b"delta".to_vec(),
    ];

    {
        let mut tx = db.begin_write().unwrap();
        for k in &keys {
            tx.put(k, b"val").unwrap();
        }
        tx.commit().unwrap();
    }

    // Invariant: scan_range returns entries strictly in ascending key order
    let rx = db.begin_read().unwrap();
    let scanned = rx.scan_range(&KeyRange::all()).unwrap();

    let scanned_keys: Vec<Vec<u8>> = scanned.into_iter().map(|kv| kv.key).collect();
    assert_eq!(
        scanned_keys,
        vec![
            b"alpha".to_vec(),
            b"beta".to_vec(),
            b"delta".to_vec(),
            b"gamma".to_vec(),
            b"zeta".to_vec(),
        ]
    );
}

#[test]
fn test_deep_multilevel_btree_splits_and_ordering() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("multilevel_splits.db");
    let db = Database::open_file(&db_path).unwrap();

    let count = 10_000u32;
    // Pseudorandom shuffled keys
    let mut keys = Vec::with_capacity(count as usize);
    for i in 0..count {
        let rand_idx = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % count;
        keys.push(format!("key_{rand_idx:06}").into_bytes());
    }

    // Insert in batches of 200
    for chunk in keys.chunks(200) {
        let mut tx = db.begin_write().unwrap();
        for k in chunk {
            let v = format!("val_{}", String::from_utf8_lossy(k));
            tx.put(k, v.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    // Scan all and verify strict ascending ordering and complete count
    let rx = db.begin_read().unwrap();
    let mut scanned_count = 0usize;
    let mut prev_key: Option<Vec<u8>> = None;

    rx.scan_range_visitor(&KeyRange::all(), |k, v| {
        if let Some(ref prev) = prev_key {
            assert!(
                k > prev.as_slice(),
                "Keys must be strictly ascending: prev={:?}, curr={:?}",
                String::from_utf8_lossy(prev),
                String::from_utf8_lossy(k)
            );
        }
        let expected_val = format!("val_{}", String::from_utf8_lossy(k));
        assert_eq!(
            v,
            expected_val.as_bytes(),
            "Value mismatch for key {:?}",
            String::from_utf8_lossy(k)
        );
        prev_key = Some(k.to_vec());
        scanned_count += 1;
        Ok(true)
    })
    .unwrap();

    // Verify all unique keys were retrieved
    let unique_keys: BTreeSet<_> = keys.iter().collect();
    assert_eq!(scanned_count, unique_keys.len());

    // Point lookup verification on sampled keys
    for k in keys.iter().step_by(50) {
        let val = rx.get(k).unwrap();
        assert!(
            val.is_some(),
            "Key {:?} must exist in tree",
            String::from_utf8_lossy(k)
        );
    }
}

#[test]
fn test_btree_deletion_underflow_and_recycling() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("btree_deletion.db");
    let db = Database::open_file(&db_path).unwrap();

    let count = 3_000u32;
    // 1. Populate initial dataset
    {
        let mut tx = db.begin_write().unwrap();
        for i in 0..count {
            let k = format!("k_{i:05}");
            let v = format!("v_{i:05}");
            tx.put(k.as_bytes(), v.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    let initial_stats = db.stats();
    assert!(
        initial_stats.total_pages > 5,
        "Database should have multiple pages"
    );

    // 2. Delete 90% of keys (keep keys where i % 10 == 0)
    let mut deleted_count = 0;
    {
        let mut tx = db.begin_write().unwrap();
        for i in 0..count {
            if i % 10 != 0 {
                let k = format!("k_{i:05}");
                let deleted = tx.delete(k.as_bytes()).unwrap();
                assert!(deleted, "Key must have been deleted");
                deleted_count += 1;
            }
        }
        tx.commit().unwrap();
    }
    assert_eq!(deleted_count, 2700);

    // 3. Verify all remaining 10% keys exist and all deleted keys are gone
    let rx = db.begin_read().unwrap();
    for i in 0..count {
        let k = format!("k_{i:05}");
        let val = rx.get(k.as_bytes()).unwrap();
        if i % 10 == 0 {
            assert_eq!(
                val,
                Some(format!("v_{i:05}").into_bytes()),
                "Remaining key {k} must exist"
            );
        } else {
            assert!(val.is_none(), "Deleted key {k} must return None");
        }
    }

    // 4. Verify scan of remaining keys is strictly sorted and contains exactly 300 items
    let remaining_items = rx.scan_range(&KeyRange::all()).unwrap();
    assert_eq!(remaining_items.len(), 300);
    for window in remaining_items.windows(2) {
        assert!(
            window[0].key < window[1].key,
            "Remaining keys must be sorted"
        );
    }

    // 5. Verify page recycling: re-insert 500 new keys into the database
    drop(rx);
    {
        let mut tx = db.begin_write().unwrap();
        for i in count..count + 500 {
            let k = format!("k_{i:05}");
            let v = format!("v_{i:05}");
            tx.put(k.as_bytes(), v.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    let final_rx = db.begin_read().unwrap();
    for i in count..count + 500 {
        let k = format!("k_{i:05}");
        assert!(final_rx.get(k.as_bytes()).unwrap().is_some());
    }
}
