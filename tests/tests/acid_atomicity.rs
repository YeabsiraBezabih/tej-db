use tej_db::Database;

#[test]
fn test_atomicity_rollback_discards_all_mutations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("atomicity.db");

    let db = Database::open_file(&db_path).unwrap();

    // 1. Initial committed state
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"account_A", b"100").unwrap();
        tx.put(b"account_B", b"50").unwrap();
        tx.commit().unwrap();
    }

    // 2. Aborted transfer (atomicity violation prevented)
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"account_A", b"70").unwrap(); // debited
        tx.put(b"account_B", b"80").unwrap(); // credited
        // Simulate failure before commit
        tx.rollback().unwrap();
    }

    // 3. Verify accounts remain at 100 and 50
    {
        let rx = db.begin_read().unwrap();
        assert_eq!(rx.get(b"account_A").unwrap(), Some(b"100".to_vec()));
        assert_eq!(rx.get(b"account_B").unwrap(), Some(b"50".to_vec()));
    }
}
