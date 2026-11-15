use crate::plan_ddl::plan_create_table;
use crate::plan_dml::{map_returning, plan_insert};
use crate::schema::{SqlType, TableSchema};
use crate::upsert::{OnConflict, ReturningClause};
use crate::value::Value;
use sqlparser::ast::{
    BinaryOperator, DataType as SqlDataType, Expr as SqlExpr, Query, SelectItem, SetExpr,
    Statement, TableFactor, Value as SqlValue,
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
        let table_name = extract_table_name(&from_table.relation)?;

        let schema = schema_provider(&table_name)
            .ok_or_else(|| DbError::TableNotFound(table_name.clone()))?;

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

fn extract_pk_equality(expr: &SqlExpr, pk_col: &str) -> Option<Value> {
    if let SqlExpr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
    } = expr
    {
        if let (SqlExpr::Identifier(id), SqlExpr::Value(v)) = (left.as_ref(), right.as_ref()) {
            if id.value.eq_ignore_ascii_case(pk_col) {
                return parse_sql_value(v);
            }
        }
        if let (SqlExpr::Value(v), SqlExpr::Identifier(id)) = (left.as_ref(), right.as_ref()) {
            if id.value.eq_ignore_ascii_case(pk_col) {
                return parse_sql_value(v);
            }
        }
    }
    None
}

#[allow(clippy::type_complexity, clippy::single_match)]
fn extract_pk_range(
    expr: &SqlExpr,
    pk_col: &str,
) -> Option<(Bound<Vec<u8>>, Bound<Vec<u8>>, Option<SqlExpr>)> {
    match expr {
        SqlExpr::BinaryOp { left, op, right } => {
            if let (SqlExpr::Identifier(id), SqlExpr::Value(v)) = (left.as_ref(), right.as_ref()) {
                if id.value.eq_ignore_ascii_case(pk_col) {
                    let val = parse_sql_value(v)?;
                    let enc = val.encode_key();
                    match op {
                        BinaryOperator::Gt => {
                            return Some((Bound::Excluded(enc), Bound::Unbounded, None));
                        }
                        BinaryOperator::GtEq => {
                            return Some((Bound::Included(enc), Bound::Unbounded, None));
                        }
                        BinaryOperator::Lt => {
                            return Some((Bound::Unbounded, Bound::Excluded(enc), None));
                        }
                        BinaryOperator::LtEq => {
                            return Some((Bound::Unbounded, Bound::Included(enc), None));
                        }
                        _ => {}
                    }
                }
            }
            if let SqlExpr::BinaryOp {
                left: l_box,
                op: BinaryOperator::And,
                right: r_box,
            } = expr
            {
                if let (Some((s1, e1, None)), Some((s2, e2, None))) = (
                    extract_pk_range(l_box, pk_col),
                    extract_pk_range(r_box, pk_col),
                ) {
                    let start = match (s1, s2) {
                        (Bound::Unbounded, s) | (s, Bound::Unbounded) => s,
                        (s1, _) => s1,
                    };
                    let end = match (e1, e2) {
                        (Bound::Unbounded, e) | (e, Bound::Unbounded) => e,
                        (e1, _) => e1,
                    };
                    return Some((start, end, None));
                }
            }
        }
        _ => {}
    }
    None
}

fn parse_sql_value(val: &SqlValue) -> Option<Value> {
    match val {
        SqlValue::Number(n, _) => {
            if let Ok(i) = n.parse::<i64>() {
                Some(Value::Integer(i))
            } else if let Ok(f) = n.parse::<f64>() {
                Some(Value::Real(f))
            } else {
                None
            }
        }
        SqlValue::SingleQuotedString(s) | SqlValue::DoubleQuotedString(s) => {
            Some(Value::Text(s.clone()))
        }
        SqlValue::Boolean(b) => Some(Value::Boolean(*b)),
        SqlValue::Null => Some(Value::Null),
        _ => None,
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
