use crate::manager::TxManager;
use crate::table::TableMut;
use smallvec::SmallVec;
use std::fmt;
use std::sync::Arc;
use tej_db_btree::BTreeIndex;
use tej_db_core::{ChangeSet, DbError, Index, KeyRange, Page, PageId, Result, Storage, TxId};

pub(crate) struct TableMutationState<S: Storage> {
    pub(crate) name: String,
    pub(crate) snapshot_root: PageId,
    pub(crate) btree: BTreeIndex<S>,
    pub(crate) pending_writes: Vec<tej_db_btree::RawBatchItem>,
    pub(crate) is_sorted: bool,
    pub(crate) tx_pages: tej_db_btree::TxPageSet,
}

impl<S: Storage> fmt::Debug for TableMutationState<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TableMutationState")
            .field("name", &self.name)
            .field("snapshot_root", &self.snapshot_root)
            .field("pending_writes", &self.pending_writes.len())
            .finish_non_exhaustive()
    }
}

/// An optimistic concurrency control (OCC) write transaction.
pub struct WriteTx<'a, S: Storage> {
    pub(crate) manager: &'a TxManager<S>,
    pub tx_id: TxId,
    pub snapshot_root: PageId,
    pub snapshot_tx_id: TxId,
    pub(crate) btree: BTreeIndex<S>,
    pub(crate) pending_writes: Vec<tej_db_btree::RawBatchItem>,
    pub modified_tables: SmallVec<[String; 2]>,
    pub(crate) is_sorted: bool,
    pub(crate) tx_pages: tej_db_btree::TxPageSet,
    pub(crate) has_hooks: bool,
    pub(crate) changes: ChangeSet,
    pub(crate) committed: bool,
    pub(crate) table_states: SmallVec<[TableMutationState<S>; 2]>,
    pub(crate) created_tables: SmallVec<[String; 2]>,
    pub(crate) dropped_tables: SmallVec<[String; 2]>,
    pub(crate) savepoints: Vec<crate::savepoint::SavepointSnapshot>,
}

impl<S: Storage> fmt::Debug for WriteTx<'_, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteTx")
            .field("tx_id", &self.tx_id)
            .field("committed", &self.committed)
            .field("pending_writes", &self.pending_writes.len())
            .field("table_states", &self.table_states.len())
            .field("modified_tables", &self.modified_tables)
            .field("is_sorted", &self.is_sorted)
            .finish_non_exhaustive()
    }
}

impl<'tx, S: Storage> WriteTx<'tx, S> {
    pub(crate) fn new(
        manager: &'tx TxManager<S>,
        tx_id: TxId,
        snapshot_root: PageId,
        snapshot_tx_id: TxId,
        btree: BTreeIndex<S>,
    ) -> Self {
        let has_hooks = manager.has_hooks();
        Self {
            manager,
            tx_id,
            snapshot_root,
            snapshot_tx_id,
            btree,
            pending_writes: Vec::with_capacity(128),
            modified_tables: SmallVec::new(),
            is_sorted: true,
            tx_pages: tej_db_btree::TxPageSet::with_capacity_and_hasher(
                16,
                tej_db_pager::FastPageIdBuildHasher,
            ),
            has_hooks,
            changes: ChangeSet::default(),
            committed: false,
            table_states: SmallVec::new(),
            created_tables: SmallVec::new(),
            dropped_tables: SmallVec::new(),
            savepoints: Vec::new(),
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        self.pending_writes.reserve(additional);
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        for (k, val_opt, is_overflow) in self.pending_writes.iter().rev() {
            if k.as_slice() == key {
                if let Some(val) = val_opt {
                    if *is_overflow {
                        let total_len = u64::from_le_bytes(val[0..8].try_into().unwrap_or([0; 8]));
                        let first_pid =
                            PageId(u64::from_le_bytes(val[8..16].try_into().unwrap_or([0; 8])));
                        return self
                            .btree
                            .read_overflow_chain(first_pid, total_len)
                            .map(Some);
                    }
                    return Ok(Some(val.to_vec()));
                }
                return Ok(None);
            }
        }
        self.btree.get(key)
    }

    pub fn put(&mut self, key: &[u8], val: &[u8]) -> Result<()> {
        if key.len() > tej_db_core::MAX_KEY_SIZE {
            return Err(DbError::KeyTooLarge(key.len()));
        }
        let is_ov = val.len() > tej_db_core::MAX_INLINE_VAL_SIZE;
        let item = if is_ov {
            let pid = self.btree.write_overflow_chain(val, &mut self.tx_pages)?;
            let mut rb = tej_db_btree::InlineVal::new();
            rb.extend_from_slice(&(val.len() as u64).to_le_bytes());
            rb.extend_from_slice(&pid.0.to_le_bytes());
            (tej_db_btree::InlineKey::from_slice(key), Some(rb), true)
        } else {
            (
                tej_db_btree::InlineKey::from_slice(key),
                Some(tej_db_btree::InlineVal::from_slice(val)),
                false,
            )
        };

        if self.is_sorted {
            if let Some(last) = self.pending_writes.last() {
                if key <= last.0.as_slice() {
                    self.is_sorted = false;
                }
            }
        }
        self.pending_writes.push(item);

        if self.has_hooks {
            self.changes.writes.push((key.to_vec(), val.to_vec()));
        }
        Ok(())
    }

    pub fn put_batch(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        for (k, v) in items {
            self.put(k, v)?;
        }
        Ok(())
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<bool> {
        let exists = self.get(key)?.is_some();
        if exists {
            let item = (tej_db_btree::InlineKey::from_slice(key), None, false);
            if self.is_sorted {
                if let Some(last) = self.pending_writes.last() {
                    if key <= last.0.as_slice() {
                        self.is_sorted = false;
                    }
                }
            }
            self.pending_writes.push(item);
            if self.has_hooks {
                self.changes.deletes.push(key.to_vec());
            }
        }
        Ok(exists)
    }

    pub(crate) fn ensure_table_state(&mut self, name: &str) -> Result<usize> {
        if let Some(pos) = self.table_states.iter().position(|s| s.name == name) {
            return Ok(pos);
        }

        if let Some(pos) = self.dropped_tables.iter().position(|t| t == name) {
            self.dropped_tables.swap_remove(pos);
        }

        if let Some(root_id) = self.manager.catalog.get_table_root(name) {
            let btree = BTreeIndex::new(Arc::clone(&self.manager.cache), Some(root_id))?;
            let tx_pages = tej_db_btree::TxPageSet::with_capacity_and_hasher(
                16,
                tej_db_pager::FastPageIdBuildHasher,
            );
            let state = TableMutationState {
                name: name.to_string(),
                snapshot_root: root_id,
                btree,
                pending_writes: Vec::with_capacity(32),
                is_sorted: true,
                tx_pages,
            };
            self.table_states.push(state);
            if !self.modified_tables.iter().any(|t| t == name) {
                self.modified_tables.push(name.to_string());
            }
            Ok(self.table_states.len() - 1)
        } else {
            let root_id = self.manager.cache.allocate_page()?;
            let leaf_page = Page::new_leaf(root_id, PageId::INVALID);
            self.manager.cache.write_page(leaf_page)?;

            let mut tx_pages = tej_db_btree::TxPageSet::with_capacity_and_hasher(
                16,
                tej_db_pager::FastPageIdBuildHasher,
            );
            tx_pages.insert(root_id);

            let btree = BTreeIndex::new(Arc::clone(&self.manager.cache), Some(root_id))?;
            let state = TableMutationState {
                name: name.to_string(),
                snapshot_root: root_id,
                btree,
                pending_writes: Vec::with_capacity(32),
                is_sorted: true,
                tx_pages,
            };
            self.table_states.push(state);
            if !self.created_tables.iter().any(|t| t == name) {
                self.created_tables.push(name.to_string());
            }
            if !self.modified_tables.iter().any(|t| t == name) {
                self.modified_tables.push(name.to_string());
            }
            Ok(self.table_states.len() - 1)
        }
    }

    pub fn put_table_key(&mut self, table: &str, key: &[u8], val: &[u8]) -> Result<()> {
        let idx = self.ensure_table_state(table)?;
        self.put_table_key_idx(idx, key, val)
    }

    pub fn put_table_key_idx(&mut self, idx: usize, key: &[u8], val: &[u8]) -> Result<()> {
        if key.len() > tej_db_core::MAX_KEY_SIZE {
            return Err(DbError::KeyTooLarge(key.len()));
        }
        let state = &mut self.table_states[idx];

        let is_ov = val.len() > tej_db_core::MAX_INLINE_VAL_SIZE;
        let item = if is_ov {
            let pid = state.btree.write_overflow_chain(val, &mut state.tx_pages)?;
            let mut rb = tej_db_btree::InlineVal::new();
            rb.extend_from_slice(&(val.len() as u64).to_le_bytes());
            rb.extend_from_slice(&pid.0.to_le_bytes());
            (tej_db_btree::InlineKey::from_slice(key), Some(rb), true)
        } else {
            (
                tej_db_btree::InlineKey::from_slice(key),
                Some(tej_db_btree::InlineVal::from_slice(val)),
                false,
            )
        };

        if state.is_sorted {
            if let Some(last) = state.pending_writes.last() {
                if key <= last.0.as_slice() {
                    state.is_sorted = false;
                }
            }
        }
        state.pending_writes.push(item);

        if self.has_hooks {
            self.changes.writes.push((key.to_vec(), val.to_vec()));
        }
        Ok(())
    }

    pub fn get_table_key(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if self.dropped_tables.iter().any(|t| t == table) {
            return Ok(None);
        }

        if let Some(state) = self.table_states.iter().find(|s| s.name == table) {
            for (k, val_opt, is_overflow) in state.pending_writes.iter().rev() {
                if k.as_slice() == key {
                    if let Some(val) = val_opt {
                        if *is_overflow {
                            let total_len =
                                u64::from_le_bytes(val[0..8].try_into().unwrap_or([0; 8]));
                            let first_pid =
                                PageId(u64::from_le_bytes(val[8..16].try_into().unwrap_or([0; 8])));
                            return state
                                .btree
                                .read_overflow_chain(first_pid, total_len)
                                .map(Some);
                        }
                        return Ok(Some(val.to_vec()));
                    }
                    return Ok(None);
                }
            }
            return state.btree.get(key);
        }

        if let Some(root_id) = self.manager.catalog.get_table_root(table) {
            let btree = BTreeIndex::new(Arc::clone(&self.manager.cache), Some(root_id))?;
            btree.get(key)
        } else {
            Ok(None)
        }
    }

    pub fn delete_table_key(&mut self, table: &str, key: &[u8]) -> Result<bool> {
        let exists = self.get_table_key(table, key)?.is_some();
        if exists {
            let idx = self.ensure_table_state(table)?;
            let state = &mut self.table_states[idx];
            let item = (tej_db_btree::InlineKey::from_slice(key), None, false);
            if state.is_sorted {
                if let Some(last) = state.pending_writes.last() {
                    if key <= last.0.as_slice() {
                        state.is_sorted = false;
                    }
                }
            }
            state.pending_writes.push(item);

            if self.has_hooks {
                self.changes.deletes.push(key.to_vec());
            }
        }
        Ok(exists)
    }

    pub fn put_table_batch(&mut self, table: &str, items: &[(&[u8], &[u8])]) -> Result<()> {
        for (k, v) in items {
            self.put_table_key(table, k, v)?;
        }
        Ok(())
    }

    pub fn scan_table_range(&self, table: &str, range: &KeyRange) -> Result<Vec<tej_db_core::KV>> {
        if self.dropped_tables.iter().any(|t| t == table) {
            return Ok(Vec::new());
        }

        if let Some(state) = self.table_states.iter().find(|s| s.name == table) {
            state.btree.scan_range(range)
        } else if let Some(root_id) = self.manager.catalog.get_table_root(table) {
            let btree = BTreeIndex::new(Arc::clone(&self.manager.cache), Some(root_id))?;
            btree.scan_range(range)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn create_table(&mut self, name: &str) -> Result<()> {
        let already_exists = (self.manager.catalog.get_table_root(name).is_some()
            && !self.dropped_tables.iter().any(|t| t == name))
            || self.table_states.iter().any(|s| s.name == name);

        if already_exists {
            return Err(DbError::TableAlreadyExists(name.to_string()));
        }
        self.ensure_table_state(name)?;
        Ok(())
    }

    pub fn drop_table(&mut self, name: &str) -> Result<bool> {
        let in_catalog = self.manager.catalog.get_table_root(name).is_some()
            && !self.dropped_tables.iter().any(|t| t == name);
        let in_states = self.table_states.iter().any(|s| s.name == name);

        if !in_catalog && !in_states {
            return Ok(false);
        }

        if let Some(pos) = self.table_states.iter().position(|s| s.name == name) {
            let state = self.table_states.swap_remove(pos);
            for &pid in &state.tx_pages {
                self.manager.cache.free_page(pid);
            }
        }
        if let Some(pos) = self.created_tables.iter().position(|t| t == name) {
            self.created_tables.swap_remove(pos);
        }
        if !self.dropped_tables.iter().any(|t| t == name) {
            self.dropped_tables.push(name.to_string());
        }
        if !self.modified_tables.iter().any(|t| t == name) {
            self.modified_tables.push(name.to_string());
        }
        Ok(true)
    }

    pub fn open_table_mut<'b>(&'b mut self, name: &str) -> Result<TableMut<'b, 'tx, S>> {
        let idx = self.ensure_table_state(name)?;
        Ok(TableMut::with_idx(name, idx, self))
    }

    pub fn list_tables(&self) -> Result<Vec<String>> {
        let mut tables = self.manager.catalog.list_tables();
        for s in &self.table_states {
            if !tables.contains(&s.name) {
                tables.push(s.name.clone());
            }
        }
        tables.retain(|t| !self.dropped_tables.iter().any(|d| d == t));
        tables.retain(|t| t != crate::sharding::SHARDS_META_TABLE && !t.contains("$shard$"));
        if let Ok(kvs) = self.scan_table_range(crate::sharding::SHARDS_META_TABLE, &KeyRange::all())
        {
            for kv in kvs {
                if let Ok(name) = String::from_utf8(kv.key) {
                    tables.push(name);
                }
            }
        }
        tables.sort();
        tables.dedup();
        Ok(tables)
    }

    pub fn rollback(mut self) -> Result<()> {
        self.pending_writes.clear();
        self.modified_tables.clear();
        for &pid in &self.tx_pages {
            self.manager.cache.free_page(pid);
        }
        for state in &mut self.table_states {
            for &pid in &state.tx_pages {
                self.manager.cache.free_page(pid);
            }
        }
        self.table_states.clear();
        self.created_tables.clear();
        self.dropped_tables.clear();
        self.savepoints.clear();
        self.committed = true;
        Ok(())
    }

    /// Creates a named savepoint capturing current transaction state.
    pub fn savepoint(&mut self, name: &str) {
        let snap = crate::savepoint::SavepointSnapshot::capture(name, self);
        self.savepoints.push(snap);
    }

    /// Rolls back all mutations performed after the named savepoint was set.
    pub fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let pos = self
            .savepoints
            .iter()
            .rposition(|s| s.name == name)
            .ok_or_else(|| DbError::Custom(format!("Savepoint not found: {name}")))?;
        let snap = self.savepoints[pos].clone();
        snap.rollback_tx(self)?;
        self.savepoints.truncate(pos + 1);
        Ok(())
    }

    /// Releases a named savepoint and any savepoints created after it without rolling back.
    pub fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let pos = self
            .savepoints
            .iter()
            .rposition(|s| s.name == name)
            .ok_or_else(|| DbError::Custom(format!("Savepoint not found: {name}")))?;
        self.savepoints.truncate(pos);
        Ok(())
    }
}

impl<S: Storage> Drop for WriteTx<'_, S> {
    fn drop(&mut self) {
        if !self.committed {
            for &pid in &self.tx_pages {
                self.manager.cache.free_page(pid);
            }
            for state in &self.table_states {
                for &pid in &state.tx_pages {
                    self.manager.cache.free_page(pid);
                }
            }
            tracing::debug!("Write transaction {} aborted/rolled back", self.tx_id);
        }
        self.manager.cache.release_reader();
    }
}
