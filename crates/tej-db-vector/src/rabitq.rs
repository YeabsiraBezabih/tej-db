use crate::simd::rabitq_asymmetric_dot_product;

/// Quantization precision for RaBitQ encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuantizationPrecision {
    /// 1-bit binary quantization (1 bit / dimension, 32x compression).
    #[default]
    OneBit,
    /// 2-bit quantization (2 bits / dimension, 4 reconstruction levels, 16x compression).
    /// Provides 4x tighter distance estimation for lower dimensional vectors (D <= 256).
    TwoBit,
}

/// RaBitQ (SIGMOD 2024) 1-bit & 2-bit vector quantization engine.
///
/// Features:
/// - Randomized Fast Walsh-Hadamard Transform (FWHT): Replaces $O(D^2)$ dense matrix
///   multiplication with $O(D \log D)$ sign-flipped butterfly passes, spreading variance
///   evenly across all dimensions while preserving exact isometric $L_2$ norms.
/// - 1-bit / 2-bit binarization per dimension with least-squares optimal scaling $\alpha$.
/// - Scalar corrective factors ($\alpha, \beta, \|\mathbf{x}\|$) to achieve high recall.
#[derive(Debug, Clone)]
pub struct RaBitQQuantizer {
    pub dim: usize,
    pub effective_dim: usize,
    pub sign_flips: Vec<f32>,
    pub inv_sqrt_n: f32,
    pub precision: QuantizationPrecision,
}

impl RaBitQQuantizer {
    /// Creates a new `RaBitQQuantizer` for vectors of dimension `dim` using a deterministic seed.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn new(dim: usize, seed: u64) -> Self {
        Self::new_with_precision(dim, seed, QuantizationPrecision::OneBit)
    }

    /// Creates a new `RaBitQQuantizer` with explicit `QuantizationPrecision`.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn new_with_precision(dim: usize, seed: u64, precision: QuantizationPrecision) -> Self {
        let effective_dim = dim.next_power_of_two();
        let sign_flips = generate_sign_flips(effective_dim, seed);
        let inv_sqrt_n = 1.0f32 / (effective_dim as f32).sqrt();
        Self {
            dim,
            effective_dim,
            sign_flips,
            inv_sqrt_n,
            precision,
        }
    }

    /// Rotates vector $\mathbf{x}$ using the Randomized Fast Walsh-Hadamard Transform:
    /// $\mathbf{x}' = \frac{1}{\sqrt{N}} H (S \cdot \mathbf{x})$.
    #[must_use]
    pub fn rotate(&self, x: &[f32]) -> Vec<f32> {
        assert_eq!(x.len(), self.dim, "Vector dimension mismatch");
        let mut buf = vec![0.0f32; self.effective_dim];

        // 1. Diagonal random sign flips: S * x
        for (i, &val) in x.iter().enumerate() {
            buf[i] = val * self.sign_flips[i];
        }

        // 2. In-place Fast Walsh-Hadamard Transform butterfly passes: O(N log N)
        fwht_in_place(&mut buf);

        // 3. Orthonormal scaling: 1 / sqrt(N) to preserve Euclidean norm
        for slot in &mut buf {
            *slot *= self.inv_sqrt_n;
        }

        buf
    }

    /// Quantizes vector $\mathbf{x}$ using 1-bit RaBitQ encoding.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn quantize_1bit(&self, x: &[f32]) -> (Vec<u8>, f32, f32, f32) {
        let rotated = self.rotate(x);
        let byte_len = self.effective_dim.div_ceil(8);
        let mut code = vec![0u8; byte_len];

        let mut sum = 0.0f32;
        let mut norm_sq = 0.0f32;

        for &val in &rotated {
            sum += val;
            norm_sq += val * val;
        }

        let beta = sum / (self.effective_dim as f32);
        let mut abs_diff_sum = 0.0f32;

        for (i, &val) in rotated.iter().enumerate() {
            let diff = val - beta;
            if diff >= 0.0 {
                let byte_idx = i / 8;
                let bit_idx = i % 8;
                code[byte_idx] |= 1 << bit_idx;
            }
            abs_diff_sum += diff.abs();
        }

        let alpha = abs_diff_sum / (self.effective_dim as f32);
        let norm = norm_sq.sqrt();

        (code, alpha, beta, norm)
    }

    /// Quantizes vector $\mathbf{x}$ using 2-bit RaBitQ encoding with 4 reconstruction levels.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn quantize_2bit(&self, x: &[f32]) -> (Vec<u8>, f32, f32, f32) {
        let rotated = self.rotate(x);
        let byte_len = self.effective_dim.div_ceil(4);
        let mut code = vec![0u8; byte_len];

        let mut sum = 0.0f32;
        let mut norm_sq = 0.0f32;

        for &val in &rotated {
            sum += val;
            norm_sq += val * val;
        }

        let beta = sum / (self.effective_dim as f32);
        let mut abs_diff_sum = 0.0f32;

        for &val in &rotated {
            abs_diff_sum += (val - beta).abs();
        }

        let threshold = abs_diff_sum / (self.effective_dim as f32);
        let mut sum_num = 0.0f32;
        let mut sum_den = 0.0f32;

        for (i, &val) in rotated.iter().enumerate() {
            let diff = val - beta;
            let (b_i, q_i) = if diff < -threshold {
                (0u8, -3.0f32)
            } else if diff < 0.0 {
                (1u8, -1.0f32)
            } else if diff < threshold {
                (2u8, 1.0f32)
            } else {
                (3u8, 3.0f32)
            };

            let byte_idx = i / 4;
            let shift = (i % 4) * 2;
            code[byte_idx] |= b_i << shift;

            sum_num += diff * q_i;
            sum_den += q_i * q_i;
        }

        let alpha = sum_num / sum_den.max(1.0);
        let norm = norm_sq.sqrt();

        (code, alpha, beta, norm)
    }

    /// Quantizes vector $\mathbf{x}$ according to the configured precision (1-bit or 2-bit).
    #[must_use]
    pub fn quantize(&self, x: &[f32]) -> (Vec<u8>, f32, f32, f32) {
        match self.precision {
            QuantizationPrecision::OneBit => self.quantize_1bit(x),
            QuantizationPrecision::TwoBit => self.quantize_2bit(x),
        }
    }

    /// Estimates the inner product between an unquantized query $\mathbf{q}$ and a quantized vector $\mathbf{x}$.
    #[must_use]
    pub fn estimate_inner_product(
        &self,
        rotated_query: &[f32],
        code: &[u8],
        alpha: f32,
        beta: f32,
    ) -> f32 {
        rabitq_asymmetric_dot_product(rotated_query, code, alpha, beta)
    }

    /// Estimates the L2 squared distance: $\|\mathbf{q} - \mathbf{x}\|^2 = \|\mathbf{q}\|^2 + \|\mathbf{x}\|^2 - 2 \langle \mathbf{q}, \mathbf{x} \rangle$.
    #[must_use]
    pub fn estimate_l2_distance(
        &self,
        rotated_query: &[f32],
        query_norm_sq: f32,
        code: &[u8],
        alpha: f32,
        beta: f32,
        vector_norm: f32,
    ) -> f32 {
        let dot = self.estimate_inner_product(rotated_query, code, alpha, beta);
        let dist_sq = query_norm_sq + (vector_norm * vector_norm) - (2.0 * dot);
        dist_sq.max(0.0)
    }
}

/// In-place Fast Walsh-Hadamard Transform (FWHT) in $O(N \log N)$ butterfly passes.
pub fn fwht_in_place(buf: &mut [f32]) {
    let n = buf.len();
    assert!(n.is_power_of_two(), "FWHT buffer size must be power of 2");
    let mut step = 1;
    while step < n {
        let step2 = step * 2;
        for i in (0..n).step_by(step2) {
            for j in 0..step {
                let u = buf[i + j];
                let v = buf[i + j + step];
                buf[i + j] = u + v;
                buf[i + j + step] = u - v;
            }
        }
        step = step2;
    }
}

/// Generates pseudo-random Rademacher diagonal signs ($\pm 1.0$) from a deterministic seed.
fn generate_sign_flips(n: usize, seed: u64) -> Vec<f32> {
    let mut signs = Vec::with_capacity(n);
    let mut state = seed ^ 0x0005_DEEC_E66D;
    for _ in 0..n {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let bit = (state >> 63) & 1;
        signs.push(if bit == 0 { 1.0f32 } else { -1.0f32 });
    }
    signs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn test_rabitq_fwht_norm_preservation() {
        let dim = 64;
        let quantizer = RaBitQQuantizer::new(dim, 42);

        let v1 = vec![0.5f32; dim];
        let original_norm = (v1.iter().map(|x| x * x).sum::<f32>()).sqrt();

        let rot = quantizer.rotate(&v1);
        let rotated_norm = (rot.iter().map(|x| x * x).sum::<f32>()).sqrt();

        // FWHT must preserve Euclidean norm
        assert!(
            (original_norm - rotated_norm).abs() < 1e-4,
            "Norm changed after FWHT: {original_norm} vs {rotated_norm}"
        );
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn test_rabitq_quantization_and_estimation() {
        let dim = 64;
        let quantizer = RaBitQQuantizer::new(dim, 42);

        let v1 = vec![0.5f32; dim];
        let (code, alpha, beta, norm) = quantizer.quantize(&v1);
        assert_eq!(code.len(), quantizer.effective_dim.div_ceil(8));
        assert!(alpha > 0.0);
        assert!((norm - (dim as f32 * 0.25).sqrt()).abs() < 1e-4);

        let q = vec![0.5f32; dim];
        let rot_q = quantizer.rotate(&q);
        let q_norm_sq = q.iter().map(|x| x * x).sum::<f32>();

        let est_dist = quantizer.estimate_l2_distance(&rot_q, q_norm_sq, &code, alpha, beta, norm);
        // 1-bit binary quantization preserves relative distance ordering within bounded error
        assert!(
            est_dist < 15.0,
            "Expected low distance estimation, got {est_dist}"
        );
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn test_rabitq_2bit_quantization() {
        let dim = 64;
        let quantizer = RaBitQQuantizer::new_with_precision(dim, 42, QuantizationPrecision::TwoBit);

        let v1 = vec![0.5f32; dim];
        let (code, alpha, _beta, norm) = quantizer.quantize(&v1);
        assert_eq!(code.len(), quantizer.effective_dim.div_ceil(4));
        assert!(alpha >= 0.0);
        assert!((norm - (dim as f32 * 0.25).sqrt()).abs() < 1e-4);
    }
}
