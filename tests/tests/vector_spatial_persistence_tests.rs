use std::sync::Arc;
use tej_db::{
    Aabb3D, BoundingSphere, ClusterMetadata, Database, DistanceMetric, PageId,
    SpatialViewportManager, VectorId, VectorIndex,
};
use tempfile::NamedTempFile;

#[test]
#[allow(clippy::cast_precision_loss)]
fn test_vector_multibatch_persistence_and_delta_merge() {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_str().expect("valid path string").to_string();

    // 1. Initial batch insert and compaction
    {
        let db = Database::open_file(&path).expect("open file");
        let v_idx = VectorIndex::new(
            "vectors",
            16,
            DistanceMetric::L2,
            Arc::clone(db.tx_manager()),
        );

        let mut tx = db.begin_write().expect("begin write");
        for i in 0..10u64 {
            let mut vec = vec![0.0f32; 16];
            vec[0] = i as f32;
            vec[1] = (i * 2) as f32;
            v_idx
                .insert(&mut tx, VectorId(i), &vec)
                .expect("insert vector");
        }
        tx.commit().expect("commit tx");

        // Search uncompacted delta
        let mut query = vec![0.0f32; 16];
        query[0] = 0.0;
        let rx = db.begin_read().expect("begin read");
        let results = v_idx
            .search(&rx, &query, 16, 3, None)
            .expect("search uncompacted");
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].vector_id, VectorId(0));

        // Compact into disk graph
        v_idx.compact().expect("compact vectors");

        // Verify search after compaction
        let rx2 = db.begin_read().expect("begin read 2");
        let results2 = v_idx
            .search(&rx2, &query, 16, 3, None)
            .expect("search compacted");
        assert_eq!(results2.len(), 3);
        assert_eq!(results2[0].vector_id, VectorId(0));
    }

    // 2. Reopen DB from file and verify persisted graph survives
    {
        let db = Database::recover_file(&path).expect("reopen file");
        let v_idx = VectorIndex::new(
            "vectors",
            16,
            DistanceMetric::L2,
            Arc::clone(db.tx_manager()),
        );

        let mut query = vec![0.0f32; 16];
        query[0] = 0.0;
        let rx = db.begin_read().expect("begin read");
        let results = v_idx
            .search(&rx, &query, 16, 3, None)
            .expect("search persisted graph");
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].vector_id, VectorId(0));

        // 3. Insert second batch and verify delta merge
        let mut tx = db.begin_write().expect("begin write batch 2");
        for i in 10..20u64 {
            let mut vec = vec![0.0f32; 16];
            vec[0] = i as f32;
            vec[1] = (i * 2) as f32;
            v_idx
                .insert(&mut tx, VectorId(i), &vec)
                .expect("insert vector batch 2");
        }
        tx.commit().expect("commit tx batch 2");

        // Query target closer to second batch
        let mut query20 = vec![0.0f32; 16];
        query20[0] = 19.0;
        query20[1] = 38.0;
        let rx2 = db.begin_read().expect("begin read");
        let results_uncompacted = v_idx
            .search(&rx2, &query20, 16, 3, None)
            .expect("search uncompacted delta batch 2");
        assert_eq!(results_uncompacted[0].vector_id, VectorId(19));

        // Compact second batch into existing graph
        v_idx.compact().expect("compact batch 2");

        let rx3 = db.begin_read().expect("begin read 3");
        let results_compacted = v_idx
            .search(&rx3, &query20, 16, 3, None)
            .expect("search compacted batch 2");
        assert_eq!(results_compacted[0].vector_id, VectorId(19));
    }
}

#[test]
fn test_spatial_morton_collision_and_restart_recovery() {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_str().expect("valid path string").to_string();

    let domain = Aabb3D::new([-100.0, -100.0, -100.0], [100.0, 100.0, 100.0]);

    // 1. Insert clusters with identical spatial position (Morton collision)
    {
        let db = Database::open_file(&path).expect("open file");
        let mut mgr = SpatialViewportManager::new(Arc::clone(db.tx_manager()), "clusters", domain)
            .expect("new spatial mgr");

        let cluster_a = ClusterMetadata::new(
            101,
            Aabb3D::new([9.0, 9.0, 9.0], [11.0, 11.0, 11.0]),
            BoundingSphere::new([10.0, 10.0, 10.0], 1.0),
            0,
            0.1,
            PageId(10),
        );

        let cluster_b = ClusterMetadata::new(
            102,
            Aabb3D::new([9.0, 9.0, 9.0], [11.0, 11.0, 11.0]),
            BoundingSphere::new([10.0, 10.0, 10.0], 1.0), // Exact same position -> Morton collision
            0,
            0.2,
            PageId(11),
        );

        let cluster_c = ClusterMetadata::new(
            103,
            Aabb3D::new([48.0, 48.0, 48.0], [52.0, 52.0, 52.0]),
            BoundingSphere::new([50.0, 50.0, 50.0], 2.0),
            1,
            0.5,
            PageId(12),
        );

        mgr.register_cluster(cluster_a).expect("register cluster a");
        mgr.register_cluster(cluster_b).expect("register cluster b");
        mgr.register_cluster(cluster_c).expect("register cluster c");
        assert_eq!(mgr.cluster_count(), 3);
    }

    // 2. Reopen DB and ensure both colliding clusters and third cluster survive
    {
        let db = Database::recover_file(&path).expect("reopen file");
        let mgr = SpatialViewportManager::new(Arc::clone(db.tx_manager()), "clusters", domain)
            .expect("reload spatial mgr");

        assert_eq!(mgr.cluster_count(), 3);

        let found_a = mgr.get_cluster(101).expect("find cluster a");
        assert_eq!(found_a.cluster_id, 101);
        assert_eq!(found_a.page_id, PageId(10));

        let found_b = mgr.get_cluster(102).expect("find cluster b");
        assert_eq!(found_b.cluster_id, 102);
        assert_eq!(found_b.page_id, PageId(11));

        let found_c = mgr.get_cluster(103).expect("find cluster c");
        assert_eq!(found_c.cluster_id, 103);
        assert_eq!(found_c.page_id, PageId(12));
    }
}
