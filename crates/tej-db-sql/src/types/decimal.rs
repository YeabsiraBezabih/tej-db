use core::cmp::Ordering;
use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

/// Maximum supported decimal scale.
pub const MAX_DECIMAL_SCALE: u8 = 28;

/// Exact fixed-point 128-bit decimal numeric representation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Decimal {
    value: i128,
    scale: u8,
}

impl Decimal {
    /// Creates a new `Decimal` from an integer value and scale (number of fractional decimal digits).
    #[must_use]
    pub const fn new(value: i128, scale: u8) -> Self {
        Self { value, scale }
    }

    /// Zero decimal value.
    #[must_use]
    pub const fn zero() -> Self {
        Self { value: 0, scale: 0 }
    }

    /// Returns the raw 128-bit integer value.
    #[must_use]
    pub const fn raw_value(&self) -> i128 {
        self.value
    }

    /// Returns the decimal scale.
    #[must_use]
    pub const fn scale(&self) -> u8 {
        self.scale
    }

    /// Returns 10^scale as an i128.
    #[must_use]
    fn pow10(scale: u8) -> i128 {
        const POW10_TABLE: [i128; 29] = [
            1,
            10,
            100,
            1_000,
            10_000,
            100_000,
            1_000_000,
            10_000_000,
            100_000_000,
            1_000_000_000,
            10_000_000_000,
            100_000_000_000,
            1_000_000_000_000,
            10_000_000_000_000,
            100_000_000_000_000,
            1_000_000_000_000_000,
            10_000_000_000_000_000,
            100_000_000_000_000_000,
            1_000_000_000_000_000_000,
            10_000_000_000_000_000_000,
            100_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000,
            10_000_000_000_000_000_000_000,
            100_000_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000_000,
            10_000_000_000_000_000_000_000_000,
            100_000_000_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000_000_000,
            10_000_000_000_000_000_000_000_000_000,
        ];
        if (scale as usize) < POW10_TABLE.len() {
            POW10_TABLE[scale as usize]
        } else {
            10i128.saturating_pow(u32::from(scale))
        }
    }

    /// Rescales this decimal to a new target scale.
    #[must_use]
    pub fn rescale(&self, target_scale: u8) -> Self {
        match self.scale.cmp(&target_scale) {
            core::cmp::Ordering::Equal => *self,
            core::cmp::Ordering::Less => {
                let diff = target_scale - self.scale;
                let factor = Self::pow10(diff);
                Self {
                    value: self.value.saturating_mul(factor),
                    scale: target_scale,
                }
            }
            core::cmp::Ordering::Greater => {
                let diff = self.scale - target_scale;
                let factor = Self::pow10(diff);
                Self {
                    value: self.value / factor,
                    scale: target_scale,
                }
            }
        }
    }

    /// Adds two decimal numbers.
    #[must_use]
    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        let max_scale = self.scale.max(other.scale);
        let a = self.rescale(max_scale);
        let b = other.rescale(max_scale);
        let val = a.value.checked_add(b.value)?;
        Some(Self {
            value: val,
            scale: max_scale,
        })
    }

    /// Subtracts `other` from this decimal.
    #[must_use]
    pub fn checked_sub(&self, other: &Self) -> Option<Self> {
        let max_scale = self.scale.max(other.scale);
        let a = self.rescale(max_scale);
        let b = other.rescale(max_scale);
        let val = a.value.checked_sub(b.value)?;
        Some(Self {
            value: val,
            scale: max_scale,
        })
    }

    /// Multiplies two decimal numbers.
    #[must_use]
    pub fn checked_mul(&self, other: &Self) -> Option<Self> {
        let new_scale = self.scale.checked_add(other.scale)?;
        let val = self.value.checked_mul(other.value)?;
        Some(Self {
            value: val,
            scale: new_scale,
        })
    }

    /// Divides this decimal by `other` with a specified result scale.
    #[must_use]
    pub fn checked_div(&self, other: &Self, result_scale: u8) -> Option<Self> {
        if other.value == 0 {
            return None;
        }
        let total_scale = u32::from(result_scale).checked_add(u32::from(other.scale))?;
        let diff_scale = total_scale.checked_sub(u32::from(self.scale))?;
        let factor = 10i128.checked_pow(diff_scale)?;
        let num = self.value.checked_mul(factor)?;
        let val = num.checked_div(other.value)?;
        Some(Self {
            value: val,
            scale: result_scale,
        })
    }

    /// Converts into `f64`.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn to_f64(&self) -> f64 {
        let factor = Self::pow10(self.scale) as f64;
        (self.value as f64) / factor
    }

    /// Encodes decimal into big-endian order-preserving binary bytes for B-tree index keys.
    #[must_use]
    #[allow(clippy::cast_sign_loss)]
    pub fn encode_key(&self) -> Vec<u8> {
        // Normalize to standard scale 18 for consistent binary key ordering
        let norm = self.rescale(18);
        let mut b = Vec::with_capacity(17);
        let unsigned = (norm.value as u128) ^ 0x8000_0000_0000_0000_0000_0000_0000_0000;
        b.extend_from_slice(&unsigned.to_be_bytes());
        b
    }
}

impl FromStr for Decimal {
    type Err = DbError;

    fn from_str(s: &str) -> Result<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(DbError::Custom("Empty decimal string".into()));
        }

        let is_neg = trimmed.starts_with('-');
        let abs_str = trimmed
            .strip_prefix('-')
            .or_else(|| trimmed.strip_prefix('+'))
            .unwrap_or(trimmed);

        let parts: Vec<&str> = abs_str.split('.').collect();
        if parts.len() > 2 {
            return Err(DbError::Custom(format!("Invalid decimal format: {s}")));
        }

        let int_part = parts[0];
        let frac_part = if parts.len() == 2 { parts[1] } else { "" };

        if frac_part.len() > MAX_DECIMAL_SCALE as usize {
            return Err(DbError::Custom(format!(
                "Decimal scale exceeds maximum {MAX_DECIMAL_SCALE}"
            )));
        }

        let mut combined = String::with_capacity(int_part.len() + frac_part.len());
        combined.push_str(int_part);
        combined.push_str(frac_part);

        let parsed = combined
            .parse::<i128>()
            .map_err(|e| DbError::Custom(format!("Failed to parse decimal: {e}")))?;

        let val = if is_neg { -parsed } else { parsed };
        #[allow(clippy::cast_possible_truncation)]
        let scale = frac_part.len() as u8;

        Ok(Self::new(val, scale))
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        let max_scale = self.scale.max(other.scale);
        self.rescale(max_scale).value == other.rescale(max_scale).value
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        let max_scale = self.scale.max(other.scale);
        self.rescale(max_scale)
            .value
            .cmp(&other.rescale(max_scale).value)
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.scale == 0 {
            return write!(f, "{}", self.value);
        }

        let is_neg = self.value < 0;
        let abs_val = self.value.unsigned_abs();
        let factor = Self::pow10(self.scale).unsigned_abs();

        let int_part = abs_val / factor;
        let frac_part = abs_val % factor;

        if is_neg {
            write!(
                f,
                "-{int_part}.{frac_part:0width$}",
                width = usize::from(self.scale)
            )
        } else {
            write!(
                f,
                "{int_part}.{frac_part:0width$}",
                width = usize::from(self.scale)
            )
        }
    }
}
