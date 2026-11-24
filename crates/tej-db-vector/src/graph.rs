use crate::arena::{QueryLut, VectorArena};
use crate::packed_page::{PACKED_NODE_MAX_DEGREE, PackedVectorNode, PackedVectorPage};
use crate::rabitq::{QuantizationPrecision, RaBitQQuantizer};
use crate::scratch::with_thread_scratch;
use crate::types::{SearchCandidate, VectorId};
use smallvec::SmallVec;
use std::sync::Arc;
use tej_db_core::{Page, PageId, Result, Storage};
use tej_db_pager::PageCache;

#[derive(Debug, Clone, Copy, PartialEq)]
struct CandidateEntry {
    dist: f32,
    node: u32,
    expanded: bool,
}

/// In-memory Page-Aligned Vamana graph coordinator.
/// Combines an L2/L3 cache-resident `VectorArena` with a compact adjacency graph
/// for ultra-fast beam search and 2-pass long-range highway construction.
#[derive(Debug, Clone)]
pub struct VamanaGraph {
    pub arena: VectorArena,
    pub adj: Vec<SmallVec<[u32; PACKED_NODE_MAX_DEGREE]>>,
    pub medoid: u32,
    pub dim: usize,
    pub precision: QuantizationPrecision,
}

impl VamanaGraph {
    /// Creates a new empty `VamanaGraph`.
    #[must_use]
    pub fn new(dim: usize, capacity: usize, precision: QuantizationPrecision) -> Self {
        Self {
            arena: VectorArena::new(capacity),
            adj: Vec::with_capacity(capacity),
            medoid: 0,
            dim,
            precision,
        }
    }

    /// Number of nodes in the graph.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.arena.len()
    }

    /// Checks if the graph is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    /// Computes the medoid (vector closest to centroid) of the dataset.
    #[must_use]
    pub fn compute_medoid(vectors: &[Vec<f32>], dim: usize) -> usize {
        if vectors.is_empty() {
            return 0;
        }
        let mut centroid = vec![0.0f32; dim];
        for v in vectors {
            for (c, &x) in centroid.iter_mut().zip(v.iter()) {
                *c += x;
            }
        }
        #[expect(clippy::cast_precision_loss, reason = "vector count fits in f32")]
        let n = vectors.len() as f32;
        for c in &mut centroid {
            *c /= n;
        }

        let mut best_idx = 0;
        let mut min_dist = f32::INFINITY;
        for (i, v) in vectors.iter().enumerate() {
            let mut d = 0.0f32;
            for (x, &c) in v.iter().zip(centroid.iter()) {
                let diff = x - c;
                d += diff * diff;
            }
            if d < min_dist {
                min_dist = d;
                best_idx = i;
            }
        }
        best_idx
    }

    /// Robust Pruning algorithm with $\alpha$-occlusion condition.
    ///
    /// Selects up to `r` neighbors from candidates `cand_nodes` for node `p`.
    /// When $\alpha > 1.0$, long-range shortcut edges are preserved.
    pub fn robust_prune(
        p: usize,
        mut cand_nodes: Vec<u32>,
        alpha: f32,
        r: usize,
        vectors: &[Vec<f32>],
    ) -> SmallVec<[u32; PACKED_NODE_MAX_DEGREE]> {
        cand_nodes.retain(|&idx| (idx as usize) != p);
        cand_nodes.sort_unstable();
        cand_nodes.dedup();

        let p_vec = &vectors[p];
        let mut scored: Vec<(u32, f32)> = cand_nodes
            .into_iter()
            .map(|idx| {
                let dist = crate::simd::l2_distance_squared(p_vec, &vectors[idx as usize]);
                (idx, dist)
            })
            .collect();

        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut pruned = SmallVec::with_capacity(r);
        for (cand_idx, cand_dist) in scored {
            if pruned.len() >= r {
                break;
            }

            let mut occluded = false;
            let cand_vec = &vectors[cand_idx as usize];
            let alpha_sq = alpha * alpha;
            for &chosen in &pruned {
                let inter_dist =
                    crate::simd::l2_distance_squared(cand_vec, &vectors[chosen as usize]);
                if alpha_sq * inter_dist <= cand_dist {
                    occluded = true;
                    break;
                }
            }

            if !occluded {
                pruned.push(cand_idx);
            }
        }

        pruned
    }

    /// Builds a 2-Pass Vamana Graph from a collection of raw vectors.
    pub fn build_from_vectors(
        raw_items: &[(VectorId, Vec<f32>)],
        quantizer: &RaBitQQuantizer,
        r: usize,
        l_build: usize,
    ) -> Self {
        let n = raw_items.len();
        let dim = quantizer.dim;
        let mut graph = Self::new(dim, n, quantizer.precision);

        let vectors_only: Vec<Vec<f32>> = raw_items.iter().map(|(_, v)| v.clone()).collect();
        let medoid_idx = Self::compute_medoid(&vectors_only, dim);
        #[expect(clippy::cast_possible_truncation, reason = "index fits in u32")]
        {
            graph.medoid = medoid_idx as u32;
        }

        for (id, vec) in raw_items {
            let (code, alpha, beta, norm) = quantizer.quantize(vec);
            graph.arena.insert(*id, &code, alpha, beta, norm);
            graph.adj.push(SmallVec::new());
        }

        // Initialize random R-regular out-edges to form a connected expander graph
        let mut rng_state = 0x853c_49e6_748f_ea9bu64;
        let r_init = r.min(n.saturating_sub(1));
        for (i, adj_slot) in graph.adj.iter_mut().enumerate() {
            let mut adj = SmallVec::with_capacity(r);
            while adj.len() < r_init {
                rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let target = ((rng_state >> 32) as usize) % n;
                #[expect(clippy::cast_possible_truncation, reason = "index fits in u32")]
                let u32_target = target as u32;
                if target != i && !adj.contains(&u32_target) {
                    adj.push(u32_target);
                }
            }
            *adj_slot = adj;
        }

        // Pass 1 (alpha = 1.0): Initial randomized connectivity
        for i in 0..n {
            let vec = &vectors_only[i];
            let candidates = graph.search_internal_exact(vec, l_build, l_build, &vectors_only);
            let mut cand_indices: Vec<u32> = candidates
                .into_iter()
                .map(|(node_idx, _)| node_idx)
                .collect();
            cand_indices.extend_from_slice(&graph.adj[i]);

            let pruned = Self::robust_prune(i, cand_indices, 1.0, r, &vectors_only);
            graph.adj[i].clone_from(&pruned);

            // Add reverse edges
            #[expect(clippy::cast_possible_truncation, reason = "index fits in u32")]
            let u32_i = i as u32;
            for &nb in &pruned {
                let nb_idx = nb as usize;
                if !graph.adj[nb_idx].contains(&u32_i) {
                    if graph.adj[nb_idx].len() < r {
                        graph.adj[nb_idx].push(u32_i);
                    } else {
                        let mut cand_nb = graph.adj[nb_idx].to_vec();
                        cand_nb.push(u32_i);
                        graph.adj[nb_idx] =
                            Self::robust_prune(nb_idx, cand_nb, 1.0, r, &vectors_only);
                    }
                }
            }
        }

        // Pass 2 (alpha = 1.2): Long-range highway edges
        for i in 0..n {
            let vec = &vectors_only[i];
            let candidates = graph.search_internal_exact(vec, l_build, l_build, &vectors_only);
            let mut cand_indices: Vec<u32> = candidates
                .into_iter()
                .map(|(node_idx, _)| node_idx)
                .collect();
            cand_indices.extend_from_slice(&graph.adj[i]);

            let pruned = Self::robust_prune(i, cand_indices, 1.35, r, &vectors_only);
            graph.adj[i].clone_from(&pruned);

            // Add reverse edges for highway connectivity
            #[expect(clippy::cast_possible_truncation, reason = "index fits in u32")]
            let u32_i = i as u32;
            for &nb in &pruned {
                let nb_idx = nb as usize;
                if !graph.adj[nb_idx].contains(&u32_i) {
                    if graph.adj[nb_idx].len() < r {
                        graph.adj[nb_idx].push(u32_i);
                    } else {
                        let mut cand_nb = graph.adj[nb_idx].to_vec();
                        cand_nb.push(u32_i);
                        graph.adj[nb_idx] =
                            Self::robust_prune(nb_idx, cand_nb, 1.35, r, &vectors_only);
                    }
                }
            }
        }

        #[allow(clippy::cast_precision_loss)]
        let avg_deg =
            graph.adj.iter().map(smallvec::SmallVec::len).sum::<usize>() as f64 / (n as f64);
        println!(
            "    [Vamana Graph Build] Final average out-degree: {avg_deg:.1} (target R = {r})"
        );

        graph
    }

    /// Internal exact-distance beam search used during offline 2-pass graph construction.
    pub fn search_internal_exact(
        &self,
        query: &[f32],
        beam_width: usize,
        k: usize,
        vectors: &[Vec<f32>],
    ) -> Vec<(u32, f32)> {
        if self.is_empty() {
            return Vec::new();
        }

        let pool_capacity = beam_width.max(k);
        let max_expansions = beam_width;

        with_thread_scratch(|scratch| {
            let mut pool: Vec<CandidateEntry> = Vec::with_capacity(pool_capacity + 1);

            let start_node = self.medoid;
            let start_dist = crate::simd::l2_distance_squared(query, &vectors[start_node as usize]);

            scratch.mark_visited(start_node as usize);
            pool.push(CandidateEntry {
                dist: start_dist,
                node: start_node,
                expanded: false,
            });

            let mut num_expanded = 0;
            while num_expanded < max_expansions {
                let next_idx = match pool.iter().position(|e| !e.expanded) {
                    Some(idx) => idx,
                    None => break,
                };
                pool[next_idx].expanded = true;
                num_expanded += 1;
                let cur_node = pool[next_idx].node as usize;

                if cur_node >= self.adj.len() {
                    continue;
                }

                for &nb in &self.adj[cur_node] {
                    let nb_idx = nb as usize;
                    if !scratch.mark_visited(nb_idx) {
                        continue;
                    }

                    let dist = crate::simd::l2_distance_squared(query, &vectors[nb_idx]);

                    if pool.len() < pool_capacity
                        || dist < pool.last().map_or(f32::INFINITY, |e| e.dist)
                    {
                        let insert_pos = pool.partition_point(|e| e.dist < dist);
                        pool.insert(
                            insert_pos,
                            CandidateEntry {
                                dist,
                                node: nb,
                                expanded: false,
                            },
                        );
                        if pool.len() > pool_capacity {
                            pool.pop();
                        }
                    }
                }
            }

            pool.truncate(k);
            pool.into_iter().map(|e| (e.node, e.dist)).collect()
        })
    }

    /// Internal fast beam search over the in-memory graph.
    /// Operates entirely inside L1/L2 cache using `VectorArena` and `SearchScratch`.
    pub fn search_internal(
        &self,
        rotated_query: &[f32],
        query_norm_sq: f32,
        beam_width: usize,
        k: usize,
        predicate_filter: Option<&dyn Fn(VectorId) -> bool>,
    ) -> Vec<(u32, f32)> {
        if self.is_empty() {
            return Vec::new();
        }

        let pool_capacity = beam_width.max(k);
        let max_expansions = beam_width;
        let lut = QueryLut::new(rotated_query, query_norm_sq, self.precision);

        with_thread_scratch(|scratch| {
            let mut pool: Vec<CandidateEntry> = Vec::with_capacity(pool_capacity + 1);

            let start_node = self.medoid;
            let start_dist = self.arena.estimate_l2_with_lut(start_node as usize, &lut);

            scratch.mark_visited(start_node as usize);
            pool.push(CandidateEntry {
                dist: start_dist,
                node: start_node,
                expanded: false,
            });

            let mut num_expanded = 0;
            while num_expanded < max_expansions {
                let next_idx = match pool.iter().position(|e| !e.expanded) {
                    Some(idx) => idx,
                    None => break,
                };
                pool[next_idx].expanded = true;
                num_expanded += 1;
                let cur_node = pool[next_idx].node as usize;

                if cur_node >= self.adj.len() {
                    continue;
                }

                for &nb in &self.adj[cur_node] {
                    let nb_idx = nb as usize;
                    if !scratch.mark_visited(nb_idx) {
                        continue;
                    }

                    let dist = self.arena.estimate_l2_with_lut(nb_idx, &lut);

                    if pool.len() < pool_capacity
                        || dist < pool.last().map_or(f32::INFINITY, |e| e.dist)
                    {
                        let insert_pos = pool.partition_point(|e| e.dist < dist);
                        pool.insert(
                            insert_pos,
                            CandidateEntry {
                                dist,
                                node: nb,
                                expanded: false,
                            },
                        );
                        if pool.len() > pool_capacity {
                            pool.pop();
                        }
                    }
                }
            }

            if let Some(f) = predicate_filter {
                pool.retain(|e| {
                    let vid = self.arena.entries[e.node as usize].vector_id;
                    f(vid)
                });
            }

            pool.truncate(k);
            pool.into_iter().map(|e| (e.node, e.dist)).collect()
        })
    }

    /// External beam search returning public `SearchCandidate` records.
    pub fn search(
        &self,
        query: &[f32],
        quantizer: &RaBitQQuantizer,
        beam_width: usize,
        k: usize,
        filter: Option<&dyn Fn(VectorId) -> bool>,
    ) -> Vec<SearchCandidate> {
        let rot = quantizer.rotate(query);
        let q_norm = query.iter().map(|x| x * x).sum::<f32>();

        let results = self.search_internal(&rot, q_norm, beam_width, k, filter);
        results
            .into_iter()
            .map(|(node_idx, dist)| {
                let vid = self.arena.entries[node_idx as usize].vector_id;
                SearchCandidate {
                    page_id: PageId::INVALID,
                    vector_id: vid,
                    distance: dist,
                }
            })
            .collect()
    }

    /// Serializes the entire graph into packed 4 KiB `PackedVectorPage`s in storage.
    ///
    /// Shrinks disk footprint down to ~3.5 MB for 10,000 vectors.
    pub fn persist_to_storage<S: Storage>(&self, cache: &Arc<PageCache<S>>) -> Result<PageId> {
        if self.is_empty() {
            return Ok(PageId::INVALID);
        }

        let mut pages = Vec::new();
        let mut cur_page = PackedVectorPage::new();

        for (i, entry) in self.arena.entries.iter().enumerate() {
            let start = entry.code_offset as usize;
            let end = start + (entry.code_len as usize);
            let code = self.arena.codes[start..end].to_vec();

            #[expect(clippy::cast_possible_truncation, reason = "index fits in u32")]
            let node = PackedVectorNode {
                node_idx: i as u32,
                vector_id: entry.vector_id,
                overflow_page: PageId::INVALID,
                alpha: entry.alpha,
                beta: entry.beta,
                norm: entry.norm,
                neighbors: self.adj[i].clone(),
                quantized_code: code,
            };

            if !cur_page.can_fit(node.serialized_size()) {
                pages.push(cur_page);
                cur_page = PackedVectorPage::new();
            }
            cur_page.push_node(node)?;
        }

        if !cur_page.nodes.is_empty() {
            pages.push(cur_page);
        }

        let mut first_pid = PageId::INVALID;
        for packed in pages {
            let pid = cache.allocate_page()?;
            if !first_pid.is_valid() {
                first_pid = pid;
            }
            let mut page = Page::default();
            packed.write_to_page(pid, &mut page)?;
            cache.write_page(page)?;
        }

        Ok(first_pid)
    }
}
