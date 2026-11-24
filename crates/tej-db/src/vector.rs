use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tej_db_core::{PageId, Result, Storage};
use tej_db_txn::{ReadOnlyTx, TxManager, WriteTx};
use tej_db_vector::{
    DistanceMetric, QuantizationPrecision, RaBitQQuantizer, SearchCandidate, VamanaGraph,
    VamanaSearcher, VectorCompactor, VectorId, insert_vector_delta, scan_vector_delta,
};

/// Internal system table storing root page IDs for vector indexes across restarts.
pub const VECTOR_CATALOG_TABLE: &str = "__sys:vector_catalog";

/// High-level Vector Index for a database table, combining:
/// 1. ACID write-ahead delta buffer in a Copy-on-Write B+ tree.
/// 2. DiskANN (Vamana) graph stored in zero-copy 4 KiB `PackedVectorPage` slotted pages.
/// 3. RaBitQ binary quantization with L2/L3 cache-resident RAM arena for microsecond beam search.
pub struct VectorIndex<S: Storage> {
    table_name: String,
    dimension: usize,
    metric: DistanceMetric,
    quantizer: RaBitQQuantizer,
    compactor: VectorCompactor<S>,
    root_page_id: Arc<AtomicU64>,
    graph: Arc<RwLock<Option<Arc<VamanaGraph>>>>,
}

impl<S: Storage> core::fmt::Debug for VectorIndex<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VectorIndex")
            .field("table_name", &self.table_name)
            .field("dimension", &self.dimension)
            .field("metric", &self.metric)
            .field("root_page_id", &self.root_page_id.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl<S: Storage> VectorIndex<S> {
    /// Creates a new vector index coordinator for `table_name`.
    #[must_use]
    pub fn new(
        table_name: impl Into<String>,
        dimension: usize,
        metric: DistanceMetric,
        tx_manager: Arc<TxManager<S>>,
    ) -> Self {
        Self::new_with_precision(
            table_name,
            dimension,
            metric,
            tx_manager,
            QuantizationPrecision::OneBit,
        )
    }

    /// Creates a new vector index coordinator with explicit quantization precision.
    #[must_use]
    pub fn new_with_precision(
        table_name: impl Into<String>,
        dimension: usize,
        metric: DistanceMetric,
        tx_manager: Arc<TxManager<S>>,
        precision: QuantizationPrecision,
    ) -> Self {
        let quantizer = RaBitQQuantizer::new_with_precision(dimension, 0x5EED_C0DE, precision);
        let table_str = table_name.into();

        // Recover persisted root_page_id from vector catalog if present
        let cat_key = format!("vector_root:{table_str}").into_bytes();
        let initial_root = if let Ok(rx) = tx_manager.begin_read() {
            if let Ok(tbl) = rx.open_table(VECTOR_CATALOG_TABLE) {
                if let Ok(Some(bytes)) = tbl.get(&cat_key) {
                    if bytes.len() == 8 {
                        let mut b = [0u8; 8];
                        b.copy_from_slice(&bytes);
                        u64::from_be_bytes(b)
                    } else {
                        PageId::INVALID.0
                    }
                } else {
                    PageId::INVALID.0
                }
            } else {
                PageId::INVALID.0
            }
        } else {
            PageId::INVALID.0
        };

        let compactor = VectorCompactor::new(tx_manager, quantizer.clone());

        Self {
            table_name: table_str,
            dimension,
            metric,
            quantizer,
            compactor,
            root_page_id: Arc::new(AtomicU64::new(initial_root)),
            graph: Arc::new(RwLock::new(None)),
        }
    }

    /// Table name this vector index is bound to.
    #[must_use]
    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    /// Vector dimensionality.
    #[must_use]
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Configured distance metric.
    #[must_use]
    pub fn metric(&self) -> DistanceMetric {
        self.metric
    }

    /// Inserts a vector mutation into the transaction's ACID delta buffer.
    pub fn insert(
        &self,
        tx: &mut WriteTx<'_, S>,
        vector_id: VectorId,
        vector: &[f32],
    ) -> Result<()> {
        if vector.len() != self.dimension {
            return Err(tej_db_core::DbError::Custom(format!(
                "Vector dimension mismatch: expected {}, got {}",
                self.dimension,
                vector.len()
            )));
        }
        if vector.iter().any(|f| f.is_nan() || f.is_infinite()) {
            return Err(tej_db_core::DbError::Custom(
                "Vector contains NaN or Infinite components".into(),
            ));
        }
        insert_vector_delta(tx, &self.table_name, vector_id, vector)
    }

    /// Compacts uncommitted/pending delta vectors into the persistent Vamana graph.
    ///
    /// Swaps the active root page pointer atomically once compaction succeeds.
    pub fn compact(&self) -> Result<PageId> {
        let current_raw = self.root_page_id.load(Ordering::Acquire);
        let current_root = PageId(current_raw);
        let new_root = self
            .compactor
            .compact_table_delta(&self.table_name, current_root)?;
        self.root_page_id.store(new_root.0, Ordering::Release);

        // Invalidate in-memory graph cache so queries load newly compacted nodes
        *self.graph.write() = None;

        if new_root.is_valid() {
            let mut tx = self.compactor.manager.begin_write()?;
            {
                let mut cat = tx.open_table_mut(VECTOR_CATALOG_TABLE)?;
                let cat_key = format!("vector_root:{}", self.table_name).into_bytes();
                cat.put(&cat_key, &new_root.0.to_be_bytes())?;
            }
            tx.commit()?;
        }

        Ok(new_root)
    }

    /// Searches the Vamana graph using RaBitQ-accelerated beam search.
    /// Prioritizes the L2/L3 cache-resident RAM arena for microsecond beam search.
    pub fn search(
        &self,
        rx: &ReadOnlyTx<S>,
        query: &[f32],
        beam_width: usize,
        top_k: usize,
        filter: Option<&dyn Fn(VectorId) -> bool>,
    ) -> Result<Vec<SearchCandidate>> {
        if query.len() != self.dimension {
            return Err(tej_db_core::DbError::Custom(format!(
                "Query dimension mismatch: expected {}, got {}",
                self.dimension,
                query.len()
            )));
        }
        if query.iter().any(|f| f.is_nan() || f.is_infinite()) {
            return Err(tej_db_core::DbError::Custom(
                "Query vector contains NaN or Infinite components".into(),
            ));
        }

        let mut candidate_map: std::collections::HashMap<VectorId, SearchCandidate> =
            std::collections::HashMap::new();

        let root = PageId(self.root_page_id.load(Ordering::Acquire));
        if root.is_valid() {
            let searcher = VamanaSearcher::new(
                Arc::clone(self.compactor.manager.cache()),
                self.quantizer.clone(),
            );
            let base_candidates = searcher.search(root, query, beam_width, top_k * 2, filter)?;
            for cand in base_candidates {
                candidate_map.insert(cand.vector_id, cand);
            }
        }

        // Incorporate pending uncompacted delta mutations
        if let Ok(deltas) = scan_vector_delta(rx, &self.table_name) {
            for d in deltas {
                if let Some(v) = d.vector {
                    if filter.is_none_or(|f| f(d.vector_id)) {
                        let dist = match self.metric {
                            DistanceMetric::L2 => {
                                tej_db_vector::simd::l2_distance_squared(query, &v)
                            }
                            DistanceMetric::Cosine => {
                                tej_db_vector::simd::cosine_distance(query, &v)
                            }
                            DistanceMetric::DotProduct => {
                                tej_db_vector::simd::dot_product(query, &v)
                            }
                        };
                        candidate_map.insert(
                            d.vector_id,
                            SearchCandidate {
                                page_id: PageId::INVALID,
                                vector_id: d.vector_id,
                                distance: dist,
                            },
                        );
                    }
                } else {
                    candidate_map.remove(&d.vector_id);
                }
            }
        }

        let mut candidates: Vec<SearchCandidate> = candidate_map.into_values().collect();
        candidates.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(top_k);

        Ok(candidates)
    }

    /// Performs exact full-precision distance reranking on top candidates.
    pub fn rerank<F, V>(
        candidates: &[SearchCandidate],
        query: &[f32],
        fetch_vector_fn: F,
    ) -> Result<Vec<SearchCandidate>>
    where
        F: Fn(VectorId) -> Result<Option<V>>,
        V: std::ops::Deref<Target = [f32]>,
    {
        VamanaSearcher::<S>::rerank(candidates, query, fetch_vector_fn)
    }

    /// High-recall oversampled search: runs beam search for `rerank_k` candidates,
    /// reranks using exact `f32` vectors, and returns top `top_k` results.
    #[allow(clippy::too_many_arguments)]
    pub fn search_and_rerank<F, V>(
        &self,
        rx: &ReadOnlyTx<S>,
        query: &[f32],
        beam_width: usize,
        top_k: usize,
        rerank_k: usize,
        filter: Option<&dyn Fn(VectorId) -> bool>,
        fetch_vector_fn: F,
    ) -> Result<Vec<SearchCandidate>>
    where
        F: Fn(VectorId) -> Result<Option<V>>,
        V: std::ops::Deref<Target = [f32]>,
    {
        let candidates = self.search(rx, query, beam_width, rerank_k, filter)?;
        let reranked = Self::rerank(&candidates, query, fetch_vector_fn)?;
        let mut top = reranked;
        top.truncate(top_k);
        Ok(top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_high_level_vector_index_lifecycle() {
        let db = crate::Database::open_memory().unwrap();
        let idx = db.create_vector_index("embeddings", 16, DistanceMetric::L2);

        // 1. Insert vectors
        {
            let mut tx = db.begin_write().unwrap();
            for i in 1..=8u64 {
                let mut v = vec![0.0f32; 16];
                #[allow(clippy::cast_precision_loss)]
                {
                    v[0] = i as f32;
                    v[1] = (i * 2) as f32;
                }
                idx.insert(&mut tx, VectorId(i), &v).unwrap();
            }
            tx.commit().unwrap();
        }

        // 2. Compact delta buffer into Vamana graph
        let root = idx.compact().unwrap();
        assert!(root.is_valid());

        // 3. Search via ReadOnlyTx
        let rx = db.begin_read().unwrap();
        let mut query = vec![0.0f32; 16];
        query[0] = 4.0;
        query[1] = 8.0;

        let results = idx.search(&rx, &query, 16, 4, None).unwrap();
        assert_eq!(results.len(), 4);

        // 4. Exact full-precision reranking
        let full_vectors: Vec<(VectorId, Vec<f32>)> = (1..=8u64)
            .map(|i| {
                let mut v = vec![0.0f32; 16];
                #[allow(clippy::cast_precision_loss)]
                {
                    v[0] = i as f32;
                    v[1] = (i * 2) as f32;
                }
                (VectorId(i), v)
            })
            .collect();

        let reranked = VectorIndex::<tej_db_pager::MemStorage>::rerank(&results, &query, |id| {
            Ok(full_vectors
                .iter()
                .find(|(vid, _)| *vid == id)
                .map(|(_, v)| v.clone()))
        })
        .unwrap();

        assert_eq!(reranked[0].vector_id, VectorId(4));
        assert!(reranked[0].distance < 1e-4);
    }

    #[test]
    fn test_high_level_vector_index_2bit_precision() {
        let db = crate::Database::open_memory().unwrap();
        let idx = db.create_vector_index_with_precision(
            "embeddings_2bit",
            16,
            DistanceMetric::L2,
            QuantizationPrecision::TwoBit,
        );

        let mut tx = db.begin_write().unwrap();
        for i in 1..=8u64 {
            let mut v = vec![0.0f32; 16];
            #[allow(clippy::cast_precision_loss)]
            {
                v[0] = i as f32;
            }
            idx.insert(&mut tx, VectorId(i), &v).unwrap();
        }
        tx.commit().unwrap();

        let root = idx.compact().unwrap();
        assert!(root.is_valid());

        let rx = db.begin_read().unwrap();
        let mut query = vec![0.0f32; 16];
        query[0] = 5.0;

        let results = idx.search(&rx, &query, 16, 4, None).unwrap();
        assert_eq!(results.len(), 4);
    }
}
