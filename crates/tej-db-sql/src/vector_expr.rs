use crate::expr::evaluate_expr;
use crate::schema::TableSchema;
use crate::value::Value;
use sqlparser::ast::{Array, Expr as SqlExpr, FunctionArg, FunctionArgExpr, Value as SqlValue};
use tej_db_core::{DbError, Result};
use tej_db_vector::simd::{cosine_distance, l2_distance_squared};

/// Evaluates built-in vector distance and utility functions in SQL expressions.
pub fn evaluate_vector_function(
    func_name: &str,
    args: &[FunctionArg],
    schema: &TableSchema,
    row: &[Value],
) -> Result<Value> {
    match func_name.to_ascii_lowercase().as_str() {
        "cosine_distance" => {
            let (v1, v2) = extract_two_vectors(args, schema, row, "cosine_distance")?;
            if v1.len() != v2.len() {
                return Err(DbError::Custom(format!(
                    "Vector dimension mismatch in cosine_distance: {} vs {}",
                    v1.len(),
                    v2.len()
                )));
            }
            let dist = cosine_distance(&v1, &v2);
            Ok(Value::Real(f64::from(dist)))
        }
        "l2_distance" => {
            let (v1, v2) = extract_two_vectors(args, schema, row, "l2_distance")?;
            if v1.len() != v2.len() {
                return Err(DbError::Custom(format!(
                    "Vector dimension mismatch in l2_distance: {} vs {}",
                    v1.len(),
                    v2.len()
                )));
            }
            let dist_sq = l2_distance_squared(&v1, &v2);
            Ok(Value::Real(f64::from(dist_sq.sqrt())))
        }
        "vector_dims" => {
            if args.len() != 1 {
                return Err(DbError::Custom(
                    "vector_dims expects exactly 1 argument".into(),
                ));
            }
            let v = extract_arg_vector(&args[0], schema, row)?;
            #[allow(clippy::cast_possible_wrap)]
            Ok(Value::Integer(v.len() as i64))
        }
        _ => Err(DbError::Custom(format!("Unknown function '{func_name}'"))),
    }
}

/// Attempts to parse an AST expression into a concrete `Vec<f32>` vector.
pub fn parse_vector_expr(expr: &SqlExpr, schema: &TableSchema, row: &[Value]) -> Result<Vec<f32>> {
    match expr {
        SqlExpr::Array(Array { elem, .. }) => {
            let mut vec = Vec::with_capacity(elem.len());
            for e in elem {
                let val = evaluate_expr(e, schema, row)?;
                match val {
                    Value::Real(r) => {
                        #[allow(clippy::cast_possible_truncation)]
                        vec.push(r as f32);
                    }
                    Value::Integer(i) => {
                        #[allow(clippy::cast_precision_loss)]
                        vec.push(i as f32);
                    }
                    _ => {
                        return Err(DbError::Custom(
                            "Vector array elements must be numeric".into(),
                        ));
                    }
                }
            }
            Ok(vec)
        }
        SqlExpr::Value(SqlValue::SingleQuotedString(s) | SqlValue::DoubleQuotedString(s)) => {
            parse_vector_from_str(s)
        }
        other => {
            let val = evaluate_expr(other, schema, row)?;
            match val {
                Value::Vector(v) => Ok(v),
                Value::Text(s) => parse_vector_from_str(&s),
                _ => Err(DbError::Custom(format!(
                    "Cannot convert expression '{other:?}' to vector"
                ))),
            }
        }
    }
}

/// Parses string representation like `[1.0, 2.0, 3.5]` or `1.0, 2.0, 3.5` into `Vec<f32>`.
pub fn parse_vector_from_str(s: &str) -> Result<Vec<f32>> {
    let trimmed = s.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    trimmed
        .split(',')
        .map(|token| {
            token
                .trim()
                .parse::<f32>()
                .map_err(|e| DbError::Custom(format!("Invalid vector element '{token}': {e}")))
        })
        .collect()
}

fn extract_two_vectors(
    args: &[FunctionArg],
    schema: &TableSchema,
    row: &[Value],
    fn_name: &str,
) -> Result<(Vec<f32>, Vec<f32>)> {
    if args.len() != 2 {
        return Err(DbError::Custom(format!(
            "{fn_name} expects exactly 2 arguments, got {}",
            args.len()
        )));
    }
    let v1 = extract_arg_vector(&args[0], schema, row)?;
    let v2 = extract_arg_vector(&args[1], schema, row)?;
    Ok((v1, v2))
}

fn extract_arg_vector(arg: &FunctionArg, schema: &TableSchema, row: &[Value]) -> Result<Vec<f32>> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => parse_vector_expr(expr, schema, row),
        _ => Err(DbError::Custom(
            "Named or wildcard function arguments are not supported for vector functions".into(),
        )),
    }
}
