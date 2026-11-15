#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless
)]

use crate::value::Value;
use std::cmp::Ordering;
use std::collections::HashMap;
use tej_db_core::{DbError, Result};

/// Window function types supported by the SQL engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFunctionType {
    RowNumber,
    Rank,
    DenseRank,
    RunningSum,
    RunningAvg,
}

/// Specification for a window frame clause `OVER (PARTITION BY ... ORDER BY ...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSpec {
    pub function: WindowFunctionType,
    pub target_col_idx: Option<usize>,
    pub partition_by_indices: Vec<usize>,
    pub order_by_col_idx: Option<usize>,
    pub ascending: bool,
}

/// Evaluator for window functions across an array of row values.
#[derive(Debug, Clone)]
pub struct WindowEvaluator;

impl WindowEvaluator {
    /// Evaluates a window function over a set of materialized rows, producing the computed window values.
    pub fn evaluate(spec: &WindowSpec, rows: &[Vec<Value>]) -> Result<Vec<Value>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        // Fast-path: single integer partition key + integer order key (common in OLAP / TPC-H)
        if spec.partition_by_indices.len() == 1 {
            let part_col = spec.partition_by_indices[0];
            let is_int_partition = rows[0]
                .get(part_col)
                .is_some_and(|v| matches!(v, Value::Integer(_)));
            let is_int_order = spec.order_by_col_idx.is_none_or(|oc| {
                rows[0]
                    .get(oc)
                    .is_some_and(|v| matches!(v, Value::Integer(_)))
            });

            if is_int_partition && is_int_order {
                return Ok(Self::evaluate_int_fast_path(spec, rows, part_col));
            }
        }

        // 1. Group rows by partition key (using binary key encoding)
        let mut partitions: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        for (idx, row) in rows.iter().enumerate() {
            let mut p_key = Vec::new();
            for &col_idx in &spec.partition_by_indices {
                if let Some(v) = row.get(col_idx) {
                    p_key.extend_from_slice(&v.encode_key());
                }
            }
            partitions.entry(p_key).or_default().push(idx);
        }

        let mut output = vec![Value::Null; rows.len()];

        // 2. Evaluate within each partition
        for (_, mut row_indices) in partitions {
            // Sort rows within partition if order_by is specified
            if let Some(order_col) = spec.order_by_col_idx {
                row_indices.sort_by(|&a, &b| {
                    let val_a = rows[a].get(order_col);
                    let val_b = rows[b].get(order_col);
                    let ord = match (val_a, val_b) {
                        (Some(va), Some(vb)) => va.partial_cmp(vb).unwrap_or(Ordering::Equal),
                        _ => Ordering::Equal,
                    };
                    if spec.ascending { ord } else { ord.reverse() }
                });
            }

            match spec.function {
                WindowFunctionType::RowNumber => {
                    for (rank, &row_idx) in row_indices.iter().enumerate() {
                        output[row_idx] = Value::Integer((rank + 1) as i64);
                    }
                }
                WindowFunctionType::Rank => {
                    let mut current_rank = 1;
                    let mut prev_val: Option<&Value> = None;
                    for (idx, &row_idx) in row_indices.iter().enumerate() {
                        let cur_val = spec.order_by_col_idx.and_then(|c| rows[row_idx].get(c));
                        if let (Some(prev), Some(cur)) = (prev_val, cur_val) {
                            if prev != cur {
                                current_rank = idx + 1;
                            }
                        }
                        prev_val = cur_val;
                        output[row_idx] = Value::Integer(current_rank as i64);
                    }
                }
                WindowFunctionType::DenseRank => {
                    let mut current_rank = 1;
                    let mut prev_val: Option<&Value> = None;
                    for &row_idx in &row_indices {
                        let cur_val = spec.order_by_col_idx.and_then(|c| rows[row_idx].get(c));
                        if let (Some(prev), Some(cur)) = (prev_val, cur_val) {
                            if prev != cur {
                                current_rank += 1;
                            }
                        }
                        prev_val = cur_val;
                        output[row_idx] = Value::Integer(current_rank as i64);
                    }
                }
                WindowFunctionType::RunningSum => {
                    let target_col = spec.target_col_idx.ok_or_else(|| {
                        DbError::Custom("RunningSum requires target column".into())
                    })?;
                    let mut cum_sum = 0_i64;
                    for &row_idx in &row_indices {
                        if let Some(Value::Integer(v)) = rows[row_idx].get(target_col) {
                            cum_sum += *v;
                        }
                        output[row_idx] = Value::Integer(cum_sum);
                    }
                }
                WindowFunctionType::RunningAvg => {
                    let target_col = spec.target_col_idx.ok_or_else(|| {
                        DbError::Custom("RunningAvg requires target column".into())
                    })?;
                    let mut cum_sum = 0.0;
                    let mut count = 0_u64;
                    for &row_idx in &row_indices {
                        match rows[row_idx].get(target_col) {
                            Some(Value::Integer(v)) => {
                                cum_sum += *v as f64;
                                count += 1;
                            }
                            Some(Value::Real(v)) => {
                                cum_sum += *v;
                                count += 1;
                            }
                            _ => {}
                        }
                        #[allow(clippy::cast_precision_loss)]
                        let avg = if count > 0 {
                            cum_sum / (count as f64)
                        } else {
                            0.0
                        };
                        output[row_idx] = Value::Real(avg);
                    }
                }
            }
        }

        Ok(output)
    }

    fn evaluate_int_fast_path(
        spec: &WindowSpec,
        rows: &[Vec<Value>],
        part_col: usize,
    ) -> Vec<Value> {
        let order_col = spec.order_by_col_idx;

        // Check if partition keys are dense in 0..256
        let mut min_p = i64::MAX;
        let mut max_p = i64::MIN;
        for row in rows {
            if let Some(Value::Integer(k)) = row.get(part_col) {
                min_p = min_p.min(*k);
                max_p = max_p.max(*k);
            }
        }

        let is_dense = min_p >= 0 && max_p < 256;
        let partitions: Vec<Vec<(i64, usize)>> = if is_dense {
            #[allow(clippy::cast_sign_loss)]
            let num_parts = (max_p + 1) as usize;
            let mut parts = vec![Vec::new(); num_parts];
            for (idx, row) in rows.iter().enumerate() {
                #[allow(clippy::cast_sign_loss)]
                let p_key = match row.get(part_col) {
                    Some(Value::Integer(k)) => *k as usize,
                    _ => 0,
                };
                let o_key = match order_col.and_then(|c| row.get(c)) {
                    Some(Value::Integer(o)) => *o,
                    _ => 0,
                };
                parts[p_key].push((o_key, idx));
            }
            parts.into_iter().filter(|p| !p.is_empty()).collect()
        } else {
            let mut map: HashMap<i64, Vec<(i64, usize)>> = HashMap::new();
            for (idx, row) in rows.iter().enumerate() {
                let p_key = match row.get(part_col) {
                    Some(Value::Integer(k)) => *k,
                    _ => 0,
                };
                let o_key = match order_col.and_then(|c| row.get(c)) {
                    Some(Value::Integer(o)) => *o,
                    _ => 0,
                };
                map.entry(p_key).or_default().push((o_key, idx));
            }
            map.into_values().collect()
        };

        let mut output = vec![Value::Null; rows.len()];

        for mut items in partitions {
            if order_col.is_some() {
                if spec.ascending {
                    items.sort_unstable_by_key(|&(o, _)| o);
                } else {
                    items.sort_unstable_by_key(|a| std::cmp::Reverse(a.0));
                }
            }

            match spec.function {
                WindowFunctionType::RowNumber => {
                    for (rank, &(_, row_idx)) in items.iter().enumerate() {
                        #[allow(clippy::cast_possible_wrap)]
                        {
                            output[row_idx] = Value::Integer((rank + 1) as i64);
                        }
                    }
                }
                WindowFunctionType::Rank => {
                    let mut current_rank = 1;
                    let mut prev_o: Option<i64> = None;
                    for (idx, &(o_val, row_idx)) in items.iter().enumerate() {
                        if let Some(prev) = prev_o {
                            if prev != o_val {
                                current_rank = idx + 1;
                            }
                        }
                        prev_o = Some(o_val);
                        #[allow(clippy::cast_possible_wrap)]
                        {
                            output[row_idx] = Value::Integer(current_rank as i64);
                        }
                    }
                }
                WindowFunctionType::DenseRank => {
                    let mut current_rank = 1;
                    let mut prev_o: Option<i64> = None;
                    for &(o_val, row_idx) in &items {
                        if let Some(prev) = prev_o {
                            if prev != o_val {
                                current_rank += 1;
                            }
                        }
                        prev_o = Some(o_val);
                        #[allow(clippy::cast_possible_wrap)]
                        {
                            output[row_idx] = Value::Integer(current_rank as i64);
                        }
                    }
                }
                WindowFunctionType::RunningSum => {
                    let target_col = spec.target_col_idx.unwrap_or(0);
                    let mut cum_sum = 0_i64;
                    for &(_, row_idx) in &items {
                        if let Some(Value::Integer(v)) = rows[row_idx].get(target_col) {
                            cum_sum += *v;
                        }
                        output[row_idx] = Value::Integer(cum_sum);
                    }
                }
                WindowFunctionType::RunningAvg => {
                    let target_col = spec.target_col_idx.unwrap_or(0);
                    let mut cum_sum = 0.0;
                    let mut count = 0_u64;
                    for &(_, row_idx) in &items {
                        match rows[row_idx].get(target_col) {
                            Some(Value::Integer(v)) => {
                                cum_sum += *v as f64;
                                count += 1;
                            }
                            Some(Value::Real(v)) => {
                                cum_sum += *v;
                                count += 1;
                            }
                            _ => {}
                        }
                        #[allow(clippy::cast_precision_loss)]
                        let avg = if count > 0 {
                            cum_sum / (count as f64)
                        } else {
                            0.0
                        };
                        output[row_idx] = Value::Real(avg);
                    }
                }
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_row_number_window() {
        let rows = vec![
            vec![Value::Text("A".into()), Value::Integer(10)],
            vec![Value::Text("A".into()), Value::Integer(20)],
            vec![Value::Text("B".into()), Value::Integer(15)],
        ];

        let spec = WindowSpec {
            function: WindowFunctionType::RowNumber,
            target_col_idx: None,
            partition_by_indices: vec![0],
            order_by_col_idx: Some(1),
            ascending: true,
        };

        let result = WindowEvaluator::evaluate(&spec, &rows).unwrap();
        assert_eq!(result[0], Value::Integer(1));
        assert_eq!(result[1], Value::Integer(2));
        assert_eq!(result[2], Value::Integer(1));
    }

    #[test]
    fn test_running_sum_window() {
        let rows = vec![
            vec![Value::Integer(100)],
            vec![Value::Integer(200)],
            vec![Value::Integer(300)],
        ];

        let spec = WindowSpec {
            function: WindowFunctionType::RunningSum,
            target_col_idx: Some(0),
            partition_by_indices: Vec::new(),
            order_by_col_idx: None,
            ascending: true,
        };

        let result = WindowEvaluator::evaluate(&spec, &rows).unwrap();
        assert_eq!(result[0], Value::Integer(100));
        assert_eq!(result[1], Value::Integer(300));
        assert_eq!(result[2], Value::Integer(600));
    }
}
