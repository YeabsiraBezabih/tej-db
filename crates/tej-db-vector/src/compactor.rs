use crate::delta::{delta_table_name, scan_vector_delta};
use crate::node::VectorNode;
use crate::rabitq::RaBitQQuantizer;
use crate::types::{DEFAULT_ALPHA_PRUNING, DEFAULT_DEGREE_R, MAX_DEGREE_R, VectorId};
use crate::vamana::VamanaSearcher;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tej_db_core::{Page, PageId, Result, Storage};
use tej_db_txn::TxManager;

/// Background compactor thread coordinator for merging FreshDiskANN delta buffers
/// into the persistent Vamana graph without blocking concurrent reads or writes.
pub struct VectorCompactor<S: Storage> {
    pub manager: Arc<TxManager<S>>,
    pub quantizer: RaBitQQuantizer,
    pub stop_signal: Arc<AtomicBool>,
}

impl<S: Storage> core::fmt::Debug for VectorCompactor<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VectorCompactor")
            .field("stopped", &self.stop_signal.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<S: Storage> VectorCompactor<S> {
    #[must_use]
    pub fn new(manager: Arc<TxManager<S>>, quantizer: RaBitQQuantizer) -> Self {
        Self {
            manager,
            quantizer,
            stop_signal: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Folds pending vector mutations from the table's delta buffer into the Vamana graph.
    ///
    /// Returns the new root `PageId` of the Vamana graph after compaction.
    pub fn compact_table_delta(&self, table_name: &str, current_root: PageId) -> Result<PageId> {
        // 1. Read pending delta entries using a snapshot read transaction
        let deltas = {
            let rx = self.manager.begin_read()?;
            scan_vector_delta(&rx, table_name)?
        };

        if deltas.is_empty() {
            return Ok(current_root);
        }

        let searcher =
            VamanaSearcher::new(Arc::clone(self.manager.cache()), self.quantizer.clone());

        let mut active_root = current_root;
        let mut committed_ids = Vec::with_capacity(deltas.len());

        // 2. Process each mutation
        for delta in deltas {
            if let Some(vector) = delta.vector {
                // Vector insertion / update
                let (code, alpha, beta, norm) = self.quantizer.quantize(&vector);
                let new_pid = self.manager.cache().allocate_page()?;

                let mut node =
                    VectorNode::new(delta.vector_id, PageId::INVALID, alpha, beta, norm, code);

                if active_root.is_valid() {
                    // Find neighbors via beam search
                    let mut candidates = searcher.search(
                        active_root,
                        &vector,
                        DEFAULT_DEGREE_R * 2,
                        DEFAULT_DEGREE_R * 2,
                        None,
                    )?;

                    let pruned_neighbors = VamanaSearcher::<S>::robust_prune(
                        &mut candidates,
                        DEFAULT_DEGREE_R,
                        DEFAULT_ALPHA_PRUNING,
                    );

                    for &nb in &pruned_neighbors {
                        node.neighbors.push(nb);
                        // Add reverse edge to neighbor so traversal can walk back
                        let _ = self.manager.cache().with_page_mut(nb, |p| {
                            if let Ok(mut nb_node) = VectorNode::from_page(p) {
                                if !nb_node.neighbors.contains(&new_pid) {
                                    if nb_node.neighbors.len() < MAX_DEGREE_R {
                                        nb_node.neighbors.push(new_pid);
                                    } else {
                                        let len = nb_node.neighbors.len();
                                        nb_node.neighbors[len - 1] = new_pid;
                                    }
                                    let _ = nb_node.write_to_page(nb, p);
                                }
                            }
                            Ok(())
                        });
                    }
                } else {
                    active_root = new_pid;
                }

                let mut page = Page::default();
                node.write_to_page(new_pid, &mut page)?;
                self.manager.cache().write_page(page)?;
                committed_ids.push(delta.vector_id);
            } else {
                // Tombstone deletion: mark node as deleted in the Vamana graph
                if active_root.is_valid() {
                    if let Some(alt_root) = self.mark_node_deleted(active_root, delta.vector_id) {
                        active_root = alt_root;
                    }
                }
                committed_ids.push(delta.vector_id);
            }
        }

        // 3. Clear processed entries from the delta table
        let delta_name = delta_table_name(table_name);
        let mut tx = self.manager.begin_write()?;
        {
            let mut tbl = tx.open_table_mut(&delta_name)?;
            for id in committed_ids {
                let key = id.0.to_be_bytes();
                tbl.delete(&key)?;
            }
        }
        tx.commit()?;

        Ok(active_root)
    }

    fn mark_node_deleted(&self, root: PageId, target_id: VectorId) -> Option<PageId> {
        let mut queue = std::collections::VecDeque::new();
        let mut visited = std::collections::HashSet::new();
        queue.push_back(root);
        visited.insert(root);

        while let Some(pid) = queue.pop_front() {
            let mut found = false;
            let is_root = pid == root;
            let mut fallback_root = None;
            let mut neighbors = smallvec::SmallVec::<[PageId; MAX_DEGREE_R]>::new();

            let _ = self.manager.cache().with_page_mut(pid, |p| {
                if let Ok(mut node) = VectorNode::from_page(p) {
                    if node.metadata.vector_id == target_id {
                        node.metadata.is_deleted = true;
                        let _ = node.write_to_page(pid, p);
                        found = true;
                        if is_root {
                            fallback_root = node.neighbors.first().copied();
                        }
                    } else {
                        neighbors.clone_from(&node.neighbors);
                    }
                }
                Ok(())
            });

            if found {
                return fallback_root;
            }

            for nb in neighbors {
                if nb.is_valid() && visited.insert(nb) {
                    queue.push_back(nb);
                }
            }
        }
        None
    }

    /// Signals the background compactor to gracefully shut down.
    pub fn stop(&self) {
        self.stop_signal.store(true, Ordering::Release);
    }
}
