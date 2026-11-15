use crate::rabitq::QuantizationPrecision;
use crate::simd::rabitq_asymmetric_dot_product;
use crate::types::VectorId;

/// Contiguous RAM entry storing RaBitQ factors and offset into the codes buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct ArenaEntry {
    pub vector_id: VectorId,
    pub alpha: f32,
    pub beta: f32,
    pub norm: f32,
    pub code_offset: u32,
    pub code_len: u16,
}

/// Contiguous RAM arena holding 1-bit RaBitQ codes and quantization factors
/// for all indexed vectors in CPU L2/L3 cache-resident layout.
#[derive(Debug, Clone, Default)]
pub struct VectorArena {
    pub entries: Vec<ArenaEntry>,
    pub codes: Vec<u8>,
}

impl VectorArena {
    /// Creates a new empty `VectorArena` with reserved capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            codes: Vec::with_capacity(capacity * 32),
        }
    }

    /// Appends a vector's quantized code and factors to the arena.
    ///
    /// Returns the dense `node_idx` (0-based) of the vector in the arena.
    pub fn insert(
        &mut self,
        vector_id: VectorId,
        code: &[u8],
        alpha: f32,
        beta: f32,
        norm: f32,
    ) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "total code buffer under 4GB"
        )]
        let code_offset = self.codes.len() as u32;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "vector code length under 64KB"
        )]
        let code_len = code.len() as u16;

        self.codes.extend_from_slice(code);

        let idx = self.entries.len();
        self.entries.push(ArenaEntry {
            vector_id,
            alpha,
            beta,
            norm,
            code_offset,
            code_len,
        });

        idx
    }

    /// Number of vectors stored in the arena.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Checks if the arena is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Retrieves the binary code slice and factors for `node_idx`.
    #[inline]
    #[must_use]
    pub fn get(&self, node_idx: usize) -> Option<(&[u8], f32, f32, f32, VectorId)> {
        let entry = self.entries.get(node_idx)?;
        let start = entry.code_offset as usize;
        let end = start + (entry.code_len as usize);
        let code = &self.codes[start..end];
        Some((code, entry.alpha, entry.beta, entry.norm, entry.vector_id))
    }

    /// Fast L2 cache-resident asymmetric distance estimation between continuous rotated query
    /// and the 1-bit quantized vector at `node_idx`.
    #[inline]
    #[must_use]
    pub fn estimate_l2_distance(
        &self,
        node_idx: usize,
        rotated_query: &[f32],
        query_norm_sq: f32,
    ) -> f32 {
        if let Some((code, alpha, beta, norm, _)) = self.get(node_idx) {
            let dot = rabitq_asymmetric_dot_product(rotated_query, code, alpha, beta);
            let dist_sq = query_norm_sq + (norm * norm) - (2.0 * dot);
            dist_sq.max(0.0)
        } else {
            f32::INFINITY
        }
    }

    /// Fast L1 cache LUT asymmetric distance estimation between query and `node_idx`.
    #[inline]
    #[must_use]
    pub fn estimate_l2_with_lut(&self, node_idx: usize, lut: &QueryLut) -> f32 {
        if let Some(entry) = self.entries.get(node_idx) {
            let start = entry.code_offset as usize;
            let end = start + (entry.code_len as usize);
            let code = &self.codes[start..end];
            let mut bit_sum = 0.0f32;
            let lut_table = &lut.table;
            for (byte_idx, &b) in code.iter().enumerate() {
                let offset = byte_idx * 256 + (b as usize);
                if offset < lut_table.len() {
                    bit_sum += lut_table[offset];
                }
            }
            let dot = match lut.precision {
                QuantizationPrecision::OneBit => {
                    entry.alpha * (2.0 * bit_sum - lut.q_sum) + (entry.beta * lut.q_sum)
                }
                QuantizationPrecision::TwoBit => {
                    entry.alpha * (2.0 * bit_sum - 3.0 * lut.q_sum) + (entry.beta * lut.q_sum)
                }
            };
            let dist_sq = lut.query_norm_sq + (entry.norm * entry.norm) - (2.0 * dot);
            dist_sq.max(0.0)
        } else {
            f32::INFINITY
        }
    }
}

/// Precomputed query lookup table for ultra-fast L1 cache distance evaluation.
#[derive(Debug, Clone)]
pub struct QueryLut {
    pub table: Vec<f32>,
    pub q_sum: f32,
    pub query_norm_sq: f32,
    pub precision: QuantizationPrecision,
}

impl QueryLut {
    /// Creates a precomputed LUT for `rotated_query` based on the specified precision.
    #[must_use]
    pub fn new(
        rotated_query: &[f32],
        query_norm_sq: f32,
        precision: QuantizationPrecision,
    ) -> Self {
        let q_sum = rotated_query.iter().sum::<f32>();
        match precision {
            QuantizationPrecision::OneBit => {
                let num_bytes = rotated_query.len().div_ceil(8);
                let mut table = vec![0.0f32; num_bytes * 256];
                for byte_idx in 0..num_bytes {
                    let base = byte_idx * 8;
                    let offset = byte_idx * 256;
                    for b in 0..256usize {
                        let mut s = 0.0f32;
                        for bit in 0..8 {
                            let idx = base + bit;
                            if idx < rotated_query.len() && ((b >> bit) & 1) != 0 {
                                s += rotated_query[idx];
                            }
                        }
                        table[offset + b] = s;
                    }
                }
                Self {
                    table,
                    q_sum,
                    query_norm_sq,
                    precision,
                }
            }
            QuantizationPrecision::TwoBit => {
                let num_bytes = rotated_query.len().div_ceil(4);
                let mut table = vec![0.0f32; num_bytes * 256];
                for byte_idx in 0..num_bytes {
                    let base = byte_idx * 4;
                    let offset = byte_idx * 256;
                    for b in 0..256usize {
                        let mut s = 0.0f32;
                        for slot in 0..4 {
                            let idx = base + slot;
                            #[allow(clippy::cast_precision_loss)]
                            let b_val = ((b >> (slot * 2)) & 3) as f32;
                            if idx < rotated_query.len() && b_val > 0.0 {
                                s += rotated_query[idx] * b_val;
                            }
                        }
                        table[offset + b] = s;
                    }
                }
                Self {
                    table,
                    q_sum,
                    query_norm_sq,
                    precision,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_arena_append_and_estimate() {
        let mut arena = VectorArena::new(10);
        let vec_id1 = VectorId(101);
        let code1 = vec![0b1010_1010u8; 4];
        let pos1 = arena.insert(vec_id1, &code1, 0.5, 0.1, 2.0);
        assert_eq!(pos1, 0);

        let vec_id2 = VectorId(102);
        let code2 = vec![0b1111_0000u8; 4];
        let pos2 = arena.insert(vec_id2, &code2, 0.6, 0.2, 3.0);
        assert_eq!(pos2, 1);

        assert_eq!(arena.len(), 2);
        let (c, a, b, n, vid) = arena.get(0).unwrap();
        assert_eq!(c, &code1[..]);
        assert!((a - 0.5).abs() < 1e-6);
        assert!((b - 0.1).abs() < 1e-6);
        assert!((n - 2.0).abs() < 1e-6);
        assert_eq!(vid, vec_id1);

        let rot_q = vec![1.0f32; 32];
        let dist = arena.estimate_l2_distance(0, &rot_q, 32.0);
        assert!(dist.is_finite());

        let lut1 = QueryLut::new(&rot_q, 32.0, QuantizationPrecision::OneBit);
        let dist_lut1 = arena.estimate_l2_with_lut(0, &lut1);
        assert!(dist_lut1.is_finite());

        let lut2 = QueryLut::new(&rot_q, 32.0, QuantizationPrecision::TwoBit);
        let dist_lut2 = arena.estimate_l2_with_lut(0, &lut2);
        assert!(dist_lut2.is_finite());
    }
}
