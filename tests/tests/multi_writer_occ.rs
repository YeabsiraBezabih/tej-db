use std::sync::{Arc, Barrier};
use std::thread;
use tej_db::{Database, DbError, KeyRange};

#[test]
fn test_multi_writer_disjoint_keys() {
    let db = Arc::new(Database::open_memory().unwrap());
    let num_threads = 8;
    let keys_per_thread = 100;
    let barrier = Arc::new(Barrier::new(num_threads));

    let mut handles = Vec::new();
    for t_idx in 0..num_threads {
        let db_c = Arc::clone(&db);
        let bar = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            // Synchronize all threads so they begin their write transactions concurrently
            bar.wait();

            let mut tx = db_c.begin_write().unwrap();
            for i in 0..keys_per_thread {
                let k = format!("thread_{t_idx:02}_key_{i:04}");
                let v = format!("thread_{t_idx:02}_val_{i:04}");
                tx.put(k.as_bytes(), v.as_bytes()).unwrap();
            }

            // All threads commit their disjoint writes
            tx.commit()
                .expect("Disjoint writes must commit successfully");
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Verify all keys from all threads are present and uncorrupted
    let rx = db.begin_read().unwrap();
    for t_idx in 0..num_threads {
        for i in 0..keys_per_thread {
            let k = format!("thread_{t_idx:02}_key_{i:04}");
            let expected_v = format!("thread_{t_idx:02}_val_{i:04}");
            let val = rx.get(k.as_bytes()).unwrap();
            assert_eq!(
                val,
                Some(expected_v.into_bytes()),
                "Missing or incorrect key for thread {t_idx}"
            );
        }
    }
}

#[test]
fn test_multi_writer_independent_tables() {
    let db = Arc::new(Database::open_memory().unwrap());
    let tables = vec!["table_alpha", "table_beta", "table_gamma", "table_delta"];
    let num_tables = tables.len();
    let barrier = Arc::new(Barrier::new(num_tables));

    let mut handles = Vec::new();
    for table_name in tables.clone() {
        let db_c = Arc::clone(&db);
        let bar = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            bar.wait();
            let mut tx = db_c.begin_write().unwrap();
            {
                let mut tbl = tx.open_table_mut(table_name).unwrap();
                for i in 0..50 {
                    let k = format!("k_{i:03}");
                    let v = format!("v_{table_name}_{i:03}");
                    tbl.put(k.as_bytes(), v.as_bytes()).unwrap();
                }
            }
            tx.commit().expect("Independent table write must commit");
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Verify each table contains its 50 items
    let rx = db.begin_read().unwrap();
    for table_name in tables {
        let tbl = rx.open_table(table_name).unwrap();
        for i in 0..50 {
            let k = format!("k_{i:03}");
            let expected_v = format!("v_{table_name}_{i:03}");
            let val = tbl.get(k.as_bytes()).unwrap();
            assert_eq!(val, Some(expected_v.into_bytes()));
        }
    }
}

#[test]
fn test_multi_writer_row_level_conflict_detected() {
    let db = Database::open_memory().unwrap();

    // 1. Initial state
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"shared_account", b"100").unwrap();
        tx.commit().unwrap();
    }

    // 2. Open two concurrent transactions at the same snapshot
    let mut tx1 = db.begin_write().unwrap();
    let mut tx2 = db.begin_write().unwrap();

    // 3. Both mutate the exact same key
    tx1.put(b"shared_account", b"150").unwrap();
    tx2.put(b"shared_account", b"200").unwrap();

    // 4. First committer wins
    tx1.commit().expect("First transaction must commit cleanly");

    // 5. Second committer must fail with WriteConflict
    let err = tx2
        .commit()
        .expect_err("Conflicting second transaction must fail");
    match err {
        DbError::WriteConflict { key, committed_tx } => {
            assert_eq!(key, b"shared_account");
            assert!(committed_tx > 0);
        }
        other => panic!("Expected WriteConflict, got {other:?}"),
    }

    // 6. Verify that tx1's update was preserved and tx2's was discarded
    let rx = db.begin_read().unwrap();
    assert_eq!(rx.get(b"shared_account").unwrap(), Some(b"150".to_vec()));
}

#[test]
fn test_multi_writer_automatic_retry_helper() {
    let db = Arc::new(Database::open_memory().unwrap());

    // Initialize shared counter
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"counter", &0u64.to_le_bytes()).unwrap();
        tx.commit().unwrap();
    }

    let num_threads = 4;
    let increments_per_thread = 25;
    let barrier = Arc::new(Barrier::new(num_threads));

    let mut handles = Vec::new();
    for _ in 0..num_threads {
        let db_c = Arc::clone(&db);
        let bar = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            bar.wait();
            for _ in 0..increments_per_thread {
                db_c.transaction_with_retry(50, |tx| {
                    let cur_bytes = tx.get(b"counter")?.expect("counter exists");
                    let cur_val = u64::from_le_bytes(cur_bytes.try_into().unwrap());
                    let new_val = cur_val + 1;
                    tx.put(b"counter", &new_val.to_le_bytes())?;
                    Ok(())
                })
                .expect("Retry helper must eventually succeed");
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Verify final counter value
    let rx = db.begin_read().unwrap();
    let final_bytes = rx.get(b"counter").unwrap().expect("counter exists");
    let final_val = u64::from_le_bytes(final_bytes.try_into().unwrap());
    assert_eq!(
        final_val,
        (num_threads * increments_per_thread) as u64,
        "Counter must match exact total increments"
    );
}

#[test]
fn test_multi_writer_concurrent_sequential_appends() {
    let db = Arc::new(Database::open_memory().unwrap());
    let num_threads = 4;
    let items_per_thread = 50;
    let barrier = Arc::new(Barrier::new(num_threads));

    let mut handles = Vec::new();
    for t_idx in 0..num_threads {
        let db_c = Arc::clone(&db);
        let bar = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            bar.wait();
            let mut tx = db_c.begin_write().unwrap();
            let start = t_idx * items_per_thread;
            let end = start + items_per_thread;

            for i in start..end {
                let k = format!("seq_{i:04}").into_bytes();
                let v = format!("val_{i:04}").into_bytes();
                tx.put(&k, &v).unwrap();
            }

            tx.commit().expect("Sequential range append must succeed");
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Verify that range scan sees all items in strictly ascending order
    let rx = db.begin_read().unwrap();
    let total_items = rx.scan_range(&KeyRange::all()).unwrap();
    assert_eq!(total_items.len(), num_threads * items_per_thread);

    for (idx, item) in total_items.iter().enumerate() {
        let expected_k = format!("seq_{idx:04}").into_bytes();
        let expected_v = format!("val_{idx:04}").into_bytes();
        assert_eq!(item.key, expected_k);
        assert_eq!(item.value, expected_v);
    }
}
