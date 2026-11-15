use std::sync::{Arc, Barrier};
use std::thread;
use tej_db::{Database, DbError, KeyRange};

#[test]
fn test_sharded_table_crud_and_scans() {
    let db = Database::open_memory().unwrap();
    let boundaries = vec![b"e".to_vec(), b"m".to_vec(), b"t".to_vec()];
    db.create_table_sharded("events", 4, boundaries).unwrap();

    let tables = db.list_tables().unwrap();
    assert!(tables.contains(&"events".to_string()));

    // Insert keys spanning all 4 shards
    // Shard 0: < "e"
    // Shard 1: >= "e" and < "m"
    // Shard 2: >= "m" and < "t"
    // Shard 3: >= "t"
    {
        let mut tx = db.begin_write().unwrap();
        {
            let mut tbl = tx.open_table_sharded_mut("events").unwrap();
            tbl.put(b"alpha", b"val_alpha").unwrap();
            tbl.put(b"bravo", b"val_bravo").unwrap();
            tbl.put(b"echo", b"val_echo").unwrap();
            tbl.put(b"hotel", b"val_hotel").unwrap();
            tbl.put(b"mike", b"val_mike").unwrap();
            tbl.put(b"oscar", b"val_oscar").unwrap();
            tbl.put(b"tango", b"val_tango").unwrap();
            tbl.put(b"zulu", b"val_zulu").unwrap();
        }
        tx.commit().unwrap();
    }

    // Read keys
    let rx = db.begin_read().unwrap();
    let tbl = rx.open_table_sharded("events").unwrap();
    assert_eq!(tbl.get(b"alpha").unwrap(), Some(b"val_alpha".to_vec()));
    assert_eq!(tbl.get(b"echo").unwrap(), Some(b"val_echo".to_vec()));
    assert_eq!(tbl.get(b"mike").unwrap(), Some(b"val_mike".to_vec()));
    assert_eq!(tbl.get(b"zulu").unwrap(), Some(b"val_zulu".to_vec()));
    assert_eq!(tbl.get(b"nonexistent").unwrap(), None);

    // Full scan should return all 8 keys in strictly ascending order
    let all_kvs = tbl.scan_all().unwrap();
    assert_eq!(all_kvs.len(), 8);
    let keys: Vec<String> = all_kvs
        .iter()
        .map(|kv| String::from_utf8(kv.key.clone()).unwrap())
        .collect();
    assert_eq!(
        keys,
        vec![
            "alpha", "bravo", "echo", "hotel", "mike", "oscar", "tango", "zulu"
        ]
    );

    // Partial range scan crossing multiple shards
    let range = KeyRange::between(b"bravo".to_vec(), b"oscar".to_vec());
    let ranged_kvs = tbl.scan_range(&range).unwrap();
    let ranged_keys: Vec<String> = ranged_kvs
        .iter()
        .map(|kv| String::from_utf8(kv.key.clone()).unwrap())
        .collect();
    assert_eq!(ranged_keys, vec!["bravo", "echo", "hotel", "mike"]);
}

#[test]
fn test_sharded_table_concurrent_disjoint_shards() {
    let db = Arc::new(Database::open_memory().unwrap());
    // 4 shards with prefixes matching 'a'..'d', 'e'..'h', 'i'..'l', 'm'..'z'
    let boundaries = vec![b"e".to_vec(), b"i".to_vec(), b"m".to_vec()];
    db.create_table_sharded("metrics", 4, boundaries).unwrap();

    let prefixes = [b"b", b"f", b"j", b"n"]; // Each maps to a distinct shard
    let num_threads = 4;
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = Vec::new();

    for (t_idx, &prefix) in prefixes.iter().enumerate() {
        let db_c = Arc::clone(&db);
        let bar = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            bar.wait();
            let mut tx = db_c.begin_write().unwrap();
            {
                let mut tbl = tx.open_table_sharded_mut("metrics").unwrap();
                for i in 0..50 {
                    let k = format!("{}{i:03}", prefix[0] as char).into_bytes();
                    let v = format!("val_{t_idx}_{i:03}").into_bytes();
                    tbl.put(&k, &v).unwrap();
                }
            }
            tx.commit().expect("Disjoint shard writes must commit");
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Verify all 200 items exist and are ordered
    let rx = db.begin_read().unwrap();
    let tbl = rx.open_table_sharded("metrics").unwrap();
    let all = tbl.scan_all().unwrap();
    assert_eq!(all.len(), 200);

    for i in 1..all.len() {
        assert!(all[i - 1].key < all[i].key);
    }
}

#[test]
fn test_sharded_table_drop_lifecycle() {
    let db = Database::open_memory().unwrap();
    let boundaries = vec![b"m".to_vec()];
    db.create_table_sharded("temp_tbl", 2, boundaries).unwrap();

    {
        let mut tx = db.begin_write().unwrap();
        {
            let mut tbl = tx.open_table_sharded_mut("temp_tbl").unwrap();
            tbl.put(b"a", b"1").unwrap();
            tbl.put(b"z", b"2").unwrap();
        }
        tx.commit().unwrap();
    }

    assert!(db.list_tables().unwrap().contains(&"temp_tbl".to_string()));

    let dropped = db.drop_table_sharded("temp_tbl").unwrap();
    assert!(dropped);

    assert!(!db.list_tables().unwrap().contains(&"temp_tbl".to_string()));

    let rx = db.begin_read().unwrap();
    assert!(matches!(
        rx.open_table_sharded("temp_tbl"),
        Err(DbError::TableNotFound(_))
    ));
}

#[test]
fn test_sharded_table_persistence_across_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("sharded_persist.db");

    // Phase 1: Create and write to sharded table in file DB
    {
        let db = Database::open_file(&db_path).unwrap();
        let boundaries = vec![b"g".to_vec(), b"p".to_vec()];
        db.create_table_sharded("inventory", 3, boundaries).unwrap();

        let mut tx = db.begin_write().unwrap();
        {
            let mut tbl = tx.open_table_sharded_mut("inventory").unwrap();
            tbl.put(b"apple", b"qty_10").unwrap();
            tbl.put(b"mango", b"qty_25").unwrap();
            tbl.put(b"zebra", b"qty_5").unwrap();
        }
        tx.commit().unwrap();
        db.sync().unwrap();
    }

    // Phase 2: Reopen from disk and verify state
    {
        let db = Database::open_file(&db_path).unwrap();
        assert!(db.list_tables().unwrap().contains(&"inventory".to_string()));

        let rx = db.begin_read().unwrap();
        let tbl = rx.open_table_sharded("inventory").unwrap();
        assert_eq!(tbl.get(b"apple").unwrap(), Some(b"qty_10".to_vec()));
        assert_eq!(tbl.get(b"mango").unwrap(), Some(b"qty_25".to_vec()));
        assert_eq!(tbl.get(b"zebra").unwrap(), Some(b"qty_5".to_vec()));

        let all = tbl.scan_all().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].key, b"apple");
        assert_eq!(all[1].key, b"mango");
        assert_eq!(all[2].key, b"zebra");
    }
}
