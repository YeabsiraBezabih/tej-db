use crate::schema::TableSchema;
use crate::value::Value;
use sqlparser::ast::{
    BinaryOperator, Expr as SqlExpr, FunctionArg, UnaryOperator, Value as SqlValue,
};
use tej_db_core::{DbError, Result};

/// Maximum recursion depth allowed during SQL expression evaluation to prevent stack overflows.
const MAX_EXPR_DEPTH: usize = 64;

/// Evaluate a SQL AST expression against a candidate row.
pub fn evaluate_expr(expr: &SqlExpr, schema: &TableSchema, row: &[Value]) -> Result<Value> {
    evaluate_expr_inner(expr, schema, row, 0)
}

fn evaluate_expr_inner(
    expr: &SqlExpr,
    schema: &TableSchema,
    row: &[Value],
    depth: usize,
) -> Result<Value> {
    if depth > MAX_EXPR_DEPTH {
        return Err(DbError::Custom(
            "SQL expression recursion depth limit exceeded (max 64)".into(),
        ));
    }

    match expr {
        SqlExpr::Identifier(ident) => {
            let col_name = ident.value.as_str();
            let idx = schema.column_index(col_name).ok_or_else(|| {
                DbError::Custom(format!("Column '{col_name}' not found in table schema"))
            })?;
            Ok(row.get(idx).cloned().unwrap_or(Value::Null))
        }
        SqlExpr::Value(val) => match val {
            SqlValue::Number(num_str, _) => {
                if let Ok(i) = num_str.parse::<i64>() {
                    Ok(Value::Integer(i))
                } else if let Ok(f) = num_str.parse::<f64>() {
                    Ok(Value::Real(f))
                } else {
                    Err(DbError::Custom(format!(
                        "Invalid numeric literal: {num_str}"
                    )))
                }
            }
            SqlValue::SingleQuotedString(s) | SqlValue::DoubleQuotedString(s) => {
                Ok(Value::Text(s.clone()))
            }
            SqlValue::Boolean(b) => Ok(Value::Boolean(*b)),
            SqlValue::Null => Ok(Value::Null),
            _ => Err(DbError::Custom(format!(
                "Unsupported SQL value literal: {val:?}"
            ))),
        },
        SqlExpr::UnaryOp { op, expr } => {
            if matches!(op, UnaryOperator::Minus) {
                if let SqlExpr::Value(SqlValue::Number(num_str, _)) = expr.as_ref() {
                    let neg = format!("-{num_str}");
                    if let Ok(i) = neg.parse::<i64>() {
                        return Ok(Value::Integer(i));
                    } else if let Ok(f) = neg.parse::<f64>() {
                        return Ok(Value::Real(f));
                    }
                }
            }
            let inner = evaluate_expr_inner(expr, schema, row, depth + 1)?;
            match op {
                UnaryOperator::Not => match inner {
                    Value::Boolean(b) => Ok(Value::Boolean(!b)),
                    Value::Null => Ok(Value::Null),
                    _ => Err(DbError::Custom(
                        "NOT operator expects boolean operand".into(),
                    )),
                },
                UnaryOperator::Minus => match inner {
                    Value::Integer(i) => match i.checked_neg() {
                        Some(res) => Ok(Value::Integer(res)),
                        None => Err(DbError::Custom("Integer negation overflow".into())),
                    },
                    Value::Real(r) => Ok(Value::Real(-r)),
                    Value::Null => Ok(Value::Null),
                    _ => Err(DbError::Custom(
                        "Minus operator expects numeric operand".into(),
                    )),
                },
                _ => Err(DbError::Custom(format!(
                    "Unsupported unary operator: {op:?}"
                ))),
            }
        }
        SqlExpr::BinaryOp { left, op, right } => {
            let left_val = evaluate_expr_inner(left, schema, row, depth + 1)?;
            match op {
                BinaryOperator::And if matches!(left_val, Value::Boolean(false)) => {
                    return Ok(Value::Boolean(false));
                }
                BinaryOperator::Or if matches!(left_val, Value::Boolean(true)) => {
                    return Ok(Value::Boolean(true));
                }
                _ => {}
            }
            let right_val = evaluate_expr_inner(right, schema, row, depth + 1)?;
            evaluate_binary_op(&left_val, op, &right_val)
        }
        SqlExpr::IsNull(inner) => {
            let val = evaluate_expr_inner(inner, schema, row, depth + 1)?;
            Ok(Value::Boolean(val.is_null()))
        }
        SqlExpr::IsNotNull(inner) => {
            let val = evaluate_expr_inner(inner, schema, row, depth + 1)?;
            Ok(Value::Boolean(!val.is_null()))
        }
        SqlExpr::Nested(inner) => evaluate_expr_inner(inner, schema, row, depth + 1),
        SqlExpr::Function(func) => {
            let func_name = func.name.to_string();
            let args: &[FunctionArg] = match &func.args {
                sqlparser::ast::FunctionArguments::List(arg_list) => arg_list.args.as_slice(),
                sqlparser::ast::FunctionArguments::None => &[],
                sqlparser::ast::FunctionArguments::Subquery(_) => {
                    return Err(DbError::Custom("Unsupported function arguments".into()));
                }
            };
            crate::vector_expr::evaluate_vector_function(&func_name, args, schema, row)
        }
        SqlExpr::Array(_) => {
            let vec = crate::vector_expr::parse_vector_expr(expr, schema, row)?;
            Ok(Value::Vector(vec))
        }
        _ => Err(DbError::Custom(format!("Unsupported expression: {expr:?}"))),
    }
}

#[allow(clippy::cast_precision_loss)]
fn evaluate_binary_op(left: &Value, op: &BinaryOperator, right: &Value) -> Result<Value> {
    match op {
        // Standard SQL 3-Valued Logic for AND
        BinaryOperator::And => match (left, right) {
            (Value::Boolean(false), _) | (_, Value::Boolean(false)) => Ok(Value::Boolean(false)),
            (Value::Boolean(true), Value::Boolean(true)) => Ok(Value::Boolean(true)),
            (Value::Boolean(true) | Value::Null, Value::Null)
            | (Value::Null, Value::Boolean(true)) => Ok(Value::Null),
            _ => Err(DbError::Custom("AND expects boolean operands".into())),
        },
        // Standard SQL 3-Valued Logic for OR
        BinaryOperator::Or => match (left, right) {
            (Value::Boolean(true), _) | (_, Value::Boolean(true)) => Ok(Value::Boolean(true)),
            (Value::Boolean(false), Value::Boolean(false)) => Ok(Value::Boolean(false)),
            (Value::Boolean(false) | Value::Null, Value::Null)
            | (Value::Null, Value::Boolean(false)) => Ok(Value::Null),
            _ => Err(DbError::Custom("OR expects boolean operands".into())),
        },
        BinaryOperator::Eq => match left.sql_cmp(right) {
            Some(core::cmp::Ordering::Equal) => Ok(Value::Boolean(true)),
            Some(_) => Ok(Value::Boolean(false)),
            None => Ok(Value::Null),
        },
        BinaryOperator::NotEq => match left.sql_cmp(right) {
            Some(core::cmp::Ordering::Equal) => Ok(Value::Boolean(false)),
            Some(_) => Ok(Value::Boolean(true)),
            None => Ok(Value::Null),
        },
        BinaryOperator::Gt => match left.sql_cmp(right) {
            Some(core::cmp::Ordering::Greater) => Ok(Value::Boolean(true)),
            Some(_) => Ok(Value::Boolean(false)),
            None => Ok(Value::Null),
        },
        BinaryOperator::GtEq => match left.sql_cmp(right) {
            Some(core::cmp::Ordering::Greater | core::cmp::Ordering::Equal) => {
                Ok(Value::Boolean(true))
            }
            Some(_) => Ok(Value::Boolean(false)),
            None => Ok(Value::Null),
        },
        BinaryOperator::Lt => match left.sql_cmp(right) {
            Some(core::cmp::Ordering::Less) => Ok(Value::Boolean(true)),
            Some(_) => Ok(Value::Boolean(false)),
            None => Ok(Value::Null),
        },
        BinaryOperator::LtEq => match left.sql_cmp(right) {
            Some(core::cmp::Ordering::Less | core::cmp::Ordering::Equal) => {
                Ok(Value::Boolean(true))
            }
            Some(_) => Ok(Value::Boolean(false)),
            None => Ok(Value::Null),
        },
        BinaryOperator::Plus => match (left, right) {
            (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
            (Value::Integer(a), Value::Integer(b)) => match a.checked_add(*b) {
                Some(res) => Ok(Value::Integer(res)),
                None => Err(DbError::Custom("Integer addition overflow".into())),
            },
            (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a + b)),
            (Value::Integer(a), Value::Real(b)) => Ok(Value::Real(*a as f64 + b)),
            (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a + *b as f64)),
            _ => Err(DbError::Custom("Addition expects numeric operands".into())),
        },
        BinaryOperator::Minus => match (left, right) {
            (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
            (Value::Integer(a), Value::Integer(b)) => match a.checked_sub(*b) {
                Some(res) => Ok(Value::Integer(res)),
                None => Err(DbError::Custom("Integer subtraction overflow".into())),
            },
            (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a - b)),
            (Value::Integer(a), Value::Real(b)) => Ok(Value::Real(*a as f64 - b)),
            (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a - *b as f64)),
            _ => Err(DbError::Custom(
                "Subtraction expects numeric operands".into(),
            )),
        },
        BinaryOperator::Multiply => match (left, right) {
            (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
            (Value::Integer(a), Value::Integer(b)) => match a.checked_mul(*b) {
                Some(res) => Ok(Value::Integer(res)),
                None => Err(DbError::Custom("Integer multiplication overflow".into())),
            },
            (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a * b)),
            (Value::Integer(a), Value::Real(b)) => Ok(Value::Real(*a as f64 * b)),
            (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a * *b as f64)),
            _ => Err(DbError::Custom(
                "Multiplication expects numeric operands".into(),
            )),
        },
        BinaryOperator::Divide => match (left, right) {
            (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
            (Value::Integer(a), Value::Integer(b)) => {
                if *b == 0 {
                    return Err(DbError::Custom("Division by zero".into()));
                }
                match a.checked_div(*b) {
                    Some(res) => Ok(Value::Integer(res)),
                    None => Err(DbError::Custom("Integer division overflow".into())),
                }
            }
            (Value::Real(a), Value::Real(b)) => {
                if *b == 0.0 {
                    return Err(DbError::Custom("Division by zero".into()));
                }
                Ok(Value::Real(a / b))
            }
            (Value::Integer(a), Value::Real(b)) => {
                if *b == 0.0 {
                    return Err(DbError::Custom("Division by zero".into()));
                }
                Ok(Value::Real(*a as f64 / b))
            }
            (Value::Real(a), Value::Integer(b)) => {
                if *b == 0 {
                    return Err(DbError::Custom("Division by zero".into()));
                }
                Ok(Value::Real(a / *b as f64))
            }
            _ => Err(DbError::Custom("Division expects numeric operands".into())),
        },
        _ => Err(DbError::Custom(format!(
            "Unsupported binary operator: {op:?}"
        ))),
    }
}
