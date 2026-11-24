pub mod hooks;
pub mod index;
pub mod spatial_cluster;
pub mod spatial_soa;
pub mod spatial_viewport;
pub mod typed;
pub mod vector;

#[cfg(test)]
mod tests;

pub use hooks::{AuditLogHook, ChangeFeedHook};
pub use index::{QueryBuilder, SecondaryIndex};
pub use spatial_viewport::{
    ClusterMetadata, CullResult, FrustumPlanes, GizmoTransform, Ray3D, RaycastScope,
    SpatialViewportManager,
};
pub use tej_db_btree::BTreeIndex;
pub use tej_db_core::spatial::{Aabb3D, BoundingSphere};
pub use tej_db_core::{
    BatchOp, ChangeSet, DbError, DbStats, DurabilityMode, Hook, Index, KV, KeyRange,
    PAGE_HEADER_SIZE, PAGE_SIZE, Page, PageHeader, PageId, PageType, Result, Storage, TxId,
    WriteBatch,
};
#[cfg(not(target_arch = "wasm32"))]
pub use tej_db_pager::FileStorage;
#[cfg(all(feature = "mmap", not(target_arch = "wasm32")))]
pub use tej_db_pager::MmapStorage;
#[cfg(feature = "compression")]
pub use tej_db_pager::{CompressedStorage, compress_page, decompress_page};
#[cfg(feature = "encryption")]
pub use tej_db_pager::{EncryptedStorage, decrypt_page_data, encrypt_page_data};
pub use tej_db_pager::{MemStorage, PageCache};
pub use tej_db_txn::{
    ReadOnlyTx, SHARDS_META_TABLE, ShardedTableDescriptor, ShardedTableMut, ShardedTableRef,
    TableMut, TableRef, TxManager, WriteTx, shard_name,
};
pub use tej_db_vector::{DistanceMetric, QuantizationPrecision, SearchCandidate, VectorId};
pub use typed::{KeyCodec, TypedTable, TypedTableMut};
pub use vector::VectorIndex;

use std::fmt;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::Arc;

/// Default page cache capacity (1024 pages = 4 MiB at 4 KiB/page).
pub const DEFAULT_CACHE_CAPACITY: usize = 1024;

/// Main embedded database instance.
pub struct Database<S: Storage> {
    cache: Arc<PageCache<S>>,
    tx_manager: Arc<TxManager<S>>,
}

impl<S: Storage> Clone for Database<S> {
    fn clone(&self) -> Self {
        Self {
            cache: Arc::clone(&self.cache),
            tx_manager: Arc::clone(&self.tx_manager),
        }
    }
}

impl<S: Storage> fmt::Debug for Database<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Database")
            .field("cache", &self.cache)
            .field("tx_manager", &self.tx_manager)
            .finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Database<FileStorage> {
    /// Open or create a file-backed database engine with default 4 MiB cache.
    pub fn open_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_file_with_capacity(path, DEFAULT_CACHE_CAPACITY)
    }

    /// Recover a database file that suffered an unaligned torn write from sudden power loss or process kill.
    /// Safely trims any trailing partial-page bytes back to the last valid page boundary,
    /// then opens the database cleanly.
    pub fn recover_file(path: impl AsRef<Path>) -> Result<Self> {
        let p = path.as_ref();
        if let Ok(metadata) = std::fs::metadata(p) {
            let current_len = metadata.len();
            let unaligned = current_len % tej_db_core::PAGE_SIZE as u64;
            if unaligned != 0 {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(p)
                    .map_err(DbError::Io)?;
                file.set_len(current_len - unaligned).map_err(DbError::Io)?;
            }
        }
        Self::open_file(p)
    }

    /// Open or create a file-backed database engine with custom cache capacity.
    pub fn open_file_with_capacity(path: impl AsRef<Path>, cache_capacity: usize) -> Result<Self> {
        let storage = FileStorage::open(path)?;
        let cache = Arc::new(PageCache::new(storage, cache_capacity));
        let tx_manager = Arc::new(TxManager::open(Arc::clone(&cache))?);

        Ok(Self { cache, tx_manager })
    }

    /// Open or create a file-backed database engine with custom cache capacity and explicit durability mode.
    pub fn open_file_with_durability(
        path: impl AsRef<Path>,
        cache_capacity: usize,
        durability: DurabilityMode,
    ) -> Result<Self> {
        let db = Self::open_file_with_capacity(path, cache_capacity)?;
        db.set_durability(durability);
        Ok(db)
    }

    /// Export an online, crash-safe, non-blocking snapshot backup to `dest_path`.
    ///
    /// Copies all physical pages directly from the flushed snapshot state.
    /// Safely backs up the default table as well as all named tables, secondary indexes,
    /// sharded partitions, catalogs, raw VectorNode pages, and spatial MeshletNode pages.
    pub fn backup(&self, dest_path: impl AsRef<Path>) -> Result<()> {
        let dest_p = dest_path.as_ref();
        let src_p = self.cache.storage().read().path().to_path_buf();
        if let (Ok(can_src), Ok(can_dest)) = (src_p.canonicalize(), dest_p.canonicalize()) {
            if can_src == can_dest {
                return Err(DbError::Custom(
                    "Cannot backup database to its own source file path".into(),
                ));
            }
        } else if src_p == dest_p {
            return Err(DbError::Custom(
                "Cannot backup database to its own source file path".into(),
            ));
        }

        // 1. Serialize backup under commit_lock to ensure point-in-time ACID crash consistency
        let _commit_guard = self.tx_manager.commit_lock().lock();

        // 2. Flush and sync all pending dirty pages to disk so storage is 100% up to date
        self.cache.flush()?;
        self.cache.sync()?;

        // 3. Copy physical pages directly to destination file storage
        // This preserves all tables, catalog metadata, VectorNode pages, and MeshletNode pages
        // with exact PageId references intact.
        let src_storage = self.cache.storage().read();
        let count = src_storage.page_count();
        let mut dest_storage = FileStorage::open(dest_p)?;
        let mut buf = [0u8; PAGE_SIZE];
        for i in 0..count {
            let pid = PageId(i);
            src_storage.read_page(pid, &mut buf)?;
            dest_storage.write_page(pid, &buf)?;
        }
        dest_storage.sync()?;
        Ok(())
    }

    /// Compact all active database pages into a new database file, removing free-list holes.
    pub fn compact(&self, dest_path: impl AsRef<Path>) -> Result<()> {
        self.backup(dest_path)
    }

    /// Explicitly close and flush all dirty pages down to storage and issue a hardware sync.
    pub fn close(self) -> Result<()> {
        self.cache.flush()?;
        self.cache.sync()
    }
}

impl Database<MemStorage> {
    /// Open a volatile in-memory database engine with default cache.
    pub fn open_memory() -> Result<Self> {
        Self::open_memory_with_capacity(DEFAULT_CACHE_CAPACITY)
    }

    /// Open a volatile in-memory database engine with custom cache capacity and explicit durability mode.
    pub fn open_memory_with_durability(
        cache_capacity: usize,
        durability: DurabilityMode,
    ) -> Result<Self> {
        let storage = MemStorage::new();
        let cache = Arc::new(PageCache::new(storage, cache_capacity));
        let tx_manager = Arc::new(TxManager::open(Arc::clone(&cache))?);
        tx_manager.set_durability(durability);

        Ok(Self { cache, tx_manager })
    }

    /// Open a volatile in-memory database engine with custom cache capacity.
    pub fn open_memory_with_capacity(cache_capacity: usize) -> Result<Self> {
        let storage = MemStorage::new();
        let cache = Arc::new(PageCache::new(storage, cache_capacity));
        let tx_manager = Arc::new(TxManager::open(Arc::clone(&cache))?);

        Ok(Self { cache, tx_manager })
    }
}

#[cfg(all(feature = "mmap", not(target_arch = "wasm32")))]
impl Database<MmapStorage> {
    /// Open or create a memory-mapped database engine with default cache.
    pub fn open_mmap(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_mmap_with_capacity(path, DEFAULT_CACHE_CAPACITY)
    }

    /// Open or create a memory-mapped database engine with custom cache capacity.
    pub fn open_mmap_with_capacity(path: impl AsRef<Path>, cache_capacity: usize) -> Result<Self> {
        let storage = MmapStorage::open(path)?;
        let cache = Arc::new(PageCache::new(storage, cache_capacity));
        let tx_manager = Arc::new(TxManager::open(Arc::clone(&cache))?);

        Ok(Self { cache, tx_manager })
    }
}

impl<S: Storage> Database<S> {
    /// Open a database with a custom storage backend and cache capacity.
    pub fn open_custom(storage: S, cache_capacity: usize) -> Result<Self> {
        let cache = Arc::new(PageCache::new(storage, cache_capacity));
        let tx_manager = Arc::new(TxManager::open(Arc::clone(&cache))?);
        Ok(Self { cache, tx_manager })
    }

    /// Begin a concurrent snapshot read-only transaction.
    pub fn begin_read(&self) -> Result<ReadOnlyTx<S>> {
        self.tx_manager.begin_read()
    }

    /// Begin a serializable write transaction.
    pub fn begin_write(&self) -> Result<WriteTx<'_, S>> {
        self.tx_manager.begin_write()
    }

    /// Execute an optimistic write transaction with automatic retry upon write-write conflicts.
    ///
    /// The closure `f` is invoked with a mutable reference to `WriteTx`. If `tx.commit()` encounters
    /// an OCC conflict (`DbError::WriteConflict`), the transaction is automatically rolled back and
    /// re-executed up to `max_retries` times.
    pub fn transaction_with_retry<T, F>(&self, max_retries: usize, mut f: F) -> Result<T>
    where
        F: FnMut(&mut WriteTx<'_, S>) -> Result<T>,
    {
        for attempt in 0..=max_retries {
            let mut tx = self.begin_write()?;
            let res = f(&mut tx);
            match res {
                Ok(val) => match tx.commit() {
                    Ok(()) => return Ok(val),
                    Err(DbError::WriteConflict { .. }) if attempt < max_retries => {
                        if attempt < 3 {
                            std::thread::yield_now();
                        } else {
                            let backoff_us = (1u64 << attempt.min(8)) * 10;
                            std::thread::sleep(std::time::Duration::from_micros(backoff_us));
                        }
                    }
                    Err(e) => return Err(e),
                },
                Err(e) => {
                    let _ = tx.rollback();
                    return Err(e);
                }
            }
        }
        Err(DbError::Concurrency(
            "Transaction exceeded maximum retry attempts due to persistent conflicts".into(),
        ))
    }

    /// Convenience wrapper executing a write transaction with default 5 retries on conflict.
    pub fn transaction<T, F>(&self, f: F) -> Result<T>
    where
        F: FnMut(&mut WriteTx<'_, S>) -> Result<T>,
    {
        self.transaction_with_retry(5, f)
    }

    /// List all named tables in the database.
    pub fn list_tables(&self) -> Result<Vec<String>> {
        let rx = self.begin_read()?;
        rx.list_tables()
    }

    /// Create a new named table/keyspace.
    pub fn create_table(&self, name: &str) -> Result<()> {
        let mut tx = self.begin_write()?;
        tx.create_table(name)?;
        tx.commit()
    }

    /// Drop an existing named table/keyspace.
    pub fn drop_table(&self, name: &str) -> Result<bool> {
        let mut tx = self.begin_write()?;
        let dropped = tx.drop_table(name)?;
        tx.commit()?;
        Ok(dropped)
    }

    /// Create a new range-partitioned sharded table across multiple independent B+ Tree roots.
    pub fn create_table_sharded(
        &self,
        name: &str,
        num_shards: usize,
        boundaries: Vec<Vec<u8>>,
    ) -> Result<()> {
        let mut tx = self.begin_write()?;
        tx.create_table_sharded(name, num_shards, boundaries)?;
        tx.commit()
    }

    /// Drop an existing sharded table, removing all its partition roots and metadata in O(1).
    pub fn drop_table_sharded(&self, name: &str) -> Result<bool> {
        let mut tx = self.begin_write()?;
        let dropped = tx.drop_table_sharded(name)?;
        tx.commit()?;
        Ok(dropped)
    }

    /// Execute an atomic batch of multi-key and multi-table operations.
    pub fn write_batch(&self, batch: WriteBatch) -> Result<()> {
        let mut tx = self.begin_write()?;
        for op in batch.ops {
            match op {
                BatchOp::Put { table, key, value } => {
                    if let Some(tbl) = table {
                        let mut t = tx.open_table_mut(&tbl)?;
                        t.put(&key, &value)?;
                    } else {
                        tx.put(&key, &value)?;
                    }
                }
                BatchOp::Delete { table, key } => {
                    if let Some(tbl) = table {
                        let mut t = tx.open_table_mut(&tbl)?;
                        t.delete(&key)?;
                    } else {
                        tx.delete(&key)?;
                    }
                }
            }
        }
        tx.commit()
    }

    /// Add a transaction lifecycle hook (e.g. audit log, change feed), returning its unique ID.
    pub fn add_hook(&self, hook: Box<dyn Hook>) -> usize {
        self.tx_manager.add_hook(hook)
    }

    /// Remove a registered transaction lifecycle hook by ID.
    pub fn remove_hook(&self, hook_id: usize) -> bool {
        self.tx_manager.remove_hook(hook_id)
    }

    /// Reference to internal transaction manager coordinator.
    #[must_use]
    pub fn tx_manager(&self) -> &Arc<TxManager<S>> {
        &self.tx_manager
    }

    /// Create a high-level vector index bound to `table_name`.
    #[must_use]
    pub fn create_vector_index(
        &self,
        table_name: impl Into<String>,
        dimension: usize,
        metric: tej_db_vector::DistanceMetric,
    ) -> VectorIndex<S> {
        VectorIndex::new(table_name, dimension, metric, Arc::clone(&self.tx_manager))
    }

    /// Create a high-level vector index with explicit quantization precision (1-bit or 2-bit).
    #[must_use]
    pub fn create_vector_index_with_precision(
        &self,
        table_name: impl Into<String>,
        dimension: usize,
        metric: tej_db_vector::DistanceMetric,
        precision: tej_db_vector::QuantizationPrecision,
    ) -> VectorIndex<S> {
        VectorIndex::new_with_precision(
            table_name,
            dimension,
            metric,
            Arc::clone(&self.tx_manager),
            precision,
        )
    }

    /// Create a high-level 3D spatial viewport manager bound to `table_name`.
    pub fn create_spatial_viewport(
        &self,
        table_name: impl Into<String>,
        domain: tej_db_core::spatial::Aabb3D,
    ) -> Result<SpatialViewportManager<S>> {
        SpatialViewportManager::new(Arc::clone(&self.tx_manager), table_name, domain)
    }

    /// Set durability policy (`Immediate` for strict fsync on commit, or `Normal` for OS page-cache commit).
    pub fn set_durability(&self, mode: DurabilityMode) {
        self.tx_manager.set_durability(mode);
    }

    /// Manually issue a hardware durability sync (`fdatasync`).
    pub fn sync(&self) -> Result<()> {
        self.cache.sync()
    }

    /// Get current engine performance and storage statistics.
    #[allow(clippy::cast_precision_loss)]
    pub fn stats(&self) -> DbStats {
        let (hits, misses, cached) = self.cache.stats();
        let total = hits + misses;
        let cache_hit_rate = if total > 0 {
            (hits as f64) / (total as f64)
        } else {
            0.0
        };
        let free_pages = self.cache.free_list_len() + self.cache.pending_free_len();
        let total_pages = self.cache.storage_page_count();
        let file_size_bytes = total_pages * (PAGE_SIZE as u64);

        DbStats {
            total_pages,
            cached_pages: cached,
            cache_hits: hits,
            cache_misses: misses,
            cache_hit_rate,
            free_pages,
            file_size_bytes,
            active_read_txns: self.tx_manager.active_readers(),
        }
    }
}
