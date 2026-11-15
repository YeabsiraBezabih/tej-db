use serde::{Deserialize, Serialize};

/// Action taken on referencing rows when a referenced primary/unique key is deleted or updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReferentialAction {
    #[default]
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

/// SQL Foreign Key constraint definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKeyConstraint {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub foreign_table: String,
    pub foreign_columns: Vec<String>,
    pub on_delete: ReferentialAction,
    pub on_update: ReferentialAction,
}

impl ForeignKeyConstraint {
    /// Creates a new foreign key constraint.
    #[must_use]
    pub fn new(
        columns: Vec<String>,
        foreign_table: impl Into<String>,
        foreign_columns: Vec<String>,
        on_delete: ReferentialAction,
        on_update: ReferentialAction,
    ) -> Self {
        Self {
            name: None,
            columns,
            foreign_table: foreign_table.into(),
            foreign_columns,
            on_delete,
            on_update,
        }
    }
}

/// SQL CHECK constraint definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckConstraint {
    pub name: Option<String>,
    pub expr_sql: String,
}

impl CheckConstraint {
    /// Creates a new check constraint with the given boolean expression string.
    #[must_use]
    pub fn new(expr_sql: impl Into<String>) -> Self {
        Self {
            name: None,
            expr_sql: expr_sql.into(),
        }
    }
}
