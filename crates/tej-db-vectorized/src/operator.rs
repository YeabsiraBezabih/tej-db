use tej_db_columnar::{ColumnVector, DataChunk, PhysicalType};
use tej_db_core::{DbError, Result};

/// Selection vector representing the row indices in a DataChunk that satisfy a predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionVector {
    indices: Vec<usize>,
}

impl SelectionVector {
    #[must_use]
    pub fn new(indices: Vec<usize>) -> Self {
        Self { indices }
    }

    #[must_use]
    pub fn indices(&self) -> &[usize] {
        &self.indices
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Applies this selection vector to produce a compact DataChunk containing only matching rows.
    pub fn apply_to_chunk(&self, chunk: &DataChunk) -> Result<DataChunk> {
        let types: Vec<PhysicalType> = (0..chunk.num_columns())
            .filter_map(|i| chunk.column(i).map(ColumnVector::physical_type))
            .collect();

        let mut out_chunk = DataChunk::new(&types, self.indices.len().max(1));

        for (col_idx, &pt) in types.iter().enumerate() {
            let src = chunk
                .column(col_idx)
                .ok_or_else(|| DbError::Custom("Source column missing".into()))?;
            let dst = out_chunk
                .column_mut(col_idx)
                .ok_or_else(|| DbError::Custom("Destination column missing".into()))?;

            for &row_idx in &self.indices {
                if src.is_null(row_idx) {
                    dst.push_null()?;
                } else {
                    match pt {
                        PhysicalType::Int64 => {
                            if let Some(v) = src.get_i64(row_idx) {
                                dst.push_i64(v)?;
                            }
                        }
                        PhysicalType::Float64 => {
                            if let Some(v) = src.get_f64(row_idx) {
                                dst.push_f64(v)?;
                            }
                        }
                        PhysicalType::Boolean => {
                            if let Some(v) = src.get_bool(row_idx) {
                                dst.push_bool(v)?;
                            }
                        }
                        PhysicalType::String => {
                            if let Some(s) = src.get_str(row_idx) {
                                dst.push_str(s)?;
                            }
                        }
                    }
                }
            }
        }

        out_chunk.set_num_rows(self.indices.len())?;
        Ok(out_chunk)
    }
}

/// Core trait implemented by all vectorized query execution operators.
pub trait PhysicalOperator: std::fmt::Debug {
    /// Initialize operator resources before execution begins.
    fn open(&mut self) -> Result<()>;

    /// Fetch the next vectorized DataChunk (standard 2,048 rows). Returns `Ok(None)` on exhaustion.
    fn next(&mut self) -> Result<Option<DataChunk>>;

    /// Release operator resources and close child streams.
    fn close(&mut self) -> Result<()>;
}
