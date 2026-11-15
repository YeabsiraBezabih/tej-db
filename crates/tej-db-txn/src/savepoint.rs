use tej_db_core::{PageId, Result, Storage};

/// State snapshot of a single table within a savepoint.
#[derive(Debug, Clone)]
pub struct TableSavepointState {
    pub name: String,
    pub root_page: PageId,
    pub pending_writes_len: usize,
    pub is_sorted: bool,
}

/// A transaction savepoint marker capturing transactional mutation state.
#[derive(Debug, Clone)]
pub struct SavepointSnapshot {
    pub name: String,
    pub default_pending_len: usize,
    pub default_root: PageId,
    pub default_is_sorted: bool,
    pub table_states: Vec<TableSavepointState>,
    pub created_tables_len: usize,
    pub dropped_tables_len: usize,
    pub modified_tables_len: usize,
    pub changes_writes_len: usize,
    pub changes_deletes_len: usize,
}

impl SavepointSnapshot {
    /// Captures the current mutation state of a `WriteTx`.
    #[must_use]
    pub fn capture<S: Storage>(name: &str, tx: &crate::write_tx::WriteTx<'_, S>) -> Self {
        let table_states = tx
            .table_states
            .iter()
            .map(|ts| TableSavepointState {
                name: ts.name.clone(),
                root_page: ts.btree.root_id(),
                pending_writes_len: ts.pending_writes.len(),
                is_sorted: ts.is_sorted,
            })
            .collect();

        Self {
            name: name.to_string(),
            default_pending_len: tx.pending_writes.len(),
            default_root: tx.btree.root_id(),
            default_is_sorted: tx.is_sorted,
            table_states,
            created_tables_len: tx.created_tables.len(),
            dropped_tables_len: tx.dropped_tables.len(),
            modified_tables_len: tx.modified_tables.len(),
            changes_writes_len: tx.changes.writes.len(),
            changes_deletes_len: tx.changes.deletes.len(),
        }
    }

    /// Reverts a `WriteTx` back to this savepoint snapshot.
    pub fn rollback_tx<S: Storage>(&self, tx: &mut crate::write_tx::WriteTx<'_, S>) -> Result<()> {
        // 1. Revert default table mutations
        tx.pending_writes.truncate(self.default_pending_len);
        tx.btree.set_root_id(self.default_root);
        tx.is_sorted = self.default_is_sorted;

        // 2. Revert named table states
        tx.table_states
            .retain(|ts| self.table_states.iter().any(|saved| saved.name == ts.name));

        for saved in &self.table_states {
            if let Some(ts) = tx.table_states.iter_mut().find(|t| t.name == saved.name) {
                ts.pending_writes.truncate(saved.pending_writes_len);
                ts.btree.set_root_id(saved.root_page);
                ts.is_sorted = saved.is_sorted;
            }
        }

        // 3. Revert DDL and hooks
        tx.created_tables.truncate(self.created_tables_len);
        tx.dropped_tables.truncate(self.dropped_tables_len);
        tx.modified_tables.truncate(self.modified_tables_len);
        tx.changes.writes.truncate(self.changes_writes_len);
        tx.changes.deletes.truncate(self.changes_deletes_len);

        Ok(())
    }
}
