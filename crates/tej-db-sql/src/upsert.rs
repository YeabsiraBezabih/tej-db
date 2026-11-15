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
