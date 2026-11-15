use std::fs::OpenOptions;
use std::io::Write;
use std::thread;
use std::time::Duration;
use tej_db::{Database, DurabilityMode};

#[test]
fn test_durability_across_engine_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("durability.db");

    // Session 1: write data and cleanly close
    {
        let db = Database::open_file(&db_path).unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.put(b"persistent_key", b"durable_value").unwrap();
        tx.commit().unwrap();
    }

    // Session 2: reopen database file and verify persistence
    {
        let db = Database::open_file(&db_path).unwrap();
        let rx = db.begin_read().unwrap();
        assert_eq!(
            rx.get(b"persistent_key").unwrap(),
            Some(b"durable_value".to_vec())
        );
    }
}

#[test]
fn test_mid_commit_crash_discards_uncommitted_root() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("crash_recovery.db");

    // 1. Initial committed state
    {
        let db = Database::open_file(&db_path).unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.put(b"committed_k1", b"val1").unwrap();
        tx.put(b"committed_k2", b"val2").unwrap();
        tx.commit().unwrap();
    }

    // 2. Simulate torn write / crash:
    // Append 4096 bytes of uncommitted garbage/torn leaf page to the end of the file.
    // The meta page (Page 0) still references the valid committed root.
    {
        let mut file = OpenOptions::new().append(true).open(&db_path).unwrap();
        let uncommitted_data = [0xEEu8; 4096];
        file.write_all(&uncommitted_data).unwrap();
        file.sync_all().unwrap();
    }

    // 3. Reopen database: engine must successfully open, read committed root, and ignore uncommitted trailing pages
    {
        let db = Database::open_file(&db_path).unwrap();
        let rx = db.begin_read().unwrap();
        assert_eq!(rx.get(b"committed_k1").unwrap(), Some(b"val1".to_vec()));
        assert_eq!(rx.get(b"committed_k2").unwrap(), Some(b"val2".to_vec()));

        // Writing new transactions works cleanly and allocates beyond the file safely
        drop(rx);
        let mut tx = db.begin_write().unwrap();
        tx.put(b"new_k3", b"val3").unwrap();
        tx.commit().unwrap();
    }

    // 4. Verify new key persisted alongside old keys
    {
        let db = Database::open_file(&db_path).unwrap();
        let rx = db.begin_read().unwrap();
        assert_eq!(rx.get(b"committed_k1").unwrap(), Some(b"val1".to_vec()));
        assert_eq!(rx.get(b"new_k3").unwrap(), Some(b"val3".to_vec()));
    }
}

#[test]
fn test_durability_mode_eventual_flushes_background() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("eventual_flush.db");

    // Session 1: Write with Eventual durability mode
    {
        let db = Database::open_file(&db_path).unwrap();
        db.set_durability(DurabilityMode::Eventual { interval_ms: 15 });
        let mut tx = db.begin_write().unwrap();
        tx.put(b"eventual_key", b"eventual_val").unwrap();
        tx.commit().unwrap();

        // Wait for background flusher thread to tick and fsync
        thread::sleep(Duration::from_millis(60));
    }

    // Session 2: Reopen without calling explicit sync, data must be durable on disk
    {
        let db = Database::open_file(&db_path).unwrap();
        let rx = db.begin_read().unwrap();
        assert_eq!(
            rx.get(b"eventual_key").unwrap(),
            Some(b"eventual_val".to_vec())
        );
    }
}

#[test]
fn test_durability_mode_none_does_not_persist_across_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("none_durability.db");

    // Session 1: Commit initial baseline with Normal durability
    {
        let db = Database::open_file(&db_path).unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.put(b"baseline", b"persisted").unwrap();
        tx.commit().unwrap();
    }

    // Session 2: Set DurabilityMode::None and commit scratch mutations
    {
        let db = Database::open_file(&db_path).unwrap();
        db.set_durability(DurabilityMode::None);

        let mut tx = db.begin_write().unwrap();
        tx.put(b"scratch_key", b"volatile_data").unwrap();
        tx.commit().unwrap();

        // Visible within the active cache
        let rx = db.begin_read().unwrap();
        assert_eq!(
            rx.get(b"scratch_key").unwrap(),
            Some(b"volatile_data".to_vec())
        );
    }

    // Session 3: Reopen database. Because Mode::None bypasses disk writes, scratch_key was not flushed to disk
    {
        let db = Database::open_file(&db_path).unwrap();
        let rx = db.begin_read().unwrap();
        assert_eq!(rx.get(b"baseline").unwrap(), Some(b"persisted".to_vec()));
        assert_eq!(rx.get(b"scratch_key").unwrap(), None);
    }
}
