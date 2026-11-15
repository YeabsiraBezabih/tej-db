/// Portable auto-vectorized L2 squared distance between two f32 slices.
#[must_use]
pub fn l2_distance_squared(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vector lengths must match");
    let mut sum = 0.0f32;
    let chunks_a = a.chunks_exact(8);
    let chunks_b = b.chunks_exact(8);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();

    for (ca, cb) in chunks_a.zip(chunks_b) {
        for i in 0..8 {
            let diff = ca[i] - cb[i];
            sum += diff * diff;
        }
    }

    for (x, y) in rem_a.iter().zip(rem_b.iter()) {
        let diff = *x - *y;
        sum += diff * diff;
    }

    sum
}

/// Portable auto-vectorized dot product between two f32 slices.
#[must_use]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vector lengths must match");
    let mut sum = 0.0f32;
    let chunks_a = a.chunks_exact(8);
    let chunks_b = b.chunks_exact(8);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();

    for (ca, cb) in chunks_a.zip(chunks_b) {
        for i in 0..8 {
            sum += ca[i] * cb[i];
        }
    }

    for (x, y) in rem_a.iter().zip(rem_b.iter()) {
        sum += *x * *y;
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
/// query vector and a 1-bit quantized database vector.
///
/// Formula: <q, x> ~ alpha * sum_i (q_i * (2 * b_i - 1)) + beta * sum_i q_i
#[must_use]
pub fn rabitq_asymmetric_dot_product(
    rotated_query: &[f32],
    code: &[u8],
    alpha: f32,
    beta: f32,
) -> f32 {
    let dim = rotated_query.len();
    let mut bit_sum = 0.0f32;
    let mut q_sum = 0.0f32;

    for &q in rotated_query {
        q_sum += q;
    }

    for (byte_idx, &byte_val) in code.iter().enumerate() {
        let base_bit = byte_idx * 8;
        if base_bit >= dim {
            break;
        }

        let limit = (dim - base_bit).min(8);
        for bit in 0..limit {
            if ((byte_val >> bit) & 1) != 0 {
                bit_sum += rotated_query[base_bit + bit];
            }
        }
    }

    alpha * (2.0 * bit_sum - q_sum) + beta * q_sum
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
