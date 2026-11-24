use tej_db_core::spatial::{Aabb3D, BoundingSphere, encode_spatial_key, quantize_position};
use tej_db_core::{DbError, PageId, Result};

/// Metadata describing a 3D spatial cluster (meshlet or Gaussian splat cluster)
/// stored within the database.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterMetadata {
    pub cluster_id: u32,
    pub bounds: Aabb3D,
    pub sphere: BoundingSphere,
    pub lod_level: u8,
    pub lod_error: f32,
    pub page_id: PageId,
}

impl ClusterMetadata {
    pub const SERIALIZED_SIZE: usize = 57;

    #[must_use]
    pub const fn new(
        cluster_id: u32,
        bounds: Aabb3D,
        sphere: BoundingSphere,
        lod_level: u8,
        lod_error: f32,
        page_id: PageId,
    ) -> Self {
        Self {
            cluster_id,
            bounds,
            sphere,
            lod_level,
            lod_error,
            page_id,
        }
    }

    /// Serializes cluster metadata into a fixed-size 57-byte binary payload.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::SERIALIZED_SIZE] {
        let mut b = [0u8; Self::SERIALIZED_SIZE];
        b[0..4].copy_from_slice(&self.cluster_id.to_le_bytes());
        b[4..8].copy_from_slice(&self.bounds.min[0].to_le_bytes());
        b[8..12].copy_from_slice(&self.bounds.min[1].to_le_bytes());
        b[12..16].copy_from_slice(&self.bounds.min[2].to_le_bytes());
        b[16..20].copy_from_slice(&self.bounds.max[0].to_le_bytes());
        b[20..24].copy_from_slice(&self.bounds.max[1].to_le_bytes());
        b[24..28].copy_from_slice(&self.bounds.max[2].to_le_bytes());
        b[28..32].copy_from_slice(&self.sphere.center[0].to_le_bytes());
        b[32..36].copy_from_slice(&self.sphere.center[1].to_le_bytes());
        b[36..40].copy_from_slice(&self.sphere.center[2].to_le_bytes());
        b[40..44].copy_from_slice(&self.sphere.radius.to_le_bytes());
        b[44] = self.lod_level;
        b[45..49].copy_from_slice(&self.lod_error.to_le_bytes());
        b[49..57].copy_from_slice(&self.page_id.0.to_le_bytes());
        b
    }

    /// Deserializes cluster metadata from a binary payload, supporting backwards-compatible
    /// 4-byte payloads for legacy cluster IDs.
    pub fn from_bytes(slice: &[u8]) -> Result<Self> {
        if slice.len() < Self::SERIALIZED_SIZE {
            if slice.len() == 4 {
                let id = u32::from_le_bytes(slice[0..4].try_into().unwrap_or_default());
                return Ok(Self::new(
                    id,
                    Aabb3D::new([0.0; 3], [0.0; 3]),
                    BoundingSphere::new([0.0; 3], 0.0),
                    0,
                    0.0,
                    PageId::INVALID,
                ));
            }
            return Err(DbError::Corruption(
                "Truncated ClusterMetadata payload".into(),
            ));
        }

        let cluster_id = u32::from_le_bytes(slice[0..4].try_into().unwrap_or_default());
        let min_x = f32::from_le_bytes(slice[4..8].try_into().unwrap_or_default());
        let min_y = f32::from_le_bytes(slice[8..12].try_into().unwrap_or_default());
        let min_z = f32::from_le_bytes(slice[12..16].try_into().unwrap_or_default());
        let max_x = f32::from_le_bytes(slice[16..20].try_into().unwrap_or_default());
        let max_y = f32::from_le_bytes(slice[20..24].try_into().unwrap_or_default());
        let max_z = f32::from_le_bytes(slice[24..28].try_into().unwrap_or_default());
        let c_x = f32::from_le_bytes(slice[28..32].try_into().unwrap_or_default());
        let c_y = f32::from_le_bytes(slice[32..36].try_into().unwrap_or_default());
        let c_z = f32::from_le_bytes(slice[36..40].try_into().unwrap_or_default());
        let radius = f32::from_le_bytes(slice[40..44].try_into().unwrap_or_default());
        let lod_level = slice[44];
        let lod_error = f32::from_le_bytes(slice[45..49].try_into().unwrap_or_default());
        let page_id = PageId(u64::from_le_bytes(
            slice[49..57].try_into().unwrap_or_default(),
        ));

        Ok(Self::new(
            cluster_id,
            Aabb3D::new([min_x, min_y, min_z], [max_x, max_y, max_z]),
            BoundingSphere::new([c_x, c_y, c_z], radius),
            lod_level,
            lod_error,
            page_id,
        ))
    }
}

/// Encodes a 12-byte composite key `(morton_curve_64, cluster_id_32)` ensuring
/// unique indexing and preventing collision overwrites between clusters sharing the same cell.
#[must_use]
pub fn encode_composite_cluster_key(
    domain: &Aabb3D,
    lod: u8,
    center: [f32; 3],
    cluster_id: u32,
) -> [u8; 12] {
    let (x, y, z) = quantize_position(center, domain);
    let morton = encode_spatial_key(lod, x, y, z).to_be_bytes();
    let mut key = [0u8; 12];
    key[0..8].copy_from_slice(&morton);
    key[8..12].copy_from_slice(&cluster_id.to_be_bytes());
    key
}
