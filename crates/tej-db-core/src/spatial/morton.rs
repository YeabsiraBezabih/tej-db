use super::bounds::Aabb3D;

/// 20-bit coordinate mask ($2^{20} - 1 = 1,048,575$).
pub const COORD_20BIT_MASK: u32 = 0x000F_FFFF;

/// Dilates a 20-bit integer by spreading each bit to every 3rd bit position (bits 0, 3, 6, ... 57).
#[inline]
pub fn dilate_3d(val: u32) -> u64 {
    let mut x = u64::from(val & COORD_20BIT_MASK);
    x = (x | (x << 32)) & 0x001F_0000_0000_FFFF;
    x = (x | (x << 16)) & 0x001F_0000_FF00_00FF;
    x = (x | (x << 8)) & 0x100F_00F0_0F00_F00F;
    x = (x | (x << 4)) & 0x10C3_0C30_C30C_30C3;
    x = (x | (x << 2)) & 0x1249_2492_4924_9249;
    x
}

/// Compacts every 3rd bit from a 60-bit integer back into a 20-bit integer.
#[inline]
pub fn undilate_3d(mut x: u64) -> u32 {
    x &= 0x1249_2492_4924_9249;
    x = (x | (x >> 2)) & 0x10C3_0C30_C30C_30C3;
    x = (x | (x >> 4)) & 0x100F_00F0_0F00_F00F;
    x = (x | (x >> 8)) & 0x001F_0000_FF00_00FF;
    x = (x | (x >> 16)) & 0x001F_0000_0000_FFFF;
    x = (x | (x >> 32)) & u64::from(COORD_20BIT_MASK);
    #[allow(clippy::cast_possible_truncation)]
    {
        x as u32
    }
}

/// Encodes 3D integer coordinates (20-bit each) and LOD into a 64-bit spatial key:
/// `[LOD (4 bits) | Z-Order 3D Morton Code (60 bits)]`.
#[inline]
#[must_use]
pub fn encode_spatial_key(lod: u8, x: u32, y: u32, z: u32) -> u64 {
    let morton = dilate_3d(x) | (dilate_3d(y) << 1) | (dilate_3d(z) << 2);
    (u64::from(lod & 0x0F) << 60) | morton
}

/// Decodes a 64-bit spatial key into LOD and 3D integer coordinates `(lod, x, y, z)`.
#[inline]
#[must_use]
pub fn decode_spatial_key(key: u64) -> (u8, u32, u32, u32) {
    #[allow(clippy::cast_possible_truncation)]
    let lod = ((key >> 60) & 0x0F) as u8;
    let morton = key & 0x0FFF_FFFF_FFFF_FFFF;
    let x = undilate_3d(morton);
    let y = undilate_3d(morton >> 1);
    let z = undilate_3d(morton >> 2);
    (lod, x, y, z)
}

/// Quantizes continuous 3D world coordinates $(x, y, z)$ within a reference AABB domain into 20-bit unsigned coordinates.
#[must_use]
pub fn quantize_position(pos: [f32; 3], domain: &Aabb3D) -> (u32, u32, u32) {
    let clamp_unit = |v: f32, min: f32, max: f32| -> f32 {
        let span = max - min;
        if span <= 1e-6 {
            0.0
        } else {
            ((v - min) / span).clamp(0.0, 1.0)
        }
    };

    let nx = clamp_unit(pos[0], domain.min[0], domain.max[0]);
    let ny = clamp_unit(pos[1], domain.min[1], domain.max[1]);
    let nz = clamp_unit(pos[2], domain.min[2], domain.max[2]);

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    (
        (nx * (COORD_20BIT_MASK as f32)).round() as u32,
        (ny * (COORD_20BIT_MASK as f32)).round() as u32,
        (nz * (COORD_20BIT_MASK as f32)).round() as u32,
    )
}

/// Dequantizes 20-bit integer coordinates back to continuous 3D world coordinates within the given domain.
#[must_use]
pub fn dequantize_position(qx: u32, qy: u32, qz: u32, domain: &Aabb3D) -> [f32; 3] {
    #[allow(clippy::cast_precision_loss)]
    let max_v = COORD_20BIT_MASK as f32;
    #[allow(clippy::cast_precision_loss)]
    let fx = (qx as f32) / max_v;
    #[allow(clippy::cast_precision_loss)]
    let fy = (qy as f32) / max_v;
    #[allow(clippy::cast_precision_loss)]
    let fz = (qz as f32) / max_v;

    [
        domain.min[0] + fx * (domain.max[0] - domain.min[0]),
        domain.min[1] + fy * (domain.max[1] - domain.min[1]),
        domain.min[2] + fz * (domain.max[2] - domain.min[2]),
    ]
}
