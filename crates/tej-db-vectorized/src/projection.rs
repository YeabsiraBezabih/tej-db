use crate::operator::PhysicalOperator;
use tej_db_columnar::{DataChunk, PhysicalType};
use tej_db_core::{DbError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionExpr {
    Column(usize),
    AddInt64(usize, usize),
    AddFloat64(usize, usize),
    LiteralInt64(i64),
}

/// Vectorized physical projection evaluating scalar expressions in batches.
#[derive(Debug)]
pub struct PhysicalProjection {
    child: Box<dyn PhysicalOperator>,
    expressions: Vec<ProjectionExpr>,
    output_types: Vec<PhysicalType>,
}

impl PhysicalProjection {
    #[must_use]
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        expressions: Vec<ProjectionExpr>,
        output_types: Vec<PhysicalType>,
    ) -> Self {
        Self {
            child,
            expressions,
            output_types,
        }
    }
}

impl PhysicalOperator for PhysicalProjection {
    fn open(&mut self) -> Result<()> {
        self.child.open()
    }

    fn next(&mut self) -> Result<Option<DataChunk>> {
        if let Some(in_chunk) = self.child.next()? {
            let mut out_chunk = DataChunk::new(&self.output_types, in_chunk.capacity());

            for (out_idx, expr) in self.expressions.iter().enumerate() {
                let out_col = out_chunk
                    .column_mut(out_idx)
                    .ok_or_else(|| DbError::Custom("Output column missing".into()))?;

                match expr {
                    ProjectionExpr::Column(src_idx) => {
                        let src_col = in_chunk.column(*src_idx).ok_or_else(|| {
                            DbError::Custom(format!("Source column {src_idx} missing"))
                        })?;
                        *out_col = src_col.clone();
                    }
                    ProjectionExpr::AddInt64(c1, c2) => {
                        let col1 = in_chunk.column(*c1).ok_or_else(|| {
                            DbError::Custom("First operand column missing".into())
                        })?;
                        let col2 = in_chunk.column(*c2).ok_or_else(|| {
                            DbError::Custom("Second operand column missing".into())
                        })?;

                        for row_idx in 0..in_chunk.num_rows() {
                            if col1.is_null(row_idx) || col2.is_null(row_idx) {
                                out_col.push_null()?;
                            } else {
                                let v1 = col1.get_i64(row_idx).unwrap_or(0);
                                let v2 = col2.get_i64(row_idx).unwrap_or(0);
                                out_col.push_i64(v1 + v2)?;
                            }
                        }
                    }
                    ProjectionExpr::AddFloat64(c1, c2) => {
                        let col1 = in_chunk.column(*c1).ok_or_else(|| {
                            DbError::Custom("First operand column missing".into())
                        })?;
                        let col2 = in_chunk.column(*c2).ok_or_else(|| {
                            DbError::Custom("Second operand column missing".into())
                        })?;

                        for row_idx in 0..in_chunk.num_rows() {
                            if col1.is_null(row_idx) || col2.is_null(row_idx) {
                                out_col.push_null()?;
                            } else {
                                let v1 = col1.get_f64(row_idx).unwrap_or(0.0);
                                let v2 = col2.get_f64(row_idx).unwrap_or(0.0);
                                out_col.push_f64(v1 + v2)?;
                            }
                        }
                    }
                    ProjectionExpr::LiteralInt64(lit) => {
                        for _ in 0..in_chunk.num_rows() {
                            out_col.push_i64(*lit)?;
                        }
                    }
                }
            }

            out_chunk.set_num_rows(in_chunk.num_rows())?;
            Ok(Some(out_chunk))
        } else {
            Ok(None)
        }
    }

    fn close(&mut self) -> Result<()> {
        self.child.close()
    }
}
