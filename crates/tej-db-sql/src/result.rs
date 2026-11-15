use crate::expr::evaluate_expr;
use crate::plan::ProjectionItem;
use crate::schema::TableSchema;
use crate::value::Value;
use sqlparser::ast::Expr as SqlExpr;
use std::fmt;
use tej_db::Result;
use tej_db_core::KV;

/// Tabular output of a SQL statement execution.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub rows_affected: usize,
}

impl QueryResult {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn affected(count: usize) -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: count,
        }
    }

    #[must_use]
    pub fn rows(columns: Vec<String>, rows: Vec<Vec<Value>>) -> Self {
        let count = rows.len();
        Self {
            columns,
            rows,
            rows_affected: count,
        }
    }
}

impl fmt::Display for QueryResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.columns.is_empty() {
            return write!(f, "Rows affected: {}", self.rows_affected);
        }

        // Calculate column widths
        let mut widths: Vec<usize> = self.columns.iter().map(String::len).collect();
        for row in &self.rows {
            for (i, val) in row.iter().enumerate() {
                let s_len = format!("{val}").len();
                if i < widths.len() {
                    widths[i] = widths[i].max(s_len);
                }
            }
        }

        // Header
        for (i, col) in self.columns.iter().enumerate() {
            write!(f, "{:<width$} | ", col, width = widths[i])?;
        }
        writeln!(f)?;

        // Separator
        for width in &widths {
            write!(f, "{}-+-", "-".repeat(*width))?;
        }
        writeln!(f)?;

        // Rows
        for row in &self.rows {
            for (i, val) in row.iter().enumerate() {
                write!(f, "{:<width$} | ", format!("{val}"), width = widths[i])?;
            }
            writeln!(f)?;
        }

        write!(f, "({} rows)", self.rows.len())
    }
}

/// Helper function to project, filter, and limit rows from scanned or retrieved KV items.
pub fn execute_select_from_items(
    schema: &TableSchema,
    projection: &[ProjectionItem],
    filter: Option<&SqlExpr>,
    limit: Option<usize>,
    items: impl IntoIterator<Item = KV>,
) -> Result<QueryResult> {
    // Resolve projected column names
    let mut out_cols = Vec::new();
    let is_wildcard = projection
        .iter()
        .any(|p| matches!(p, ProjectionItem::Wildcard));

    if is_wildcard {
        for col in &schema.columns {
            out_cols.push(col.name.clone());
        }
    } else {
        for item in projection {
            match item {
                ProjectionItem::Column(name) => {
                    if let Some(idx) = schema.column_index(name) {
                        out_cols.push(schema.columns[idx].name.clone());
                    } else {
                        out_cols.push(name.clone());
                    }
                }
                ProjectionItem::Expr { name, .. } => {
                    out_cols.push(name.clone());
                }
                ProjectionItem::Wildcard => {}
            }
        }
    }

    let mut out_rows = Vec::new();
    for item in items {
        let row = schema.decode_row_value(&item.value)?;

        // Evaluate residual filter predicate
        if let Some(expr) = filter {
            let matches = evaluate_expr(expr, schema, &row)?;
            if matches != Value::Boolean(true) {
                continue;
            }
        }

        // Project values
        let mut proj_row = Vec::new();
        if is_wildcard {
            proj_row = row;
        } else {
            for item in projection {
                match item {
                    ProjectionItem::Column(name) => {
                        if let Some(idx) = schema.column_index(name) {
                            proj_row.push(row.get(idx).cloned().unwrap_or(Value::Null));
                        } else {
                            proj_row.push(Value::Null);
                        }
                    }
                    ProjectionItem::Expr { expr, .. } => {
                        let v = evaluate_expr(expr, schema, &row)?;
                        proj_row.push(v);
                    }
                    ProjectionItem::Wildcard => {}
                }
            }
        }
        out_rows.push(proj_row);

        if let Some(lim) = limit {
            if out_rows.len() >= lim {
                break;
            }
        }
    }

    Ok(QueryResult {
        columns: out_cols,
        rows: out_rows,
        rows_affected: 0,
    })
}
