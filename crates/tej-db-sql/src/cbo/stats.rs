use std::collections::HashMap;

#[inline]
fn fmix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.wrapping_mul(0xff51_afd7_ed55_8ccd);
    k ^= k >> 33;
    k = k.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    k ^= k >> 33;
    k
}

/// 64-register HyperLogLog for $O(1)$ memory cardinality estimation of columns.
#[derive(Debug, Clone, PartialEq)]
pub struct HyperLogLog {
    registers: [u8; 64],
}

impl Default for HyperLogLog {
    fn default() -> Self {
        Self { registers: [0; 64] }
    }
}

impl HyperLogLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a 64-bit hashed value to the HyperLogLog registers.
    pub fn add_hash(&mut self, val: u64) {
        let hash = fmix64(val);
        let register_idx = (hash >> 58) as usize;
        let w = hash << 6;
        let leading_zeros = if w == 0 {
            59
        } else {
            (w.leading_zeros() + 1) as u8
        };
        if leading_zeros > self.registers[register_idx] {
            self.registers[register_idx] = leading_zeros;
        }
    }

    /// Estimates the cardinality (count of distinct elements).
    #[must_use]
    pub fn estimate(&self) -> u64 {
        let m = 64.0_f64;
        let alpha = 0.709_f64; // Standard correction constant for m = 64

        let mut sum = 0.0_f64;
        let mut zero_registers = 0;

        for &val in &self.registers {
            sum += 2.0_f64.powi(-i32::from(val));
            if val == 0 {
                zero_registers += 1;
            }
        }

        let raw_estimate = alpha * m * m / sum;

        // Linear counting for small cardinalities
        if raw_estimate <= 2.5 * m && zero_registers > 0 {
            let v = f64::from(zero_registers);
            let linear_estimate = m * (m / v).ln();
            u64::try_from(linear_estimate.round() as i64).unwrap_or(0)
        } else {
            u64::try_from(raw_estimate.round() as i64).unwrap_or(0)
        }
    }
}

/// 64-bucket equi-width histogram for estimating selectivity of numerical predicates.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnHistogram {
    min_val: i64,
    max_val: i64,
    bucket_counts: [u32; 64],
    total_count: u64,
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
impl ColumnHistogram {
    #[must_use]
    pub fn new(min_val: i64, max_val: i64) -> Self {
        Self {
            min_val,
            max_val,
            bucket_counts: [0; 64],
            total_count: 0,
        }
    }

    pub fn add_value(&mut self, val: i64) {
        if val < self.min_val || val > self.max_val || self.min_val == self.max_val {
            self.bucket_counts[0] += 1;
        } else {
            let span = self.max_val - self.min_val;
            let ratio = (val - self.min_val) as f64 / span as f64;
            let bucket = ((ratio * 64.0).floor() as usize).min(63);
            self.bucket_counts[bucket] += 1;
        }
        self.total_count += 1;
    }

    /// Estimates selectivity of a range predicate `val >= low && val <= high` in [0.0, 1.0].
    #[must_use]
    pub fn estimate_range_selectivity(&self, low: Option<i64>, high: Option<i64>) -> f64 {
        if self.total_count == 0 {
            return 1.0;
        }

        let min_bound = low.unwrap_or(self.min_val).max(self.min_val);
        let max_bound = high.unwrap_or(self.max_val).min(self.max_val);

        if min_bound > max_bound || min_bound > self.max_val || max_bound < self.min_val {
            return 0.0;
        }

        if self.min_val == self.max_val {
            return 1.0;
        }

        let span = (self.max_val - self.min_val) as f64;
        let low_ratio = (min_bound - self.min_val) as f64 / span;
        let high_ratio = (max_bound - self.min_val) as f64 / span;

        let low_bucket = ((low_ratio * 64.0).floor() as usize).min(63);
        let high_bucket = ((high_ratio * 64.0).floor() as usize).min(63);

        let mut matched_samples = 0_u64;
        for &count in &self.bucket_counts[low_bucket..=high_bucket] {
            matched_samples += u64::from(count);
        }

        (matched_samples as f64 / self.total_count as f64).clamp(0.0, 1.0)
    }
}

/// Statistics for a table column.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnStats {
    pub hll: HyperLogLog,
    pub histogram: Option<ColumnHistogram>,
    pub null_count: u64,
}

/// Table-level catalog statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct TableStats {
    pub row_count: u64,
    pub columns: HashMap<String, ColumnStats>,
}

impl TableStats {
    #[must_use]
    pub fn new(row_count: u64) -> Self {
        Self {
            row_count,
            columns: HashMap::new(),
        }
    }
}
