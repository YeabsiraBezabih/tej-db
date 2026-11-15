use crate::vector::{ColumnVector, PhysicalType};
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

/// Standard batch size for vectorized query processing (matching DuckDB's 2,048-row morsel).
pub const STANDARD_VECTOR_SIZE: usize = 2048;

/// A DataChunk represents a horizontal slice of a table in columnar format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataChunk {
    columns: Vec<ColumnVector>,
    num_rows: usize,
    capacity: usize,
}

impl DataChunk {
    /// Creates an empty DataChunk for the given physical schema types.
    #[must_use]
    pub fn new(types: &[PhysicalType], capacity: usize) -> Self {
        let columns = types
            .iter()
            .map(|&pt| ColumnVector::new(pt, capacity))
            .collect();
        Self {
            columns,
            num_rows: 0,
            capacity,
        }
    }

    /// Creates a DataChunk with standard vector size (2,048 rows).
    #[must_use]
    pub fn standard(types: &[PhysicalType]) -> Self {
        Self::new(types, STANDARD_VECTOR_SIZE)
    }

    #[must_use]
    pub fn num_rows(&self) -> usize {
        self.num_rows
    }

    #[must_use]
    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.num_rows == 0
    }

    #[must_use]
    pub fn is_full(&self) -> bool {
        self.num_rows >= self.capacity
    }

    #[must_use]
    pub fn column(&self, idx: usize) -> Option<&ColumnVector> {
        self.columns.get(idx)
    }

    pub fn column_mut(&mut self, idx: usize) -> Option<&mut ColumnVector> {
        self.columns.get_mut(idx)
    }

    #[must_use]
    pub fn columns(&self) -> &[ColumnVector] {
        &self.columns
    }

    /// Sets the row count for this chunk after column vectors have been populated.
    pub fn set_num_rows(&mut self, rows: usize) -> Result<()> {
        if rows > self.capacity {
            return Err(DbError::Custom("Chunk row count exceeds capacity".into()));
        }
        for col in &self.columns {
            if col.len() != rows {
                return Err(DbError::Custom(
                    "ColumnVector length mismatch in DataChunk".into(),
                ));
            }
        }
        self.num_rows = rows;
        Ok(())
    }

    /// Resets the chunk for reuse without deallocating vector buffers.
    pub fn reset(&mut self) {
        self.num_rows = 0;
        let types: Vec<_> = self
            .columns
            .iter()
            .map(ColumnVector::physical_type)
            .collect();
        self.columns = types
            .into_iter()
            .map(|pt| ColumnVector::new(pt, self.capacity))
            .collect();
    }
}
