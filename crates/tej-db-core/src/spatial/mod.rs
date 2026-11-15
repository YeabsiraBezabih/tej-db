pub mod bounds;
pub mod gaussian_splat;
pub mod meshlet;
pub mod morton;

pub use bounds::{Aabb3D, BoundingSphere};
pub use gaussian_splat::{
    GAUSSIAN_SPLAT_HEADER_SIZE, GAUSSIAN_SPLAT_MAGIC, GaussianSplatPageHeader,
};
pub use meshlet::{MESHLET_HEADER_SIZE, MESHLET_MAGIC, MeshletPageHeader};
pub use morton::{
    COORD_20BIT_MASK, decode_spatial_key, dequantize_position, dilate_3d, encode_spatial_key,
    quantize_position, undilate_3d,
};

#[cfg(test)]
mod tests;
