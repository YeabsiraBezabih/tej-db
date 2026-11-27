/// Portable + SIMD-accelerated L2 squared distance between two f32 slices.
#[must_use]
pub fn l2_distance_squared(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vector lengths must match");
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: Runtime feature check confirmed AVX2 + FMA support.
            return unsafe { l2_distance_squared_avx2(a, b) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is mandatory on AArch64 targets.
        return unsafe { l2_distance_squared_neon(a, b) };
    }
    l2_distance_squared_scalar(a, b)
}

#[inline]
fn l2_distance_squared_scalar(a: &[f32], b: &[f32]) -> f32 {
    let mut acc0 = 0.0f32;
    let mut acc1 = 0.0f32;
    let mut acc2 = 0.0f32;
    let mut acc3 = 0.0f32;
    let chunks_a = a.chunks_exact(4);
    let chunks_b = b.chunks_exact(4);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();
    for (ca, cb) in chunks_a.zip(chunks_b) {
        let d0 = ca[0] - cb[0];
        let d1 = ca[1] - cb[1];
        let d2 = ca[2] - cb[2];
        let d3 = ca[3] - cb[3];
        acc0 += d0 * d0;
        acc1 += d1 * d1;
        acc2 += d2 * d2;
        acc3 += d3 * d3;
    }
    let mut sum = (acc0 + acc1) + (acc2 + acc3);
    for (x, y) in rem_a.iter().zip(rem_b.iter()) {
        let d = *x - *y;
        sum += d * d;
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn l2_distance_squared_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::{
        _mm_add_ps, _mm_add_ss, _mm_cvtss_f32, _mm_movehl_ps, _mm_shuffle_ps, _mm256_add_ps,
        _mm256_castps256_ps128, _mm256_extractf128_ps, _mm256_fmadd_ps, _mm256_loadu_ps,
        _mm256_setzero_ps, _mm256_sub_ps,
    };
    let len = a.len();
    let pa = a.as_ptr();
    let pb = b.as_ptr();
    let mut acc0 = _mm256_setzero_ps();
    let mut acc1 = _mm256_setzero_ps();
    let mut acc2 = _mm256_setzero_ps();
    let mut acc3 = _mm256_setzero_ps();
    let mut i = 0usize;
    while i + 32 <= len {
        unsafe {
            let d0 = _mm256_sub_ps(_mm256_loadu_ps(pa.add(i)), _mm256_loadu_ps(pb.add(i)));
            let d1 = _mm256_sub_ps(
                _mm256_loadu_ps(pa.add(i + 8)),
                _mm256_loadu_ps(pb.add(i + 8)),
            );
            let d2 = _mm256_sub_ps(
                _mm256_loadu_ps(pa.add(i + 16)),
                _mm256_loadu_ps(pb.add(i + 16)),
            );
            let d3 = _mm256_sub_ps(
                _mm256_loadu_ps(pa.add(i + 24)),
                _mm256_loadu_ps(pb.add(i + 24)),
            );
            acc0 = _mm256_fmadd_ps(d0, d0, acc0);
            acc1 = _mm256_fmadd_ps(d1, d1, acc1);
            acc2 = _mm256_fmadd_ps(d2, d2, acc2);
            acc3 = _mm256_fmadd_ps(d3, d3, acc3);
        }
        i += 32;
    }
    while i + 8 <= len {
        unsafe {
            let d = _mm256_sub_ps(_mm256_loadu_ps(pa.add(i)), _mm256_loadu_ps(pb.add(i)));
            acc0 = _mm256_fmadd_ps(d, d, acc0);
        }
        i += 8;
    }
    let sum256 = _mm256_add_ps(_mm256_add_ps(acc0, acc1), _mm256_add_ps(acc2, acc3));
    let hi = _mm256_extractf128_ps(sum256, 1);
    let lo = _mm256_castps256_ps128(sum256);
    let s128 = _mm_add_ps(lo, hi);
    let shuf = _mm_movehl_ps(s128, s128);
    let s64 = _mm_add_ps(s128, shuf);
    let s32 = _mm_add_ss(s64, _mm_shuffle_ps(s64, s64, 1));
    let mut sum = _mm_cvtss_f32(s32);
    while i < len {
        let d = a[i] - b[i];
        sum += d * d;
        i += 1;
    }
    sum
}

#[cfg(target_arch = "aarch64")]
unsafe fn l2_distance_squared_neon(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::{vaddq_f32, vaddvq_f32, vdupq_n_f32, vfmaq_f32, vld1q_f32, vsubq_f32};
    let len = a.len();
    let pa = a.as_ptr();
    let pb = b.as_ptr();
    let mut acc0 = vdupq_n_f32(0.0);
    let mut acc1 = vdupq_n_f32(0.0);
    let mut acc2 = vdupq_n_f32(0.0);
    let mut acc3 = vdupq_n_f32(0.0);
    let mut i = 0usize;
    while i + 16 <= len {
        unsafe {
            let d0 = vsubq_f32(vld1q_f32(pa.add(i)), vld1q_f32(pb.add(i)));
            let d1 = vsubq_f32(vld1q_f32(pa.add(i + 4)), vld1q_f32(pb.add(i + 4)));
            let d2 = vsubq_f32(vld1q_f32(pa.add(i + 8)), vld1q_f32(pb.add(i + 8)));
            let d3 = vsubq_f32(vld1q_f32(pa.add(i + 12)), vld1q_f32(pb.add(i + 12)));
            acc0 = vfmaq_f32(acc0, d0, d0);
            acc1 = vfmaq_f32(acc1, d1, d1);
            acc2 = vfmaq_f32(acc2, d2, d2);
            acc3 = vfmaq_f32(acc3, d3, d3);
        }
        i += 16;
    }
    while i + 4 <= len {
        unsafe {
            let d = vsubq_f32(vld1q_f32(pa.add(i)), vld1q_f32(pb.add(i)));
            acc0 = vfmaq_f32(acc0, d, d);
        }
        i += 4;
    }
    let mut sum = vaddvq_f32(vaddq_f32(vaddq_f32(acc0, acc1), vaddq_f32(acc2, acc3)));
    while i < len {
        let d = a[i] - b[i];
        sum += d * d;
        i += 1;
    }
    sum
}

/// Portable + SIMD-accelerated dot product between two f32 slices.
#[must_use]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vector lengths must match");
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: Runtime feature check confirmed AVX2 + FMA support.
            return unsafe { dot_product_avx2(a, b) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is mandatory on AArch64 targets.
        return unsafe { dot_product_neon(a, b) };
    }
    dot_product_scalar(a, b)
}

#[inline]
fn dot_product_scalar(a: &[f32], b: &[f32]) -> f32 {
    let mut acc0 = 0.0f32;
    let mut acc1 = 0.0f32;
    let mut acc2 = 0.0f32;
    let mut acc3 = 0.0f32;
    let chunks_a = a.chunks_exact(4);
    let chunks_b = b.chunks_exact(4);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();
    for (ca, cb) in chunks_a.zip(chunks_b) {
        acc0 += ca[0] * cb[0];
        acc1 += ca[1] * cb[1];
        acc2 += ca[2] * cb[2];
        acc3 += ca[3] * cb[3];
    }
    let mut sum = (acc0 + acc1) + (acc2 + acc3);
    for (x, y) in rem_a.iter().zip(rem_b.iter()) {
        sum += *x * *y;
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn dot_product_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::{
        _mm_add_ps, _mm_add_ss, _mm_cvtss_f32, _mm_movehl_ps, _mm_shuffle_ps, _mm256_add_ps,
        _mm256_castps256_ps128, _mm256_extractf128_ps, _mm256_fmadd_ps, _mm256_loadu_ps,
        _mm256_setzero_ps,
    };
    let len = a.len();
    let pa = a.as_ptr();
    let pb = b.as_ptr();
    let mut acc0 = _mm256_setzero_ps();
    let mut acc1 = _mm256_setzero_ps();
    let mut acc2 = _mm256_setzero_ps();
    let mut acc3 = _mm256_setzero_ps();
    let mut i = 0usize;
    while i + 32 <= len {
        unsafe {
            acc0 = _mm256_fmadd_ps(_mm256_loadu_ps(pa.add(i)), _mm256_loadu_ps(pb.add(i)), acc0);
            acc1 = _mm256_fmadd_ps(
                _mm256_loadu_ps(pa.add(i + 8)),
                _mm256_loadu_ps(pb.add(i + 8)),
                acc1,
            );
            acc2 = _mm256_fmadd_ps(
                _mm256_loadu_ps(pa.add(i + 16)),
                _mm256_loadu_ps(pb.add(i + 16)),
                acc2,
            );
            acc3 = _mm256_fmadd_ps(
                _mm256_loadu_ps(pa.add(i + 24)),
                _mm256_loadu_ps(pb.add(i + 24)),
                acc3,
            );
        }
        i += 32;
    }
    while i + 8 <= len {
        unsafe {
            acc0 = _mm256_fmadd_ps(_mm256_loadu_ps(pa.add(i)), _mm256_loadu_ps(pb.add(i)), acc0);
        }
        i += 8;
    }
    let sum256 = _mm256_add_ps(_mm256_add_ps(acc0, acc1), _mm256_add_ps(acc2, acc3));
    let hi = _mm256_extractf128_ps(sum256, 1);
    let lo = _mm256_castps256_ps128(sum256);
    let s128 = _mm_add_ps(lo, hi);
    let shuf = _mm_movehl_ps(s128, s128);
    let s64 = _mm_add_ps(s128, shuf);
    let s32 = _mm_add_ss(s64, _mm_shuffle_ps(s64, s64, 1));
    let mut sum = _mm_cvtss_f32(s32);
    while i < len {
        sum += a[i] * b[i];
        i += 1;
    }
    sum
}

#[cfg(target_arch = "aarch64")]
unsafe fn dot_product_neon(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::{vaddq_f32, vaddvq_f32, vdupq_n_f32, vfmaq_f32, vld1q_f32};
    let len = a.len();
    let pa = a.as_ptr();
    let pb = b.as_ptr();
    let mut acc0 = vdupq_n_f32(0.0);
    let mut acc1 = vdupq_n_f32(0.0);
    let mut acc2 = vdupq_n_f32(0.0);
    let mut acc3 = vdupq_n_f32(0.0);
    let mut i = 0usize;
    while i + 16 <= len {
        unsafe {
            acc0 = vfmaq_f32(acc0, vld1q_f32(pa.add(i)), vld1q_f32(pb.add(i)));
            acc1 = vfmaq_f32(acc1, vld1q_f32(pa.add(i + 4)), vld1q_f32(pb.add(i + 4)));
            acc2 = vfmaq_f32(acc2, vld1q_f32(pa.add(i + 8)), vld1q_f32(pb.add(i + 8)));
            acc3 = vfmaq_f32(acc3, vld1q_f32(pa.add(i + 12)), vld1q_f32(pb.add(i + 12)));
        }
        i += 16;
    }
    while i + 4 <= len {
        unsafe {
            acc0 = vfmaq_f32(acc0, vld1q_f32(pa.add(i)), vld1q_f32(pb.add(i)));
        }
        i += 4;
    }
    let mut sum = vaddvq_f32(vaddq_f32(vaddq_f32(acc0, acc1), vaddq_f32(acc2, acc3)));
    while i < len {
        sum += a[i] * b[i];
        i += 1;
    }
    sum
}

/// Cosine distance: 1.0 - (dot_product / (norm_a * norm_b)).
#[must_use]
#[allow(clippy::similar_names)]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let dot = dot_product(a, b);
    let norm_a_sq = dot_product(a, a);
    let norm_b_sq = dot_product(b, b);
    if norm_a_sq == 0.0 || norm_b_sq == 0.0 {
        return 1.0;
    }
    let sim = dot / (norm_a_sq.sqrt() * norm_b_sq.sqrt());
    (1.0 - sim).clamp(0.0, 2.0)
}

/// Hardware-accelerated popcount Hamming distance between binary-quantized vectors.
#[must_use]
pub fn rabitq_hamming_distance(a: &[u8], b: &[u8]) -> u32 {
    assert_eq!(a.len(), b.len(), "Binary code lengths must match");
    let mut dist = 0u32;
    let chunks_a = a.chunks_exact(8);
    let chunks_b = b.chunks_exact(8);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();

    for (ca, cb) in chunks_a.zip(chunks_b) {
        let u_a = u64::from_le_bytes(ca.try_into().unwrap_or([0; 8]));
        let u_b = u64::from_le_bytes(cb.try_into().unwrap_or([0; 8]));
        dist += (u_a ^ u_b).count_ones();
    }

    for (x, y) in rem_a.iter().zip(rem_b.iter()) {
        dist += (*x ^ *y).count_ones();
    }

    dist
}

/// Computes the asymmetric RaBitQ inner product estimate between a continuous rotated
/// query vector and a 1-bit quantized database vector (branchless 8-lane unrolled).
#[must_use]
pub fn rabitq_asymmetric_dot_product(
    rotated_query: &[f32],
    code: &[u8],
    alpha: f32,
    beta: f32,
) -> f32 {
    let dim = rotated_query.len();
    let q_sum: f32 = rotated_query.iter().copied().sum();
    let mut acc0 = 0.0f32;
    let mut acc1 = 0.0f32;
    let mut acc2 = 0.0f32;
    let mut acc3 = 0.0f32;

    let full_bytes = (dim / 8).min(code.len());
    for (byte_idx, &byte_val) in code[..full_bytes].iter().enumerate() {
        let base = byte_idx * 8;
        let q = &rotated_query[base..base + 8];
        acc0 += q[0] * f32::from(byte_val & 1) + q[4] * f32::from((byte_val >> 4) & 1);
        acc1 += q[1] * f32::from((byte_val >> 1) & 1) + q[5] * f32::from((byte_val >> 5) & 1);
        acc2 += q[2] * f32::from((byte_val >> 2) & 1) + q[6] * f32::from((byte_val >> 6) & 1);
        acc3 += q[3] * f32::from((byte_val >> 3) & 1) + q[7] * f32::from((byte_val >> 7) & 1);
    }

    let mut bit_sum = (acc0 + acc1) + (acc2 + acc3);
    if full_bytes < code.len() && full_bytes * 8 < dim {
        let byte_val = code[full_bytes];
        let base = full_bytes * 8;
        let limit = (dim - base).min(8);
        for bit in 0..limit {
            bit_sum += rotated_query[base + bit] * f32::from((byte_val >> bit) & 1);
        }
    }

    alpha * (2.0 * bit_sum - q_sum) + beta * q_sum
}

#[must_use]
pub fn rabitq_asymmetric_dot_product_2bit(
    rotated_query: &[f32],
    code: &[u8],
    alpha: f32,
    beta: f32,
) -> f32 {
    let dim = rotated_query.len();
    let q_sum: f32 = rotated_query.iter().copied().sum();
    let mut acc0 = 0.0f32;
    let mut acc1 = 0.0f32;
    let mut acc2 = 0.0f32;
    let mut acc3 = 0.0f32;

    let full_bytes = (dim / 4).min(code.len());
    for (byte_idx, &byte_val) in code[..full_bytes].iter().enumerate() {
        let base = byte_idx * 4;
        let q = &rotated_query[base..base + 4];
        acc0 += q[0] * f32::from(byte_val & 3);
        acc1 += q[1] * f32::from((byte_val >> 2) & 3);
        acc2 += q[2] * f32::from((byte_val >> 4) & 3);
        acc3 += q[3] * f32::from((byte_val >> 6) & 3);
    }

    let mut bit_sum = (acc0 + acc1) + (acc2 + acc3);
    if full_bytes < code.len() && full_bytes * 4 < dim {
        let byte_val = code[full_bytes];
        let base = full_bytes * 4;
        let limit = (dim - base).min(4);
        for slot in 0..limit {
            bit_sum += rotated_query[base + slot] * f32::from((byte_val >> (slot * 2)) & 3);
        }
    }

    alpha * (2.0 * bit_sum - 3.0 * q_sum) + beta * q_sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l2_and_dot_and_cosine() {
        let v1 = vec![1.0, 0.0, 0.0];
        let v2 = vec![0.0, 1.0, 0.0];
        let v3 = vec![1.0, 0.0, 0.0];

        assert!((l2_distance_squared(&v1, &v2) - 2.0).abs() < 1e-6);
        assert!((l2_distance_squared(&v1, &v3) - 0.0).abs() < 1e-6);

        assert!((dot_product(&v1, &v2) - 0.0).abs() < 1e-6);
        assert!((dot_product(&v1, &v3) - 1.0).abs() < 1e-6);

        assert!((cosine_distance(&v1, &v2) - 1.0).abs() < 1e-6);
        assert!((cosine_distance(&v1, &v3) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_hamming_distance() {
        let a = [0b10101010u8, 0b11110000u8];
        let b = [0b10101010u8, 0b00001111u8];
        assert_eq!(rabitq_hamming_distance(&a, &b), 8);
    }
}
