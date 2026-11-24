use tej_db::{Database, DbError, DurabilityMode, SecondaryIndex};
use tempfile::tempdir;

#[test]
fn test_repeated_same_key_writes_last_write_wins() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("dedup_test.db");
    let db = Database::open_file(&db_path).unwrap();

    // 1. Default table: multiple writes to same key within single transaction
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"user:100", b"v1").unwrap();
        tx.put(b"user:200", b"other").unwrap();
        tx.put(b"user:100", b"v2").unwrap();
        tx.put(b"user:100", b"v3_final").unwrap();
        tx.commit().unwrap();
    }

    // Verify default table read
    {
        let rx = db.begin_read().unwrap();
        assert_eq!(rx.get(b"user:100").unwrap(), Some(b"v3_final".to_vec()));
        assert_eq!(rx.get(b"user:200").unwrap(), Some(b"other".to_vec()));
    }

    // 2. Named table: multiple writes to same key within single transaction
    db.create_table("accounts").unwrap();
    {
        let mut tx = db.begin_write().unwrap();
        tx.put_table_key("accounts", b"acc:1", b"balance_10")
            .unwrap();
        tx.put_table_key("accounts", b"acc:2", b"balance_50")
            .unwrap();
        tx.put_table_key("accounts", b"acc:1", b"balance_100")
            .unwrap();
        tx.commit().unwrap();
    }

    // Verify named table read
    {
        let rx = db.begin_read().unwrap();
        let tbl = rx.open_table("accounts").unwrap();
        assert_eq!(tbl.get(b"acc:1").unwrap(), Some(b"balance_100".to_vec()));
    }
}

#[test]
fn test_cow_page_retirement_reader_isolation() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("isolation_test.db");
    let db = Database::open_file_with_capacity(&db_path, 64).unwrap();

    // Seed initial dataset
    {
        let mut tx = db.begin_write().unwrap();
        for i in 0..100 {
            let key = format!("k:{i:04}");
            let val = format!("v:{i:04}");
            tx.put(key.as_bytes(), val.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    // Open a long-running snapshot reader
    let snapshot_rx = db.begin_read().unwrap();

    // Mutate and replace pages in multiple subsequent write transactions
    for round in 1..=5 {
        let mut tx = db.begin_write().unwrap();
        for i in 0..100 {
            let key = format!("k:{i:04}");
            let val = format!("v_round_{round}:{i:04}");
            tx.put(key.as_bytes(), val.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    // The snapshot reader must still observe the EXACT original snapshot
    for i in 0..100 {
        let key = format!("k:{i:04}");
        let expected = format!("v:{i:04}");
        assert_eq!(
            snapshot_rx.get(key.as_bytes()).unwrap(),
            Some(expected.into_bytes()),
            "Snapshot reader saw corrupted or prematurely recycled page on key {key}"
        );
    }

    drop(snapshot_rx);

    // New readers observe the latest round
    let latest_rx = db.begin_read().unwrap();
    for i in 0..100 {
        let key = format!("k:{i:04}");
        let expected = format!("v_round_5:{i:04}");
        assert_eq!(
            latest_rx.get(key.as_bytes()).unwrap(),
            Some(expected.into_bytes())
        );
    }
}

#[test]
fn test_rollback_preserves_committed_pages() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("rollback_test.db");
    let db = Database::open_file(&db_path).unwrap();

    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"keep_me", b"initial").unwrap();
        tx.commit().unwrap();
    }

    // Start a transaction, allocate new pages / overwrite, then abort
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"keep_me", b"overwritten").unwrap();
        tx.put(b"uncommitted", b"garbage").unwrap();
        // Drop without commit (aborts)
    }

    // Verify original data is preserved intact
    let rx = db.begin_read().unwrap();
    assert_eq!(rx.get(b"keep_me").unwrap(), Some(b"initial".to_vec()));
    assert_eq!(rx.get(b"uncommitted").unwrap(), None);
}

#[test]
fn test_btree_max_key_size_enforced() {
    let db = Database::open_memory().unwrap();
    let mut tx = db.begin_write().unwrap();

    // 1000 bytes is allowed
    let valid_key = vec![b'x'; 1000];
    tx.put(&valid_key, b"fits").unwrap();

    // 1001 bytes is rejected
    let oversize_key = vec![b'y'; 1001];
    let err = tx.put(&oversize_key, b"too_large").unwrap_err();
    match err {
        DbError::KeyTooLarge(len) => assert_eq!(len, 1001),
        other => panic!("Expected KeyTooLarge, got: {other:?}"),
    }
}

#[test]
fn test_database_backup_all_tables_and_self_reject() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("source.db");
    let backup_path = dir.path().join("backup.db");

    let db = Database::open_file(&db_path).unwrap();

    // Populate default table and multiple named tables
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"default_key", b"default_val").unwrap();
        tx.create_table("users").unwrap();
        tx.put_table_key("users", b"user:1", b"Alice").unwrap();
        tx.create_table("logs").unwrap();
        tx.put_table_key("logs", b"log:1", b"System boot").unwrap();
        tx.commit().unwrap();
    }

    // 1. Attempting to backup to self must be rejected
    let self_backup_res = db.backup(&db_path);
    assert!(self_backup_res.is_err(), "Backup to self must fail");

    // 2. Backup to a new path
    db.backup(&backup_path).unwrap();

    // 3. Open the backup database and verify all data across all tables
    let backup_db = Database::open_file(&backup_path).unwrap();
    let rx = backup_db.begin_read().unwrap();

    assert_eq!(
        rx.get(b"default_key").unwrap(),
        Some(b"default_val".to_vec())
    );

    let users_tbl = rx.open_table("users").unwrap();
    assert_eq!(users_tbl.get(b"user:1").unwrap(), Some(b"Alice".to_vec()));

    let logs_tbl = rx.open_table("logs").unwrap();
    assert_eq!(
        logs_tbl.get(b"log:1").unwrap(),
        Some(b"System boot".to_vec())
    );
}

#[test]
fn test_secondary_index_0xff_trailing_bytes() {
    let db = Database::open_memory().unwrap();
    db.create_table("entities").unwrap();
    let index = SecondaryIndex::new("idx_binary");

    let mut tx = db.begin_write().unwrap();
    let mut tbl = tx.open_table_mut("entities").unwrap();
    tbl.put(b"pk1", b"val1").unwrap();
    tbl.put(b"pk2", b"val2").unwrap();
    tbl.put(b"pk3", b"val3").unwrap();

    // Secondary keys ending in 0xFF or consisting of all 0xFF
    let key_with_ff = vec![0x10, 0x20, 0xFF];
    let key_all_ff = vec![0xFF, 0xFF];
    let key_normal = vec![0x10, 0x20, 0x01];

    index.insert_entry(&mut tx, &key_with_ff, b"pk1").unwrap();
    index.insert_entry(&mut tx, &key_all_ff, b"pk2").unwrap();
    index.insert_entry(&mut tx, &key_normal, b"pk3").unwrap();
    tx.commit().unwrap();

    let rx = db.begin_read().unwrap();

    let pks1 = index.lookup_primary_keys(&rx, &key_with_ff).unwrap();
    assert_eq!(pks1, vec![b"pk1".to_vec()]);

    let pks2 = index.lookup_primary_keys(&rx, &key_all_ff).unwrap();
    assert_eq!(pks2, vec![b"pk2".to_vec()]);

    let pks3 = index.lookup_primary_keys(&rx, &key_normal).unwrap();
    assert_eq!(pks3, vec![b"pk3".to_vec()]);
}

#[test]
fn test_two_phase_commit_flush_barrier() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("twophase.db");

    // Open with immediate durability
    let db = Database::open_file_with_durability(&db_path, 128, DurabilityMode::Immediate).unwrap();

    for i in 0..10 {
        let mut tx = db.begin_write().unwrap();
        tx.put(format!("key_{i}").as_bytes(), b"persisted_value")
            .unwrap();
        tx.commit().unwrap();
    }

    drop(db);

    // Reopen and verify data and superblock integrity
    let reopened = Database::open_file(&db_path).unwrap();
    let rx = reopened.begin_read().unwrap();
    for i in 0..10 {
        assert_eq!(
            rx.get(format!("key_{i}").as_bytes()).unwrap(),
            Some(b"persisted_value".to_vec())
        );
    }
}
