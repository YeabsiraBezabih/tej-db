use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tej_db_core::{PageId, Result, Storage};
use tej_db_txn::{ReadOnlyTx, TxManager, WriteTx};
use tej_db_vector::{
    DistanceMetric, QuantizationPrecision, RaBitQQuantizer, SearchCandidate, VamanaGraph,
    VamanaSearcher, VectorCompactor, VectorId, delta_table_name, insert_vector_delta,
    scan_vector_delta,
};

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
        let compactor = VectorCompactor::new(tx_manager, quantizer.clone());
        Self {
            table_name: table_name.into(),
            dimension,
            metric,
            quantizer,
            compactor,
            root_page_id: Arc::new(AtomicU64::new(PageId::INVALID.0)),
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
        insert_vector_delta(tx, &self.table_name, vector_id, vector)
    }

    /// Compacts uncommitted/pending delta vectors into the persistent Vamana graph.
    ///
    /// Swaps the active root page pointer atomically once compaction succeeds.
    pub fn compact(&self) -> Result<PageId> {
        let deltas = {
            let rx = self.compactor.manager.begin_read()?;
            scan_vector_delta(&rx, &self.table_name)?
        };

        if deltas.is_empty() {
            let current_raw = self.root_page_id.load(Ordering::Acquire);
            return Ok(PageId(current_raw));
        }

        let raw_items: Vec<(VectorId, Vec<f32>)> = deltas
            .iter()
            .filter_map(|d| d.vector.as_ref().map(|v| (d.vector_id, v.clone())))
            .collect();

        if !raw_items.is_empty() {
            let graph = Arc::new(VamanaGraph::build_from_vectors(
                &raw_items,
                &self.quantizer,
                64,
                160,
            ));
            let root_pid = graph.persist_to_storage(self.compactor.manager.cache())?;
            *self.graph.write() = Some(Arc::clone(&graph));
            self.root_page_id.store(root_pid.0, Ordering::Release);

            // Clear processed entries from delta table
            let delta_name = delta_table_name(&self.table_name);
            let mut tx = self.compactor.manager.begin_write()?;
            {
                let mut tbl = tx.open_table_mut(&delta_name)?;
                for d in &deltas {
                    let key = d.vector_id.0.to_be_bytes();
                    tbl.delete(&key)?;
                }
            }
            tx.commit()?;

            return Ok(root_pid);
        }

        let current_raw = self.root_page_id.load(Ordering::Acquire);
        let current_root = PageId(current_raw);
        let new_root = self
            .compactor
            .compact_table_delta(&self.table_name, current_root)?;
        self.root_page_id.store(new_root.0, Ordering::Release);
        Ok(new_root)
    }

    /// Searches the Vamana graph using RaBitQ-accelerated beam search.
    /// Prioritizes the L2/L3 cache-resident RAM arena for microsecond beam search.
    pub fn search(
        &self,
        _rx: &ReadOnlyTx<S>,
        query: &[f32],
        beam_width: usize,
        top_k: usize,
        filter: Option<&dyn Fn(VectorId) -> bool>,
    ) -> Result<Vec<SearchCandidate>> {
        if let Some(graph) = self.graph.read().clone() {
            return Ok(graph.search(query, &self.quantizer, beam_width, top_k, filter));
        }

        let root = PageId(self.root_page_id.load(Ordering::Acquire));
        if !root.is_valid() {
            return Ok(Vec::new());
        }
        let searcher = VamanaSearcher::new(
            Arc::clone(self.compactor.manager.cache()),
            self.quantizer.clone(),
        );
        searcher.search(root, query, beam_width, top_k, filter)
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
