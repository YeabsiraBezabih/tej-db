use crate::value::Value;
use sqlparser::ast::{BinaryOperator, Expr as SqlExpr, UnaryOperator, Value as SqlValue};
use std::ops::Bound;

pub(crate) fn parse_sql_value(val: &SqlValue) -> Option<Value> {
    match val {
        SqlValue::Number(n, _) => {
            if let Ok(i) = n.parse::<i64>() {
                Some(Value::Integer(i))
            } else if let Ok(f) = n.parse::<f64>() {
                Some(Value::Real(f))
            } else {
                None
            }
        }
        SqlValue::SingleQuotedString(s) | SqlValue::DoubleQuotedString(s) => {
            Some(Value::Text(s.clone()))
        }
        SqlValue::Boolean(b) => Some(Value::Boolean(*b)),
        SqlValue::Null => Some(Value::Null),
        _ => None,
    }
}

pub(crate) fn extract_literal_value(expr: &SqlExpr) -> Option<Value> {
    match expr {
        SqlExpr::Value(v) => parse_sql_value(v),
        SqlExpr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: inner,
        } => match inner.as_ref() {
            SqlExpr::Value(SqlValue::Number(n, _)) => {
                if let Ok(i) = n.parse::<i64>() {
                    Some(Value::Integer(-i))
                } else if let Ok(f) = n.parse::<f64>() {
                    Some(Value::Real(-f))
                } else {
                    None
                }
            }
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn extract_pk_equality(expr: &SqlExpr, pk_col: &str) -> Option<Value> {
    if let SqlExpr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
    } = expr
    {
        if let SqlExpr::Identifier(id) = left.as_ref() {
            if id.value.eq_ignore_ascii_case(pk_col) {
                if let Some(parsed) = extract_literal_value(right) {
                    if !parsed.is_null() {
                        return Some(parsed);
                    }
                }
                return None;
            }
        }
        if let SqlExpr::Identifier(id) = right.as_ref() {
            if id.value.eq_ignore_ascii_case(pk_col) {
                if let Some(parsed) = extract_literal_value(left) {
                    if !parsed.is_null() {
                        return Some(parsed);
                    }
                }
                return None;
            }
        }
    }
    None
}

pub(crate) fn max_start_bound(b1: Bound<Vec<u8>>, b2: Bound<Vec<u8>>) -> Bound<Vec<u8>> {
    match (b1, b2) {
        (Bound::Unbounded, b) | (b, Bound::Unbounded) => b,
        (Bound::Included(k1), Bound::Included(k2)) => {
            if k1 >= k2 {
                Bound::Included(k1)
            } else {
                Bound::Included(k2)
            }
        }
        (Bound::Excluded(k1), Bound::Excluded(k2)) => {
            if k1 >= k2 {
                Bound::Excluded(k1)
            } else {
                Bound::Excluded(k2)
            }
        }
        (Bound::Included(k1), Bound::Excluded(k2)) => {
            if k1 > k2 {
                Bound::Included(k1)
            } else {
                Bound::Excluded(k2)
            }
        }
        (Bound::Excluded(k1), Bound::Included(k2)) => {
            if k1 >= k2 {
                Bound::Excluded(k1)
            } else {
                Bound::Included(k2)
            }
        }
    }
}

pub(crate) fn min_end_bound(b1: Bound<Vec<u8>>, b2: Bound<Vec<u8>>) -> Bound<Vec<u8>> {
    match (b1, b2) {
        (Bound::Unbounded, b) | (b, Bound::Unbounded) => b,
        (Bound::Included(k1), Bound::Included(k2)) => {
            if k1 <= k2 {
                Bound::Included(k1)
            } else {
                Bound::Included(k2)
            }
        }
        (Bound::Excluded(k1), Bound::Excluded(k2)) => {
            if k1 <= k2 {
                Bound::Excluded(k1)
            } else {
                Bound::Excluded(k2)
            }
        }
        (Bound::Included(k1), Bound::Excluded(k2)) => {
            if k1 < k2 {
                Bound::Included(k1)
            } else {
                Bound::Excluded(k2)
            }
        }
        (Bound::Excluded(k1), Bound::Included(k2)) => {
            if k1 <= k2 {
                Bound::Excluded(k1)
            } else {
                Bound::Included(k2)
            }
        }
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn extract_pk_range(
    expr: &SqlExpr,
    pk_col: &str,
) -> Option<(Bound<Vec<u8>>, Bound<Vec<u8>>, Option<SqlExpr>)> {
    if let SqlExpr::BinaryOp { left, op, right } = expr {
        if let SqlExpr::Identifier(id) = left.as_ref() {
            if id.value.eq_ignore_ascii_case(pk_col) {
                let val = extract_literal_value(right)?;
                if val.is_null() {
                    return Some((
                        Bound::Excluded(Vec::new()),
                        Bound::Included(Vec::new()),
                        None,
                    ));
                }
                let enc = val.encode_key();
                match op {
                    BinaryOperator::Gt => {
                        return Some((Bound::Excluded(enc), Bound::Unbounded, None));
                    }
                    BinaryOperator::GtEq => {
                        return Some((Bound::Included(enc), Bound::Unbounded, None));
                    }
                    BinaryOperator::Lt => {
                        return Some((Bound::Unbounded, Bound::Excluded(enc), None));
                    }
                    BinaryOperator::LtEq => {
                        return Some((Bound::Unbounded, Bound::Included(enc), None));
                    }
                    _ => {}
                }
            }
        }
        if let SqlExpr::Identifier(id) = right.as_ref() {
            if id.value.eq_ignore_ascii_case(pk_col) {
                let val = extract_literal_value(left)?;
                if val.is_null() {
                    return Some((
                        Bound::Excluded(Vec::new()),
                        Bound::Included(Vec::new()),
                        None,
                    ));
                }
                let enc = val.encode_key();
                match op {
                    BinaryOperator::Gt => {
                        return Some((Bound::Unbounded, Bound::Excluded(enc), None));
                    }
                    BinaryOperator::GtEq => {
                        return Some((Bound::Unbounded, Bound::Included(enc), None));
                    }
                    BinaryOperator::Lt => {
                        return Some((Bound::Excluded(enc), Bound::Unbounded, None));
                    }
                    BinaryOperator::LtEq => {
                        return Some((Bound::Included(enc), Bound::Unbounded, None));
                    }
                    _ => {}
                }
            }
        }
        if *op == BinaryOperator::And {
            let left_res = extract_pk_range(left, pk_col);
            let right_res = extract_pk_range(right, pk_col);

            match (left_res, right_res) {
                (Some((l_start, l_end, l_res)), Some((r_start, r_end, r_res))) => {
                    let combined_start = max_start_bound(l_start, r_start);
                    let combined_end = min_end_bound(l_end, r_end);
                    let residual = match (l_res, r_res) {
                        (Some(l), Some(r)) => Some(SqlExpr::BinaryOp {
                            left: Box::new(l),
                            op: BinaryOperator::And,
                            right: Box::new(r),
                        }),
                        (Some(l), None) => Some(l),
                        (None, Some(r)) => Some(r),
                        (None, None) => None,
                    };
                    return Some((combined_start, combined_end, residual));
                }
                (Some((start, end, res)), None) => {
                    let residual = match res {
                        Some(r) => Some(SqlExpr::BinaryOp {
                            left: Box::new(r),
                            op: BinaryOperator::And,
                            right: right.clone(),
                        }),
                        None => Some((**right).clone()),
                    };
                    return Some((start, end, residual));
                }
                (None, Some((start, end, res))) => {
                    let residual = match res {
                        Some(r) => Some(SqlExpr::BinaryOp {
                            left: left.clone(),
                            op: BinaryOperator::And,
                            right: Box::new(r),
                        }),
                        None => Some((**left).clone()),
                    };
                    return Some((start, end, residual));
                }
                (None, None) => {}
            }
        }
    }
    None
}
