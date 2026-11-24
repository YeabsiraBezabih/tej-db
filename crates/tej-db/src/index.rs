use std::ops::Bound;
use tej_db_core::{KV, KeyRange, Result, Storage};
use tej_db_txn::{ReadOnlyTx, WriteTx};

/// Secondary index manager that maps `secondary_key -> primary_key`.
#[derive(Debug, Clone)]
pub struct SecondaryIndex {
    index_name: String,
}

impl SecondaryIndex {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            index_name: name.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.index_name
    }

    fn table_name(&self) -> String {
        format!("__idx:{}", self.index_name)
    }

    pub fn insert_entry<S: Storage>(
        &self,
        tx: &mut WriteTx<'_, S>,
        sec_key: &[u8],
        pri_key: &[u8],
    ) -> Result<()> {
        let mut table = tx.open_table_mut(&self.table_name())?;
        // Compound key encoding: [sec_len: 2 bytes big endian][sec_key][pri_key]
        let mut compound = Vec::with_capacity(2 + sec_key.len() + pri_key.len());
        compound.extend_from_slice(&(sec_key.len() as u16).to_be_bytes());
        compound.extend_from_slice(sec_key);
        compound.extend_from_slice(pri_key);
        table.put(&compound, b"")
    }

    pub fn lookup_primary_keys<S: Storage>(
        &self,
        rx: &ReadOnlyTx<S>,
        sec_key: &[u8],
    ) -> Result<Vec<Vec<u8>>> {
        let table = match rx.open_table(&self.table_name()) {
            Ok(t) => t,
            Err(_) => return Ok(Vec::new()),
        };

        let mut prefix = Vec::with_capacity(2 + sec_key.len());
        prefix.extend_from_slice(&(sec_key.len() as u16).to_be_bytes());
        prefix.extend_from_slice(sec_key);

        let end_bound = prefix_successor_bound(&prefix);

        let range = KeyRange {
            start: Bound::Included(prefix.clone()),
            end: end_bound,
        };

        let items: Vec<KV> = table.scan_range(&range)?;
        let mut pks = Vec::with_capacity(items.len());
        for item in items {
            if item.key.starts_with(&prefix) {
                let pk = item.key[prefix.len()..].to_vec();
                pks.push(pk);
            }
        }
        Ok(pks)
    }

    pub fn remove_entry<S: Storage>(
        &self,
        tx: &mut WriteTx<'_, S>,
        sec_key: &[u8],
        pri_key: &[u8],
    ) -> Result<bool> {
        let mut table = tx.open_table_mut(&self.table_name())?;
        let mut compound = Vec::with_capacity(2 + sec_key.len() + pri_key.len());
        compound.extend_from_slice(&(sec_key.len() as u16).to_be_bytes());
        compound.extend_from_slice(sec_key);
        compound.extend_from_slice(pri_key);
        table.delete(&compound)
    }
}

pub type QueryFilter<'a> = Box<dyn Fn(&[u8], &[u8]) -> bool + 'a>;

/// Query builder for filtering and projection over table key-values.
pub struct QueryBuilder<'a, S: Storage> {
    table: &'a tej_db_txn::TableRef<'a, S>,
    range: KeyRange,
    filter: Option<QueryFilter<'a>>,
}

impl<S: Storage> core::fmt::Debug for QueryBuilder<'_, S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("QueryBuilder")
            .field("table", &self.table.name())
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

impl<'a, S: Storage> QueryBuilder<'a, S> {
    pub fn new(table: &'a tej_db_txn::TableRef<'a, S>) -> Self {
        Self {
            table,
            range: KeyRange::all(),
            filter: None,
        }
    }

    #[must_use]
    pub fn range(mut self, range: KeyRange) -> Self {
        self.range = range;
        self
    }

    #[must_use]
    pub fn filter<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&[u8], &[u8]) -> bool + 'a,
    {
        self.filter = Some(Box::new(predicate));
        self
    }

    pub fn project<T, P>(self, projection: P) -> Result<Vec<T>>
    where
        P: Fn(&[u8], &[u8]) -> T,
    {
        let items = self.table.scan_range(&self.range)?;
        let mut results = Vec::new();
        for item in items {
            if let Some(ref f) = self.filter {
                if !f(&item.key, &item.value) {
                    continue;
                }
            }
            results.push(projection(&item.key, &item.value));
        }
        Ok(results)
    }

    pub fn execute(self) -> Result<Vec<KV>> {
        self.project(|k, v| KV::new(k.to_vec(), v.to_vec()))
    }
}

/// Compute the lexicographical upper exclusive bound for a prefix query.
/// Carries across trailing 0xFF bytes, or returns `Bound::Unbounded` if all bytes are 0xFF.
fn prefix_successor_bound(prefix: &[u8]) -> Bound<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last < 0xFF {
            end.push(last + 1);
            return Bound::Excluded(end);
        }
    }
    Bound::Unbounded
}
