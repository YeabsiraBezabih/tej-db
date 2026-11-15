use crate::{RaBitQQuantizer, VamanaSearcher, VectorCompactor, VectorId, insert_vector_delta};
use std::sync::Arc;
use tej_db_pager::{MemStorage, PageCache};
use tej_db_txn::TxManager;

#[test]
#[allow(clippy::cast_precision_loss)]
fn test_vector_engine_full_lifecycle() {
    let dim = 16;
    let storage = MemStorage::new();
    let cache = Arc::new(PageCache::new(storage, 50));
    let manager = Arc::new(TxManager::open(cache).unwrap());
    let quantizer = RaBitQQuantizer::new(dim, 42);

    // 1. Insert 10 vectors into the delta buffer table
    {
        let mut tx = manager.begin_write().unwrap();
        for i in 1..=10u64 {
            let mut vec = vec![0.0f32; dim];
            vec[0] = i as f32;
            vec[1] = (i * 2) as f32;
            insert_vector_delta(&mut tx, "items", VectorId(i), &vec).unwrap();
        }
        tx.commit().unwrap();
    }

    // 2. Compact delta buffer into persistent Vamana graph
    let compactor = VectorCompactor::new(Arc::clone(&manager), quantizer.clone());
    let root_page = compactor
        .compact_table_delta("items", tej_db_core::PageId::INVALID)
        .unwrap();
    assert!(root_page.is_valid());

    // 3. Search Vamana graph for vector closest to [5.0, 10.0, 0.0, ...]
    let searcher = VamanaSearcher::new(Arc::clone(manager.cache()), quantizer);
    let mut query = vec![0.0f32; dim];
    query[0] = 5.0;
    query[1] = 10.0;

    let results = searcher.search(root_page, &query, 16, 3, None).unwrap();
    assert_eq!(results.len(), 3);
    // VectorId(5) should be the closest candidate
    assert_eq!(results[0].vector_id, VectorId(5));

    // 4. Test pre-filtered search: exclude odd VectorIds
    let filtered_results = searcher
        .search(root_page, &query, 16, 3, Some(&|id| id.0 % 2 == 0))
        .unwrap();
    assert_eq!(filtered_results.len(), 3);
    assert!(filtered_results.iter().all(|c| c.vector_id.0 % 2 == 0));
    // Closest even candidate should be VectorId(4) or VectorId(6)
    assert!(
        filtered_results[0].vector_id == VectorId(4)
            || filtered_results[0].vector_id == VectorId(6)
    );
}

#[test]
fn test_vector_rerank_fidelity() {
    let dim = 8;
    let query = vec![1.0; dim];
    let candidates = vec![
        crate::SearchCandidate {
            page_id: tej_db_core::PageId(1),
            vector_id: VectorId(1),
            distance: 1.5,
        },
        crate::SearchCandidate {
            page_id: tej_db_core::PageId(2),
            vector_id: VectorId(2),
            distance: 2.0,
        },
    ];

    let full_vectors = [
        (VectorId(1), vec![1.0; dim]), // distance 0.0
        (VectorId(2), vec![2.0; dim]), // distance > 0.0
    ];

    let reranked = VamanaSearcher::<MemStorage>::rerank(&candidates, &query, |id| {
        Ok(full_vectors
            .iter()
            .find(|(vid, _)| *vid == id)
            .map(|(_, v)| v.clone()))
    })
    .unwrap();

    assert_eq!(reranked[0].vector_id, VectorId(1));
    assert_eq!(reranked[0].distance, 0.0);
}
