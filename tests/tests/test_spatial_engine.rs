use tej_db::{Database, RaycastScope};
use tej_db_core::spatial::{
    Aabb3D, GAUSSIAN_SPLAT_HEADER_SIZE, GaussianSplatPageHeader, MeshletPageHeader,
    decode_spatial_key, encode_spatial_key, quantize_position,
};
use tej_db_core::{KeyRange, Page, PageType};
use tej_db_vectorized::spatial_culling::{FrustumPlanes, Ray3D};

#[test]
fn test_nanite_meshlet_zero_copy_and_culling() {
    let db = Database::open_memory().unwrap();
    let domain = Aabb3D::new([-100.0, -100.0, -100.0], [100.0, 100.0, 100.0]);
    let mut viewport = db
        .create_spatial_viewport("scene_meshlets", domain)
        .unwrap();

    // Store 4 meshlet clusters:
    // Cluster 1: In front of camera at Z = -10 (LOD 0, detailed)
    let header1 = MeshletPageHeader::new(
        0,
        1,
        64,
        124,
        10,
        [0.0, 0.0, -10.0, 2.0],
        [-2.0, -2.0, -12.0],
        [2.0, 2.0, -8.0],
        0.02,
    );
    let vdata1 = vec![0x11; 256];
    let page1 = viewport.store_meshlet_page(1, &header1, &vdata1).unwrap();

    // Cluster 2: In front of camera at Z = -30 (LOD 1, coarse)
    let header2 = MeshletPageHeader::new(
        1,
        1,
        32,
        60,
        10,
        [0.0, 0.0, -30.0, 4.0],
        [-4.0, -4.0, -34.0],
        [4.0, 4.0, -26.0],
        0.08,
    );
    let vdata2 = vec![0x22; 128];
    let page2 = viewport.store_meshlet_page(2, &header2, &vdata2).unwrap();

    // Cluster 3: Behind camera at Z = +15 (should be culled by near/direction)
    let header3 = MeshletPageHeader::new(
        0,
        1,
        64,
        124,
        11,
        [0.0, 0.0, 15.0, 2.0],
        [-2.0, -2.0, 13.0],
        [2.0, 2.0, 17.0],
        0.02,
    );
    let vdata3 = vec![0x33; 256];
    let _page3 = viewport.store_meshlet_page(3, &header3, &vdata3).unwrap();

    assert_eq!(viewport.cluster_count(), 3);

    // Verify zero-copy retrieval
    viewport
        .with_meshlet_bytes(page1, |bytes| {
            let parsed = MeshletPageHeader::from_page_bytes(bytes).unwrap();
            assert_eq!(parsed.magic, 0x4D53_484C);
            assert_eq!(parsed.vertex_count, 64);
            let payload = MeshletPageHeader::payload_slice(bytes);
            assert_eq!(&payload[..256], &vdata1[..]);
        })
        .unwrap();

    viewport
        .with_meshlet_bytes(page2, |bytes| {
            let parsed = MeshletPageHeader::from_page_bytes(bytes).unwrap();
            assert_eq!(parsed.magic, 0x4D53_484C);
            assert_eq!(parsed.vertex_count, 32);
            let payload = MeshletPageHeader::payload_slice(bytes);
            assert_eq!(&payload[..128], &vdata2[..]);
        })
        .unwrap();

    // Camera at origin, looking along -Z
    let mut planes = [[0.0f32; 4]; 6];
    planes[4] = [0.0, 0.0, -1.0, 1.0]; // Near plane facing -Z
    planes[5] = [0.0, 0.0, 1.0, 50.0]; // Far plane at Z = -50
    planes[0] = [1.0, 0.0, 0.0, 20.0]; // Left plane
    planes[1] = [-1.0, 0.0, 0.0, 20.0]; // Right plane
    planes[2] = [0.0, 1.0, 0.0, 20.0]; // Bottom plane
    planes[3] = [0.0, -1.0, 0.0, 20.0]; // Top plane
    let frustum = FrustumPlanes::new(planes);

    // Frame 1: Camera at origin
    let cull1 = viewport.on_camera_move(&frustum, [0.0, 0.0, 0.0], 0.05);
    // Cluster 1 (dist 10, lod_error 0.02 / 10 = 0.002 <= 0.05) -> Visible
    // Cluster 2 (dist 30, lod_error 0.08 / 30 = 0.00267 <= 0.05) -> Visible
    // Cluster 3 is behind the camera (dist < 0) -> Culled
    assert_eq!(cull1.visible_clusters.len(), 2);
    assert!(cull1.visible_clusters.contains(1));
    assert!(cull1.visible_clusters.contains(2));
    assert!(!cull1.visible_clusters.contains(3));
    assert_eq!(cull1.to_upload.len(), 2);
    assert_eq!(cull1.to_evict, Vec::<u32>::new());

    // Frame 2: Same position -> 0 differential uploads or evictions
    let cull2 = viewport.on_camera_move(&frustum, [0.0, 0.0, 0.0], 0.05);
    assert_eq!(cull2.visible_clusters.len(), 2);
    assert_eq!(cull2.to_upload, Vec::<u32>::new());
    assert_eq!(cull2.to_evict, Vec::<u32>::new());

    // Frame 3: Turn camera 180 degrees (looking along +Z)
    let mut planes_behind = [[0.0f32; 4]; 6];
    planes_behind[4] = [0.0, 0.0, 1.0, 1.0]; // Near plane facing +Z
    planes_behind[5] = [0.0, 0.0, -1.0, 50.0]; // Far plane at Z = +50
    planes_behind[0] = [1.0, 0.0, 0.0, 20.0];
    planes_behind[1] = [-1.0, 0.0, 0.0, 20.0];
    planes_behind[2] = [0.0, 1.0, 0.0, 20.0];
    planes_behind[3] = [0.0, -1.0, 0.0, 20.0];
    let frustum_behind = FrustumPlanes::new(planes_behind);

    let cull3 = viewport.on_camera_move(&frustum_behind, [0.0, 0.0, 0.0], 0.05);
    // Clusters 1 & 2 are now evicted, Cluster 3 is newly visible
    assert_eq!(cull3.visible_clusters.len(), 1);
    assert!(cull3.visible_clusters.contains(3));
    assert_eq!(cull3.to_upload, vec![3]);
    assert_eq!(cull3.to_evict.len(), 2);
    assert!(cull3.to_evict.contains(&1));
    assert!(cull3.to_evict.contains(&2));
}

#[test]
fn test_spatial_raycast_picking_and_undo_redo() {
    let db = Database::open_memory().unwrap();
    let domain = Aabb3D::new([-50.0, -50.0, -50.0], [50.0, 50.0, 50.0]);
    let mut viewport = db.create_spatial_viewport("picking_scene", domain).unwrap();

    // Cluster at X = 0, Y = 0, Z in [-12, -8] (center at -10)
    let header = MeshletPageHeader::new(
        0,
        1,
        64,
        124,
        1,
        [0.0, 0.0, -10.0, 2.0],
        [-2.0, -2.0, -12.0],
        [2.0, 2.0, -8.0],
        0.01,
    );
    let _ = viewport.store_meshlet_page(10, &header, &[]).unwrap();

    // Ray directed straight at the cluster along -Z
    let ray = Ray3D::new([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
    let hit = viewport.pick_ray(&ray, RaycastScope::AllClusters);
    assert!(hit.is_some());
    let (cluster_id, hit_dist) = hit.unwrap();
    assert_eq!(cluster_id, 10);
    // Entry at min Z = -8 (distance = 8.0)
    assert!((hit_dist - 8.0).abs() < 1e-4);

    // Apply gizmo transformation: translate +10 along X
    let transform = tej_db::GizmoTransform {
        cluster_id: 10,
        translation: [10.0, 0.0, 0.0],
    };
    assert!(viewport.apply_transform(transform).unwrap());

    // The original ray along X = 0, Y = 0 should now MISS the cluster
    let hit_miss = viewport.pick_ray(&ray, RaycastScope::AllClusters);
    assert!(hit_miss.is_none());

    // A ray along X = 10, Y = 0 should HIT the moved cluster
    let ray_shifted = Ray3D::new([10.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
    let hit_shifted = viewport.pick_ray(&ray_shifted, RaycastScope::AllClusters);
    assert!(hit_shifted.is_some());
    assert_eq!(hit_shifted.unwrap().0, 10);

    // UNDO the transform
    let undone = viewport.undo().unwrap();
    assert!(undone.is_some());

    // Ray along X = 0 should HIT again
    let hit_reverted = viewport.pick_ray(&ray, RaycastScope::AllClusters);
    assert!(hit_reverted.is_some());
    assert_eq!(hit_reverted.unwrap().0, 10);

    // REDO the transform
    let redone = viewport.redo().unwrap();
    assert!(redone.is_some());

    // Ray along X = 0 should MISS again
    assert!(viewport.pick_ray(&ray, RaycastScope::AllClusters).is_none());
}

#[test]
fn test_gaussian_splat_page_storage() {
    let db = Database::open_memory().unwrap();
    let tx_mgr = db.tx_manager();

    let page_id = tx_mgr.cache().allocate_page().unwrap();
    let mut page = Page::new(page_id, PageType::GaussianSplatNode);

    let splat_header = GaussianSplatPageHeader::new(
        0,
        1,
        128,
        5,
        [1.0, 2.0, 3.0, 0.5],
        [0.5, 1.5, 2.5],
        [1.5, 2.5, 3.5],
        0.001,
    );

    let header_bytes = splat_header.to_bytes();
    page.data[64..64 + GAUSSIAN_SPLAT_HEADER_SIZE].copy_from_slice(&header_bytes);

    // Dummy spherical harmonics and covariance attributes
    let splat_attributes = vec![0xBE; 1024];
    page.data[128..128 + 1024].copy_from_slice(&splat_attributes);
    page.update_checksum();

    tx_mgr.cache().write_page(page).unwrap();

    // Read back and verify zero-copy parsing
    let loaded_page = tx_mgr.cache().get_page(page_id).unwrap();
    let parsed_splat = GaussianSplatPageHeader::from_page_bytes(loaded_page.as_slice()).unwrap();
    assert_eq!(parsed_splat.magic, 0x4753_504C);
    assert_eq!(parsed_splat.splat_count, 128);
    assert_eq!(parsed_splat.material_id, 5);

    let payload = GaussianSplatPageHeader::payload_slice(loaded_page.as_slice());
    assert_eq!(&payload[..1024], &splat_attributes[..]);
}

#[test]
fn test_morton_spatial_locality_bplus_tree() {
    let domain = Aabb3D::new([0.0, 0.0, 0.0], [100.0, 100.0, 100.0]);

    // Points that are close in 3D space
    let p1 = [10.0, 10.0, 10.0];
    let p2 = [10.1, 10.1, 10.1];
    let p3 = [90.0, 90.0, 90.0];

    let (x1, y1, z1) = quantize_position(p1, &domain);
    let (x2, y2, z2) = quantize_position(p2, &domain);
    let (x3, y3, z3) = quantize_position(p3, &domain);

    let k1 = encode_spatial_key(0, x1, y1, z1);
    let k2 = encode_spatial_key(0, x2, y2, z2);
    let k3 = encode_spatial_key(0, x3, y3, z3);

    // k1 and k2 share common prefix bits and are closer in Morton space than k1 and k3
    let diff_12 = k1.abs_diff(k2);
    let diff_13 = k1.abs_diff(k3);
    assert!(diff_12 < diff_13);

    // Verify roundtrip decoding
    let (lod1, rx1, ry1, rz1) = decode_spatial_key(k1);
    assert_eq!(lod1, 0);
    assert_eq!(rx1, x1);
    assert_eq!(ry1, y1);
    assert_eq!(rz1, z1);

    // Test B+ Tree range scan query over Morton keys
    let db = Database::open_memory().unwrap();
    db.create_table("spatial_index").unwrap();

    let mut tx = db.begin_write().unwrap();
    {
        let mut tbl = tx.open_table_mut("spatial_index").unwrap();
        tbl.put(&k1.to_be_bytes(), b"cluster_1").unwrap();
        tbl.put(&k2.to_be_bytes(), b"cluster_2").unwrap();
        tbl.put(&k3.to_be_bytes(), b"cluster_3").unwrap();
    }
    tx.commit().unwrap();

    let rx = db.begin_read().unwrap();
    let tbl = rx.open_table("spatial_index").unwrap();

    // Range query bounding the neighborhood around p1 & p2
    let min_k = k1.min(k2).to_be_bytes();
    let max_k = (k1.max(k2) + 1).to_be_bytes();
    let range = KeyRange::between(min_k.to_vec(), max_k.to_vec());

    let neighborhood = tbl.scan_range(&range).unwrap();
    assert_eq!(neighborhood.len(), 2);
    assert_eq!(neighborhood[0].value, b"cluster_1");
    assert_eq!(neighborhood[1].value, b"cluster_2");
}
