use std::ops::Bound;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;
use tej_db::{Database, DurabilityMode, KeyRange};

#[test]
fn test_isolation_snapshot_readers_unaffected_by_writer() {
    let db = Database::open_memory().unwrap();

    // 1. Initial value
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"item", b"v1").unwrap();
        tx.commit().unwrap();
    }

    // 2. Open snapshot reader
    let snapshot_reader = db.begin_read().unwrap();
    assert_eq!(snapshot_reader.get(b"item").unwrap(), Some(b"v1".to_vec()));

    // 3. Mutate and commit with writer
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"item", b"v2").unwrap();
        tx.put(b"new_item", b"v3").unwrap();
        tx.commit().unwrap();
    }

    // 4. Existing snapshot reader still observes initial snapshot state (v1, no new_item)
    assert_eq!(snapshot_reader.get(b"item").unwrap(), Some(b"v1".to_vec()));
    assert_eq!(snapshot_reader.get(b"new_item").unwrap(), None);

    // 5. New reader observes updated state
    let new_reader = db.begin_read().unwrap();
    assert_eq!(new_reader.get(b"item").unwrap(), Some(b"v2".to_vec()));
    assert_eq!(new_reader.get(b"new_item").unwrap(), Some(b"v3".to_vec()));
}

#[test]
fn test_concurrent_reader_writer_contention() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("concurrent_acid.db");
    let db = Arc::new(Database::open_file(&db_path).unwrap());

    let num_keys = 200usize;
    // 1. Seed initial dataset
    {
        let mut tx = db.begin_write().unwrap();
        for i in 0..num_keys {
            let k = format!("key_{i:04}");
            let v = format!("val_{i:04}_gen0");
            tx.put(k.as_bytes(), v.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    let running = Arc::new(AtomicBool::new(true));
    let read_ops_completed = Arc::new(AtomicU64::new(0));

    // 2. Spawn 6 concurrent reader threads
    let mut reader_handles = Vec::new();
    for thread_idx in 0..6 {
        let db_clone = Arc::clone(&db);
        let running_clone = Arc::clone(&running);
        let read_ops_clone = Arc::clone(&read_ops_completed);

        let handle = thread::spawn(move || {
            let mut iters = 0u64;
            while running_clone.load(Ordering::Acquire) {
                let rx = db_clone.begin_read().unwrap();

                // Point lookup
                let key_idx = (iters as usize + thread_idx * 17) % num_keys;
                let k = format!("key_{key_idx:04}");
                let val_opt = rx.get(k.as_bytes()).unwrap();
                assert!(val_opt.is_some(), "Key {k} must exist");
                let val_bytes = val_opt.unwrap();
                let val_str = String::from_utf8(val_bytes).unwrap();
                assert!(
                    val_str.starts_with(&format!("val_{key_idx:04}_")),
                    "Value {val_str} corrupt for key {k}"
                );

                // Range scan: ensure ascending order and consistent contents
                let start_idx = (iters as usize * 7) % (num_keys - 20);
                let end_idx = start_idx + 15;
                let start_k = format!("key_{start_idx:04}");
                let end_k = format!("key_{end_idx:04}");
                let range = KeyRange {
                    start: Bound::Included(start_k.as_bytes().to_vec()),
                    end: Bound::Included(end_k.as_bytes().to_vec()),
                };

                let items = rx.scan_range(&range).unwrap();
                let mut last_key: Option<Vec<u8>> = None;
                for item in &items {
                    if let Some(ref prev) = last_key {
                        assert!(
                            &item.key > prev,
                            "Range scan keys must be strictly ascending: prev={:?}, curr={:?}",
                            String::from_utf8_lossy(prev),
                            String::from_utf8_lossy(&item.key)
                        );
                    }
                    last_key = Some(item.key.clone());
                }

                iters += 1;
                read_ops_clone.fetch_add(1, Ordering::Relaxed);
            }
        });
        reader_handles.push(handle);
    }

    // 3. Writer thread: performs 50 transactions mutating batches
    let writer_db = Arc::clone(&db);
    let writer_handle = thread::spawn(move || {
        for generation in 1..=50 {
            let mut tx = writer_db.begin_write().unwrap();
            let batch_offset = (generation * 13) % (num_keys - 30);
            for i in batch_offset..batch_offset + 25 {
                let k = format!("key_{i:04}");
                let v = format!("val_{i:04}_gen{generation}");
                tx.put(k.as_bytes(), v.as_bytes()).unwrap();
            }
            tx.commit().unwrap();
            thread::sleep(Duration::from_millis(2));
        }
    });

    writer_handle.join().unwrap();
    running.store(false, Ordering::Release);

    for handle in reader_handles {
        handle.join().unwrap();
    }

    assert!(
        read_ops_completed.load(Ordering::Acquire) > 100,
        "Concurrent readers should have executed significant reads"
    );
}

#[test]
fn test_background_flusher_race_conditions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("eventual_flusher_race.db");
    let db = Arc::new(Database::open_file(&db_path).unwrap());
    db.set_durability(DurabilityMode::Eventual { interval_ms: 10 });

    let running = Arc::new(AtomicBool::new(true));

    // 4 reader threads
    let mut handles = Vec::new();
    for _ in 0..4 {
        let db_c = Arc::clone(&db);
        let running_c = Arc::clone(&running);
        handles.push(thread::spawn(move || {
            let mut count = 0;
            while running_c.load(Ordering::Acquire) {
                let rx = db_c.begin_read().unwrap();
                let _ = rx.get(b"flush_key").unwrap();
                count += 1;
                if count % 50 == 0 {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }));
    }

    // 1 writer mutating continuously
    let db_w = Arc::clone(&db);
    let writer = thread::spawn(move || {
        for i in 0..100 {
            let mut tx = db_w.begin_write().unwrap();
            tx.put(b"flush_key", format!("val_{i}").as_bytes()).unwrap();
            tx.commit().unwrap();
            thread::sleep(Duration::from_millis(1));
        }
    });

    writer.join().unwrap();
    running.store(false, Ordering::Release);

    for h in handles {
        h.join().unwrap();
    }

    // Check final state
    let rx = db.begin_read().unwrap();
    assert_eq!(rx.get(b"flush_key").unwrap(), Some(b"val_99".to_vec()));
}
