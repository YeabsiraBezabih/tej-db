use crate::cbo::cost::CostModel;
use crate::plan::Plan;

/// Indicates whether a query contains vector distance search predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorPredicate {
    Present,
    Absent,
}

/// Physical execution path chosen by the Adaptive Execution Router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPath {
    /// Low-latency row-at-a-time Volcano execution against B+ Tree indices (OLTP point queries).
    VolcanoRowIndex,
    /// High-throughput vectorized SIMD execution over Columnar Row-Groups (OLAP analytical queries).
    VectorizedColumnar,
    /// Hybrid relational + vector search path combining DiskANN beam search with relational filtering.
    VectorHybrid,
}

/// Dynamic query execution router choosing between row-oriented and vectorized engines.
#[derive(Debug, Clone, Copy, Default)]
pub struct AdaptiveQueryRouter {
    cost_model: CostModel,
}

impl AdaptiveQueryRouter {
    #[must_use]
    pub fn new(cost_model: CostModel) -> Self {
        Self { cost_model }
    }

    /// Selects optimal execution path for a given SQL plan and table statistics.
    #[must_use]
    pub fn route_plan(
        &self,
        plan: &Plan,
        estimated_rows: u64,
        vector_pred: VectorPredicate,
    ) -> ExecutionPath {
        if vector_pred == VectorPredicate::Present {
            return ExecutionPath::VectorHybrid;
        }

        match plan {
            // Point queries by PK or single row updates/deletes always take the Volcano row-store fast path
            Plan::Select {
                limit: Some(lim), ..
            } if *lim <= 100 => ExecutionPath::VolcanoRowIndex,
            Plan::Select { .. } if estimated_rows <= 1_000 => {
                let index_cost = self.cost_model.cost_index_seek(3, estimated_rows);
                let col_cost =
                    self.cost_model
                        .cost_columnar_scan(10, estimated_rows, 2, 5, 2.0, 0.0);
                if index_cost <= col_cost {
                    ExecutionPath::VolcanoRowIndex
                } else {
                    ExecutionPath::VectorizedColumnar
                }
            }
            // Analytical queries (large scans, joins, full table aggregates) take the vectorized engine
            Plan::Select { .. } => ExecutionPath::VectorizedColumnar,
            _ => ExecutionPath::VolcanoRowIndex,
        }
    }
}
