use super::*;

#[test]
fn test_database_crud_lifecycle() {
    let db = Database::open_memory().unwrap();

    // Write
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"hello", b"world").unwrap();
        tx.commit().unwrap();
    }

    // Read
    {
        let rx = db.begin_read().unwrap();
        assert_eq!(rx.get(b"hello").unwrap(), Some(b"world".to_vec()));
    }

    // Delete
    {
        let mut tx = db.begin_write().unwrap();
        assert!(tx.delete(b"hello").unwrap());
        tx.commit().unwrap();
    }

    // Verify Delete
    {
        let rx = db.begin_read().unwrap();
        assert_eq!(rx.get(b"hello").unwrap(), None);
    }

    // Verify Database Statistics API
    let stats = db.stats();
    assert!(stats.total_pages > 0);
    assert!(stats.file_size_bytes > 0);
    assert!(stats.cached_pages > 0);
    assert!(stats.cache_hits > 0);
    assert!(stats.cache_hit_rate > 0.0 && stats.cache_hit_rate <= 1.0);
}

#[test]
fn test_typed_table_api() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
    struct Item {
        id: u64,
        name: String,
        qty: u32,
    }

    let db = Database::open_memory().unwrap();
    db.create_table("inventory").unwrap();

    let item = Item {
        id: 101,
        name: "Mechanical Keyboard".into(),
        qty: 5,
    };

    // Put typed
    {
        let mut tx = db.begin_write().unwrap();
        let mut table = tx.open_table_mut("inventory").unwrap();
        table.put(b"raw_k", b"raw_v").unwrap();
        let mut typed_table = TypedTableMut::new(table);
        typed_table.put(&101u64, &item).unwrap();
        tx.commit().unwrap();
    }

    // Get typed
    {
        let rx = db.begin_read().unwrap();
        let table = rx.open_table("inventory").unwrap();
        let typed_table: TypedTable<'_, _, u64, Item> = TypedTable::new(table);
        let fetched = typed_table.get(&101u64).unwrap();
        assert_eq!(fetched, Some(item));
        assert_eq!(typed_table.get(&999u64).unwrap(), None);
    }
}

#[test]
fn test_secondary_index() {
    let db = Database::open_memory().unwrap();
    let email_index = SecondaryIndex::new("users_by_email");

    // Insert primary data and secondary index entry
    {
        let mut tx = db.begin_write().unwrap();
        tx.create_table("users").unwrap();
        {
            let mut users = tx.open_table_mut("users").unwrap();
            users.put(b"user_101", b"Alice Smith").unwrap();
        }
        email_index
            .insert_entry(&mut tx, b"alice@example.com", b"user_101")
            .unwrap();
        tx.commit().unwrap();
    }

    // Lookup by secondary key
    {
        let rx = db.begin_read().unwrap();
        let pks = email_index
            .lookup_primary_keys(&rx, b"alice@example.com")
            .unwrap();
        assert_eq!(pks.len(), 1);
        assert_eq!(pks[0], b"user_101");

        let users = rx.open_table("users").unwrap();
        let user_data = users.get(&pks[0]).unwrap();
        assert_eq!(user_data, Some(b"Alice Smith".to_vec()));
    }
}

#[test]
fn test_online_backup_and_compaction() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_path = temp_dir.path().join("source.db");
    let backup_path = temp_dir.path().join("backup.db");

    let db = Database::open_file(&src_path).unwrap();
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"alpha", b"1").unwrap();
        tx.put(b"beta", b"2").unwrap();
        tx.commit().unwrap();
    }

    // Perform online backup while writers can still operate
    db.backup(&backup_path).unwrap();

    // Verify backup database has exact snapshot contents
    let backup_db = Database::open_file(&backup_path).unwrap();
    let rx = backup_db.begin_read().unwrap();
    assert_eq!(rx.get(b"alpha").unwrap(), Some(b"1".to_vec()));
    assert_eq!(rx.get(b"beta").unwrap(), Some(b"2".to_vec()));
}

#[test]
fn test_write_batch() {
    let db = Database::open_memory().unwrap();
    db.create_table("accounts").unwrap();

    let mut batch = WriteBatch::new();
    batch.put(b"root_key", b"root_val");
    batch.put_table("accounts", b"acc:1", b"$100");
    batch.put_table("accounts", b"acc:2", b"$250");
    batch.delete_table("accounts", b"acc:nonexistent");

    db.write_batch(batch).unwrap();

    let rx = db.begin_read().unwrap();
    assert_eq!(rx.get(b"root_key").unwrap(), Some(b"root_val".to_vec()));

    let accounts = rx.open_table("accounts").unwrap();
    assert_eq!(accounts.get(b"acc:1").unwrap(), Some(b"$100".to_vec()));
    assert_eq!(accounts.get(b"acc:2").unwrap(), Some(b"$250".to_vec()));
}

#[test]
fn test_durability_modes() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("durability_test.db");

    let db = Database::open_file(&db_path).unwrap();

    // Immediate
    db.set_durability(DurabilityMode::Immediate);
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"dur:1", b"val:1").unwrap();
        tx.commit().unwrap();
    }

    // Eventual with background flusher
    db.set_durability(DurabilityMode::Eventual { interval_ms: 10 });
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"dur:2", b"val:2").unwrap();
        tx.commit().unwrap();
    }

    // None
    db.set_durability(DurabilityMode::None);
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"dur:3", b"val:3").unwrap();
        tx.commit().unwrap();
    }

    let rx = db.begin_read().unwrap();
    assert_eq!(rx.get(b"dur:1").unwrap(), Some(b"val:1".to_vec()));
    assert_eq!(rx.get(b"dur:2").unwrap(), Some(b"val:2".to_vec()));
    assert_eq!(rx.get(b"dur:3").unwrap(), Some(b"val:3".to_vec()));
}

#[test]
fn test_audit_and_change_feed_hooks() {
    let db = Database::open_memory().unwrap();

    // 1. AuditLogHook
    let audit_records = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let audit_records_clone = Arc::clone(&audit_records);
    let audit_hook = AuditLogHook::new(move |changes| {
        let mut list = audit_records_clone.lock();
        list.push(changes.clone());
    });
    let hook_id = db.add_hook(Box::new(audit_hook));

    // 2. ChangeFeedHook
    let (tx, rx) = std::sync::mpsc::channel();
    let feed_hook = ChangeFeedHook::new(tx);
    let feed_hook_id = db.add_hook(Box::new(feed_hook));

    // Execute transaction
    {
        let mut wtx = db.begin_write().unwrap();
        wtx.put(b"k_hook", b"v_hook").unwrap();
        wtx.commit().unwrap();
    }

    // Verify audit log captured mutation
    {
        let list = audit_records.lock();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].writes.len(), 1);
        assert_eq!(list[0].writes[0].0, b"k_hook");
        assert_eq!(list[0].writes[0].1, b"v_hook");
    }

    // Verify change feed channel received event
    let feed_event = rx.recv().unwrap();
    assert_eq!(feed_event.writes.len(), 1);
    assert_eq!(feed_event.writes[0].0, b"k_hook");

    // Remove hooks
    assert!(db.remove_hook(hook_id));
    assert!(db.remove_hook(feed_hook_id));
    assert!(!db.remove_hook(999)); // Nonexistent ID
}

#[test]
fn test_typed_table_scans() {
    let db = Database::open_memory().unwrap();
    db.create_table("scores").unwrap();

    {
        let mut tx = db.begin_write().unwrap();
        let mut scores: TypedTableMut<'_, '_, _, String, u32> =
            TypedTableMut::new(tx.open_table_mut("scores").unwrap());
        scores.put(&"Alice".into(), &95).unwrap();
        scores.put(&"Bob".into(), &88).unwrap();
        scores.put(&"Charlie".into(), &72).unwrap();
        tx.commit().unwrap();
    }

    let rx = db.begin_read().unwrap();
    let scores: TypedTable<'_, _, String, u32> = TypedTable::new(rx.open_table("scores").unwrap());

    let all_entries = scores.scan_all().unwrap();
    assert_eq!(all_entries.len(), 3);

    let ranged = scores
        .scan_range(Some(&"Alice".into()), Some(&"Bob".into()))
        .unwrap();
    assert_eq!(ranged.len(), 2);
    assert_eq!(ranged[0].0, "Alice");
    assert_eq!(ranged[0].1, 95);
    assert_eq!(ranged[1].0, "Bob");
    assert_eq!(ranged[1].1, 88);
}

#[test]
fn test_query_builder_filter_and_projection() {
    let db = Database::open_memory().unwrap();
    db.create_table("employees").unwrap();

    {
        let mut tx = db.begin_write().unwrap();
        let mut table = tx.open_table_mut("employees").unwrap();
        table.put(b"emp:1", b"Engineer:5000").unwrap();
        table.put(b"emp:2", b"Manager:9000").unwrap();
        table.put(b"emp:3", b"Engineer:6500").unwrap();
        table.put(b"emp:4", b"Intern:2000").unwrap();
        tx.commit().unwrap();
    }

    let rx = db.begin_read().unwrap();
    let table = rx.open_table("employees").unwrap();

    // Query: Filter employees that are Engineers and project their (ID, salary)
    let engineers: Vec<(String, u32)> = QueryBuilder::new(&table)
        .filter(|_k, v| v.starts_with(b"Engineer:"))
        .project(|k, v| {
            let id = String::from_utf8_lossy(k).to_string();
            let v_str = String::from_utf8_lossy(v);
            let salary: u32 = v_str["Engineer:".len()..].parse().unwrap_or(0);
            (id, salary)
        })
        .unwrap();

    assert_eq!(engineers.len(), 2);
    assert_eq!(engineers[0], ("emp:1".to_string(), 5000));
    assert_eq!(engineers[1], ("emp:3".to_string(), 6500));
}

#[test]
fn test_secondary_index_removal() {
    let db = Database::open_memory().unwrap();
    db.create_table("users").unwrap();
    let index = SecondaryIndex::new("idx_age");

    // Insert 2 records
    {
        let mut tx = db.begin_write().unwrap();
        let mut tbl = tx.open_table_mut("users").unwrap();
        tbl.put(b"u1", b"data1").unwrap();
        tbl.put(b"u2", b"data2").unwrap();

        index.insert_entry(&mut tx, b"30", b"u1").unwrap();
        index.insert_entry(&mut tx, b"40", b"u2").unwrap();
        tx.commit().unwrap();
    }

    // Lookup
    {
        let rx = db.begin_read().unwrap();
        let pks = index.lookup_primary_keys(&rx, b"30").unwrap();
        assert_eq!(pks, vec![b"u1".to_vec()]);
    }

    // Remove entry
    {
        let mut tx = db.begin_write().unwrap();
        index.remove_entry(&mut tx, b"30", b"u1").unwrap();
        tx.commit().unwrap();
    }

    // Verify lookup is empty
    {
        let rx = db.begin_read().unwrap();
        let pks = index.lookup_primary_keys(&rx, b"30").unwrap();
        assert_eq!(pks.len(), 0);
        let pks2 = index.lookup_primary_keys(&rx, b"40").unwrap();
        assert_eq!(pks2, vec![b"u2".to_vec()]);
    }
}
