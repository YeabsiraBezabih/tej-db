use tej_db_vectorized::spatial_culling::FrustumPlanes;

/// Structure-of-Arrays (SoA) layout for high-throughput vectorized SIMD frustum
/// and screen-space LOD culling across clusters without heap allocations.
#[derive(Debug, Default, Clone)]
pub struct SpatialSoACache {
    pub x: Vec<f32>,
    pub y: Vec<f32>,
    pub z: Vec<f32>,
    pub r: Vec<f32>,
    pub lod_sq: Vec<f32>,
    pub cluster_ids: Vec<u32>,
}

impl SpatialSoACache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, center: [f32; 3], radius: f32, lod_error: f32, cluster_id: u32) {
        self.x.push(center[0]);
        self.y.push(center[1]);
        self.z.push(center[2]);
        self.r.push(radius);
        self.lod_sq.push(lod_error * lod_error);
        self.cluster_ids.push(cluster_id);
    }

    pub fn update_position(&mut self, cluster_id: u32, center: [f32; 3]) {
        if let Some(pos) = self.cluster_ids.iter().position(|&id| id == cluster_id) {
            self.x[pos] = center[0];
            self.y[pos] = center[1];
            self.z[pos] = center[2];
        }
    }

    pub fn evaluate_cull(
        &self,
        frustum: &FrustumPlanes,
        camera_pos: [f32; 3],
        max_screen_error: f32,
        visible_ids: &mut Vec<u32>,
        visible_indices: &mut Vec<usize>,
    ) {
        visible_ids.clear();
        visible_indices.clear();
        let len = self.cluster_ids.len();
        let max_screen_error_sq = max_screen_error * max_screen_error;
        let planes = frustum.planes;

        for i in 0..len {
            let cx = self.x[i];
            let cy = self.y[i];
            let cz = self.z[i];
            let r = self.r[i];

            let mut inside = true;
            for p in &planes {
                let dist = p[0] * cx + p[1] * cy + p[2] * cz + p[3];
                if dist < -r {
                    inside = false;
                    break;
                }
            }

            if !inside {
                continue;
            }

            let dx = cx - camera_pos[0];
            let dy = cy - camera_pos[1];
            let dz = cz - camera_pos[2];
            let dist_sq = dx * dx + dy * dy + dz * dz;

            if self.lod_sq[i] <= max_screen_error_sq * dist_sq {
                visible_ids.push(self.cluster_ids[i]);
                visible_indices.push(i);
            }
        }
    }

    /// Evaluates SIMD frustum and LOD culling across all registered clusters in cache,
    /// returning the number of visible clusters with zero heap allocation.
    #[inline]
    #[must_use]
    pub fn cull_frustum_simd(
        &self,
        frustum: &FrustumPlanes,
        camera_pos: [f32; 3],
        max_screen_error: f32,
    ) -> usize {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                // SAFETY: CPUID check guarantees AVX2 and FMA instructions are supported by hardware.
                return unsafe { self.cull_frustum_avx2(frustum, camera_pos, max_screen_error) };
            }
        }
        self.cull_frustum_fallback(frustum, camera_pos, max_screen_error)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2", enable = "fma")]
    #[allow(clippy::wildcard_imports, clippy::cast_sign_loss)]
    // SAFETY: Host CPU must support AVX2 and FMA extensions.
    unsafe fn cull_frustum_avx2(
        &self,
        frustum: &FrustumPlanes,
        camera_pos: [f32; 3],
        max_screen_error: f32,
    ) -> usize {
        use std::arch::x86_64::*;

        let len = self.cluster_ids.len();
        if len == 0 {
            return 0;
        }

        let max_screen_error_sq = max_screen_error * max_screen_error;
        let [p0, p1, p2, p3, p4, p5] = frustum.planes;

        let vp0_x = _mm256_set1_ps(p0[0]);
        let vp0_y = _mm256_set1_ps(p0[1]);
        let vp0_z = _mm256_set1_ps(p0[2]);
        let vp0_w = _mm256_set1_ps(p0[3]);

        let vp1_x = _mm256_set1_ps(p1[0]);
        let vp1_y = _mm256_set1_ps(p1[1]);
        let vp1_z = _mm256_set1_ps(p1[2]);
        let vp1_w = _mm256_set1_ps(p1[3]);

        let vp2_x = _mm256_set1_ps(p2[0]);
        let vp2_y = _mm256_set1_ps(p2[1]);
        let vp2_z = _mm256_set1_ps(p2[2]);
        let vp2_w = _mm256_set1_ps(p2[3]);

        let vp3_x = _mm256_set1_ps(p3[0]);
        let vp3_y = _mm256_set1_ps(p3[1]);
        let vp3_z = _mm256_set1_ps(p3[2]);
        let vp3_w = _mm256_set1_ps(p3[3]);

        let vp4_x = _mm256_set1_ps(p4[0]);
        let vp4_y = _mm256_set1_ps(p4[1]);
        let vp4_z = _mm256_set1_ps(p4[2]);
        let vp4_w = _mm256_set1_ps(p4[3]);

        let vp5_x = _mm256_set1_ps(p5[0]);
        let vp5_y = _mm256_set1_ps(p5[1]);
        let vp5_z = _mm256_set1_ps(p5[2]);
        let vp5_w = _mm256_set1_ps(p5[3]);

        let vcam_x = _mm256_set1_ps(camera_pos[0]);
        let vcam_y = _mm256_set1_ps(camera_pos[1]);
        let vcam_z = _mm256_set1_ps(camera_pos[2]);
        let vmax_err_sq = _mm256_set1_ps(max_screen_error_sq);
        let zero = _mm256_setzero_ps();

        let mut count = 0;
        let chunks = len / 8;

        for chunk_idx in 0..chunks {
            let offset = chunk_idx * 8;
            // SAFETY: `offset + 8 <= chunks * 8 <= len`, so pointers remain strictly within buffer allocations.
            let (vx, vy, vz, vr, vlod_sq) = unsafe {
                (
                    _mm256_loadu_ps(self.x.as_ptr().add(offset)),
                    _mm256_loadu_ps(self.y.as_ptr().add(offset)),
                    _mm256_loadu_ps(self.z.as_ptr().add(offset)),
                    _mm256_loadu_ps(self.r.as_ptr().add(offset)),
                    _mm256_loadu_ps(self.lod_sq.as_ptr().add(offset)),
                )
            };

            let d0 = _mm256_add_ps(
                _mm256_fmadd_ps(
                    vp0_x,
                    vx,
                    _mm256_fmadd_ps(vp0_y, vy, _mm256_fmadd_ps(vp0_z, vz, vp0_w)),
                ),
                vr,
            );
            let mut mask = _mm256_cmp_ps::<_CMP_GE_OQ>(d0, zero);

            let d1 = _mm256_add_ps(
                _mm256_fmadd_ps(
                    vp1_x,
                    vx,
                    _mm256_fmadd_ps(vp1_y, vy, _mm256_fmadd_ps(vp1_z, vz, vp1_w)),
                ),
                vr,
            );
            mask = _mm256_and_ps(mask, _mm256_cmp_ps::<_CMP_GE_OQ>(d1, zero));

            let d2 = _mm256_add_ps(
                _mm256_fmadd_ps(
                    vp2_x,
                    vx,
                    _mm256_fmadd_ps(vp2_y, vy, _mm256_fmadd_ps(vp2_z, vz, vp2_w)),
                ),
                vr,
            );
            mask = _mm256_and_ps(mask, _mm256_cmp_ps::<_CMP_GE_OQ>(d2, zero));

            let d3 = _mm256_add_ps(
                _mm256_fmadd_ps(
                    vp3_x,
                    vx,
                    _mm256_fmadd_ps(vp3_y, vy, _mm256_fmadd_ps(vp3_z, vz, vp3_w)),
                ),
                vr,
            );
            mask = _mm256_and_ps(mask, _mm256_cmp_ps::<_CMP_GE_OQ>(d3, zero));

            let d4 = _mm256_add_ps(
                _mm256_fmadd_ps(
                    vp4_x,
                    vx,
                    _mm256_fmadd_ps(vp4_y, vy, _mm256_fmadd_ps(vp4_z, vz, vp4_w)),
                ),
                vr,
            );
            mask = _mm256_and_ps(mask, _mm256_cmp_ps::<_CMP_GE_OQ>(d4, zero));

            let d5 = _mm256_add_ps(
                _mm256_fmadd_ps(
                    vp5_x,
                    vx,
                    _mm256_fmadd_ps(vp5_y, vy, _mm256_fmadd_ps(vp5_z, vz, vp5_w)),
                ),
                vr,
            );
            mask = _mm256_and_ps(mask, _mm256_cmp_ps::<_CMP_GE_OQ>(d5, zero));

            let dx = _mm256_sub_ps(vx, vcam_x);
            let dy = _mm256_sub_ps(vy, vcam_y);
            let dz = _mm256_sub_ps(vz, vcam_z);
            let dist_sq = _mm256_fmadd_ps(dx, dx, _mm256_fmadd_ps(dy, dy, _mm256_mul_ps(dz, dz)));

            let max_allowed_err = _mm256_mul_ps(vmax_err_sq, dist_sq);
            let lod_mask = _mm256_cmp_ps::<_CMP_LE_OQ>(vlod_sq, max_allowed_err);

            mask = _mm256_and_ps(mask, lod_mask);
            let bits = _mm256_movemask_ps(mask) as u32;
            count += bits.count_ones() as usize;
        }

        for i in (chunks * 8)..len {
            let x = self.x[i];
            let y = self.y[i];
            let z = self.z[i];
            let r = self.r[i];

            let d0 = p0[0] * x + p0[1] * y + p0[2] * z + p0[3] + r;
            let d1 = p1[0] * x + p1[1] * y + p1[2] * z + p1[3] + r;
            let d2 = p2[0] * x + p2[1] * y + p2[2] * z + p2[3] + r;
            let d3 = p3[0] * x + p3[1] * y + p3[2] * z + p3[3] + r;
            let d4 = p4[0] * x + p4[1] * y + p4[2] * z + p4[3] + r;
            let d5 = p5[0] * x + p5[1] * y + p5[2] * z + p5[3] + r;

            let culled =
                (d0 < 0.0) || (d1 < 0.0) || (d2 < 0.0) || (d3 < 0.0) || (d4 < 0.0) || (d5 < 0.0);
            if !culled {
                let dx = x - camera_pos[0];
                let dy = y - camera_pos[1];
                let dz = z - camera_pos[2];
                let dist_sq = dx * dx + dy * dy + dz * dz;
                if self.lod_sq[i] <= max_screen_error_sq * dist_sq {
                    count += 1;
                }
            }
        }

        count
    }

    fn cull_frustum_fallback(
        &self,
        frustum: &FrustumPlanes,
        camera_pos: [f32; 3],
        max_screen_error: f32,
    ) -> usize {
        let len = self.cluster_ids.len();
        if len == 0 {
            return 0;
        }

        let max_screen_error_sq = max_screen_error * max_screen_error;
        let [p0, p1, p2, p3, p4, p5] = frustum.planes;

        let mut count = 0;
        for i in 0..len {
            let x = self.x[i];
            let y = self.y[i];
            let z = self.z[i];
            let r = self.r[i];

            let d0 = p0[0] * x + p0[1] * y + p0[2] * z + p0[3] + r;
            let d1 = p1[0] * x + p1[1] * y + p1[2] * z + p1[3] + r;
            let d2 = p2[0] * x + p2[1] * y + p2[2] * z + p2[3] + r;
            let d3 = p3[0] * x + p3[1] * y + p3[2] * z + p3[3] + r;
            let d4 = p4[0] * x + p4[1] * y + p4[2] * z + p4[3] + r;
            let d5 = p5[0] * x + p5[1] * y + p5[2] * z + p5[3] + r;

            let culled =
                (d0 < 0.0) || (d1 < 0.0) || (d2 < 0.0) || (d3 < 0.0) || (d4 < 0.0) || (d5 < 0.0);
            if !culled {
                let dx = x - camera_pos[0];
                let dy = y - camera_pos[1];
                let dz = z - camera_pos[2];
                let dist_sq = dx * dx + dy * dy + dz * dz;
                if self.lod_sq[i] <= max_screen_error_sq * dist_sq {
                    count += 1;
                }
            }
        }
        count
    }
}
