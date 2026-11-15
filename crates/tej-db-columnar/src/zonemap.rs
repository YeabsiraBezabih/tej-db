use crate::vector::{ColumnVector, PhysicalType};
use serde::{Deserialize, Serialize};

/// Column statistics captured for a columnar chunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColumnStat {
    Int64 { min: i64, max: i64 },
    Float64 { min: f64, max: f64 },
    String { min: String, max: String },
}

/// Zone Map statistics for a column chunk enabling O(1) page/chunk skip filtering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneMap {
    pub stat: Option<ColumnStat>,
    pub null_count: u32,
    pub row_count: u32,
}

impl ZoneMap {
    #[must_use]
    pub fn empty(row_count: u32) -> Self {
        Self {
            stat: None,
            null_count: 0,
            row_count,
        }
    }

    /// Computes zone map bounds from a populated ColumnVector.
    #[must_use]
    pub fn build(vec: &ColumnVector) -> Self {
        let row_count = u32::try_from(vec.len()).unwrap_or(u32::MAX);
        let null_count = vec.null_count();

        if null_count == row_count || vec.is_empty() {
            return Self {
                stat: None,
                null_count,
                row_count,
            };
        }

        let stat = match vec.physical_type() {
            PhysicalType::Int64 => {
                let mut min_val = i64::MAX;
                let mut max_val = i64::MIN;
                for i in 0..vec.len() {
                    if let Some(v) = vec.get_i64(i) {
                        min_val = min_val.min(v);
                        max_val = max_val.max(v);
                    }
                }
                if min_val <= max_val {
                    Some(ColumnStat::Int64 {
                        min: min_val,
                        max: max_val,
                    })
                } else {
                    None
                }
            }
            PhysicalType::Float64 => {
                let mut min_val = f64::INFINITY;
                let mut max_val = f64::NEG_INFINITY;
                for i in 0..vec.len() {
                    if let Some(v) = vec.get_f64(i) {
                        min_val = min_val.min(v);
                        max_val = max_val.max(v);
                    }
                }
                if min_val <= max_val {
                    Some(ColumnStat::Float64 {
                        min: min_val,
                        max: max_val,
                    })
                } else {
                    None
                }
            }
            PhysicalType::String => {
                let mut min_val: Option<&str> = None;
                let mut max_val: Option<&str> = None;
                for i in 0..vec.len() {
                    if let Some(v) = vec.get_str(i) {
                        min_val = Some(match min_val {
                            Some(cur) => cur.min(v),
                            None => v,
                        });
                        max_val = Some(match max_val {
                            Some(cur) => cur.max(v),
                            None => v,
                        });
                    }
                }
                match (min_val, max_val) {
                    (Some(min_s), Some(max_s)) => Some(ColumnStat::String {
                        min: min_s.to_string(),
                        max: max_s.to_string(),
                    }),
                    _ => None,
                }
            }
            PhysicalType::Boolean => None,
        };

        Self {
            stat,
            null_count,
            row_count,
        }
    }

    /// Evaluates whether any rows can satisfy `col = val` for Int64.
    #[must_use]
    pub fn can_match_eq_i64(&self, val: i64) -> bool {
        match &self.stat {
            Some(ColumnStat::Int64 { min, max }) => val >= *min && val <= *max,
            _ => self.null_count < self.row_count,
        }
    }

    /// Evaluates whether any rows can satisfy `low <= col <= high` for Int64.
    #[must_use]
    pub fn can_match_range_i64(&self, low: Option<i64>, high: Option<i64>) -> bool {
        match &self.stat {
            Some(ColumnStat::Int64 { min, max }) => {
                if let Some(l) = low {
                    if *max < l {
                        return false;
                    }
                }
                if let Some(h) = high {
                    if *min > h {
                        return false;
                    }
                }
                true
            }
            _ => self.null_count < self.row_count,
        }
    }

    /// Evaluates whether ALL non-null rows satisfy `low <= col <= high` for Int64.
    #[must_use]
    pub fn all_match_range_i64(&self, low: Option<i64>, high: Option<i64>) -> bool {
        if self.null_count > 0 || self.row_count == 0 {
            return false;
        }
        match &self.stat {
            Some(ColumnStat::Int64 { min, max }) => {
                if let Some(l) = low {
                    if *min < l {
                        return false;
                    }
                }
                if let Some(h) = high {
                    if *max > h {
                        return false;
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Evaluates whether any rows can satisfy `col = val` for Float64.
    #[must_use]
    pub fn can_match_eq_f64(&self, val: f64) -> bool {
        match &self.stat {
            Some(ColumnStat::Float64 { min, max }) => val >= *min && val <= *max,
            _ => self.null_count < self.row_count,
        }
    }

    /// Evaluates whether any rows can satisfy `low <= col <= high` for Float64.
    #[must_use]
    pub fn can_match_range_f64(&self, low: Option<f64>, high: Option<f64>) -> bool {
        match &self.stat {
            Some(ColumnStat::Float64 { min, max }) => {
                if let Some(l) = low {
                    if *max < l {
                        return false;
                    }
                }
                if let Some(h) = high {
                    if *min > h {
                        return false;
                    }
                }
                true
            }
            _ => self.null_count < self.row_count,
        }
    }

    /// Evaluates whether any rows can satisfy `col = val` for String.
    #[must_use]
    pub fn can_match_eq_str(&self, val: &str) -> bool {
        match &self.stat {
            Some(ColumnStat::String { min, max }) => val >= min.as_str() && val <= max.as_str(),
            _ => self.null_count < self.row_count,
        }
    }

    /// Evaluates whether any rows can satisfy `col IS NULL`.
    #[must_use]
    pub fn can_match_null(&self) -> bool {
        self.null_count > 0
    }

    /// Evaluates whether any rows can satisfy `col IS NOT NULL`.
    #[must_use]
    pub fn can_match_not_null(&self) -> bool {
        self.null_count < self.row_count
    }
}
