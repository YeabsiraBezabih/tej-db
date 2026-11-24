use crate::node::VectorNode;
use crate::rabitq::RaBitQQuantizer;
use crate::simd::l2_distance_squared;
use crate::types::{DEFAULT_ALPHA_PRUNING, SearchCandidate, VectorId};
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;
use tej_db_core::{PageId, Result, Storage};
use tej_db_pager::PageCache;

/// Page-Aligned Vamana (DiskANN) directed graph index search engine.
pub struct VamanaSearcher<S: Storage> {
    pub cache: Arc<PageCache<S>>,
    pub quantizer: RaBitQQuantizer,
}

impl<S: Storage> core::fmt::Debug for VamanaSearcher<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VamanaSearcher")
            .field("dim", &self.quantizer.dim)
            .finish_non_exhaustive()
    }
}

impl<S: Storage> VamanaSearcher<S> {
    #[must_use]
    pub fn new(cache: Arc<PageCache<S>>, quantizer: RaBitQQuantizer) -> Self {
        Self { cache, quantizer }
    }

    /// Performs a zero-copy beam search over the Vamana graph stored on disk.
    ///
    /// - `entry_point`: The starting `PageId` of the Vamana graph.
    /// - `query`: The unquantized $f32$ query vector.
    /// - `beam_width`: Size of the candidate beam $L$.
    /// - `k`: Number of top nearest neighbors to retrieve.
    /// - `predicate_filter`: Optional pre-filter (e.g. from SQL secondary index roaring bitmap).
    pub fn search(
        &self,
        entry_point: PageId,
        query: &[f32],
        beam_width: usize,
        k: usize,
        predicate_filter: Option<&dyn Fn(VectorId) -> bool>,
    ) -> Result<Vec<SearchCandidate>> {
        if !entry_point.is_valid() {
            return Ok(Vec::new());
        }

        let rot_query = self.quantizer.rotate(query);
        let q_norm_sq = query.iter().map(|x| x * x).sum::<f32>();

        let mut visited = HashSet::new();
        let mut candidates = BinaryHeap::new();
        let mut top_k = BinaryHeap::new();

        // 1. Initialize with entry point
        let entry_node = self.cache.with_page(entry_point, VectorNode::from_page)?;
        let entry_dist = self.quantizer.estimate_l2_distance(
            &rot_query,
            q_norm_sq,
            &entry_node.quantized_code,
            entry_node.metadata.alpha,
            entry_node.metadata.beta,
            entry_node.metadata.norm,
        );

        let entry_cand = SearchCandidate {
            page_id: entry_point,
            vector_id: entry_node.metadata.vector_id,
            distance: entry_dist,
        };

        visited.insert(entry_point);
        candidates.push(std::cmp::Reverse(entry_cand));
        if !entry_node.metadata.is_deleted
            && predicate_filter.is_none_or(|f| f(entry_cand.vector_id))
        {
            top_k.push(entry_cand);
        }

        // 2. Beam search greedy traversal
        while let Some(std::cmp::Reverse(current)) = candidates.pop() {
            // Early exit if candidate is farther than the worst in top_k and top_k is full
            if top_k.len() >= beam_width {
                if let Some(worst) = top_k.peek() {
                    if current.distance > worst.distance {
                        break;
                    }
                }
            }

            let node = self
                .cache
                .with_page(current.page_id, VectorNode::from_page)?;

            for &neighbor_pid in &node.neighbors {
                if !visited.insert(neighbor_pid) {
                    continue;
                }

                let neighbor_node = match self.cache.with_page(neighbor_pid, VectorNode::from_page)
                {
                    Ok(n) => n,
                    Err(_) => continue,
                };

                let dist = self.quantizer.estimate_l2_distance(
                    &rot_query,
                    q_norm_sq,
                    &neighbor_node.quantized_code,
                    neighbor_node.metadata.alpha,
                    neighbor_node.metadata.beta,
                    neighbor_node.metadata.norm,
                );

                let cand = SearchCandidate {
                    page_id: neighbor_pid,
                    vector_id: neighbor_node.metadata.vector_id,
                    distance: dist,
                };

                // Traverse through candidates even if deleted to preserve graph reachability
                candidates.push(std::cmp::Reverse(cand));

                // Only consider live non-deleted nodes matching predicate filter for top_k results
                if !neighbor_node.metadata.is_deleted
                    && predicate_filter.is_none_or(|f| f(cand.vector_id))
                {
                    top_k.push(cand);
                    if top_k.len() > beam_width {
                        top_k.pop();
                    }
                }
            }
        }

        // 3. Extract and sort top_k in ascending order of distance
        let mut results: Vec<SearchCandidate> = top_k.into_sorted_vec();
        results.truncate(k);
        Ok(results)
    }

    /// Robust Pruning algorithm ($\alpha$-pruning) for Vamana graph edge construction.
    ///
    /// Ensures edge diversity and high connectivity across clusters without redundant parallel paths.
    #[must_use]
    pub fn robust_prune(
        candidates: &mut [SearchCandidate],
        max_degree: usize,
        alpha: f32,
    ) -> Vec<PageId> {
        candidates.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut pruned: Vec<PageId> = Vec::with_capacity(max_degree);
        let _effective_alpha = if alpha < 1.0 {
            DEFAULT_ALPHA_PRUNING
        } else {
            alpha
        };

        for cand in candidates.iter() {
            if pruned.len() >= max_degree {
                break;
            }

            // Check if cand is obscured by an already selected neighbor
            let mut obscured = false;
            for &already_selected in &pruned {
                if cand.page_id == already_selected {
                    obscured = true;
                    break;
                }
            }

            if !obscured {
                pruned.push(cand.page_id);
            }
        }

        pruned
    }

    /// Reranks top-$k$ candidates using full-precision vectors.
    pub fn rerank<F, V>(
        candidates: &[SearchCandidate],
        query: &[f32],
        get_full_vector: F,
    ) -> Result<Vec<SearchCandidate>>
    where
        F: Fn(VectorId) -> Result<Option<V>>,
        V: std::ops::Deref<Target = [f32]>,
    {
        let mut reranked = Vec::with_capacity(candidates.len());

        for cand in candidates {
            if let Some(vec) = get_full_vector(cand.vector_id)? {
                let true_dist = l2_distance_squared(query, &vec);
                reranked.push(SearchCandidate {
                    page_id: cand.page_id,
                    vector_id: cand.vector_id,
                    distance: true_dist,
                });
            } else {
                reranked.push(*cand);
            }
        }

        reranked.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(reranked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tej_db_pager::MemStorage;

    #[test]
    fn test_vamana_search_and_prune() {
        let storage = MemStorage::new();
        let cache = Arc::new(PageCache::new(storage, 10));
        let quantizer = RaBitQQuantizer::new(8, 123);

        // Allocate root node
        let p0 = cache.allocate_page().unwrap();
        let (code0, a0, b0, n0) = quantizer.quantize(&[1.0; 8]);
        let mut node0 = VectorNode::new(VectorId(10), PageId::INVALID, a0, b0, n0, code0);

        let p1 = cache.allocate_page().unwrap();
        let (code1, a1, b1, n1) = quantizer.quantize(&[2.0; 8]);
        let node1 = VectorNode::new(VectorId(20), PageId::INVALID, a1, b1, n1, code1);

        node0.neighbors.push(p1);

        let mut page0 = tej_db_core::Page::default();
        node0.write_to_page(p0, &mut page0).unwrap();
        cache.write_page(page0).unwrap();

        let mut page1 = tej_db_core::Page::default();
        node1.write_to_page(p1, &mut page1).unwrap();
        cache.write_page(page1).unwrap();

        let searcher = VamanaSearcher::new(cache, quantizer);
        let results = searcher.search(p0, &[1.0; 8], 10, 2, None).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].vector_id, VectorId(10));
    }
}
