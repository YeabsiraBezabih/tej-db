use crate::operator::PhysicalOperator;
use tej_db_columnar::{ColumnVector, DataChunk, PhysicalType, RowGroup};
use tej_db_core::{DbError, Result};

/// Filter predicate evaluated against zone-map statistics before decompressing a row-group.
#[derive(Debug, Clone, PartialEq)]
pub enum ZoneMapPredicate {
    EqualInt64 {
        column_idx: usize,
        value: i64,
    },
    RangeInt64 {
        column_idx: usize,
        low: Option<i64>,
        high: Option<i64>,
    },
    EqualString {
        column_idx: usize,
        value: String,
    },
    IsNull {
        column_idx: usize,
    },
    IsNotNull {
        column_idx: usize,
    },
}

impl ZoneMapPredicate {
    #[must_use]
    pub fn can_match(&self, rg: &RowGroup) -> bool {
        match self {
            Self::EqualInt64 { column_idx, value } => {
                if let Some(col) = rg.metadata.columns.get(*column_idx) {
                    col.zonemap.can_match_eq_i64(*value)
                } else {
                    true
                }
            }
            Self::RangeInt64 {
                column_idx,
                low,
                high,
            } => {
                if let Some(col) = rg.metadata.columns.get(*column_idx) {
                    col.zonemap.can_match_range_i64(*low, *high)
                } else {
                    true
                }
            }
            Self::EqualString { column_idx, value } => {
                if let Some(col) = rg.metadata.columns.get(*column_idx) {
                    col.zonemap.can_match_eq_str(value)
                } else {
                    true
                }
            }
            Self::IsNull { column_idx } => {
                if let Some(col) = rg.metadata.columns.get(*column_idx) {
                    col.zonemap.can_match_null()
                } else {
                    true
                }
            }
            Self::IsNotNull { column_idx } => {
                if let Some(col) = rg.metadata.columns.get(*column_idx) {
                    col.zonemap.can_match_not_null()
                } else {
                    true
                }
            }
        }
    }

    #[must_use]
    pub fn all_match(&self, rg: &RowGroup) -> bool {
        match self {
            Self::RangeInt64 {
                column_idx,
                low,
                high,
            } => {
                if let Some(col) = rg.metadata.columns.get(*column_idx) {
                    col.zonemap.all_match_range_i64(*low, *high)
                } else {
                    false
                }
            }
            Self::IsNotNull { column_idx } => {
                if let Some(col) = rg.metadata.columns.get(*column_idx) {
                    col.zonemap.null_count == 0
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

/// Vectorized physical scan operator reading RowGroups with projection and zonemap pushdown.
#[derive(Debug)]
pub struct PhysicalColumnScan {
    row_groups: std::sync::Arc<[RowGroup]>,
    projected_columns: Vec<usize>,
    predicates: Vec<ZoneMapPredicate>,
    current_rg_idx: usize,
    is_open: bool,
}

impl PhysicalColumnScan {
    #[must_use]
    pub fn new(
        row_groups: impl Into<std::sync::Arc<[RowGroup]>>,
        projected_columns: Vec<usize>,
        predicates: Vec<ZoneMapPredicate>,
    ) -> Self {
        Self {
            row_groups: row_groups.into(),
            projected_columns,
            predicates,
            current_rg_idx: 0,
            is_open: false,
        }
    }

    #[must_use]
    pub fn row_groups(&self) -> &std::sync::Arc<[RowGroup]> {
        &self.row_groups
    }

    #[must_use]
    pub fn projected_columns(&self) -> &[usize] {
        &self.projected_columns
    }

    #[must_use]
    pub fn predicates(&self) -> &[ZoneMapPredicate] {
        &self.predicates
    }
}

impl PhysicalOperator for PhysicalColumnScan {
    fn open(&mut self) -> Result<()> {
        self.current_rg_idx = 0;
        self.is_open = true;
        Ok(())
    }

    fn next(&mut self) -> Result<Option<DataChunk>> {
        if !self.is_open {
            return Err(DbError::Custom("PhysicalColumnScan not open".into()));
        }

        while self.current_rg_idx < self.row_groups.len() {
            let rg = &self.row_groups[self.current_rg_idx];
            self.current_rg_idx += 1;

            // 1. ZoneMap block skipping: if any predicate cannot match, skip entire RowGroup!
            let can_match_all = self.predicates.iter().all(|pred| pred.can_match(rg));
            if !can_match_all {
                continue;
            }

            // 2. Projection pushdown: only decompress requested columns
            let full_chunk = rg.decompress_to_chunk()?;
            if self.projected_columns.is_empty() {
                return Ok(Some(full_chunk));
            }

            let projected_types: Vec<PhysicalType> = self
                .projected_columns
                .iter()
                .filter_map(|&col_idx| full_chunk.column(col_idx).map(ColumnVector::physical_type))
                .collect();

            let mut out_chunk = DataChunk::new(&projected_types, full_chunk.capacity());
            for (out_idx, &src_idx) in self.projected_columns.iter().enumerate() {
                let src_col = full_chunk
                    .column(src_idx)
                    .ok_or_else(|| DbError::Custom(format!("Column idx {src_idx} out of range")))?;
                let dst_col = out_chunk
                    .column_mut(out_idx)
                    .ok_or_else(|| DbError::Custom("Output column missing".into()))?;
                *dst_col = src_col.clone();
            }
            out_chunk.set_num_rows(full_chunk.num_rows())?;
            return Ok(Some(out_chunk));
        }

        Ok(None)
    }

    fn close(&mut self) -> Result<()> {
        self.is_open = false;
        Ok(())
    }
}
