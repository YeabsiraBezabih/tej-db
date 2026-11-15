use crate::error::{DbError, Result};

/// Magic bytes for a Gaussian Splat Page payload ('GSPL').
pub const GAUSSIAN_SPLAT_MAGIC: u32 = 0x4753_504C;

/// 64-byte aligned Gaussian Splat header stored directly inside a 4 KiB TejDb Page.
/// Raw packed Gaussian splat parameters (position, covariance / rotation quaternion,
/// scale, opacity, and spherical harmonics) immediately follow at byte offset 64.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C, align(64))]
pub struct GaussianSplatPageHeader {
    pub magic: u32,
    pub lod_level: u8,
    pub flags: u8,
    pub splat_count: u16,
    pub material_id: u32,
    pub bounding_sphere: [f32; 4], // [center_x, center_y, center_z, radius]
    pub min_bounds: [f32; 3],
    pub max_bounds: [f32; 3],
    pub lod_error: f32,
    pub reserved: [u8; 6],
}

pub const GAUSSIAN_SPLAT_HEADER_SIZE: usize = 64;

impl GaussianSplatPageHeader {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        lod_level: u8,
        flags: u8,
        splat_count: u16,
        material_id: u32,
        bounding_sphere: [f32; 4],
        min_bounds: [f32; 3],
        max_bounds: [f32; 3],
        lod_error: f32,
    ) -> Self {
        Self {
            magic: GAUSSIAN_SPLAT_MAGIC,
            lod_level,
            flags,
            splat_count,
            material_id,
            bounding_sphere,
            min_bounds,
            max_bounds,
            lod_error,
            reserved: [0; 6],
        }
    }

    /// Serializes the header into an exact 64-byte array.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; GAUSSIAN_SPLAT_HEADER_SIZE] {
        let mut out = [0u8; GAUSSIAN_SPLAT_HEADER_SIZE];
        out[0..4].copy_from_slice(&self.magic.to_le_bytes());
        out[4] = self.lod_level;
        out[5] = self.flags;
        out[6..8].copy_from_slice(&self.splat_count.to_le_bytes());
        out[8..12].copy_from_slice(&self.material_id.to_le_bytes());

        for (i, &f) in self.bounding_sphere.iter().enumerate() {
            let offset = 12 + i * 4;
            out[offset..offset + 4].copy_from_slice(&f.to_le_bytes());
        }

        for (i, &f) in self.min_bounds.iter().enumerate() {
            let offset = 28 + i * 4;
            out[offset..offset + 4].copy_from_slice(&f.to_le_bytes());
        }

        for (i, &f) in self.max_bounds.iter().enumerate() {
            let offset = 40 + i * 4;
            out[offset..offset + 4].copy_from_slice(&f.to_le_bytes());
        }

        out[52..56].copy_from_slice(&self.lod_error.to_le_bytes());
        out[56..62].copy_from_slice(&self.reserved);
        out
    }

    /// Deserializes a `GaussianSplatPageHeader` from either a raw 64-byte slice or a full 4 KiB TejDb Page.
    pub fn from_page_bytes(bytes: &[u8]) -> Result<Self> {
        let offset = if bytes.len() >= 4 && bytes[0..4] == crate::page::PAGE_MAGIC {
            64
        } else {
            0
        };
        if bytes.len() < offset + GAUSSIAN_SPLAT_HEADER_SIZE {
            return Err(DbError::Custom(
                "Slice too small for GaussianSplatHeader".into(),
            ));
        }
        Self::from_bytes(&bytes[offset..offset + GAUSSIAN_SPLAT_HEADER_SIZE])
    }

    /// Deserializes a 64-byte slice into a `GaussianSplatPageHeader`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < GAUSSIAN_SPLAT_HEADER_SIZE {
            return Err(DbError::Custom(
                "Slice too small for GaussianSplatHeader".into(),
            ));
        }

        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap_or([0; 4]));
        if magic != GAUSSIAN_SPLAT_MAGIC {
            return Err(DbError::Custom(format!(
                "Invalid gaussian splat magic 0x{magic:08X}"
            )));
        }

        let lod_level = bytes[4];
        let flags = bytes[5];
        let splat_count = u16::from_le_bytes(bytes[6..8].try_into().unwrap_or([0; 2]));
        let material_id = u32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4]));

        let mut bounding_sphere = [0.0f32; 4];
        for (i, item) in bounding_sphere.iter_mut().enumerate() {
            let offset = 12 + i * 4;
            *item = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
        }

        let mut min_bounds = [0.0f32; 3];
        for (i, item) in min_bounds.iter_mut().enumerate() {
            let offset = 28 + i * 4;
            *item = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
        }

        let mut max_bounds = [0.0f32; 3];
        for (i, item) in max_bounds.iter_mut().enumerate() {
            let offset = 40 + i * 4;
            *item = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
        }

        let lod_error = f32::from_le_bytes(bytes[52..56].try_into().unwrap_or([0; 4]));
        let reserved = bytes[56..62].try_into().unwrap_or([0; 6]);

        Ok(Self {
            magic,
            lod_level,
            flags,
            splat_count,
            material_id,
            bounding_sphere,
            min_bounds,
            max_bounds,
            lod_error,
            reserved,
        })
    }

    /// Returns the splat buffer payload slice offset.
    #[must_use]
    pub fn payload_slice(page_bytes: &[u8]) -> &[u8] {
        let offset = if page_bytes.len() >= 4 && page_bytes[0..4] == crate::page::PAGE_MAGIC {
            128
        } else {
            64
        };
        if page_bytes.len() > offset {
            &page_bytes[offset..]
        } else {
            &[]
        }
    }
}
