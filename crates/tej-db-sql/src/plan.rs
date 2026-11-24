use crate::bounds::{extract_pk_equality, extract_pk_range};
use crate::plan_ddl::plan_create_table;
use crate::plan_dml::{map_returning, plan_insert};
use crate::schema::{SqlType, TableSchema};
use crate::upsert::{OnConflict, ReturningClause};
use sqlparser::ast::{
    DataType as SqlDataType, Expr as SqlExpr, GroupByExpr, Query, SelectItem, SetExpr, Statement,
    TableFactor, Value as SqlValue,
};
use std::ops::Bound;
use tej_db_core::{DbError, KeyRange, Result};

#[derive(Debug, Clone)]
pub enum ProjectionItem {
    Wildcard,
    Column(String),
    Expr { expr: Box<SqlExpr>, name: String },
}

#[derive(Debug, Clone)]
pub enum Plan {
    CreateTable {
        schema: TableSchema,
    },
    Insert {
        table: String,
        columns: Vec<String>,
        rows: Vec<Vec<SqlExpr>>,
        on_conflict: Option<OnConflict>,
        returning: Option<ReturningClause>,
    },
    Select {
        table: String,
        projection: Vec<ProjectionItem>,
        key_range: KeyRange,
        filter: Option<SqlExpr>,
        limit: Option<usize>,
    },
    Update {
        table: String,
        assignments: Vec<(String, SqlExpr)>,
        key_range: KeyRange,
        filter: Option<SqlExpr>,
        returning: Option<ReturningClause>,
    },
    Delete {
        table: String,
        key_range: KeyRange,
        filter: Option<SqlExpr>,
        returning: Option<ReturningClause>,
    },
    Savepoint {
        name: String,
    },
    RollbackToSavepoint {
        name: String,
    },
    ReleaseSavepoint {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Planner;

impl Planner {
    pub fn plan(
        stmt: Statement,
        schema_provider: impl Fn(&str) -> Option<TableSchema>,
    ) -> Result<Plan> {
        match stmt {
            Statement::CreateTable(ct) => {
                plan_create_table(ct).map(|schema| Plan::CreateTable { schema })
            }
            Statement::Insert(ins) => plan_insert(ins),
            Statement::Savepoint { name } => Ok(Plan::Savepoint { name: name.value }),
            Statement::Rollback {
                savepoint: Some(ident),
                ..
            } => Ok(Plan::RollbackToSavepoint { name: ident.value }),
            Statement::ReleaseSavepoint { name } => Ok(Plan::ReleaseSavepoint { name: name.value }),
            Statement::Query(query) => Self::plan_query(*query, schema_provider),
            Statement::Update {
                table,
                assignments,
                selection,
                returning,
                ..
            } => {
                let table_name = extract_table_name(&table.relation)?;
                let schema = schema_provider(&table_name)
                    .ok_or_else(|| DbError::TableNotFound(table_name.clone()))?;

                let assign_items = assignments
                    .into_iter()
                    .map(|a| {
                        let col_name = match a.target {
                            sqlparser::ast::AssignmentTarget::ColumnName(object_name) => {
                                object_name.to_string()
                            }
                            sqlparser::ast::AssignmentTarget::Tuple(_) => "unknown".to_string(),
                        };
                        (col_name, a.value)
                    })
                    .collect();

                let (key_range, filter) = Self::optimize_where_clause(selection, &schema);
                Ok(Plan::Update {
                    table: table_name,
                    assignments: assign_items,
                    key_range,
                    filter,
                    returning: returning.map(map_returning),
                })
            }
            Statement::Delete(delete) => {
                let table_name = match delete.from {
                    sqlparser::ast::FromTable::WithFromKeyword(tables)
                    | sqlparser::ast::FromTable::WithoutKeyword(tables) => {
                        if let Some(t) = tables.first() {
                            extract_table_name(&t.relation)?
                        } else {
                            return Err(DbError::Custom(
                                "Missing table in DELETE statement".into(),
                            ));
                        }
                    }
                };

                let schema = schema_provider(&table_name)
                    .ok_or_else(|| DbError::TableNotFound(table_name.clone()))?;

                let (key_range, filter) = Self::optimize_where_clause(delete.selection, &schema);
                Ok(Plan::Delete {
                    table: table_name,
                    key_range,
                    filter,
                    returning: delete.returning.map(map_returning),
                })
            }
            _ => Err(DbError::Custom(format!(
                "Unsupported SQL statement: {stmt:?}"
            ))),
        }
    }

    fn plan_query(
        query: Query,
        schema_provider: impl Fn(&str) -> Option<TableSchema>,
    ) -> Result<Plan> {
        let select = match *query.body {
            SetExpr::Select(s) => s,
            _ => return Err(DbError::Custom("Only SELECT queries are supported".into())),
        };

        let from_table = select
            .from
            .first()
            .ok_or_else(|| DbError::Custom("SELECT query missing FROM clause".into()))?;
        if !from_table.joins.is_empty() {
            return Err(DbError::Custom(
                "JOIN clauses are currently unsupported in SQL engine".into(),
            ));
        }
        let table_name = extract_table_name(&from_table.relation)?;

        let schema = schema_provider(&table_name)
            .ok_or_else(|| DbError::TableNotFound(table_name.clone()))?;

        if query.order_by.is_some() {
            return Err(DbError::Custom(
                "ORDER BY is currently unsupported in SQL engine".into(),
            ));
        }
        if query.offset.is_some() {
            return Err(DbError::Custom(
                "OFFSET is currently unsupported in SQL engine".into(),
            ));
        }
        match &select.group_by {
            GroupByExpr::Expressions(exprs, _) if !exprs.is_empty() => {
                return Err(DbError::Custom(
                    "GROUP BY is currently unsupported in SQL engine".into(),
                ));
            }
            GroupByExpr::All(_) => {
                return Err(DbError::Custom(
                    "GROUP BY is currently unsupported in SQL engine".into(),
                ));
            }
            GroupByExpr::Expressions(..) => {}
        }
        if select.having.is_some() {
            return Err(DbError::Custom(
                "HAVING is currently unsupported in SQL engine".into(),
            ));
        }
        if select.distinct.is_some() {
            return Err(DbError::Custom(
                "DISTINCT is currently unsupported in SQL engine".into(),
            ));
        }

        let mut projection = Vec::new();
        for item in select.projection {
            match item {
                SelectItem::Wildcard(_) => {
                    projection.push(ProjectionItem::Wildcard);
                }
                SelectItem::UnnamedExpr(SqlExpr::Identifier(ident)) => {
                    projection.push(ProjectionItem::Column(ident.value));
                }
                SelectItem::UnnamedExpr(expr) => {
                    let name = format!("{expr}");
                    projection.push(ProjectionItem::Expr {
                        expr: Box::new(expr),
                        name,
                    });
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    projection.push(ProjectionItem::Expr {
                        expr: Box::new(expr),
                        name: alias.value,
                    });
                }
                SelectItem::QualifiedWildcard(..) => {
                    return Err(DbError::Custom(
                        "Qualified wildcards are currently unsupported".into(),
                    ));
                }
            }
        }

        let limit = query.limit.and_then(|l| match l {
            SqlExpr::Value(SqlValue::Number(n, _)) => n.parse::<usize>().ok(),
            _ => None,
        });

        let (key_range, filter) = Self::optimize_where_clause(select.selection, &schema);

        Ok(Plan::Select {
            table: table_name,
            projection,
            key_range,
            filter,
            limit,
        })
    }

    /// AST Optimizer: Extracts primary key point/range predicates into engine KeyRange.
    fn optimize_where_clause(
        selection: Option<SqlExpr>,
        schema: &TableSchema,
    ) -> (KeyRange, Option<SqlExpr>) {
        let Some(expr) = selection else {
            return (KeyRange::all(), None);
        };

        // Check if table has a single primary key column
        if schema.pk_indices.len() == 1 {
            let pk_col_name = &schema.columns[schema.pk_indices[0]].name;

            // 1. Point lookup: pk = <lit>
            if let Some(val) = extract_pk_equality(&expr, pk_col_name) {
                let key_bytes = val.encode_key();
                let range = KeyRange {
                    start: Bound::Included(key_bytes.clone()),
                    end: Bound::Included(key_bytes),
                };
                return (range, None);
            }

            // 2. Range scan: pk >= <lit> AND pk <= <lit>
            if let Some((start_bound, end_bound, residual)) = extract_pk_range(&expr, pk_col_name) {
                let range = KeyRange {
                    start: start_bound,
                    end: end_bound,
                };
                return (range, residual);
            }
        }

        (KeyRange::all(), Some(expr))
    }
}

fn extract_table_name(factor: &TableFactor) -> Result<String> {
    match factor {
        TableFactor::Table { name, .. } => Ok(name.to_string()),
        _ => Err(DbError::Custom("Unsupported table factor".into())),
    }
}

pub(crate) fn map_sql_data_type(dt: &SqlDataType) -> Result<SqlType> {
    match dt {
        SqlDataType::Int(_)
        | SqlDataType::Integer(_)
        | SqlDataType::BigInt(_)
        | SqlDataType::SmallInt(_)
        | SqlDataType::TinyInt(_) => Ok(SqlType::Integer),
        SqlDataType::Float(_) | SqlDataType::Double(_) | SqlDataType::Real => Ok(SqlType::Real),
        SqlDataType::Text
        | SqlDataType::Varchar(_)
        | SqlDataType::Char(_)
        | SqlDataType::String(_) => Ok(SqlType::Text),
        SqlDataType::Boolean | SqlDataType::Bool => Ok(SqlType::Boolean),
        SqlDataType::Blob(_) | SqlDataType::Bytea | SqlDataType::Binary(_) => Ok(SqlType::Blob),
        SqlDataType::Decimal(exact_info) | SqlDataType::Numeric(exact_info) => {
            let (p, s) = match exact_info {
                sqlparser::ast::ExactNumberInfo::PrecisionAndScale(p, s) => (*p as u8, *s as u8),
                sqlparser::ast::ExactNumberInfo::Precision(p) => (*p as u8, 0),
                sqlparser::ast::ExactNumberInfo::None => (18, 2),
            };
            Ok(SqlType::Decimal(p, s))
        }
        SqlDataType::Uuid => Ok(SqlType::Uuid),
        SqlDataType::Timestamp(..) | SqlDataType::Datetime(..) => Ok(SqlType::Timestamp),
        SqlDataType::Interval => Ok(SqlType::Interval),
        SqlDataType::JSON => Ok(SqlType::Json),
        SqlDataType::Array(
            sqlparser::ast::ArrayElemTypeDef::AngleBracket(inner)
            | sqlparser::ast::ArrayElemTypeDef::SquareBracket(inner, _),
        ) => Ok(SqlType::List(Box::new(map_sql_data_type(inner)?))),
        SqlDataType::Custom(name, modifiers) => {
            let n = name.to_string();
            if n.eq_ignore_ascii_case("VECTOR") {
                let dim = modifiers
                    .first()
                    .and_then(|m| m.parse::<usize>().ok())
                    .unwrap_or(0);
                Ok(SqlType::Vector(dim))
            } else if n.eq_ignore_ascii_case("JSONB") {
                Ok(SqlType::Json)
            } else if n.eq_ignore_ascii_case("UUID") {
                Ok(SqlType::Uuid)
            } else if n.eq_ignore_ascii_case("TIMESTAMP") {
                Ok(SqlType::Timestamp)
            } else if n.eq_ignore_ascii_case("INTERVAL") {
                Ok(SqlType::Interval)
            } else {
                Err(DbError::Custom(format!(
                    "Unsupported custom SQL column data type: {n}"
                )))
            }
        }
        _ => Err(DbError::Custom(format!(
            "Unsupported SQL column data type: {dt:?}"
        ))),
    }
}
