use super::*;
use std::sync::Arc;
use tej_db_pager::{MemStorage, PageCache};

#[test]
fn test_tx_commit_and_rollback() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 20));
    let manager = TxManager::open(cache).unwrap();

    // 1. Commit transaction
    {
        let mut tx = manager.begin_write().unwrap();
        tx.put(b"k1", b"v1").unwrap();
        tx.commit().unwrap();
    }

    // 2. Read committed value
    {
        let rx = manager.begin_read().unwrap();
        assert_eq!(rx.get(b"k1").unwrap(), Some(b"v1".to_vec()));
    }

    // 3. Rollback transaction
    {
        let mut tx = manager.begin_write().unwrap();
        tx.put(b"k2", b"v2").unwrap();
        tx.rollback().unwrap();
    }

    // 4. Verify k2 was discarded
    {
        let rx = manager.begin_read().unwrap();
        assert_eq!(rx.get(b"k2").unwrap(), None);
        assert_eq!(rx.get(b"k1").unwrap(), Some(b"v1".to_vec()));
    }
}

#[test]
fn test_multiple_tables_lifecycle() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 20));
    let manager = TxManager::open(cache).unwrap();

    // Create table and insert data
    {
        let mut tx = manager.begin_write().unwrap();
        tx.create_table("users").unwrap();
        tx.create_table("orders").unwrap();

        {
            let mut users = tx.open_table_mut("users").unwrap();
            users.put(b"alice", b"alice_data").unwrap();
        }
        {
            let mut orders = tx.open_table_mut("orders").unwrap();
            orders.put(b"order_1", b"100_usd").unwrap();
        }
        tx.commit().unwrap();
    }

    // Read from multiple tables
    {
        let rx = manager.begin_read().unwrap();
        let tables = rx.list_tables().unwrap();
        assert!(tables.contains(&"users".to_string()));
        assert!(tables.contains(&"orders".to_string()));

        let users = rx.open_table("users").unwrap();
        assert_eq!(users.get(b"alice").unwrap(), Some(b"alice_data".to_vec()));
        assert_eq!(users.get(b"order_1").unwrap(), None);

        let orders = rx.open_table("orders").unwrap();
        assert_eq!(orders.get(b"order_1").unwrap(), Some(b"100_usd".to_vec()));
        assert_eq!(orders.get(b"alice").unwrap(), None);
    }

    // Drop table
    {
        let mut tx = manager.begin_write().unwrap();
        assert!(tx.drop_table("users").unwrap());
        tx.commit().unwrap();
    }

    {
        let rx = manager.begin_read().unwrap();
        assert!(rx.open_table("users").is_err());
        let orders = rx.open_table("orders").unwrap();
        assert_eq!(orders.get(b"order_1").unwrap(), Some(b"100_usd".to_vec()));
    }
}

#[test]
fn test_table_open_mut_concurrency() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 100));
    let manager = Arc::new(TxManager::open(cache).unwrap());

    let table_names = ["users", "orders", "payments", "inventory"];
    {
        let mut tx = manager.begin_write().unwrap();
        for t in &table_names {
            tx.create_table(t).unwrap();
        }
        tx.commit().unwrap();
    }

    let mut handles = Vec::new();
    for w in 0..8 {
        let mgr = Arc::clone(&manager);
        let table = table_names[w % table_names.len()];
        let prefix = b'a' + (w as u8);
        handles.push(std::thread::spawn(move || {
            for i in 0..50u32 {
                let mut tx = mgr.begin_write().unwrap();
                {
                    let mut tbl = tx.open_table_mut(table).unwrap();
                    let k = [prefix, (i & 0xff) as u8];
                    tbl.put(&k, b"val").unwrap();
                }
                if let Err(e) = tx.commit() {
                    eprintln!("Thread {w} failed on iter {i}: {e:?}");
                    panic!("commit failed: {e:?}");
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn test_adaptive_spinlock_and_try_lock() {
    let lock = spin::AdaptiveSpinLock::new();
    assert!(lock.try_lock().is_some());

    let guard = lock.lock();
    assert!(lock.try_lock().is_none());
    drop(guard);

    assert!(lock.try_lock().is_some());
}

#[test]
fn test_occ_conflict_detection_helpers() {
    use crate::conflict::{raw_and_keys_intersect, tables_overlap};
    use tej_db_btree::{InlineKey, InlineVal};

    // Table overlap
    let t1 = vec!["users".to_string()];
    let t2 = vec!["orders".to_string()];
    let t3 = vec!["users".to_string(), "logs".to_string()];
    assert!(!tables_overlap(&t1, &t2));
    assert!(tables_overlap(&t1, &t3));

    // Key intersection
    let raw = vec![
        (
            InlineKey::from_slice(b"k1"),
            Some(InlineVal::from_slice(b"v1")),
            false,
        ),
        (
            InlineKey::from_slice(b"k3"),
            Some(InlineVal::from_slice(b"v3")),
            false,
        ),
    ];
    let disjoint_keys = vec![InlineKey::from_slice(b"k0"), InlineKey::from_slice(b"k2")];
    let overlapping_keys = vec![InlineKey::from_slice(b"k2"), InlineKey::from_slice(b"k3")];

    assert!(!raw_and_keys_intersect(&raw, &disjoint_keys));
    assert!(raw_and_keys_intersect(&raw, &overlapping_keys));
}

#[test]
fn test_transaction_savepoint_rollback_and_release() {
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 50));
    let manager = TxManager::open(cache).unwrap();

    let mut tx = manager.begin_write().unwrap();
    tx.put(b"k1", b"v1").unwrap();
    assert_eq!(tx.get(b"k1").unwrap(), Some(b"v1".to_vec()));

    // Create savepoint sp1
    tx.savepoint("sp1");

    tx.put(b"k2", b"v2").unwrap();
    assert_eq!(tx.get(b"k2").unwrap(), Some(b"v2".to_vec()));

    // Create savepoint sp2
    tx.savepoint("sp2");
    tx.put(b"k3", b"v3").unwrap();
    assert_eq!(tx.get(b"k3").unwrap(), Some(b"v3".to_vec()));

    // Rollback to sp2
    tx.rollback_to_savepoint("sp2").unwrap();
    assert_eq!(tx.get(b"k3").unwrap(), None);
    assert_eq!(tx.get(b"k2").unwrap(), Some(b"v2".to_vec()));
    assert_eq!(tx.get(b"k1").unwrap(), Some(b"v1".to_vec()));

    // Rollback to sp1
    tx.rollback_to_savepoint("sp1").unwrap();
    assert_eq!(tx.get(b"k2").unwrap(), None);
    assert_eq!(tx.get(b"k1").unwrap(), Some(b"v1".to_vec()));

    // Release sp1
    tx.release_savepoint("sp1").unwrap();
    assert!(tx.rollback_to_savepoint("sp1").is_err());

    // Commit transaction with k1
    tx.commit().unwrap();

    let rx = manager.begin_read().unwrap();
    assert_eq!(rx.get(b"k1").unwrap(), Some(b"v1".to_vec()));
    assert_eq!(rx.get(b"k2").unwrap(), None);
}
