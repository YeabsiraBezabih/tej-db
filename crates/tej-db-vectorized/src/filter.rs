use crate::operator::{PhysicalOperator, SelectionVector};
use tej_db_columnar::{ColumnVector, DataChunk};
use tej_db_core::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOp {
    Eq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterExpr {
    CompareInt64 {
        column_idx: usize,
        op: ComparisonOp,
        val: i64,
    },
    CompareFloat64 {
        column_idx: usize,
        op: ComparisonOp,
        val: f64,
    },
    CompareString {
        column_idx: usize,
        op: ComparisonOp,
        val: String,
    },
    IsNull {
        column_idx: usize,
    },
    IsNotNull {
        column_idx: usize,
    },
}

impl FilterExpr {
    /// Evaluates this filter over a chunk, taking an optional active selection vector and updating matching indices.
    pub fn filter_vector(
        &self,
        chunk: &DataChunk,
        active_sel: Option<&[usize]>,
        out: &mut Vec<usize>,
    ) {
        out.clear();
        match self {
            Self::CompareInt64 {
                column_idx,
                op,
                val,
            } => {
                if let Some(col) = chunk.column(*column_idx) {
                    Self::filter_i64(col, *op, *val, active_sel, out);
                }
            }
            Self::CompareFloat64 {
                column_idx,
                op,
                val,
            } => {
                if let Some(col) = chunk.column(*column_idx) {
                    Self::filter_f64(col, *op, *val, active_sel, out);
                }
            }
            Self::CompareString {
                column_idx,
                op,
                val,
            } => {
                if let Some(col) = chunk.column(*column_idx) {
                    Self::filter_str(col, *op, val, active_sel, out);
                }
            }
            Self::IsNull { column_idx } => {
                if let Some(col) = chunk.column(*column_idx) {
                    if let Some(sel) = active_sel {
                        for &idx in sel {
                            if col.is_null(idx) {
                                out.push(idx);
                            }
                        }
                    } else {
                        for idx in 0..col.len() {
                            if col.is_null(idx) {
                                out.push(idx);
                            }
                        }
                    }
                }
            }
            Self::IsNotNull { column_idx } => {
                if let Some(col) = chunk.column(*column_idx) {
                    if let Some(sel) = active_sel {
                        for &idx in sel {
                            if !col.is_null(idx) {
                                out.push(idx);
                            }
                        }
                    } else {
                        for idx in 0..col.len() {
                            if !col.is_null(idx) {
                                out.push(idx);
                            }
                        }
                    }
                }
            }
        }
    }

    #[inline]
    fn filter_i64(
        col: &ColumnVector,
        op: ComparisonOp,
        target: i64,
        active_sel: Option<&[usize]>,
        out: &mut Vec<usize>,
    ) {
        let slice = col.as_i64_slice();
        let nulls = col.null_bitmap();
        let has_nulls = !nulls.is_empty();

        if let Some(sel) = active_sel {
            for &idx in sel {
                if (!has_nulls || !col.is_null(idx)) && idx < slice.len() {
                    let actual = slice[idx];
                    let pass = match op {
                        ComparisonOp::Eq => actual == target,
                        ComparisonOp::NotEq => actual != target,
                        ComparisonOp::Lt => actual < target,
                        ComparisonOp::Lte => actual <= target,
                        ComparisonOp::Gt => actual > target,
                        ComparisonOp::Gte => actual >= target,
                    };
                    if pass {
                        out.push(idx);
                    }
                }
            }
        } else if !has_nulls {
            // Tight unrolled SIMD comparison across contiguous slice
            match op {
                ComparisonOp::Eq => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v == target {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::NotEq => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v != target {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::Lt => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v < target {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::Lte => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v <= target {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::Gt => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v > target {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::Gte => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v >= target {
                            out.push(i);
                        }
                    }
                }
            }
        } else {
            for (i, &v) in slice.iter().enumerate() {
                if !col.is_null(i) {
                    let pass = match op {
                        ComparisonOp::Eq => v == target,
                        ComparisonOp::NotEq => v != target,
                        ComparisonOp::Lt => v < target,
                        ComparisonOp::Lte => v <= target,
                        ComparisonOp::Gt => v > target,
                        ComparisonOp::Gte => v >= target,
                    };
                    if pass {
                        out.push(i);
                    }
                }
            }
        }
    }

    #[inline]
    fn filter_f64(
        col: &ColumnVector,
        op: ComparisonOp,
        target: f64,
        active_sel: Option<&[usize]>,
        out: &mut Vec<usize>,
    ) {
        let slice = col.as_f64_slice();
        let nulls = col.null_bitmap();
        let has_nulls = !nulls.is_empty();

        if let Some(sel) = active_sel {
            for &idx in sel {
                if (!has_nulls || !col.is_null(idx)) && idx < slice.len() {
                    let actual = slice[idx];
                    let pass = match op {
                        ComparisonOp::Eq => (actual - target).abs() < f64::EPSILON,
                        ComparisonOp::NotEq => (actual - target).abs() >= f64::EPSILON,
                        ComparisonOp::Lt => actual < target,
                        ComparisonOp::Lte => actual <= target,
                        ComparisonOp::Gt => actual > target,
                        ComparisonOp::Gte => actual >= target,
                    };
                    if pass {
                        out.push(idx);
                    }
                }
            }
        } else if !has_nulls {
            match op {
                ComparisonOp::Eq => {
                    for (i, &v) in slice.iter().enumerate() {
                        if (v - target).abs() < f64::EPSILON {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::NotEq => {
                    for (i, &v) in slice.iter().enumerate() {
                        if (v - target).abs() >= f64::EPSILON {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::Lt => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v < target {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::Lte => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v <= target {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::Gt => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v > target {
                            out.push(i);
                        }
                    }
                }
                ComparisonOp::Gte => {
                    for (i, &v) in slice.iter().enumerate() {
                        if v >= target {
                            out.push(i);
                        }
                    }
                }
            }
        } else {
            for (i, &v) in slice.iter().enumerate() {
                if !col.is_null(i) {
                    let pass = match op {
                        ComparisonOp::Eq => (v - target).abs() < f64::EPSILON,
                        ComparisonOp::NotEq => (v - target).abs() >= f64::EPSILON,
                        ComparisonOp::Lt => v < target,
                        ComparisonOp::Lte => v <= target,
                        ComparisonOp::Gt => v > target,
                        ComparisonOp::Gte => v >= target,
                    };
                    if pass {
                        out.push(i);
                    }
                }
            }
        }
    }

    #[inline]
    fn filter_str(
        col: &ColumnVector,
        op: ComparisonOp,
        target: &str,
        active_sel: Option<&[usize]>,
        out: &mut Vec<usize>,
    ) {
        if let Some(sel) = active_sel {
            for &idx in sel {
                if let Some(s) = col.get_str(idx) {
                    let pass = match op {
                        ComparisonOp::Eq => s == target,
                        ComparisonOp::NotEq => s != target,
                        ComparisonOp::Lt => s < target,
                        ComparisonOp::Lte => s <= target,
                        ComparisonOp::Gt => s > target,
                        ComparisonOp::Gte => s >= target,
                    };
                    if pass {
                        out.push(idx);
                    }
                }
            }
        } else {
            for idx in 0..col.len() {
                if let Some(s) = col.get_str(idx) {
                    let pass = match op {
                        ComparisonOp::Eq => s == target,
                        ComparisonOp::NotEq => s != target,
                        ComparisonOp::Lt => s < target,
                        ComparisonOp::Lte => s <= target,
                        ComparisonOp::Gt => s > target,
                        ComparisonOp::Gte => s >= target,
                    };
                    if pass {
                        out.push(idx);
                    }
                }
            }
        }
    }

    #[must_use]
    pub fn matches(&self, chunk: &DataChunk, row_idx: usize) -> bool {
        match self {
            Self::CompareInt64 {
                column_idx,
                op,
                val,
            } => {
                if let Some(col) = chunk.column(*column_idx) {
                    if let Some(actual) = col.get_i64(row_idx) {
                        return match op {
                            ComparisonOp::Eq => actual == *val,
                            ComparisonOp::NotEq => actual != *val,
                            ComparisonOp::Lt => actual < *val,
                            ComparisonOp::Lte => actual <= *val,
                            ComparisonOp::Gt => actual > *val,
                            ComparisonOp::Gte => actual >= *val,
                        };
                    }
                }
                false
            }
            Self::CompareFloat64 {
                column_idx,
                op,
                val,
            } => {
                if let Some(col) = chunk.column(*column_idx) {
                    if let Some(actual) = col.get_f64(row_idx) {
                        return match op {
                            ComparisonOp::Eq => (actual - *val).abs() < f64::EPSILON,
                            ComparisonOp::NotEq => (actual - *val).abs() >= f64::EPSILON,
                            ComparisonOp::Lt => actual < *val,
                            ComparisonOp::Lte => actual <= *val,
                            ComparisonOp::Gt => actual > *val,
                            ComparisonOp::Gte => actual >= *val,
                        };
                    }
                }
                false
            }
            Self::CompareString {
                column_idx,
                op,
                val,
            } => {
                if let Some(col) = chunk.column(*column_idx) {
                    if let Some(actual) = col.get_str(row_idx) {
                        return match op {
                            ComparisonOp::Eq => actual == val.as_str(),
                            ComparisonOp::NotEq => actual != val.as_str(),
                            ComparisonOp::Lt => actual < val.as_str(),
                            ComparisonOp::Lte => actual <= val.as_str(),
                            ComparisonOp::Gt => actual > val.as_str(),
                            ComparisonOp::Gte => actual >= val.as_str(),
                        };
                    }
                }
                false
            }
            Self::IsNull { column_idx } => chunk
                .column(*column_idx)
                .is_some_and(|c| c.is_null(row_idx)),
            Self::IsNotNull { column_idx } => chunk
                .column(*column_idx)
                .is_some_and(|c| !c.is_null(row_idx)),
        }
    }
}

/// Vectorized physical filter evaluating expressions to produce filtered DataChunks.
#[derive(Debug)]
pub struct PhysicalFilter {
    child: Box<dyn PhysicalOperator>,
    predicates: Vec<FilterExpr>,
    scratch_a: Vec<usize>,
    scratch_b: Vec<usize>,
}

impl PhysicalFilter {
    #[must_use]
    pub fn new(child: Box<dyn PhysicalOperator>, predicates: Vec<FilterExpr>) -> Self {
        Self {
            child,
            predicates,
            scratch_a: Vec::new(),
            scratch_b: Vec::new(),
        }
    }
}

impl PhysicalOperator for PhysicalFilter {
    fn open(&mut self) -> Result<()> {
        self.child.open()
    }

    fn next(&mut self) -> Result<Option<DataChunk>> {
        while let Some(chunk) = self.child.next()? {
            let total_rows = chunk.num_rows();
            if total_rows == 0 {
                continue;
            }

            if self.predicates.is_empty() {
                return Ok(Some(chunk));
            }

            // Vectorized cascade across predicates
            self.predicates[0].filter_vector(&chunk, None, &mut self.scratch_a);
            if self.scratch_a.is_empty() {
                continue;
            }

            let mut use_b = true;
            for pred in &self.predicates[1..] {
                if use_b {
                    pred.filter_vector(&chunk, Some(&self.scratch_a), &mut self.scratch_b);
                    if self.scratch_b.is_empty() {
                        break;
                    }
                    use_b = false;
                } else {
                    pred.filter_vector(&chunk, Some(&self.scratch_b), &mut self.scratch_a);
                    if self.scratch_a.is_empty() {
                        break;
                    }
                    use_b = true;
                }
            }

            let final_indices = if use_b {
                &self.scratch_a
            } else {
                &self.scratch_b
            };

            if final_indices.is_empty() {
                continue;
            }

            // Zero-copy fast-path: if all rows passed all filters, return chunk directly!
            if final_indices.len() == total_rows {
                return Ok(Some(chunk));
            }

            let sel = SelectionVector::new(final_indices.clone());
            return sel.apply_to_chunk(&chunk).map(Some);
        }

        Ok(None)
    }

    fn close(&mut self) -> Result<()> {
        self.child.close()
    }
}
