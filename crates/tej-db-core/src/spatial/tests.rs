use super::bounds::{Aabb3D, BoundingSphere};
use super::gaussian_splat::{GAUSSIAN_SPLAT_HEADER_SIZE, GaussianSplatPageHeader};
use super::meshlet::{MESHLET_HEADER_SIZE, MeshletPageHeader};
use super::morton::{
    decode_spatial_key, dequantize_position, dilate_3d, encode_spatial_key, quantize_position,
    undilate_3d,
};

#[test]
fn test_morton_dilation_roundtrip() {
    for val in [0u32, 1, 42, 1024, 65535, 1_048_575] {
        let dilated = dilate_3d(val);
        let undilated = undilate_3d(dilated);
        assert_eq!(undilated, val, "Failed roundtrip for {val}");
    }
}

#[test]
fn test_spatial_key_encoding_decoding_roundtrip() {
    let test_cases = [
        (0u8, 0u32, 0u32, 0u32),
        (15u8, 1_048_575, 1_048_575, 1_048_575),
        (3u8, 100, 200, 300),
        (7u8, 999_999, 123_456, 654_321),
    ];

    for &(lod, x, y, z) in &test_cases {
        let key = encode_spatial_key(lod, x, y, z);
        let (d_lod, d_x, d_y, d_z) = decode_spatial_key(key);
        assert_eq!(d_lod, lod);
        assert_eq!(d_x, x);
        assert_eq!(d_y, y);
        assert_eq!(d_z, z);
    }
}

#[test]
fn test_spatial_quantization_roundtrip() {
    let domain = Aabb3D::new([-100.0, -50.0, 0.0], [100.0, 50.0, 200.0]);
    let original = [0.0f32, 0.0f32, 100.0f32];

    let (qx, qy, qz) = quantize_position(original, &domain);
    let reconstructed = dequantize_position(qx, qy, qz, &domain);

    for i in 0..3 {
        assert!(
            (original[i] - reconstructed[i]).abs() < 1e-3,
            "Axis {i} error too large: original={}, recon={}",
            original[i],
            reconstructed[i]
        );
    }
}

#[test]
fn test_bounds_containment_and_intersection() {
    let aabb = Aabb3D::new([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]);
    assert!(aabb.contains_point([5.0, 5.0, 5.0]));
    assert!(!aabb.contains_point([15.0, 5.0, 5.0]));

    let sphere = BoundingSphere::new([5.0, 5.0, 5.0], 5.0);
    assert!(sphere.contains_point([5.0, 5.0, 5.0]));
    assert!(sphere.contains_point([9.0, 5.0, 5.0]));
    assert!(!sphere.contains_point([12.0, 5.0, 5.0]));

    let other_aabb = Aabb3D::new([9.0, 9.0, 9.0], [20.0, 20.0, 20.0]);
    assert!(aabb.intersects_aabb(&other_aabb));

    let disjoint_aabb = Aabb3D::new([15.0, 15.0, 15.0], [20.0, 20.0, 20.0]);
    assert!(!aabb.intersects_aabb(&disjoint_aabb));
}

#[test]
fn test_meshlet_header_serialization_and_alignment() {
    assert_eq!(std::mem::size_of::<MeshletPageHeader>(), 64);
    assert_eq!(std::mem::align_of::<MeshletPageHeader>(), 64);
    assert_eq!(MESHLET_HEADER_SIZE, 64);

    let header = MeshletPageHeader::new(
        2,
        0,
        64,
        124,
        42,
        [1.0, 2.0, 3.0, 10.0],
        [-5.0, -5.0, -5.0],
        [5.0, 5.0, 5.0],
        0.05,
    );

    let bytes = header.to_bytes();
    assert_eq!(bytes.len(), 64);

    let parsed = MeshletPageHeader::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.lod_level, 2);
    assert_eq!(parsed.vertex_count, 64);
    assert_eq!(parsed.triangle_count, 124);
    assert_eq!(parsed.material_id, 42);
    assert_eq!(parsed.bounding_sphere, [1.0, 2.0, 3.0, 10.0]);
    assert_eq!(parsed.min_bounds, [-5.0, -5.0, -5.0]);
    assert_eq!(parsed.max_bounds, [5.0, 5.0, 5.0]);
    assert!((parsed.lod_error - 0.05).abs() < 1e-6);
}

#[test]
fn test_gaussian_splat_header_serialization_and_alignment() {
    assert_eq!(std::mem::size_of::<GaussianSplatPageHeader>(), 64);
    assert_eq!(std::mem::align_of::<GaussianSplatPageHeader>(), 64);
    assert_eq!(GAUSSIAN_SPLAT_HEADER_SIZE, 64);

    let header = GaussianSplatPageHeader::new(
        1,
        0,
        256,
        101,
        [0.0, 0.0, 0.0, 50.0],
        [-25.0, -25.0, -25.0],
        [25.0, 25.0, 25.0],
        0.1,
    );

    let bytes = header.to_bytes();
    assert_eq!(bytes.len(), 64);

    let parsed = GaussianSplatPageHeader::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.lod_level, 1);
    assert_eq!(parsed.splat_count, 256);
    assert_eq!(parsed.material_id, 101);
    assert_eq!(parsed.bounding_sphere, [0.0, 0.0, 0.0, 50.0]);
    assert_eq!(parsed.min_bounds, [-25.0, -25.0, -25.0]);
    assert_eq!(parsed.max_bounds, [25.0, 25.0, 25.0]);
    assert!((parsed.lod_error - 0.1).abs() < 1e-6);
}
