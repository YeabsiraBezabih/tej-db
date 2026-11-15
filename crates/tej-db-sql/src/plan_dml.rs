use crate::plan::{Plan, ProjectionItem};
use crate::upsert::{OnConflict, ReturningClause};
use sqlparser::ast::{
    Insert, OnConflict as SqlOnConflict, OnConflictAction as SqlOnConflictAction, OnInsert,
    SelectItem, SetExpr,
};
use tej_db_core::Result;

/// Maps AST SelectItem list to ReturningClause.
pub(crate) fn map_returning(items: Vec<SelectItem>) -> ReturningClause {
    ReturningClause {
        items: items
            .into_iter()
            .map(|item| match item {
                SelectItem::UnnamedExpr(expr) => {
                    let name = expr.to_string();
                    ProjectionItem::Expr {
                        expr: Box::new(expr),
                        name,
                    }
                }
                SelectItem::ExprWithAlias { expr, alias } => ProjectionItem::Expr {
                    expr: Box::new(expr),
                    name: alias.value,
                },
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {
                    ProjectionItem::Wildcard
                }
            })
            .collect(),
    }
}

/// Plans an `INSERT` statement into `Plan::Insert`.
pub fn plan_insert(ins: Insert) -> Result<Plan> {
    let table = ins.table.to_string();
    let col_names = ins.columns.into_iter().map(|c| c.value).collect();

    let mut rows = Vec::new();
    if let Some(source) = ins.source {
        if let SetExpr::Values(values) = *source.body {
            rows = values.rows;
        }
    }

    let on_conflict = match ins.on {
        Some(OnInsert::OnConflict(sql_on_conflict)) => Some(map_on_conflict(sql_on_conflict)),
        _ => None,
    };

    let returning = ins.returning.map(map_returning);

    Ok(Plan::Insert {
        table,
        columns: col_names,
        rows,
        on_conflict,
        returning,
    })
}

fn map_on_conflict(c: SqlOnConflict) -> OnConflict {
    match c.action {
        SqlOnConflictAction::DoNothing => OnConflict::DoNothing,
        SqlOnConflictAction::DoUpdate(do_upd) => {
            let target_cols = match c.conflict_target {
                Some(sqlparser::ast::ConflictTarget::Columns(cols)) => {
                    cols.into_iter().map(|c| c.value).collect()
                }
                _ => Vec::new(),
            };
            let mut assignments = Vec::with_capacity(do_upd.assignments.len());
            for assign in do_upd.assignments {
                let col_name = match assign.target {
                    sqlparser::ast::AssignmentTarget::ColumnName(obj_name) => obj_name.to_string(),
                    sqlparser::ast::AssignmentTarget::Tuple(_) => String::new(),
                };
                assignments.push((col_name, assign.value));
            }
            OnConflict::DoUpdate {
                target_columns: target_cols,
                assignments,
            }
        }
    }
}
