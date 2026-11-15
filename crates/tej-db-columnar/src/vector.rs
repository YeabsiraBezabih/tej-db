use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

/// Physical storage type for a column vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhysicalType {
    Int64,
    Float64,
    Boolean,
    String,
}

/// Contiguous, SIMD-aligned columnar buffer representing values of a single column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnVector {
    physical_type: PhysicalType,
    null_bitmap: RoaringBitmap,
    length: usize,
    i64_data: Vec<i64>,
    f64_data: Vec<f64>,
    bool_data: RoaringBitmap,
    str_offsets: Vec<u32>,
    str_data: Vec<u8>,
}

impl ColumnVector {
    /// Creates an empty column vector with pre-allocated capacity.
    #[must_use]
    pub fn new(physical_type: PhysicalType, capacity: usize) -> Self {
        let (i64_data, f64_data, str_offsets, str_data) = match physical_type {
            PhysicalType::Int64 => (
                Vec::with_capacity(capacity),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            PhysicalType::Float64 => (
                Vec::new(),
                Vec::with_capacity(capacity),
                Vec::new(),
                Vec::new(),
            ),
            PhysicalType::Boolean => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            PhysicalType::String => {
                let mut offsets = Vec::with_capacity(capacity + 1);
                offsets.push(0);
                (
                    Vec::new(),
                    Vec::new(),
                    offsets,
                    Vec::with_capacity(capacity * 16),
                )
            }
        };

        Self {
            physical_type,
            null_bitmap: RoaringBitmap::new(),
            length: 0,
            i64_data,
            f64_data,
            bool_data: RoaringBitmap::new(),
            str_offsets,
            str_data,
        }
    }

    /// Creates an Int64 column vector taking ownership of pre-allocated data with zero copying.
    #[must_use]
    pub fn from_i64_vec(data: Vec<i64>) -> Self {
        let length = data.len();
        Self {
            physical_type: PhysicalType::Int64,
            null_bitmap: RoaringBitmap::new(),
            length,
            i64_data: data,
            f64_data: Vec::new(),
            bool_data: RoaringBitmap::new(),
            str_offsets: Vec::new(),
            str_data: Vec::new(),
        }
    }

    #[must_use]
    pub fn physical_type(&self) -> PhysicalType {
        self.physical_type
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.length
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    #[must_use]
    pub fn is_null(&self, idx: usize) -> bool {
        self.null_bitmap
            .contains(u32::try_from(idx).unwrap_or(u32::MAX))
    }

    #[must_use]
    pub fn null_count(&self) -> u32 {
        u32::try_from(self.null_bitmap.len()).unwrap_or(u32::MAX)
    }

    /// Appends a 64-bit integer value.
    pub fn push_i64(&mut self, val: i64) -> Result<()> {
        if self.physical_type != PhysicalType::Int64 {
            return Err(DbError::Custom("Type mismatch: expected Int64".into()));
        }
        self.i64_data.push(val);
        self.length += 1;
        Ok(())
    }

    /// Appends a 64-bit floating point value.
    pub fn push_f64(&mut self, val: f64) -> Result<()> {
        if self.physical_type != PhysicalType::Float64 {
            return Err(DbError::Custom("Type mismatch: expected Float64".into()));
        }
        self.f64_data.push(val);
        self.length += 1;
        Ok(())
    }

    /// Appends a boolean value.
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn push_bool(&mut self, val: bool) -> Result<()> {
        if self.physical_type != PhysicalType::Boolean {
            return Err(DbError::Custom("Type mismatch: expected Boolean".into()));
        }
        if val {
            self.bool_data
                .insert(u32::try_from(self.length).unwrap_or(0));
        }
        self.length += 1;
        Ok(())
    }

    /// Appends a string value into the contiguous string buffer.
    pub fn push_str(&mut self, val: &str) -> Result<()> {
        if self.physical_type != PhysicalType::String {
            return Err(DbError::Custom("Type mismatch: expected String".into()));
        }
        let bytes = val.as_bytes();
        self.str_data.extend_from_slice(bytes);
        let next_offset = u32::try_from(self.str_data.len())
            .map_err(|_| DbError::Custom("String data exceeds 4GB limit".into()))?;
        self.str_offsets.push(next_offset);
        self.length += 1;
        Ok(())
    }

    /// Appends a NULL value of the column's physical type.
    pub fn push_null(&mut self) -> Result<()> {
        let idx = u32::try_from(self.length)
            .map_err(|_| DbError::Custom("ColumnVector row overflow".into()))?;
        self.null_bitmap.insert(idx);

        match self.physical_type {
            PhysicalType::Int64 => self.i64_data.push(0),
            PhysicalType::Float64 => self.f64_data.push(0.0),
            PhysicalType::Boolean => {}
            PhysicalType::String => {
                let last = *self.str_offsets.last().unwrap_or(&0);
                self.str_offsets.push(last);
            }
        }
        self.length += 1;
        Ok(())
    }

    #[must_use]
    pub fn get_i64(&self, idx: usize) -> Option<i64> {
        if idx >= self.length || self.is_null(idx) || self.physical_type != PhysicalType::Int64 {
            None
        } else {
            self.i64_data.get(idx).copied()
        }
    }

    #[must_use]
    pub fn get_f64(&self, idx: usize) -> Option<f64> {
        if idx >= self.length || self.is_null(idx) || self.physical_type != PhysicalType::Float64 {
            None
        } else {
            self.f64_data.get(idx).copied()
        }
    }

    #[must_use]
    pub fn get_bool(&self, idx: usize) -> Option<bool> {
        if idx >= self.length || self.is_null(idx) || self.physical_type != PhysicalType::Boolean {
            None
        } else {
            let u = u32::try_from(idx).unwrap_or(u32::MAX);
            Some(self.bool_data.contains(u))
        }
    }

    #[must_use]
    pub fn get_str(&self, idx: usize) -> Option<&str> {
        if idx >= self.length || self.is_null(idx) || self.physical_type != PhysicalType::String {
            return None;
        }
        let start = *self.str_offsets.get(idx)? as usize;
        let end = *self.str_offsets.get(idx + 1)? as usize;
        let slice = self.str_data.get(start..end)?;
        core::str::from_utf8(slice).ok()
    }

    #[must_use]
    pub fn as_i64_slice(&self) -> &[i64] {
        &self.i64_data
    }

    #[must_use]
    pub fn as_f64_slice(&self) -> &[f64] {
        &self.f64_data
    }

    #[must_use]
    pub fn null_bitmap(&self) -> &RoaringBitmap {
        &self.null_bitmap
    }
}
