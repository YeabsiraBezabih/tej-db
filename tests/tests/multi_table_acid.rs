use tej_db::Database;

#[test]
fn test_cross_table_atomicity_and_rollback() {
    let db = Database::open_memory().unwrap();
    db.create_table("users").unwrap();
    db.create_table("orders").unwrap();

    // 1. Initial baseline state across two tables
    {
        let mut tx = db.begin_write().unwrap();
        {
            let mut users = tx.open_table_mut("users").unwrap();
            users.put(b"alice", b"points:100").unwrap();
        }
        {
            let mut orders = tx.open_table_mut("orders").unwrap();
            orders.put(b"ord_1", b"status:pending").unwrap();
        }
        tx.commit().unwrap();
    }

    // 2. Aborted cross-table mutation (atomicity rollback across multiple tables)
    {
        let mut tx = db.begin_write().unwrap();
        {
            let mut users = tx.open_table_mut("users").unwrap();
            users.put(b"alice", b"points:50").unwrap();
        }
        {
            let mut orders = tx.open_table_mut("orders").unwrap();
            orders.put(b"ord_1", b"status:completed").unwrap();
            orders.put(b"ord_2", b"status:new").unwrap();
        }
        tx.rollback().unwrap();
    }

    // 3. Verify both tables reverted cleanly to baseline
    {
        let rx = db.begin_read().unwrap();
        let users = rx.open_table("users").unwrap();
        let orders = rx.open_table("orders").unwrap();

        assert_eq!(users.get(b"alice").unwrap(), Some(b"points:100".to_vec()));
        assert_eq!(
            orders.get(b"ord_1").unwrap(),
            Some(b"status:pending".to_vec())
        );
        assert_eq!(orders.get(b"ord_2").unwrap(), None);
    }

    // 4. Committed cross-table mutation
    {
        let mut tx = db.begin_write().unwrap();
        {
            let mut users = tx.open_table_mut("users").unwrap();
            users.put(b"alice", b"points:50").unwrap();
        }
        {
            let mut orders = tx.open_table_mut("orders").unwrap();
            orders.put(b"ord_1", b"status:completed").unwrap();
            orders.put(b"ord_2", b"status:new").unwrap();
        }
        tx.commit().unwrap();
    }

    // 5. Verify both tables applied atomically
    {
        let rx = db.begin_read().unwrap();
        let users = rx.open_table("users").unwrap();
        let orders = rx.open_table("orders").unwrap();

        assert_eq!(users.get(b"alice").unwrap(), Some(b"points:50".to_vec()));
        assert_eq!(
            orders.get(b"ord_1").unwrap(),
            Some(b"status:completed".to_vec())
        );
        assert_eq!(orders.get(b"ord_2").unwrap(), Some(b"status:new".to_vec()));
    }
}

#[test]
fn test_table_drop_under_active_snapshot_reader() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("table_drop_reader.db");
    let db = Database::open_file(&db_path).unwrap();

    let table_name = "ephemeral_data";
    db.create_table(table_name).unwrap();

    // 1. Populate table with items
    {
        let mut tx = db.begin_write().unwrap();
        let mut t = tx.open_table_mut(table_name).unwrap();
        for i in 0..50 {
            let k = format!("k_{i:03}");
            let v = format!("v_{i:03}");
            t.put(k.as_bytes(), v.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    // 2. Open snapshot reader holding active reference to table
    let snapshot_reader = db.begin_read().unwrap();
    let snapshot_table = snapshot_reader.open_table(table_name).unwrap();
    assert_eq!(
        snapshot_table.get(b"k_025").unwrap(),
        Some(b"v_025".to_vec())
    );

    // 3. Drop table in a concurrent writer transaction
    let dropped = db.drop_table(table_name).unwrap();
    assert!(dropped, "Table should be successfully dropped");

    // 4. Verify existing snapshot reader still observes all items in its snapshot
    for i in 0..50 {
        let k = format!("k_{i:03}");
        let val = snapshot_table.get(k.as_bytes()).unwrap();
        assert_eq!(val, Some(format!("v_{i:03}").into_bytes()));
    }

    // 5. New reader observes that table does not exist
    let new_reader = db.begin_read().unwrap();
    let tables = new_reader.list_tables().unwrap();
    assert!(!tables.contains(&table_name.to_string()));

    let open_res = new_reader.open_table(table_name);
    assert!(
        matches!(open_res, Err(tej_db_core::DbError::TableNotFound(_))),
        "Dropped table must return TableNotFound error"
    );
}
