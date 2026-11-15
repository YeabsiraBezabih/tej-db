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
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn test_btree_matches_btreemap_model(ops in prop::collection::vec(op_strategy(), 1..40)) {
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
