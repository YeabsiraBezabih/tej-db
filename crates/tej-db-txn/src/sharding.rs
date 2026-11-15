use crate::{ReadOnlyTx, TableRef, WriteTx};
use std::ops::Bound;
use tej_db_core::{DbError, KV, KeyRange, Result, Storage};

pub const SHARDS_META_TABLE: &str = "__tej_db_shards__";

#[inline]
#[must_use]
pub fn shard_name(table_name: &str, shard_idx: usize) -> String {
    format!("{table_name}$shard${shard_idx}")
}

/// Metadata descriptor for a range-partitioned / sharded table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardedTableDescriptor {
    pub name: String,
    pub num_shards: usize,
    pub boundaries: Vec<Vec<u8>>,
}

impl ShardedTableDescriptor {
    pub fn new(
        name: impl Into<String>,
        num_shards: usize,
        boundaries: Vec<Vec<u8>>,
    ) -> Result<Self> {
        let name = name.into();
        if num_shards == 0 {
            return Err(DbError::Custom(
                "Sharded table must have at least 1 shard".into(),
            ));
        }
        if boundaries.len() != num_shards.saturating_sub(1) {
            return Err(DbError::Custom(format!(
                "Sharded table with {num_shards} shards requires {} boundaries, got {}",
                num_shards.saturating_sub(1),
                boundaries.len()
            )));
        }
        for i in 1..boundaries.len() {
            if boundaries[i - 1] >= boundaries[i] {
                return Err(DbError::Custom(
                    "Shard boundaries must be strictly ascending".into(),
                ));
            }
        }
        Ok(Self {
            name,
            num_shards,
            boundaries,
        })
    }

    #[must_use]
    pub fn route_key(&self, key: &[u8]) -> usize {
        if self.boundaries.is_empty() {
            return 0;
        }
        match self.boundaries.binary_search_by(|b| b.as_slice().cmp(key)) {
            Ok(idx) => idx + 1,
            Err(idx) => idx,
        }
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32 + self.boundaries.len() * 16);
        buf.extend_from_slice(&(self.num_shards as u16).to_le_bytes());
        buf.extend_from_slice(&(self.boundaries.len() as u16).to_le_bytes());
        for b in &self.boundaries {
            buf.extend_from_slice(&(b.len() as u16).to_le_bytes());
            buf.extend_from_slice(b);
        }
        buf
    }

    pub fn decode(name: impl Into<String>, data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(DbError::Corruption(
                "Sharded descriptor data too short".into(),
            ));
        }
        let num_shards = u16::from_le_bytes(
            data[0..2]
                .try_into()
                .map_err(|_| DbError::Corruption("Invalid num_shards".into()))?,
        ) as usize;
        let b_count = u16::from_le_bytes(
            data[2..4]
                .try_into()
                .map_err(|_| DbError::Corruption("Invalid boundary count".into()))?,
        ) as usize;

        let mut offset = 4;
        let mut boundaries = Vec::with_capacity(b_count);
        for _ in 0..b_count {
            if offset + 2 > data.len() {
                return Err(DbError::Corruption("Malformed boundary length".into()));
            }
            let len = u16::from_le_bytes(
                data[offset..offset + 2]
                    .try_into()
                    .map_err(|_| DbError::Corruption("Invalid boundary len".into()))?,
            ) as usize;
            offset += 2;
            if offset + len > data.len() {
                return Err(DbError::Corruption("Malformed boundary payload".into()));
            }
            boundaries.push(data[offset..offset + len].to_vec());
            offset += len;
        }

        Self::new(name, num_shards, boundaries)
    }
}

/// A read-only view of a sharded table routing queries across partition B+ Trees.
#[derive(Debug)]
pub struct ShardedTableRef<'a, S: Storage> {
    descriptor: ShardedTableDescriptor,
    shards: Vec<TableRef<'a, S>>,
}

impl<'a, S: Storage> ShardedTableRef<'a, S> {
    pub fn new(descriptor: ShardedTableDescriptor, shards: Vec<TableRef<'a, S>>) -> Self {
        Self { descriptor, shards }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.descriptor.name
    }

    #[must_use]
    pub fn num_shards(&self) -> usize {
        self.descriptor.num_shards
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let shard_idx = self.descriptor.route_key(key);
        self.shards[shard_idx].get(key)
    }

    pub fn scan_range(&self, range: &KeyRange) -> Result<Vec<KV>> {
        let start_shard = match range.start {
            Bound::Included(ref k) | Bound::Excluded(ref k) => self.descriptor.route_key(k),
            Bound::Unbounded => 0,
        };
        let end_shard = match range.end {
            Bound::Included(ref k) | Bound::Excluded(ref k) => self.descriptor.route_key(k),
            Bound::Unbounded => self.descriptor.num_shards.saturating_sub(1),
        };

        let mut results = Vec::new();
        for s in start_shard..=end_shard {
            let shard_start = if s == start_shard {
                range.start.clone()
            } else {
                Bound::Included(self.descriptor.boundaries[s - 1].clone())
            };
            let shard_end = if s == end_shard {
                range.end.clone()
            } else {
                Bound::Excluded(self.descriptor.boundaries[s].clone())
            };
            let shard_range = KeyRange::new(shard_start, shard_end);
            let mut kvs = self.shards[s].scan_range(&shard_range)?;
            results.append(&mut kvs);
        }
        Ok(results)
    }

    pub fn scan_all(&self) -> Result<Vec<KV>> {
        self.scan_range(&KeyRange::all())
    }
}

/// A mutable view of a sharded table routing writes across partition B+ Trees.
#[derive(Debug)]
pub struct ShardedTableMut<'a, 'tx, S: Storage> {
    descriptor: ShardedTableDescriptor,
    tx: &'a mut WriteTx<'tx, S>,
}

impl<'a, 'tx, S: Storage> ShardedTableMut<'a, 'tx, S> {
    pub fn new(descriptor: ShardedTableDescriptor, tx: &'a mut WriteTx<'tx, S>) -> Self {
        Self { descriptor, tx }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.descriptor.name
    }

    #[must_use]
    pub fn num_shards(&self) -> usize {
        self.descriptor.num_shards
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let shard_idx = self.descriptor.route_key(key);
        let sname = shard_name(&self.descriptor.name, shard_idx);
        self.tx.get_table_key(&sname, key)
    }

    pub fn put(&mut self, key: &[u8], val: &[u8]) -> Result<()> {
        let shard_idx = self.descriptor.route_key(key);
        let sname = shard_name(&self.descriptor.name, shard_idx);
        self.tx.put_table_key(&sname, key, val)
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<bool> {
        let shard_idx = self.descriptor.route_key(key);
        let sname = shard_name(&self.descriptor.name, shard_idx);
        self.tx.delete_table_key(&sname, key)
    }

    pub fn scan_range(&self, range: &KeyRange) -> Result<Vec<KV>> {
        let start_shard = match range.start {
            Bound::Included(ref k) | Bound::Excluded(ref k) => self.descriptor.route_key(k),
            Bound::Unbounded => 0,
        };
        let end_shard = match range.end {
            Bound::Included(ref k) | Bound::Excluded(ref k) => self.descriptor.route_key(k),
            Bound::Unbounded => self.descriptor.num_shards.saturating_sub(1),
        };

        let mut results = Vec::new();
        for s in start_shard..=end_shard {
            let sname = shard_name(&self.descriptor.name, s);
            let shard_start = if s == start_shard {
                range.start.clone()
            } else {
                Bound::Included(self.descriptor.boundaries[s - 1].clone())
            };
            let shard_end = if s == end_shard {
                range.end.clone()
            } else {
                Bound::Excluded(self.descriptor.boundaries[s].clone())
            };
            let shard_range = KeyRange::new(shard_start, shard_end);
            let mut kvs = self.tx.scan_table_range(&sname, &shard_range)?;
            results.append(&mut kvs);
        }
        Ok(results)
    }

    pub fn scan_all(&self) -> Result<Vec<KV>> {
        self.scan_range(&KeyRange::all())
    }
}

impl<S: Storage> ReadOnlyTx<S> {
    pub fn open_table_sharded(&self, name: &str) -> Result<ShardedTableRef<'_, S>> {
        let meta_table = match self.open_table(SHARDS_META_TABLE) {
            Ok(t) => t,
            Err(DbError::TableNotFound(_)) => return Err(DbError::TableNotFound(name.to_string())),
            Err(e) => return Err(e),
        };
        let desc_bytes = meta_table
            .get(name.as_bytes())?
            .ok_or_else(|| DbError::TableNotFound(name.to_string()))?;
        let descriptor = ShardedTableDescriptor::decode(name, &desc_bytes)?;

        let mut shards = Vec::with_capacity(descriptor.num_shards);
        for i in 0..descriptor.num_shards {
            let sname = shard_name(name, i);
            shards.push(self.open_table(&sname)?);
        }
        Ok(ShardedTableRef::new(descriptor, shards))
    }
}

impl<'tx, S: Storage> WriteTx<'tx, S> {
    pub fn create_table_sharded(
        &mut self,
        name: &str,
        num_shards: usize,
        boundaries: Vec<Vec<u8>>,
    ) -> Result<()> {
        let descriptor = ShardedTableDescriptor::new(name, num_shards, boundaries)?;
        let already_exists =
            if let Ok(Some(_)) = self.get_table_key(SHARDS_META_TABLE, name.as_bytes()) {
                true
            } else {
                self.manager
                    .catalog
                    .get_table_root(&shard_name(name, 0))
                    .is_some()
            };
        if already_exists {
            return Err(DbError::TableAlreadyExists(name.to_string()));
        }

        for i in 0..descriptor.num_shards {
            let sname = shard_name(name, i);
            self.create_table(&sname)?;
        }

        let encoded = descriptor.encode();
        self.put_table_key(SHARDS_META_TABLE, name.as_bytes(), &encoded)?;
        Ok(())
    }

    pub fn drop_table_sharded(&mut self, name: &str) -> Result<bool> {
        let desc_bytes = match self.get_table_key(SHARDS_META_TABLE, name.as_bytes())? {
            Some(b) => b,
            None => return Ok(false),
        };
        let descriptor = ShardedTableDescriptor::decode(name, &desc_bytes)?;

        for i in 0..descriptor.num_shards {
            let sname = shard_name(name, i);
            let _ = self.drop_table(&sname);
        }

        self.delete_table_key(SHARDS_META_TABLE, name.as_bytes())?;
        Ok(true)
    }

    pub fn open_table_sharded_mut<'b>(
        &'b mut self,
        name: &str,
    ) -> Result<ShardedTableMut<'b, 'tx, S>> {
        let desc_bytes = self
            .get_table_key(SHARDS_META_TABLE, name.as_bytes())?
            .ok_or_else(|| DbError::TableNotFound(name.to_string()))?;
        let descriptor = ShardedTableDescriptor::decode(name, &desc_bytes)?;

        Ok(ShardedTableMut::new(descriptor, self))
    }
}
