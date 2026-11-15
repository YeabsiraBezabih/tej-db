use roaring::RoaringBitmap;
use tej_db_columnar::DataChunk;

/// Represents the 6 planes of a camera view frustum:
/// Left (0), Right (1), Bottom (2), Top (3), Near (4), Far (5).
/// Plane equation: `A * x + B * y + C * z + D = 0` (normalized where `A^2 + B^2 + C^2 = 1`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrustumPlanes {
    pub planes: [[f32; 4]; 6],
}

impl FrustumPlanes {
    #[must_use]
    pub const fn new(planes: [[f32; 4]; 6]) -> Self {
        Self { planes }
    }

    /// Extracts normalized frustum planes from a $4 \times 4$ column-major View-Projection matrix.
    #[must_use]
    pub fn from_matrix(m: &[f32; 16]) -> Self {
        let mut planes = [
            // Left: row 4 + row 1
            [m[3] + m[0], m[7] + m[4], m[11] + m[8], m[15] + m[12]],
            // Right: row 4 - row 1
            [m[3] - m[0], m[7] - m[4], m[11] - m[8], m[15] - m[12]],
            // Bottom: row 4 + row 2
            [m[3] + m[1], m[7] + m[5], m[11] + m[9], m[15] + m[13]],
            // Top: row 4 - row 2
            [m[3] - m[1], m[7] - m[5], m[11] - m[9], m[15] - m[13]],
            // Near: row 4 + row 3
            [m[3] + m[2], m[7] + m[6], m[11] + m[10], m[15] + m[14]],
            // Far: row 4 - row 3
            [m[3] - m[2], m[7] - m[6], m[11] - m[10], m[15] - m[14]],
        ];

        // Normalize plane equations
        for p in &mut planes {
            let len = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            if len > 1e-6 {
                let inv = 1.0 / len;
                p[0] *= inv;
                p[1] *= inv;
                p[2] *= inv;
                p[3] *= inv;
            }
        }

        Self { planes }
    }

    /// Tests if a bounding sphere is inside or intersects the frustum.
    #[inline]
    #[must_use]
    pub fn contains_sphere(&self, center: [f32; 3], radius: f32) -> bool {
        for p in &self.planes {
            let dist = p[0] * center[0] + p[1] * center[1] + p[2] * center[2] + p[3];
            if dist < -radius {
                return false;
            }
        }
        true
    }

    /// Tests if an AABB is inside or intersects the frustum using p-vertex slab checks.
    #[inline]
    #[must_use]
    pub fn intersects_aabb(&self, min: [f32; 3], max: [f32; 3]) -> bool {
        for p in &self.planes {
            let px = if p[0] > 0.0 { max[0] } else { min[0] };
            let py = if p[1] > 0.0 { max[1] } else { min[1] };
            let pz = if p[2] > 0.0 { max[2] } else { min[2] };

            if p[0] * px + p[1] * py + p[2] * pz + p[3] < 0.0 {
                return false;
            }
        }
        true
    }
}

/// 3D Ray with precomputed inverse direction for branch-free slab intersection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray3D {
    pub origin: [f32; 3],
    pub dir: [f32; 3],
    pub inv_dir: [f32; 3],
}

impl Ray3D {
    #[must_use]
    pub fn new(origin: [f32; 3], mut dir: [f32; 3]) -> Self {
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        if len > 1e-6 {
            let inv = 1.0 / len;
            dir[0] *= inv;
            dir[1] *= inv;
            dir[2] *= inv;
        }

        let inv_dir = [
            if dir[0].abs() < 1e-9 {
                1e9
            } else {
                1.0 / dir[0]
            },
            if dir[1].abs() < 1e-9 {
                1e9
            } else {
                1.0 / dir[1]
            },
            if dir[2].abs() < 1e-9 {
                1e9
            } else {
                1.0 / dir[2]
            },
        ];

        Self {
            origin,
            dir,
            inv_dir,
        }
    }

    /// Fast branch-free slab method ray-AABB intersection test.
    /// Returns the entry distance $t \ge 0$ if the ray intersects the AABB.
    #[must_use]
    pub fn intersect_aabb(&self, min: [f32; 3], max: [f32; 3]) -> Option<f32> {
        let t1 = (min[0] - self.origin[0]) * self.inv_dir[0];
        let t2 = (max[0] - self.origin[0]) * self.inv_dir[0];
        let tmin_x = t1.min(t2);
        let tmax_x = t1.max(t2);

        let t3 = (min[1] - self.origin[1]) * self.inv_dir[1];
        let t4 = (max[1] - self.origin[1]) * self.inv_dir[1];
        let tmin_y = t3.min(t4);
        let tmax_y = t3.max(t4);

        let t5 = (min[2] - self.origin[2]) * self.inv_dir[2];
        let t6 = (max[2] - self.origin[2]) * self.inv_dir[2];
        let tmin_z = t5.min(t6);
        let tmax_z = t5.max(t6);

        let t_enter = tmin_x.max(tmin_y).max(tmin_z);
        let t_exit = tmax_x.min(tmax_y).min(tmax_z);

        if t_exit >= t_enter && t_exit >= 0.0 {
            Some(if t_enter < 0.0 { 0.0 } else { t_enter })
        } else {
            None
        }
    }
}

/// Evaluates frustum culling and screen-space LOD error across a DataChunk of clusters:
/// Columns expected:
/// - 0: `cluster_id` (Int64)
/// - 1: `center_x` (Float64)
/// - 2: `center_y` (Float64)
/// - 3: `center_z` (Float64)
/// - 4: `radius` (Float64)
/// - 5: `lod_error` (Float64)
pub fn cull_clusters_morsel(
    chunk: &DataChunk,
    frustum: &FrustumPlanes,
    cam_pos: [f32; 3],
    lod_threshold: f32,
    out: &mut RoaringBitmap,
) {
    let num_rows = chunk.num_rows();
    if num_rows == 0 || chunk.num_columns() < 6 {
        return;
    }

    let ids = chunk.column(0).map_or(&[] as &[i64], |c| c.as_i64_slice());
    let cx = chunk.column(1).map_or(&[] as &[f64], |c| c.as_f64_slice());
    let cy = chunk.column(2).map_or(&[] as &[f64], |c| c.as_f64_slice());
    let cz = chunk.column(3).map_or(&[] as &[f64], |c| c.as_f64_slice());
    let r = chunk.column(4).map_or(&[] as &[f64], |c| c.as_f64_slice());
    let lod_err = chunk.column(5).map_or(&[] as &[f64], |c| c.as_f64_slice());

    let len = ids
        .len()
        .min(cx.len())
        .min(cy.len())
        .min(cz.len())
        .min(r.len())
        .min(lod_err.len());

    for i in 0..len {
        #[allow(clippy::cast_possible_truncation)]
        let center = [cx[i] as f32, cy[i] as f32, cz[i] as f32];
        #[allow(clippy::cast_possible_truncation)]
        let radius = r[i] as f32;

        // 1. Frustum sphere test
        if !frustum.contains_sphere(center, radius) {
            continue;
        }

        // 2. Projected LOD error test
        let dx = center[0] - cam_pos[0];
        let dy = center[1] - cam_pos[1];
        let dz = center[2] - cam_pos[2];
        let dist = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-4);

        #[allow(clippy::cast_possible_truncation)]
        let projected_error = (lod_err[i] as f32) / dist;
        if projected_error > lod_threshold {
            continue; // LOD error exceeds threshold -> higher detail required
        }

        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        out.insert(ids[i] as u32);
    }
}

/// Vectorized Raycast: finds the closest cluster hit by a ray among candidate bounding boxes.
/// Columns expected:
/// - 0: `cluster_id` (Int64)
/// - 1: `min_x` (Float64)
/// - 2: `min_y` (Float64)
/// - 3: `min_z` (Float64)
/// - 4: `max_x` (Float64)
/// - 5: `max_y` (Float64)
/// - 6: `max_z` (Float64)
#[must_use]
pub fn raycast_closest_cluster(
    chunk: &DataChunk,
    ray: &Ray3D,
    visible_filter: Option<&RoaringBitmap>,
) -> Option<(u64, f32)> {
    if chunk.num_columns() < 7 {
        return None;
    }

    let ids = chunk.column(0).map_or(&[] as &[i64], |c| c.as_i64_slice());
    let min_x = chunk.column(1).map_or(&[] as &[f64], |c| c.as_f64_slice());
    let min_y = chunk.column(2).map_or(&[] as &[f64], |c| c.as_f64_slice());
    let min_z = chunk.column(3).map_or(&[] as &[f64], |c| c.as_f64_slice());
    let max_x = chunk.column(4).map_or(&[] as &[f64], |c| c.as_f64_slice());
    let max_y = chunk.column(5).map_or(&[] as &[f64], |c| c.as_f64_slice());
    let max_z = chunk.column(6).map_or(&[] as &[f64], |c| c.as_f64_slice());

    let len = ids
        .len()
        .min(min_x.len())
        .min(min_y.len())
        .min(min_z.len())
        .min(max_x.len())
        .min(max_y.len())
        .min(max_z.len());

    let mut closest_hit: Option<(u64, f32)> = None;

    for i in 0..len {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let id_u32 = ids[i] as u32;

        if let Some(mask) = visible_filter {
            if !mask.contains(id_u32) {
                continue;
            }
        }

        #[allow(clippy::cast_possible_truncation)]
        let aabb_min = [min_x[i] as f32, min_y[i] as f32, min_z[i] as f32];
        #[allow(clippy::cast_possible_truncation)]
        let aabb_max = [max_x[i] as f32, max_y[i] as f32, max_z[i] as f32];

        if let Some(dist) = ray.intersect_aabb(aabb_min, aabb_max) {
            match closest_hit {
                None => closest_hit = Some((u64::from(id_u32), dist)),
                Some((_, cur_dist)) if dist < cur_dist => {
                    closest_hit = Some((u64::from(id_u32), dist));
                }
                _ => {}
            }
        }
    }

    closest_hit
}
