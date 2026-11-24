use crate::engine::SqlEngine;
use crate::expr::evaluate_expr;
use crate::plan::Plan;
use crate::result::{QueryResult, execute_select_from_items};
use crate::schema::TableSchema;
use crate::value::Value;
use std::ops::Bound;
use std::sync::atomic::Ordering;
use tej_db::{Result, Storage, WriteTx};
use tej_db_core::{DbError, KV};

impl<S: Storage> SqlEngine<S> {
    /// Execute a planned query plan. If it is a read-only query (SELECT), a snapshot
    /// reader transaction is used; otherwise, a dedicated write transaction is opened and committed.
    pub fn execute_plan(&self, plan: Plan) -> Result<QueryResult> {
        match plan {
            Plan::Select { .. } => {
                let rx = self.db.begin_read()?;
                self.execute_select_on_rx(&rx, &plan)
            }
            dml_or_ddl => {
                let mut tx = self.db.begin_write()?;
                let res = self.execute_plan_in_tx(&mut tx, &dml_or_ddl)?;
                tx.commit()?;
                Ok(res)
            }
        }
    }

    /// Execute a SELECT plan against an existing read-only transaction.
    pub(crate) fn execute_select_on_rx(
        &self,
        rx: &tej_db::ReadOnlyTx<S>,
        plan: &Plan,
    ) -> Result<QueryResult> {
        if let Plan::Select {
            table,
            projection,
            key_range,
            filter,
            limit,
        } = plan
        {
            let schema = self
                .get_schema(table)?
                .ok_or_else(|| DbError::TableNotFound(table.clone()))?;

            let tbl = rx.open_table(table)?;

            // Fast-path O(log N) point lookup when key range has identical included bounds
            let is_point_lookup = match (&key_range.start, &key_range.end) {
                (Bound::Included(k1), Bound::Included(k2)) if k1 == k2 => Some(k1.as_slice()),
                _ => None,
            };

            let items = if let Some(pk) = is_point_lookup {
                tbl.get(pk)?
                    .map(|val| {
                        vec![KV {
                            key: pk.to_vec(),
                            value: val,
                        }]
                    })
                    .unwrap_or_default()
            } else {
                tbl.scan_range(key_range)?
            };

            execute_select_from_items(&schema, projection, filter.as_ref(), *limit, items)
        } else {
            Err(DbError::Custom("Expected Plan::Select".into()))
        }
    }

    /// Execute a planned query plan inside an existing write transaction without committing.
    pub fn execute_plan_in_tx(&self, tx: &mut WriteTx<'_, S>, plan: &Plan) -> Result<QueryResult> {
        match plan {
            Plan::CreateTable { schema } => {
                let table_name = schema.name.clone();
                // 1. Create table in database
                tx.create_table(&table_name)?;
                let _ = tx.create_table(TableSchema::SQL_CATALOG_TABLE);

                // 2. Persist schema in dedicated system catalog table
                let cat_key = TableSchema::catalog_key(&table_name);
                let cat_bytes = bincode::serialize(schema).map_err(|e| {
                    DbError::Serialization(format!("Catalog schema serialization failed: {e}"))
                })?;
                let mut cat_table = tx.open_table_mut(TableSchema::SQL_CATALOG_TABLE)?;
                cat_table.put(&cat_key, &cat_bytes)?;

                // 3. Cache schema
                self.schemas
                    .write()
                    .insert(table_name.to_ascii_lowercase(), schema.clone());
                Ok(QueryResult::affected(0))
            }
            Plan::Insert {
                table,
                columns,
                rows,
                on_conflict,
                returning,
            } => {
                let schema = self
                    .get_schema(table)?
                    .ok_or_else(|| DbError::TableNotFound(table.clone()))?;

                let mut inserted_count = 0;
                let mut returned_rows = Vec::new();

                for expr_row in rows {
                    let mut row_values = vec![Value::Null; schema.columns.len()];

                    if columns.is_empty() {
                        // Align by schema column order
                        for (i, expr) in expr_row.iter().enumerate() {
                            if i < row_values.len() {
                                row_values[i] = evaluate_expr(expr, &schema, &[])?;
                            }
                        }
                    } else {
                        // Align by named columns
                        for (col_name, expr) in columns.iter().zip(expr_row.iter()) {
                            if let Some(idx) = schema.column_index(col_name) {
                                row_values[idx] = evaluate_expr(expr, &schema, &[])?;
                            }
                        }
                    }

                    for (i, val) in row_values.iter_mut().enumerate() {
                        if i < schema.columns.len() {
                            *val = val.coerce(&schema.columns[i].data_type)?;
                        }
                    }

                    // 1. Validate CHECK constraints
                    crate::constraint_eval::validate_check_constraints(&schema, &row_values)?;

                    // 2. Validate Foreign Keys
                    crate::constraint_eval::validate_foreign_keys_on_insert(
                        &schema,
                        &row_values,
                        tx,
                        |t| self.get_schema(t),
                    )?;

                    let rowid = self.auto_rowid.fetch_add(1, Ordering::SeqCst);
                    let key_bytes = schema.encode_row_key(&row_values, rowid);
                    if !schema.pk_indices.is_empty()
                        && tx.get_table_key(table, &key_bytes)?.is_some()
                    {
                        match on_conflict {
                            Some(crate::upsert::OnConflict::DoNothing) => {
                                continue;
                            }
                            Some(crate::upsert::OnConflict::DoUpdate { assignments, .. }) => {
                                for (col_name, expr) in assignments {
                                    if let Some(idx) = schema.column_index(col_name) {
                                        let new_val = evaluate_expr(expr, &schema, &row_values)?;
                                        row_values[idx] =
                                            new_val.coerce(&schema.columns[idx].data_type)?;
                                    }
                                }
                                crate::constraint_eval::validate_check_constraints(
                                    &schema,
                                    &row_values,
                                )?;
                                let val_bytes = schema.encode_row_value(&row_values)?;
                                tx.put_table_key(table, &key_bytes, &val_bytes)?;
                                inserted_count += 1;
                                if let Some(ret) = returning {
                                    returned_rows.push(Self::project_returning_row(
                                        &schema,
                                        &ret.items,
                                        &row_values,
                                    )?);
                                }
                                continue;
                            }
                            None => {
                                return Err(DbError::Custom(format!(
                                    "Duplicate primary key constraint violation in table '{table}'"
                                )));
                            }
                        }
                    }
                    let val_bytes = schema.encode_row_value(&row_values)?;
                    tx.put_table_key(table, &key_bytes, &val_bytes)?;
                    inserted_count += 1;

                    if let Some(ret) = returning {
                        returned_rows.push(Self::project_returning_row(
                            &schema,
                            &ret.items,
                            &row_values,
                        )?);
                    }
                }

                if let Some(ret) = returning {
                    let col_names = ret
                        .items
                        .iter()
                        .map(|it| match it {
                            crate::plan::ProjectionItem::Column(c) => c.clone(),
                            crate::plan::ProjectionItem::Expr { name, .. } => name.clone(),
                            crate::plan::ProjectionItem::Wildcard => "*".into(),
                        })
                        .collect();
                    Ok(QueryResult::rows(col_names, returned_rows))
                } else {
                    Ok(QueryResult::affected(inserted_count))
                }
            }
            Plan::Select {
                table,
                projection,
                key_range,
                filter,
                limit,
            } => {
                let schema = self
                    .get_schema(table)?
                    .ok_or_else(|| DbError::TableNotFound(table.clone()))?;

                let tbl = tx.open_table_mut(table)?;
                let is_point_lookup = match (&key_range.start, &key_range.end) {
                    (Bound::Included(k1), Bound::Included(k2)) if k1 == k2 => Some(k1.as_slice()),
                    _ => None,
                };

                let items = if let Some(pk) = is_point_lookup {
                    tbl.get(pk)?
                        .map(|val| {
                            vec![KV {
                                key: pk.to_vec(),
                                value: val,
                            }]
                        })
                        .unwrap_or_default()
                } else {
                    tbl.scan_range(key_range)?
                };

                execute_select_from_items(&schema, projection, filter.as_ref(), *limit, items)
            }
            Plan::Update {
                table,
                assignments,
                key_range,
                filter,
                returning,
            } => {
                let schema = self
                    .get_schema(table)?
                    .ok_or_else(|| DbError::TableNotFound(table.clone()))?;

                let is_point_lookup = match (&key_range.start, &key_range.end) {
                    (Bound::Included(k1), Bound::Included(k2)) if k1 == k2 => Some(k1.as_slice()),
                    _ => None,
                };

                let mut tbl = tx.open_table_mut(table)?;
                let mut updated_count = 0;
                let mut returned_rows = Vec::new();

                if let Some(pk) = is_point_lookup {
                    if let Some(val_bytes) = tbl.get(pk)? {
                        let mut row = schema.decode_row_value(&val_bytes)?;
                        let mut matches = true;
                        if let Some(expr) = filter {
                            if evaluate_expr(expr, &schema, &row)? != Value::Boolean(true) {
                                matches = false;
                            }
                        }
                        if matches {
                            for (col_name, expr) in assignments {
                                if let Some(idx) = schema.column_index(col_name) {
                                    let new_val = evaluate_expr(expr, &schema, &row)?;
                                    row[idx] = new_val.coerce(&schema.columns[idx].data_type)?;
                                }
                            }
                            crate::constraint_eval::validate_check_constraints(&schema, &row)?;
                            let updated_val = schema.encode_row_value(&row)?;
                            let new_key = if schema.pk_indices.is_empty() {
                                pk.to_vec()
                            } else {
                                schema.encode_row_key(&row, 0)
                            };
                            if new_key == pk {
                                tbl.put(pk, &updated_val)?;
                            } else {
                                if tbl.get(&new_key)?.is_some() {
                                    return Err(DbError::Custom(
                                        "Primary key uniqueness violation".into(),
                                    ));
                                }
                                tbl.delete(pk)?;
                                tbl.put(&new_key, &updated_val)?;
                            }
                            updated_count = 1;
                            if let Some(ret) = returning {
                                returned_rows
                                    .push(Self::project_returning_row(&schema, &ret.items, &row)?);
                            }
                        }
                    }
                } else {
                    let items = tbl.scan_range(key_range)?;
                    for item in items {
                        let mut row = schema.decode_row_value(&item.value)?;

                        if let Some(expr) = filter {
                            let matches = evaluate_expr(expr, &schema, &row)?;
                            if matches != Value::Boolean(true) {
                                continue;
                            }
                        }

                        for (col_name, expr) in assignments {
                            if let Some(idx) = schema.column_index(col_name) {
                                let new_val = evaluate_expr(expr, &schema, &row)?;
                                row[idx] = new_val.coerce(&schema.columns[idx].data_type)?;
                            }
                        }

                        crate::constraint_eval::validate_check_constraints(&schema, &row)?;
                        let updated_val = schema.encode_row_value(&row)?;
                        let new_key = if schema.pk_indices.is_empty() {
                            item.key.clone()
                        } else {
                            schema.encode_row_key(&row, 0)
                        };
                        if new_key == item.key {
                            tbl.put(&item.key, &updated_val)?;
                        } else {
                            if tbl.get(&new_key)?.is_some() {
                                return Err(DbError::Custom(
                                    "Primary key uniqueness violation".into(),
                                ));
                            }
                            tbl.delete(&item.key)?;
                            tbl.put(&new_key, &updated_val)?;
                        }
                        updated_count += 1;
                        if let Some(ret) = returning {
                            returned_rows
                                .push(Self::project_returning_row(&schema, &ret.items, &row)?);
                        }
                    }
                }

                if let Some(ret) = returning {
                    let col_names = ret
                        .items
                        .iter()
                        .map(|it| match it {
                            crate::plan::ProjectionItem::Column(c) => c.clone(),
                            crate::plan::ProjectionItem::Expr { name, .. } => name.clone(),
                            crate::plan::ProjectionItem::Wildcard => "*".into(),
                        })
                        .collect();
                    Ok(QueryResult::rows(col_names, returned_rows))
                } else {
                    Ok(QueryResult::affected(updated_count))
                }
            }
            Plan::Delete {
                table,
                key_range,
                filter,
                returning,
            } => {
                let schema = self
                    .get_schema(table)?
                    .ok_or_else(|| DbError::TableNotFound(table.clone()))?;

                let is_point_lookup = match (&key_range.start, &key_range.end) {
                    (Bound::Included(k1), Bound::Included(k2)) if k1 == k2 => Some(k1.as_slice()),
                    _ => None,
                };

                let mut tbl = tx.open_table_mut(table)?;
                let mut deleted_count = 0;
                let mut returned_rows = Vec::new();

                if let Some(pk) = is_point_lookup {
                    if let Some(val_bytes) = tbl.get(pk)? {
                        let row = schema.decode_row_value(&val_bytes)?;
                        let mut matches = true;
                        if let Some(expr) = filter {
                            if evaluate_expr(expr, &schema, &row)? != Value::Boolean(true) {
                                matches = false;
                            }
                        }
                        if matches && tbl.delete(pk)? {
                            deleted_count = 1;
                            if let Some(ret) = returning {
                                returned_rows
                                    .push(Self::project_returning_row(&schema, &ret.items, &row)?);
                            }
                        }
                    }
                } else {
                    let items = tbl.scan_range(key_range)?;
                    for item in items {
                        let row = schema.decode_row_value(&item.value)?;
                        if let Some(expr) = filter {
                            let matches = evaluate_expr(expr, &schema, &row)?;
                            if matches != Value::Boolean(true) {
                                continue;
                            }
                        }

                        tbl.delete(&item.key)?;
                        deleted_count += 1;
                        if let Some(ret) = returning {
                            returned_rows
                                .push(Self::project_returning_row(&schema, &ret.items, &row)?);
                        }
                    }
                }

                if let Some(ret) = returning {
                    let col_names = ret
                        .items
                        .iter()
                        .map(|it| match it {
                            crate::plan::ProjectionItem::Column(c) => c.clone(),
                            crate::plan::ProjectionItem::Expr { name, .. } => name.clone(),
                            crate::plan::ProjectionItem::Wildcard => "*".into(),
                        })
                        .collect();
                    Ok(QueryResult::rows(col_names, returned_rows))
                } else {
                    Ok(QueryResult::affected(deleted_count))
                }
            }
            Plan::Savepoint { name } => {
                tx.savepoint(name);
                Ok(QueryResult::affected(0))
            }
            Plan::RollbackToSavepoint { name } => {
                tx.rollback_to_savepoint(name)?;
                Ok(QueryResult::affected(0))
            }
            Plan::ReleaseSavepoint { name } => {
                tx.release_savepoint(name)?;
                Ok(QueryResult::affected(0))
            }
        }
    }

    fn project_returning_row(
        schema: &TableSchema,
        items: &[crate::plan::ProjectionItem],
        row: &[Value],
    ) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            match it {
                crate::plan::ProjectionItem::Wildcard => {
                    out.extend_from_slice(row);
                }
                crate::plan::ProjectionItem::Column(name) => {
                    let idx = schema
                        .column_index(name)
                        .ok_or_else(|| DbError::Custom(format!("Column not found: {name}")))?;
                    out.push(row.get(idx).cloned().unwrap_or(Value::Null));
                }
                crate::plan::ProjectionItem::Expr { expr, .. } => {
                    out.push(evaluate_expr(expr, schema, row)?);
                }
            }
        }
        Ok(out)
    }
}
