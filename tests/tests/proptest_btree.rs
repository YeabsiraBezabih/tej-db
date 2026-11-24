use proptest::prelude::*;
use std::collections::BTreeMap;
use tej_db::Database;

#[derive(Debug, Clone)]
enum Op {
    Put(Vec<u8>, Vec<u8>),
    Get(Vec<u8>),
    Delete(Vec<u8>),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (
            prop::collection::vec(any::<u8>(), 1..16),
            prop::collection::vec(any::<u8>(), 1..32)
        )
            .prop_map(|(k, v)| Op::Put(k, v)),
        prop::collection::vec(any::<u8>(), 1..16).prop_map(Op::Get),
        prop::collection::vec(any::<u8>(), 1..16).prop_map(Op::Delete),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    #[test]
    fn test_btree_matches_btreemap_model(ops in prop::collection::vec(op_strategy(), 50..120)) {
        let db = Database::open_memory().unwrap();
        let mut model = BTreeMap::new();

        for op in ops {
            match op {
                Op::Put(k, v) => {
                    let mut tx = db.begin_write().unwrap();
                    tx.put(&k, &v).unwrap();
                    tx.commit().unwrap();
                    model.insert(k, v);
                }
                Op::Get(k) => {
                    let rx = db.begin_read().unwrap();
                    let db_val = rx.get(&k).unwrap();
                    let model_val = model.get(&k).cloned();
                    prop_assert_eq!(db_val, model_val);
                }
                Op::Delete(k) => {
                    let mut tx = db.begin_write().unwrap();
                    let db_deleted = tx.delete(&k).unwrap();
                    tx.commit().unwrap();
                    let model_deleted = model.remove(&k).is_some();
                    prop_assert_eq!(db_deleted, model_deleted);
                }
            }
        }
    }
}

#[test]
fn test_btree_guaranteed_page_splits_and_structural_invariants() {
    let db = Database::open_memory().unwrap();
    let mut model = BTreeMap::new();

    // 1. Insert 250 key-value pairs (guaranteeing multiple leaf splits and root split)
    {
        let mut tx = db.begin_write().unwrap();
        for i in 0..250u32 {
            let key = format!("split_key_{i:06}").into_bytes();
            let val = format!("split_payload_content_for_item_{i:06}_{}", i * 17).into_bytes();
            tx.put(&key, &val).unwrap();
            model.insert(key, val);
        }
        tx.commit().unwrap();
    }

    // Verify stats prove multiple pages were allocated
    let stats = db.stats();
    assert!(
        stats.total_pages >= 4,
        "Total pages must exceed 4 after 250 insertions (got {})",
        stats.total_pages
    );

    // 2. Full range scan verification against model
    {
        let rx = db.begin_read().unwrap();
        let mut scanned_count = 0;
        for (k, v) in &model {
            let db_v = rx.get(k).unwrap();
            assert_eq!(db_v.as_ref(), Some(v));
            scanned_count += 1;
        }
        assert_eq!(scanned_count, 250);
    }

    // 3. Delete every other key to test underflow / merge / leaf compaction
    {
        let mut tx = db.begin_write().unwrap();
        for i in (0..250u32).step_by(2) {
            let key = format!("split_key_{i:06}").into_bytes();
            let deleted = tx.delete(&key).unwrap();
            assert!(deleted);
            model.remove(&key);
        }
        tx.commit().unwrap();
    }

    // 4. Verify remaining 125 keys match model exactly
    {
        let rx = db.begin_read().unwrap();
        for i in 0..250u32 {
            let key = format!("split_key_{i:06}").into_bytes();
            let db_v = rx.get(&key).unwrap();
            if i % 2 == 0 {
                assert_eq!(db_v, None);
            } else {
                let model_v = model.get(&key).cloned();
                assert_eq!(db_v, model_v);
            }
        }
    }
}
