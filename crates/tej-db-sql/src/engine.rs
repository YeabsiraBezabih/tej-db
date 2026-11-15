use crate::plan::{Plan, Planner, ProjectionItem};
use crate::prepared::{CompiledStatement, PreparedStatement};
use crate::result::QueryResult;
use crate::schema::TableSchema;
use parking_lot::RwLock;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::AtomicU64;
use tej_db::{Database, Result, Storage, WriteTx};
use tej_db_core::DbError;

/// SQL query execution engine operating over a modular ACID database.
pub struct SqlEngine<'a, S: Storage> {
    pub(crate) db: &'a Database<S>,
    pub(crate) schemas: RwLock<HashMap<String, TableSchema>>,
    plan_cache: RwLock<HashMap<String, Plan>>,
    pub(crate) auto_rowid: AtomicU64,
}

impl<S: Storage> fmt::Debug for SqlEngine<'_, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlEngine")
            .field("cached_schemas", &self.schemas.read().len())
            .field("cached_plans", &self.plan_cache.read().len())
            .finish_non_exhaustive()
    }
}

impl<'a, S: Storage> SqlEngine<'a, S> {
    pub fn new(db: &'a Database<S>) -> Self {
        Self {
            db,
            schemas: RwLock::new(HashMap::new()),
            plan_cache: RwLock::new(HashMap::new()),
            auto_rowid: AtomicU64::new(1),
        }
    }

    /// Load or retrieve cached table schema from database catalog.
    pub fn get_schema(&self, table_name: &str) -> Result<Option<TableSchema>> {
        let key_name = table_name.to_ascii_lowercase();
        {
            let guard = self.schemas.read();
            if let Some(schema) = guard.get(&key_name) {
                return Ok(Some(schema.clone()));
            }
        }

        // Read from DB system catalog
        let rx = self.db.begin_read()?;
        let cat_key = TableSchema::catalog_key(&key_name);
        if let Some(bytes) = rx.get(&cat_key)? {
            let schema: TableSchema = bincode::deserialize(&bytes).map_err(|e| {
                DbError::Serialization(format!("Catalog schema deserialization failed: {e}"))
            })?;
            self.schemas.write().insert(key_name, schema.clone());
            return Ok(Some(schema));
        }

        Ok(None)
    }

    /// Parse and prepare a single SQL statement into a planned query plan.
    pub fn prepare(&self, sql: &str) -> Result<Plan> {
        if let Some(plan) = self.plan_cache.read().get(sql) {
            return Ok(plan.clone());
        }

        let dialect = GenericDialect {};
        let mut ast = Parser::parse_sql(&dialect, sql)
            .map_err(|e| DbError::Custom(format!("SQL parse error: {e}")))?;

        if ast.len() != 1 {
            return Err(DbError::Custom(
                "prepare requires exactly one SQL statement".into(),
            ));
        }

        let stmt = ast.remove(0);
        let plan = Planner::plan(stmt, |name| self.get_schema(name).ok().flatten())?;
        self.plan_cache
            .write()
            .insert(sql.to_string(), plan.clone());
        Ok(plan)
    }

    pub fn plan_query(&self, sql: &str) -> Result<Plan> {
        self.prepare(sql)
    }

    /// Compile a parameterized statement with parameter binding slots.
    pub fn prepare_statement(&'a self, sql: &str) -> Result<PreparedStatement<'a, S>> {
        let dialect = GenericDialect {};
        let mut ast = Parser::parse_sql(&dialect, sql)
            .map_err(|e| DbError::Custom(format!("SQL parse error: {e}")))?;

        if ast.len() != 1 {
            return Err(DbError::Custom(
                "prepare_statement requires exactly one SQL statement".into(),
            ));
        }

        let stmt = ast.remove(0);
        let compiled = match stmt {
            sqlparser::ast::Statement::Insert(insert) => {
                let table_name = insert.table.to_string();
                let schema = self
                    .get_schema(&table_name)?
                    .ok_or_else(|| DbError::TableNotFound(table_name.clone()))?;
                let is_parameterized_insert = if let Some(source) = &insert.source {
                    if let sqlparser::ast::SetExpr::Values(values) = source.body.as_ref() {
                        values.rows.len() == 1
                            && values.rows[0].iter().all(|expr| {
                                matches!(
                                    expr,
                                    sqlparser::ast::Expr::Value(
                                        sqlparser::ast::Value::Placeholder(_)
                                    )
                                )
                            })
                    } else {
                        false
                    }
                } else {
                    false
                };

                if is_parameterized_insert {
                    CompiledStatement::PointInsert {
                        table_name,
                        schema: std::sync::Arc::new(schema),
                    }
                } else {
                    let plan = Planner::plan(sqlparser::ast::Statement::Insert(insert), |name| {
                        self.get_schema(name).ok().flatten()
                    })?;
                    CompiledStatement::General {
                        plan: Box::new(plan),
                    }
                }
            }
            sqlparser::ast::Statement::Query(query) => {
                let plan = Planner::plan(sqlparser::ast::Statement::Query(query), |name| {
                    self.get_schema(name).ok().flatten()
                })?;
                if let Plan::Select {
                    table,
                    projection,
                    filter:
                        Some(sqlparser::ast::Expr::BinaryOp {
                            left,
                            op: sqlparser::ast::BinaryOperator::Eq,
                            right,
                        }),
                    limit: None,
                    ..
                } = &plan
                {
                    if let Some(schema) = self.get_schema(table).ok().flatten() {
                        if schema.pk_indices.len() == 1 {
                            let pk_col_name = &schema.columns[schema.pk_indices[0]].name;
                            if matches!((left.as_ref(), right.as_ref()), (sqlparser::ast::Expr::Identifier(id), sqlparser::ast::Expr::Value(sqlparser::ast::Value::Placeholder(_))) if id.value.eq_ignore_ascii_case(pk_col_name))
                            {
                                let is_wildcard = projection.is_empty()
                                    || projection
                                        .iter()
                                        .any(|p| matches!(p, ProjectionItem::Wildcard));
                                let (projected_col_indices, result_column_names) = if is_wildcard {
                                    let indices = (0..schema.columns.len()).collect();
                                    let names =
                                        schema.columns.iter().map(|c| c.name.clone()).collect();
                                    (indices, names)
                                } else {
                                    let mut indices = Vec::with_capacity(projection.len());
                                    let mut names = Vec::with_capacity(projection.len());
                                    for item in projection {
                                        match item {
                                            ProjectionItem::Column(name)
                                            | ProjectionItem::Expr { name, .. } => {
                                                names.push(name.clone());
                                                indices
                                                    .push(schema.column_index(name).unwrap_or(0));
                                            }
                                            ProjectionItem::Wildcard => {}
                                        }
                                    }
                                    (indices, names)
                                };
                                CompiledStatement::PointQuery {
                                    table_name: table.clone(),
                                    schema: std::sync::Arc::new(schema),
                                    projection: projection.clone(),
                                    projected_col_indices,
                                    result_column_names,
                                    pk_param_idx: 0,
                                }
                            } else {
                                CompiledStatement::General {
                                    plan: Box::new(plan),
                                }
                            }
                        } else {
                            CompiledStatement::General {
                                plan: Box::new(plan),
                            }
                        }
                    } else {
                        CompiledStatement::General {
                            plan: Box::new(plan),
                        }
                    }
                } else {
                    CompiledStatement::General {
                        plan: Box::new(plan),
                    }
                }
            }
            sqlparser::ast::Statement::Update {
                table,
                assignments,
                selection,
                ..
            } => {
                let table_name = match &table.relation {
                    sqlparser::ast::TableFactor::Table { name, .. } => name.to_string(),
                    _ => String::new(),
                };
                let schema = self
                    .get_schema(&table_name)?
                    .ok_or_else(|| DbError::TableNotFound(table_name.clone()))?;

                let mut is_point_update = false;
                let mut target_col_idx = 0;

                if assignments.len() == 1 {
                    let col_name = match &assignments[0].target {
                        sqlparser::ast::AssignmentTarget::ColumnName(object_name) => {
                            object_name.to_string()
                        }
                        sqlparser::ast::AssignmentTarget::Tuple(_) => String::new(),
                    };
                    if let Some(idx) = schema.column_index(&col_name) {
                        target_col_idx = idx;
                        if matches!(
                            &assignments[0].value,
                            sqlparser::ast::Expr::Value(sqlparser::ast::Value::Placeholder(_))
                        ) {
                            is_point_update = true;
                        }
                    }
                }

                if is_point_update && schema.pk_indices.len() == 1 {
                    let pk_col_name = &schema.columns[schema.pk_indices[0]].name;
                    let matches_pk = if let Some(sqlparser::ast::Expr::BinaryOp {
                        left,
                        op: sqlparser::ast::BinaryOperator::Eq,
                        right,
                    }) = &selection
                    {
                        matches!((left.as_ref(), right.as_ref()), (sqlparser::ast::Expr::Identifier(id), sqlparser::ast::Expr::Value(sqlparser::ast::Value::Placeholder(_))) if id.value.eq_ignore_ascii_case(pk_col_name))
                    } else {
                        false
                    };

                    if matches_pk {
                        CompiledStatement::PointUpdate {
                            table_name,
                            schema: std::sync::Arc::new(schema),
                            col_idx: target_col_idx,
                            val_param_idx: 0,
                            pk_param_idx: 1,
                        }
                    } else {
                        let plan = Planner::plan(
                            sqlparser::ast::Statement::Update {
                                table,
                                assignments,
                                selection,
                                from: None,
                                returning: None,
                                or: None,
                            },
                            |name| self.get_schema(name).ok().flatten(),
                        )?;
                        CompiledStatement::General {
                            plan: Box::new(plan),
                        }
                    }
                } else {
                    let plan = Planner::plan(
                        sqlparser::ast::Statement::Update {
                            table,
                            assignments,
                            selection,
                            from: None,
                            returning: None,
                            or: None,
                        },
                        |name| self.get_schema(name).ok().flatten(),
                    )?;
                    CompiledStatement::General {
                        plan: Box::new(plan),
                    }
                }
            }
            other => {
                let plan = Planner::plan(other, |name| self.get_schema(name).ok().flatten())?;
                CompiledStatement::General {
                    plan: Box::new(plan),
                }
            }
        };

        Ok(PreparedStatement::new(self, compiled))
    }

    /// Clear the internal statement plan cache.
    pub fn clear_plan_cache(&self) {
        self.plan_cache.write().clear();
    }

    /// Execute any SQL statement (DDL, DML, or DQL) and return the query result.
    pub fn execute(&self, sql: &str) -> Result<QueryResult> {
        if let Some(plan) = self.plan_cache.read().get(sql) {
            return self.execute_plan(plan.clone());
        }

        let dialect = GenericDialect {};
        let ast = Parser::parse_sql(&dialect, sql)
            .map_err(|e| DbError::Custom(format!("SQL parse error: {e}")))?;

        let mut last_res = QueryResult::empty();
        let is_single = ast.len() == 1;
        for stmt in ast {
            let plan = Planner::plan(stmt, |name| self.get_schema(name).ok().flatten())?;
            last_res = self.execute_plan(plan.clone())?;
            if is_single {
                self.plan_cache.write().insert(sql.to_string(), plan);
            }
        }

        Ok(last_res)
    }

    /// Execute a SQL statement inside an existing active write transaction without committing.
    pub fn execute_in_tx(&self, tx: &mut WriteTx<'_, S>, sql: &str) -> Result<QueryResult> {
        if let Some(plan) = self.plan_cache.read().get(sql) {
            return self.execute_plan_in_tx(tx, plan);
        }

        let dialect = GenericDialect {};
        let ast = Parser::parse_sql(&dialect, sql)
            .map_err(|e| DbError::Custom(format!("SQL parse error: {e}")))?;

        let mut last_res = QueryResult::empty();
        let is_single = ast.len() == 1;
        for stmt in ast {
            let plan = Planner::plan(stmt, |name| self.get_schema(name).ok().flatten())?;
            last_res = self.execute_plan_in_tx(tx, &plan)?;
            if is_single {
                self.plan_cache.write().insert(sql.to_string(), plan);
            }
        }

        Ok(last_res)
    }
}
