use crate::expr::evaluate_expr;
use crate::plan::{OrderBySpec, ProjectionItem};
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
#[derive(Debug, Clone, Copy)]
pub struct SelectOptions<'a> {
    pub distinct: bool,
    pub order_by: &'a [OrderBySpec],
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub fn execute_select_from_items(
    schema: &TableSchema,
    projection: &[ProjectionItem],
    filter: Option<&SqlExpr>,
    opts: SelectOptions<'_>,
    items: impl IntoIterator<Item = KV>,
) -> Result<QueryResult> {
    let SelectOptions {
        distinct,
        order_by,
        limit,
        offset,
    } = opts;
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

    if limit == Some(0) {
        return Ok(QueryResult {
            columns: out_cols,
            rows: Vec::new(),
            rows_affected: 0,
        });
    }

    let can_early_limit = order_by.is_empty() && !distinct && offset.unwrap_or(0) == 0;
    let mut sort_entries: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();

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
            proj_row.clone_from(&row);
        } else {
            for item in projection {
                match item {
                    ProjectionItem::Column(name) => {
                        if let Some(idx) = schema.column_index(name) {
                            proj_row.push(row.get(idx).cloned().unwrap_or(Value::Null));
                        } else {
                            return Err(tej_db_core::DbError::Custom(format!(
                                "Column '{name}' not found in table '{}'",
                                schema.name
                            )));
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

        let mut sort_keys = Vec::with_capacity(order_by.len());
        for ob in order_by {
            sort_keys.push(evaluate_expr(&ob.expr, schema, &row)?);
        }
        sort_entries.push((sort_keys, proj_row));

        if can_early_limit {
            if let Some(lim) = limit {
                if sort_entries.len() >= lim {
                    break;
                }
            }
        }
    }

    if !order_by.is_empty() {
        sort_entries.sort_by(|(ka, _), (kb, _)| {
            for (i, ob) in order_by.iter().enumerate() {
                let va = &ka[i];
                let vb = &kb[i];
                let nulls_first = ob.nulls_first.unwrap_or(!ob.asc);
                let ord = match (va.is_null(), vb.is_null()) {
                    (true, true) => std::cmp::Ordering::Equal,
                    (true, false) => {
                        if nulls_first {
                            std::cmp::Ordering::Less
                        } else {
                            std::cmp::Ordering::Greater
                        }
                    }
                    (false, true) => {
                        if nulls_first {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        }
                    }
                    (false, false) => {
                        let c = va.sql_cmp(vb).unwrap_or(std::cmp::Ordering::Equal);
                        if ob.asc { c } else { c.reverse() }
                    }
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });
    }

    let mut out_rows: Vec<Vec<Value>> = sort_entries.into_iter().map(|(_, r)| r).collect();
    if distinct {
        let mut deduped = Vec::with_capacity(out_rows.len());
        for row in out_rows {
            if !deduped.contains(&row) {
                deduped.push(row);
            }
        }
        out_rows = deduped;
    }
    if let Some(off) = offset {
        if off >= out_rows.len() {
            out_rows.clear();
        } else if off > 0 {
            out_rows.drain(..off);
        }
    }
    if let Some(lim) = limit {
        out_rows.truncate(lim);
    }

    Ok(QueryResult {
        columns: out_cols,
        rows: out_rows,
        rows_affected: 0,
    })
}
