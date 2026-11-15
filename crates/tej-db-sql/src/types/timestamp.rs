use core::cmp::Ordering;
use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

/// Number of microseconds per second.
pub const MICROS_PER_SEC: i64 = 1_000_000;
/// Number of microseconds per day.
pub const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SEC;

/// High-precision SQL Timestamp (microseconds since UNIX Epoch 1970-01-01 00:00:00 UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// Creates a timestamp from raw microseconds since epoch.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Creates a timestamp from integer seconds since epoch.
    #[must_use]
    pub const fn from_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(MICROS_PER_SEC))
    }

    /// Returns microseconds since epoch.
    #[must_use]
    pub const fn as_micros(&self) -> i64 {
        self.0
    }

    /// Encodes timestamp into big-endian order-preserving binary bytes for B-tree index keys.
    #[must_use]
    #[allow(clippy::cast_sign_loss)]
    pub fn encode_key(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(8);
        let unsigned = (self.0 as u64) ^ 0x8000_0000_0000_0000;
        b.extend_from_slice(&unsigned.to_be_bytes());
        b
    }
}

impl FromStr for Timestamp {
    type Err = DbError;

    fn from_str(s: &str) -> Result<Self> {
        let trimmed = s.trim();
        // Support raw integer microseconds or seconds
        if let Ok(num) = trimmed.parse::<i64>() {
            return Ok(Self::from_micros(num));
        }

        // Parse ISO/SQL date time: "YYYY-MM-DD[ T]HH:MM:SS[.ffffff][Z]"
        let clean = trimmed.trim_end_matches('Z');
        let parts: Vec<&str> = clean.split([' ', 'T']).collect();
        if parts.is_empty() {
            return Err(DbError::Custom(format!("Invalid timestamp format: {s}")));
        }

        let date_parts: Vec<&str> = parts[0].split('-').collect();
        if date_parts.len() != 3 {
            return Err(DbError::Custom(format!(
                "Invalid date format in timestamp: {s}"
            )));
        }

        let year: i32 = date_parts[0]
            .parse()
            .map_err(|e| DbError::Custom(format!("Invalid year: {e}")))?;
        let month: u32 = date_parts[1]
            .parse()
            .map_err(|e| DbError::Custom(format!("Invalid month: {e}")))?;
        let day: u32 = date_parts[2]
            .parse()
            .map_err(|e| DbError::Custom(format!("Invalid day: {e}")))?;

        let mut hour = 0u32;
        let mut min = 0u32;
        let mut sec = 0u32;
        let mut micros = 0u32;

        if parts.len() > 1 {
            let time_parts: Vec<&str> = parts[1].split(':').collect();
            if time_parts.len() >= 2 {
                hour = time_parts[0].parse().unwrap_or(0);
                min = time_parts[1].parse().unwrap_or(0);
                if time_parts.len() >= 3 {
                    let sec_parts: Vec<&str> = time_parts[2].split('.').collect();
                    sec = sec_parts[0].parse().unwrap_or(0);
                    if sec_parts.len() > 1 {
                        let mut frac = sec_parts[1].to_string();
                        if frac.len() > 6 {
                            frac.truncate(6);
                        } else {
                            while frac.len() < 6 {
                                frac.push('0');
                            }
                        }
                        micros = frac.parse().unwrap_or(0);
                    }
                }
            }
        }

        let days = days_from_civil(year, month, day);
        let total_micros = i64::from(days) * MICROS_PER_DAY
            + i64::from(hour) * 3600 * MICROS_PER_SEC
            + i64::from(min) * 60 * MICROS_PER_SEC
            + i64::from(sec) * MICROS_PER_SEC
            + i64::from(micros);

        Ok(Self(total_micros))
    }
}

/// Computes days since 1970-01-01 from civil year, month, day (Gregorian calendar).
fn days_from_civil(year: i32, month: u32, day: u32) -> i32 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = u32::try_from(y - era * 400).unwrap_or(0);
    let m = month;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let doe_i32 = i32::try_from(doe).unwrap_or(0);
    (era * 146_097 + doe_i32) - 719_468
}

impl PartialOrd for Timestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Timestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total_secs = self.0.div_euclid(MICROS_PER_SEC);
        let rem_micros = self.0.rem_euclid(MICROS_PER_SEC);

        let days = total_secs.div_euclid(86400);
        let day_secs = total_secs.rem_euclid(86400);

        let hour = day_secs / 3600;
        let min = (day_secs % 3600) / 60;
        let sec = day_secs % 60;

        let days_i32 = i32::try_from(days).unwrap_or(0);
        let (year, month, day) = civil_from_days(days_i32);
        if rem_micros > 0 {
            write!(
                f,
                "{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}.{rem_micros:06}"
            )
        } else {
            write!(
                f,
                "{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}"
            )
        }
    }
}

/// Computes (year, month, day) from days since 1970-01-01.
fn civil_from_days(days: i32) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = u32::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let yoe_i32 = i32::try_from(yoe).unwrap_or(0);
    let y = yoe_i32 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
