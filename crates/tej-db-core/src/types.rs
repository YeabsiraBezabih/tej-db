use alloc::vec::Vec;
use core::fmt;
use core::ops::Bound;

/// Strongly typed page identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PageId(pub u64);

impl PageId {
    pub const INVALID: Self = Self(u64::MAX);

    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.0 != u64::MAX
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for PageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == u64::MAX {
            write!(f, "PageId(INVALID)")
        } else {
            write!(f, "PageId({})", self.0)
        }
    }
}

/// Strongly typed transaction identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TxId(pub u64);

impl TxId {
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TxId({})", self.0)
    }
}

/// Durability synchronization policy for transaction commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurabilityMode {
    /// Flush dirty pages to OS storage and issue a hardware `fdatasync` on every commit (Strict ACID / default).
    #[default]
    Immediate,
    /// Flush dirty pages to OS storage on every commit, deferring fsync to database close or checkpoint
    /// (corresponds to SQLite's `PRAGMA synchronous = NORMAL`).
    Normal,
    /// Batched fsync: Flush to storage on commit and rely on a periodic background sync worker.
    Eventual { interval_ms: u64 },
    /// In-memory / Ephemeral testing mode: Bypasses flushing dirty pages to storage on commit.
    None,
}

/// Operation within an atomic batch write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOp {
    Put {
        table: Option<alloc::string::String>,
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Delete {
        table: Option<alloc::string::String>,
        key: Vec<u8>,
    },
}

/// Atomic multi-key and multi-table batch mutation builder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteBatch {
    pub ops: Vec<BatchOp>,
}

impl WriteBatch {
    #[must_use]
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            ops: Vec::with_capacity(capacity),
        }
    }

    pub fn put(&mut self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) {
        self.ops.push(BatchOp::Put {
            table: None,
            key: key.into(),
            value: value.into(),
        });
    }

    pub fn put_table(
        &mut self,
        table: impl Into<alloc::string::String>,
        key: impl Into<Vec<u8>>,
        value: impl Into<Vec<u8>>,
    ) {
        self.ops.push(BatchOp::Put {
            table: Some(table.into()),
            key: key.into(),
            value: value.into(),
        });
    }

    pub fn delete(&mut self, key: impl Into<Vec<u8>>) {
        self.ops.push(BatchOp::Delete {
            table: None,
            key: key.into(),
        });
    }

    pub fn delete_table(
        &mut self,
        table: impl Into<alloc::string::String>,
        key: impl Into<Vec<u8>>,
    ) {
        self.ops.push(BatchOp::Delete {
            table: Some(table.into()),
            key: key.into(),
        });
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn clear(&mut self) {
        self.ops.clear();
    }
}

/// Key-Value pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KV {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

impl KV {
    #[must_use]
    pub fn new(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// Range of keys for scans.
#[derive(Debug, Clone)]
pub struct KeyRange {
    pub start: Bound<Vec<u8>>,
    pub end: Bound<Vec<u8>>,
}

impl KeyRange {
    #[must_use]
    pub fn new(start: Bound<Vec<u8>>, end: Bound<Vec<u8>>) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub fn between(start: Vec<u8>, end: Vec<u8>) -> Self {
        Self {
            start: Bound::Included(start),
            end: Bound::Excluded(end),
        }
    }

    #[must_use]
    pub fn all() -> Self {
        Self {
            start: Bound::Unbounded,
            end: Bound::Unbounded,
        }
    }

    /// Empty range that contains no keys.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            start: Bound::Excluded(alloc::vec::Vec::new()),
            end: Bound::Included(alloc::vec::Vec::new()),
        }
    }

    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        let after_start = match &self.start {
            Bound::Included(s) => key >= s.as_slice(),
            Bound::Excluded(s) => key > s.as_slice(),
            Bound::Unbounded => true,
        };
        let before_end = match &self.end {
            Bound::Included(e) => key <= e.as_slice(),
            Bound::Excluded(e) => key < e.as_slice(),
            Bound::Unbounded => true,
        };
        after_start && before_end
    }
}

/// Represents the set of key mutations made within a transaction.
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    pub writes: Vec<(Vec<u8>, Vec<u8>)>,
    pub deletes: Vec<Vec<u8>>,
}

/// Engine performance and storage statistics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DbStats {
    pub total_pages: u64,
    pub cached_pages: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_hit_rate: f64,
    pub free_pages: usize,
    pub file_size_bytes: u64,
    pub active_read_txns: usize,
}
