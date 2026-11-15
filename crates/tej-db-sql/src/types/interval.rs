use super::timestamp::{MICROS_PER_DAY, MICROS_PER_SEC, Timestamp};
use core::cmp::Ordering;
use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

/// High-precision SQL Interval representing elapsed duration in months, days, and microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Interval {
    pub months: i32,
    pub days: i32,
    pub micros: i64,
}

impl Interval {
    /// Creates a new Interval.
    #[must_use]
    pub const fn new(months: i32, days: i32, micros: i64) -> Self {
        Self {
            months,
            days,
            micros,
        }
    }

    /// Zero interval.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            months: 0,
            days: 0,
            micros: 0,
        }
    }

    /// Creates an interval of specified days.
    #[must_use]
    pub const fn from_days(days: i32) -> Self {
        Self {
            months: 0,
            days,
            micros: 0,
        }
    }

    /// Creates an interval of specified seconds.
    #[must_use]
    pub const fn from_secs(secs: i64) -> Self {
        Self {
            months: 0,
            days: 0,
            micros: secs.saturating_mul(MICROS_PER_SEC),
        }
    }

    /// Adds this interval to a `Timestamp`.
    #[must_use]
    pub fn add_to_timestamp(&self, ts: Timestamp) -> Timestamp {
        // Approximate month addition by 30 days if months present, or exact civil math
        let total_days = i64::from(self.days) + i64::from(self.months) * 30;
        let added_micros = total_days * MICROS_PER_DAY + self.micros;
        Timestamp::from_micros(ts.as_micros().saturating_add(added_micros))
    }

    /// Encodes into big-endian order-preserving binary bytes for B-tree index keys.
    #[must_use]
    #[allow(clippy::cast_sign_loss)]
    pub fn encode_key(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(16);
        let u_months = (self.months as u32) ^ 0x8000_0000;
        let u_days = (self.days as u32) ^ 0x8000_0000;
        let u_micros = (self.micros as u64) ^ 0x8000_0000_0000_0000;
        b.extend_from_slice(&u_months.to_be_bytes());
        b.extend_from_slice(&u_days.to_be_bytes());
        b.extend_from_slice(&u_micros.to_be_bytes());
        b
    }
}

impl FromStr for Interval {
    type Err = DbError;

    fn from_str(s: &str) -> Result<Self> {
        let trimmed = s.trim().to_lowercase();
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();

        let mut months = 0i32;
        let mut days = 0i32;
        let mut micros = 0i64;

        let mut i = 0;
        while i < tokens.len() {
            let num: i64 = tokens[i]
                .parse()
                .map_err(|e| DbError::Custom(format!("Invalid number in interval: {e}")))?;

            if i + 1 >= tokens.len() {
                // Default to seconds if unit omitted
                micros += num.saturating_mul(MICROS_PER_SEC);
                break;
            }

            let unit = tokens[i + 1].trim_end_matches('s');
            match unit {
                "year" => months += (num * 12) as i32,
                "month" => months += num as i32,
                "week" => days += (num * 7) as i32,
                "day" => days += num as i32,
                "hour" => micros += num * 3600 * MICROS_PER_SEC,
                "minute" => micros += num * 60 * MICROS_PER_SEC,
                "second" => micros += num * MICROS_PER_SEC,
                "millisecond" | "ms" => micros += num * 1_000,
                "microsecond" | "us" => micros += num,
                _ => {
                    return Err(DbError::Custom(format!(
                        "Unknown interval unit: {}",
                        tokens[i + 1]
                    )));
                }
            }
            i += 2;
        }

        Ok(Self {
            months,
            days,
            micros,
        })
    }
}

impl PartialOrd for Interval {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Interval {
    fn cmp(&self, other: &Self) -> Ordering {
        // Approximate total duration in microseconds for comparison
        let total_self = i64::from(self.months) * 30 * MICROS_PER_DAY
            + i64::from(self.days) * MICROS_PER_DAY
            + self.micros;
        let total_other = i64::from(other.months) * 30 * MICROS_PER_DAY
            + i64::from(other.days) * MICROS_PER_DAY
            + other.micros;
        total_self.cmp(&total_other)
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.months != 0 {
            parts.push(format!("{} months", self.months));
        }
        if self.days != 0 {
            parts.push(format!("{} days", self.days));
        }
        if self.micros != 0 {
            let whole_secs = self.micros / MICROS_PER_SEC;
            let rem_micros = self.micros % MICROS_PER_SEC;
            if rem_micros == 0 {
                parts.push(format!("{whole_secs} seconds"));
            } else {
                let frac = rem_micros.unsigned_abs();
                parts.push(format!("{whole_secs}.{frac:06} seconds"));
            }
        }
        if parts.is_empty() {
            write!(f, "0 seconds")
        } else {
            write!(f, "{}", parts.join(" "))
        }
    }
}
