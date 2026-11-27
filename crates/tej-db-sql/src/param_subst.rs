use crate::plan::{Plan, ProjectionItem};
use crate::value::Value;
use sqlparser::ast::{
    Array, Expr as SqlExpr, FunctionArg, FunctionArgExpr, FunctionArguments, Value as SqlValue,
};
use tej_db_core::{DbError, Result};

/// Maximum recursion depth allowed during SQL parameter substitution.
const MAX_SUBST_DEPTH: usize = 64;

/// Substitute parameters into a planned query execution plan before execution.
pub fn substitute_plan_params(plan: &mut Plan, params: &[Value]) -> Result<()> {
    let mut counter = 0;
    match plan {
        Plan::Insert { rows, .. } => {
            for row in rows {
                for expr in row {
                    substitute_expr(expr, params, &mut counter, 0)?;
                }
            }
        }
        Plan::Select {
            projection,
            filter,
            order_by,
            ..
        } => {
            for item in projection {
                if let ProjectionItem::Expr { expr, .. } = item {
                    substitute_expr(expr, params, &mut counter, 0)?;
                }
            }
            if let Some(f) = filter {
                substitute_expr(f, params, &mut counter, 0)?;
            }
            for ob in order_by {
                substitute_expr(&mut ob.expr, params, &mut counter, 0)?;
            }
        }
        Plan::Update {
            assignments,
            filter,
            ..
        } => {
            for (_col, expr) in assignments {
                substitute_expr(expr, params, &mut counter, 0)?;
            }
            if let Some(f) = filter {
                substitute_expr(f, params, &mut counter, 0)?;
            }
        }
        Plan::Delete { filter, .. } => {
            if let Some(f) = filter {
                substitute_expr(f, params, &mut counter, 0)?;
            }
        }
        Plan::CreateTable { .. }
        | Plan::Savepoint { .. }
        | Plan::RollbackToSavepoint { .. }
        | Plan::ReleaseSavepoint { .. } => {}
    }
    Ok(())
}

fn placeholder_index(name: &str, counter: &mut usize) -> usize {
    if name.is_empty() || name == "?" {
        let idx = *counter;
        *counter += 1;
        return idx;
    }

    let num_slice = if let Some(stripped) = name.strip_prefix('$') {
        stripped
    } else if let Some(stripped) = name.strip_prefix('?') {
        stripped
    } else if let Some(stripped) = name.strip_prefix(':') {
        stripped
    } else {
        name
    };

    if let Ok(num) = num_slice.parse::<usize>() {
        if num >= 1 {
            return num - 1;
        }
        return num;
    }

    let idx = *counter;
    *counter += 1;
    idx
}

fn value_to_sql_expr(val: &Value) -> SqlExpr {
    match val {
        Value::Boolean(b) => SqlExpr::Value(SqlValue::Boolean(*b)),
        Value::Integer(i) => SqlExpr::Value(SqlValue::Number(i.to_string(), false)),
        Value::Real(r) => SqlExpr::Value(SqlValue::Number(r.to_string(), false)),
        Value::Text(s) => SqlExpr::Value(SqlValue::SingleQuotedString(s.clone())),
        Value::Blob(b) => {
            let mut hex_str = String::with_capacity(b.len() * 2);
            for byte in b {
                use std::fmt::Write;
                let _ = write!(&mut hex_str, "{byte:02x}");
            }
            SqlExpr::Value(SqlValue::HexStringLiteral(hex_str))
        }
        Value::Timestamp(t) => SqlExpr::Value(SqlValue::SingleQuotedString(t.to_string())),
        Value::Uuid(u) => SqlExpr::Value(SqlValue::SingleQuotedString(u.to_string())),
        Value::Decimal(d) => SqlExpr::Value(SqlValue::Number(d.to_string(), false)),
        Value::Interval(it) => SqlExpr::Value(SqlValue::SingleQuotedString(it.to_string())),
        Value::Json(j) => SqlExpr::Value(SqlValue::SingleQuotedString(j.to_string())),
        Value::Vector(v) => {
            let exprs = v
                .iter()
                .map(|f| SqlExpr::Value(SqlValue::Number(f.to_string(), false)))
                .collect();
            SqlExpr::Array(Array {
                elem: exprs,
                named: false,
            })
        }
        Value::List(list) => {
            let exprs = list.iter().map(value_to_sql_expr).collect();
            SqlExpr::Array(Array {
                elem: exprs,
                named: false,
            })
        }
        _ => SqlExpr::Value(SqlValue::Null),
    }
}

pub fn substitute_expr(
    expr: &mut SqlExpr,
    params: &[Value],
    counter: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_SUBST_DEPTH {
        return Err(DbError::Custom(
            "SQL expression recursion depth limit exceeded (max 64)".into(),
        ));
    }

    match expr {
        SqlExpr::Value(SqlValue::Placeholder(name)) => {
            let idx = placeholder_index(name, counter);
            let val = params.get(idx).ok_or_else(|| {
                DbError::Custom(format!(
                    "Missing parameter at index {idx} (provided {} params)",
                    params.len()
                ))
            })?;
            *expr = value_to_sql_expr(val);
        }
        SqlExpr::UnaryOp { expr: inner, .. }
        | SqlExpr::Nested(inner)
        | SqlExpr::IsNull(inner)
        | SqlExpr::IsNotNull(inner) => {
            substitute_expr(inner, params, counter, depth + 1)?;
        }
        SqlExpr::BinaryOp { left, right, .. } => {
            substitute_expr(left, params, counter, depth + 1)?;
            substitute_expr(right, params, counter, depth + 1)?;
        }
        SqlExpr::InList {
            expr: inner, list, ..
        } => {
            substitute_expr(inner, params, counter, depth + 1)?;
            for item in list {
                substitute_expr(item, params, counter, depth + 1)?;
            }
        }
        SqlExpr::Between {
            expr: inner,
            low,
            high,
            ..
        } => {
            substitute_expr(inner, params, counter, depth + 1)?;
            substitute_expr(low, params, counter, depth + 1)?;
            substitute_expr(high, params, counter, depth + 1)?;
        }
        SqlExpr::Case {
            operand,
            conditions,
            results,
            else_result,
        } => {
            if let Some(op) = operand {
                substitute_expr(op, params, counter, depth + 1)?;
            }
            for cond in conditions {
                substitute_expr(cond, params, counter, depth + 1)?;
            }
            for res in results {
                substitute_expr(res, params, counter, depth + 1)?;
            }
            if let Some(el) = else_result {
                substitute_expr(el, params, counter, depth + 1)?;
            }
        }
        SqlExpr::Function(func) => {
            if let FunctionArguments::List(arg_list) = &mut func.args {
                for arg in &mut arg_list.args {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) = arg {
                        substitute_expr(e, params, counter, depth + 1)?;
                    }
                }
            }
        }
        SqlExpr::Array(arr) => {
            for elem in &mut arr.elem {
                substitute_expr(elem, params, counter, depth + 1)?;
            }
        }
        _ => {}
    }

    Ok(())
}
