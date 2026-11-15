use std::sync::Arc;

use tej_db_core::spatial::{Aabb3D, MeshletPageHeader};
use tej_db_pager::MemStorage;
use tej_db_txn::TxManager;
use tej_db_vectorized::spatial_culling::{FrustumPlanes, Ray3D};

use super::{GizmoTransform, RaycastScope, SpatialViewportManager};

#[test]
fn test_spatial_viewport_lifecycle() {
    let storage = MemStorage::new();
    let cache = Arc::new(tej_db_pager::PageCache::new(storage, 64));
    let tx_manager = Arc::new(TxManager::open(cache).unwrap());

    let domain = Aabb3D::new([-100.0, -100.0, -100.0], [100.0, 100.0, 100.0]);
    let mut viewport = SpatialViewportManager::new(tx_manager, "spatial_meshlets", domain).unwrap();

    // 1. Store a meshlet page
    let header = MeshletPageHeader {
        magic: 0x4D53_484C,
        lod_level: 0,
        flags: 1,
        vertex_count: 64,
        triangle_count: 124,
        material_id: 42,
        bounding_sphere: [0.0, 0.0, -10.0, 2.0],
        min_bounds: [-2.0, -2.0, -12.0],
        max_bounds: [2.0, 2.0, -8.0],
        lod_error: 0.05,
        reserved: [0; 4],
    };

    let dummy_vertices = vec![0xAA; 512];
    let page_id = viewport
        .store_meshlet_page(101, &header, &dummy_vertices)
        .unwrap();

    assert_eq!(viewport.cluster_count(), 1);

    // Verify zero-copy read
    viewport
        .with_meshlet_bytes(page_id, |bytes| {
            let parsed = MeshletPageHeader::from_page_bytes(bytes).unwrap();
            assert_eq!(parsed.magic, 0x4D53_484C);
            assert_eq!(parsed.vertex_count, 64);
            let payload = MeshletPageHeader::payload_slice(bytes);
            assert_eq!(&payload[..512], &dummy_vertices[..]);
        })
        .unwrap();

    // 2. Frustum culling test
    let mut planes = [[0.0f32; 4]; 6];
    planes[4] = [0.0, 0.0, -1.0, 1.0]; // Near plane facing -Z
    planes[5] = [0.0, 0.0, 1.0, 50.0]; // Far plane at Z = -50
    planes[0] = [1.0, 0.0, 0.0, 20.0];
    planes[1] = [-1.0, 0.0, 0.0, 20.0];
    planes[2] = [0.0, 1.0, 0.0, 20.0];
    planes[3] = [0.0, -1.0, 0.0, 20.0];
    let frustum = FrustumPlanes::new(planes);

    let cull = viewport.on_camera_move(&frustum, [0.0, 0.0, 0.0], 0.1);
    assert_eq!(cull.visible_clusters.len(), 1);
    assert_eq!(cull.to_upload, vec![101]);
    assert_eq!(cull.to_evict, Vec::<u32>::new());

    // 3. Raycast picking
    let ray = Ray3D::new([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
    let hit = viewport.pick_ray(&ray, RaycastScope::VisibleOnly);
    assert!(hit.is_some());
    let (cluster_id, hit_dist) = hit.unwrap();
    assert_eq!(cluster_id, 101);
    assert!((hit_dist - 8.0).abs() < 1e-4);

    // 4. Gizmo Transform and Undo/Redo
    let transform = GizmoTransform {
        cluster_id: 101,
        translation: [0.0, 0.0, -5.0],
    };
    assert!(viewport.apply_transform(transform).unwrap());

    // Hit distance should now be 13.0
    let hit2 = viewport.pick_ray(&ray, RaycastScope::VisibleOnly).unwrap();
    assert!((hit2.1 - 13.0).abs() < 1e-4);

    // Undo
    assert!(viewport.undo().unwrap().is_some());
    let hit_undone = viewport.pick_ray(&ray, RaycastScope::VisibleOnly).unwrap();
    assert!((hit_undone.1 - 8.0).abs() < 1e-4);

    // Redo
    assert!(viewport.redo().unwrap().is_some());
    let hit_redone = viewport.pick_ray(&ray, RaycastScope::VisibleOnly).unwrap();
    assert!((hit_redone.1 - 13.0).abs() < 1e-4);
}
