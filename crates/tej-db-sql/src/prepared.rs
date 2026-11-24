use crate::engine::SqlEngine;
use crate::plan::{Plan, ProjectionItem};
use crate::result::QueryResult;
use crate::schema::TableSchema;
use crate::value::Value;
use std::fmt;
use tej_db::{Result, Storage, WriteTx};
use tej_db_core::DbError;

#[derive(Clone, Debug)]
pub enum CompiledStatement {
    PointInsert {
        table_name: String,
        schema: std::sync::Arc<TableSchema>,
    },
    PointUpdate {
        table_name: String,
        schema: std::sync::Arc<TableSchema>,
        col_idx: usize,
        val_param_idx: usize,
        pk_param_idx: usize,
    },
    PointQuery {
        table_name: String,
        schema: std::sync::Arc<TableSchema>,
        projection: Vec<ProjectionItem>,
        projected_col_indices: Vec<usize>,
        result_column_names: Vec<String>,
        pk_param_idx: usize,
    },
    General {
        plan: Box<Plan>,
    },
}

/// A prepared, compiled SQL statement ready for parameter execution.
pub struct PreparedStatement<S: Storage> {
    pub(crate) engine: SqlEngine<S>,
    pub(crate) compiled: CompiledStatement,
}

impl<S: Storage> Clone for PreparedStatement<S> {
    fn clone(&self) -> Self {
        Self {
            engine: self.engine.clone(),
            compiled: self.compiled.clone(),
        }
    }
}

impl<S: Storage> fmt::Debug for PreparedStatement<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedStatement")
            .field("compiled", &self.compiled)
            .finish_non_exhaustive()
    }
}

impl<S: Storage> PreparedStatement<S> {
    #[must_use]
    pub fn new(engine: SqlEngine<S>, compiled: CompiledStatement) -> Self {
        Self { engine, compiled }
    }
}

impl<S: Storage> PreparedStatement<S> {
    /// Execute prepared statement inside an existing active write transaction without committing.
    pub fn execute_in_tx(&self, tx: &mut WriteTx<'_, S>, params: &[Value]) -> Result<QueryResult> {
        match &self.compiled {
            CompiledStatement::PointInsert { table_name, schema } => {
                thread_local! {
                    static ROW_BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
                }
                crate::constraint_eval::validate_check_constraints(schema, params)?;
                let mut tbl = tx.open_table_mut(table_name)?;
                let rowid = self
                    .engine
                    .auto_rowid
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                schema.with_row_key(params, rowid, |key_bytes| -> Result<()> {
                    ROW_BUF.with(|cell| -> Result<()> {
                        let mut val_buf = cell.borrow_mut();
                        schema.encode_row_value_into(params, &mut val_buf)?;
                        tbl.put(key_bytes, &val_buf)
                    })
                })?;
                Ok(QueryResult::affected(1))
            }
            CompiledStatement::PointUpdate {
                table_name,
                schema,
                col_idx,
                val_param_idx,
                pk_param_idx,
            } => {
                let pk_val = params
                    .get(*pk_param_idx)
                    .ok_or_else(|| DbError::Custom("Missing primary key parameter".into()))?;
                let new_val = params
                    .get(*val_param_idx)
                    .ok_or_else(|| DbError::Custom("Missing value parameter".into()))?;
                let key_bytes = pk_val.encode_key();

                let mut tbl = tx.open_table_mut(table_name)?;
                if let Some(val_bytes) = tbl.get(&key_bytes)? {
                    let mut row = schema.decode_row_value(&val_bytes)?;
                    if *col_idx < row.len() {
                        row[*col_idx] = new_val.clone();
                        crate::constraint_eval::validate_check_constraints(schema, &row)?;
                        let updated_bytes = schema.encode_row_value(&row)?;
                        let is_pk_col = schema.pk_indices.contains(col_idx);
                        if is_pk_col {
                            let new_key = schema.encode_row_key(&row, 0);
                            if new_key != key_bytes {
                                if tbl.get(&new_key)?.is_some() {
                                    return Err(DbError::Custom(
                                        "Primary key uniqueness violation".into(),
                                    ));
                                }
                                tbl.delete(&key_bytes)?;
                                tbl.put(&new_key, &updated_bytes)?;
                                return Ok(QueryResult::affected(1));
                            }
                        }
                        tbl.put(&key_bytes, &updated_bytes)?;
                        return Ok(QueryResult::affected(1));
                    }
                }
                Ok(QueryResult::affected(0))
            }
            CompiledStatement::PointQuery {
                table_name,
                schema,
                projected_col_indices,
                result_column_names,
                pk_param_idx,
                ..
            } => {
                let pk_val = params
                    .get(*pk_param_idx)
                    .ok_or_else(|| DbError::Custom("Missing primary key parameter".into()))?;

                let tbl = tx.open_table_mut(table_name)?;
                let found = pk_val.with_key_bytes(|key_bytes| -> Result<Option<QueryResult>> {
                    let val_opt = tbl.get(key_bytes)?;
                    let Some(val_bytes) = val_opt else {
                        return Ok(None);
                    };
                    let full_row = schema.decode_row_value(&val_bytes)?;
                    let mut row = Vec::with_capacity(projected_col_indices.len());
                    for &idx in projected_col_indices {
                        row.push(full_row.get(idx).cloned().unwrap_or(Value::Null));
                    }
                    Ok(Some(QueryResult::rows(
                        result_column_names.clone(),
                        vec![row],
                    )))
                })?;
                Ok(found
                    .unwrap_or_else(|| QueryResult::rows(result_column_names.clone(), Vec::new())))
            }
            CompiledStatement::General { plan } => {
                let mut substituted = *plan.clone();
                crate::param_subst::substitute_plan_params(&mut substituted, params)?;
                self.engine.execute_plan_in_tx(tx, &substituted)
            }
        }
    }

    fn execute_point_lookup(
        schema: &TableSchema,
        projected_col_indices: &[usize],
        result_column_names: &[String],
        tbl: &tej_db::TableRef<'_, S>,
        pk_val: &Value,
    ) -> Result<QueryResult> {
        let found = pk_val.with_key_bytes(|key_bytes| -> Result<Option<QueryResult>> {
            tbl.with_value(key_bytes, |slice| -> Result<QueryResult> {
                let full_row = schema.decode_row_value(slice)?;
                let mut row = Vec::with_capacity(projected_col_indices.len());
                for &idx in projected_col_indices {
                    row.push(full_row.get(idx).cloned().unwrap_or(Value::Null));
                }
                Ok(QueryResult::rows(result_column_names.to_vec(), vec![row]))
            })
        })?;
        Ok(found.unwrap_or_else(|| QueryResult::rows(result_column_names.to_vec(), Vec::new())))
    }

    /// Execute prepared query inside an existing read transaction (zero atomic txn overhead per query).
    pub fn query_in_read_tx(
        &self,
        rx: &tej_db::ReadOnlyTx<S>,
        params: &[Value],
    ) -> Result<QueryResult> {
        match &self.compiled {
            CompiledStatement::PointQuery {
                table_name,
                schema,
                projected_col_indices,
                result_column_names,
                pk_param_idx,
                ..
            } => {
                let pk_val = params
                    .get(*pk_param_idx)
                    .ok_or_else(|| DbError::Custom("Missing primary key parameter".into()))?;

                let tbl = rx.open_table(table_name)?;
                Self::execute_point_lookup(
                    schema,
                    projected_col_indices,
                    result_column_names,
                    &tbl,
                    pk_val,
                )
            }
            CompiledStatement::General { plan } if matches!(&**plan, Plan::Select { .. }) => {
                let mut substituted = *plan.clone();
                crate::param_subst::substitute_plan_params(&mut substituted, params)?;
                self.engine.execute_select_on_rx(rx, &substituted)
            }
            _ => Err(DbError::Custom("Statement is not a query".into())),
        }
    }

    /// Execute prepared query against a pre-opened table reference for maximum point lookup speed.
    pub fn query_in_table(
        &self,
        tbl: &tej_db::TableRef<'_, S>,
        params: &[Value],
    ) -> Result<QueryResult> {
        if let CompiledStatement::PointQuery {
            schema,
            projected_col_indices,
            result_column_names,
            pk_param_idx,
            ..
        } = &self.compiled
        {
            let pk_val = params
                .get(*pk_param_idx)
                .ok_or_else(|| DbError::Custom("Missing primary key parameter".into()))?;
            Self::execute_point_lookup(
                schema,
                projected_col_indices,
                result_column_names,
                tbl,
                pk_val,
            )
        } else {
            Err(DbError::Custom("Statement is not a query".into()))
        }
    }

    /// Zero-copy point lookup returning row values directly without QueryResult or column cloning overhead.
    pub fn query_row_in_table(
        &self,
        tbl: &tej_db::TableRef<'_, S>,
        params: &[Value],
    ) -> Result<Option<Vec<Value>>> {
        if let CompiledStatement::PointQuery {
            schema,
            projected_col_indices,
            pk_param_idx,
            ..
        } = &self.compiled
        {
            let pk_val = params
                .get(*pk_param_idx)
                .ok_or_else(|| DbError::Custom("Missing primary key parameter".into()))?;

            let found = pk_val.with_key_bytes(|key_bytes| -> Result<Option<Vec<Value>>> {
                tbl.with_value(key_bytes, |slice| -> Result<Vec<Value>> {
                    let full_row = schema.decode_row_value(slice)?;
                    if projected_col_indices.len() == full_row.len()
                        && projected_col_indices
                            .iter()
                            .enumerate()
                            .all(|(i, &idx)| i == idx)
                    {
                        Ok(full_row)
                    } else {
                        let mut row = Vec::with_capacity(projected_col_indices.len());
                        for &idx in projected_col_indices {
                            row.push(full_row.get(idx).cloned().unwrap_or(Value::Null));
                        }
                        Ok(row)
                    }
                })
            })?;
            Ok(found)
        } else {
            Err(DbError::Custom("Statement is not a point query".into()))
        }
    }

    /// Execute prepared statement against a pre-opened mutable table reference with caller-provided buffer,
    /// eliminating TLS lookup and repeated allocations in tight loops.
    pub fn execute_in_table_buf(
        &self,
        tbl: &mut tej_db::TableMut<'_, '_, S>,
        params: &[Value],
        val_buf: &mut Vec<u8>,
    ) -> Result<QueryResult> {
        match &self.compiled {
            CompiledStatement::PointInsert { schema, .. } => {
                crate::constraint_eval::validate_check_constraints(schema, params)?;
                let rowid = self
                    .engine
                    .auto_rowid
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                schema.with_row_key(params, rowid, |key_bytes| -> Result<()> {
                    schema.encode_row_value_into(params, val_buf)?;
                    tbl.put(key_bytes, val_buf)
                })?;
                Ok(QueryResult::affected(1))
            }
            CompiledStatement::PointUpdate {
                schema,
                col_idx,
                val_param_idx,
                pk_param_idx,
                ..
            } => {
                let pk_val = params
                    .get(*pk_param_idx)
                    .ok_or_else(|| DbError::Custom("Missing primary key parameter".into()))?;
                let new_val = params
                    .get(*val_param_idx)
                    .ok_or_else(|| DbError::Custom("Missing value parameter".into()))?;
                let key_bytes = pk_val.encode_key();

                if let Some(val_bytes) = tbl.get(&key_bytes)? {
                    let mut row = schema.decode_row_value(&val_bytes)?;
                    if *col_idx < row.len() {
                        row[*col_idx] = new_val.clone();
                        crate::constraint_eval::validate_check_constraints(schema, &row)?;
                        let updated_bytes = schema.encode_row_value(&row)?;
                        let is_pk_col = schema.pk_indices.contains(col_idx);
                        if is_pk_col {
                            let new_key = schema.encode_row_key(&row, 0);
                            if new_key != key_bytes {
                                if tbl.get(&new_key)?.is_some() {
                                    return Err(DbError::Custom(
                                        "Primary key uniqueness violation".into(),
                                    ));
                                }
                                tbl.delete(&key_bytes)?;
                                tbl.put(&new_key, &updated_bytes)?;
                                return Ok(QueryResult::affected(1));
                            }
                        }
                        tbl.put(&key_bytes, &updated_bytes)?;
                        return Ok(QueryResult::affected(1));
                    }
                }
                Ok(QueryResult::affected(0))
            }
            CompiledStatement::PointQuery {
                schema,
                projected_col_indices,
                result_column_names,
                pk_param_idx,
                ..
            } => {
                let pk_val = params
                    .get(*pk_param_idx)
                    .ok_or_else(|| DbError::Custom("Missing primary key parameter".into()))?;
                let found = pk_val.with_key_bytes(|key_bytes| -> Result<Option<QueryResult>> {
                    let val_opt = tbl.get(key_bytes)?;
                    let Some(val_bytes) = val_opt else {
                        return Ok(None);
                    };
                    let full_row = schema.decode_row_value(&val_bytes)?;
                    let mut row = Vec::with_capacity(projected_col_indices.len());
                    for &idx in projected_col_indices {
                        row.push(full_row.get(idx).cloned().unwrap_or(Value::Null));
                    }
                    Ok(Some(QueryResult::rows(
                        result_column_names.clone(),
                        vec![row],
                    )))
                })?;
                Ok(found
                    .unwrap_or_else(|| QueryResult::rows(result_column_names.clone(), Vec::new())))
            }
            CompiledStatement::General { .. } => {
                Err(DbError::Custom("General plans require full WriteTx".into()))
            }
        }
    }

    /// Execute prepared statement against a pre-opened mutable table reference for maximum write speed.
    pub fn execute_in_table(
        &self,
        tbl: &mut tej_db::TableMut<'_, '_, S>,
        params: &[Value],
    ) -> Result<QueryResult> {
        thread_local! {
            static ROW_BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        ROW_BUF.with(|cell| {
            let mut val_buf = cell.borrow_mut();
            self.execute_in_table_buf(tbl, params, &mut val_buf)
        })
    }

    /// Execute prepared statement by beginning a dedicated transaction and committing.
    pub fn execute(&self, params: &[Value]) -> Result<QueryResult> {
        match &self.compiled {
            CompiledStatement::PointQuery {
                table_name,
                schema,
                projected_col_indices,
                result_column_names,
                pk_param_idx,
                ..
            } => {
                let pk_val = params
                    .get(*pk_param_idx)
                    .ok_or_else(|| DbError::Custom("Missing primary key parameter".into()))?;

                let rx = self.engine.db.begin_read()?;
                let tbl = rx.open_table(table_name)?;
                Self::execute_point_lookup(
                    schema,
                    projected_col_indices,
                    result_column_names,
                    &tbl,
                    pk_val,
                )
            }
            CompiledStatement::General { plan } if matches!(&**plan, Plan::Select { .. }) => {
                let mut substituted = *plan.clone();
                crate::param_subst::substitute_plan_params(&mut substituted, params)?;
                self.engine.execute_plan(substituted)
            }
            _ => {
                let mut tx = self.engine.db.begin_write()?;
                let res = self.execute_in_tx(&mut tx, params)?;
                tx.commit()?;
                Ok(res)
            }
        }
    }
}
