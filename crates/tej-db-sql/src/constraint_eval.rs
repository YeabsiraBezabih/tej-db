use crate::expr::evaluate_expr;
use crate::schema::TableSchema;
use crate::value::Value;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use tej_db::{Result, Storage, WriteTx};
use tej_db_core::{DbError, KeyRange};

/// Validates all CHECK constraints defined on a table for a given row.
pub fn validate_check_constraints(schema: &TableSchema, row: &[Value]) -> Result<()> {
    for check in &schema.check_constraints {
        let dialect = GenericDialect {};
        let mut ast = Parser::parse_sql(&dialect, &format!("SELECT {}", check.expr_sql))
            .map_err(|e| DbError::Custom(format!("Failed to parse CHECK constraint: {e}")))?;

        if let Some(sqlparser::ast::Statement::Query(q)) = ast.pop() {
            if let sqlparser::ast::SetExpr::Select(select) = *q.body {
                if let Some(sqlparser::ast::SelectItem::UnnamedExpr(expr)) =
                    select.projection.into_iter().next()
                {
                    let result = evaluate_expr(&expr, schema, row)?;
                    if result == Value::Boolean(false) {
                        let name = check.name.as_deref().unwrap_or(&check.expr_sql);
                        return Err(DbError::Custom(format!(
                            "CHECK constraint violation '{name}' on table '{}'",
                            schema.name
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Validates that foreign key references exist in referenced parent tables when inserting a row.
pub fn validate_foreign_keys_on_insert<S: Storage>(
    schema: &TableSchema,
    row: &[Value],
    tx: &WriteTx<'_, S>,
    get_schema_fn: impl Fn(&str) -> Result<Option<TableSchema>>,
) -> Result<()> {
    for fk in &schema.foreign_keys {
        let mut has_null = false;
        let mut ref_values = Vec::with_capacity(fk.columns.len());
        for col_name in &fk.columns {
            let idx = schema
                .column_index(col_name)
                .ok_or_else(|| DbError::Custom(format!("Unknown FK column: {col_name}")))?;
            let val = row.get(idx).cloned().unwrap_or(Value::Null);
            if val.is_null() {
                has_null = true;
                break;
            }
            ref_values.push(val);
        }

        if has_null {
            // SQL standard: NULL in FK column satisfies the constraint
            continue;
        }

        let parent_schema = get_schema_fn(&fk.foreign_table)?
            .ok_or_else(|| DbError::TableNotFound(fk.foreign_table.clone()))?;

        let items = tx.scan_table_range(&fk.foreign_table, &KeyRange::all())?;
        let mut found = false;

        for item in items {
            let parent_row = parent_schema.decode_row_value(&item.value)?;
            let mut matches = true;
            for (fk_col, ref_val) in fk.foreign_columns.iter().zip(ref_values.iter()) {
                if let Some(p_idx) = parent_schema.column_index(fk_col) {
                    if parent_row.get(p_idx) != Some(ref_val) {
                        matches = false;
                        break;
                    }
                } else {
                    matches = false;
                    break;
                }
            }
            if matches {
                found = true;
                break;
            }
        }

        if !found {
            return Err(DbError::Custom(format!(
                "FOREIGN KEY constraint violation on table '{}': parent row not found in '{}'",
                schema.name, fk.foreign_table
            )));
        }
    }
    Ok(())
}
