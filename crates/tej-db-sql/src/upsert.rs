use crate::plan::ProjectionItem;
use sqlparser::ast::Expr as SqlExpr;

/// Action to perform when an `INSERT` statement violates a UNIQUE or PRIMARY KEY constraint.
#[derive(Debug, Clone)]
pub enum OnConflict {
    DoNothing,
    DoUpdate {
        target_columns: Vec<String>,
        assignments: Vec<(String, SqlExpr)>,
    },
}

/// Optional RETURNING projection for DML operations (INSERT, UPDATE, DELETE).
#[derive(Debug, Clone, Default)]
pub struct ReturningClause {
    pub items: Vec<ProjectionItem>,
}

pub(crate) fn project_returning_row(
    schema: &crate::schema::TableSchema,
    items: &[ProjectionItem],
    row: &[crate::value::Value],
) -> tej_db_core::Result<Vec<crate::value::Value>> {
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        match it {
            ProjectionItem::Wildcard => {
                out.extend_from_slice(row);
            }
            ProjectionItem::Column(name) => {
                let idx = schema.column_index(name).ok_or_else(|| {
                    tej_db_core::DbError::Custom(format!("Column not found: {name}"))
                })?;
                out.push(row.get(idx).cloned().unwrap_or(crate::value::Value::Null));
            }
            ProjectionItem::Expr { expr, .. } => {
                out.push(crate::expr::evaluate_expr(expr, schema, row)?);
            }
        }
    }
    Ok(out)
}
