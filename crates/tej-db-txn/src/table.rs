use crate::{ReadOnlyTx, WriteTx};
use tej_db_btree::BTreeIndex;
use tej_db_core::{Index, KV, KeyRange, Result, Storage};

/// A read-only view of a named table/keyspace within a transaction snapshot.
#[derive(Debug)]
pub struct TableRef<'a, S: Storage> {
    name: String,
    btree: BTreeIndex<S>,
    _tx: &'a ReadOnlyTx<S>,
}

impl<'a, S: Storage> TableRef<'a, S> {
    pub fn new(name: impl Into<String>, btree: BTreeIndex<S>, tx: &'a ReadOnlyTx<S>) -> Self {
        Self {
            name: name.into(),
            btree,
            _tx: tx,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.btree.get(key)
    }

    /// Zero-allocation, high-throughput value visitor executing against in-page memory.
    pub fn with_value<R>(
        &self,
        key: &[u8],
        f: impl FnOnce(&[u8]) -> Result<R>,
    ) -> Result<Option<R>> {
        self.btree.with_value(key, f)
    }

    pub fn scan_range(&self, range: &KeyRange) -> Result<Vec<KV>> {
        self.btree.scan_range(range)
    }

    /// Zero-allocation, high-throughput streaming cursor scan visiting page references directly.
    pub fn scan_range_visitor(
        &self,
        range: &KeyRange,
        visitor: impl FnMut(&[u8], &[u8]) -> Result<bool>,
    ) -> Result<()> {
        self.btree.scan_range_visitor(range, visitor)
    }

    pub fn scan_all(&self) -> Result<Vec<KV>> {
        self.btree.scan_range(&KeyRange::all())
    }
}

/// A mutable view of a named table/keyspace within a write transaction.
#[derive(Debug)]
pub struct TableMut<'a, 'tx, S: Storage> {
    name: String,
    idx: usize,
    tx: &'a mut WriteTx<'tx, S>,
}

impl<'a, 'tx, S: Storage> TableMut<'a, 'tx, S> {
    pub fn new(name: impl Into<String>, tx: &'a mut WriteTx<'tx, S>) -> Self {
        let n = name.into();
        let idx = tx.ensure_table_state(&n).unwrap_or(0);
        Self { name: n, idx, tx }
    }

    pub fn with_idx(name: impl Into<String>, idx: usize, tx: &'a mut WriteTx<'tx, S>) -> Self {
        Self {
            name: name.into(),
            idx,
            tx,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn reserve(&mut self, additional: usize) {
        if let Some(state) = self.tx.table_states.get_mut(self.idx) {
            state.pending_writes.reserve(additional);
        }
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.tx.get_table_key(&self.name, key)
    }

    pub fn put(&mut self, key: &[u8], val: &[u8]) -> Result<()> {
        self.tx.put_table_key_idx(self.idx, key, val)
    }

    pub fn put_batch(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        self.tx.put_table_batch(&self.name, items)
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<bool> {
        self.tx.delete_table_key(&self.name, key)
    }

    pub fn scan_range(&self, range: &KeyRange) -> Result<Vec<KV>> {
        self.tx.scan_table_range(&self.name, range)
    }

    pub fn scan_all(&self) -> Result<Vec<KV>> {
        self.tx.scan_table_range(&self.name, &KeyRange::all())
    }
}
