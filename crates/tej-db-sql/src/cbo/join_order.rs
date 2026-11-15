use crate::cbo::cost::CostModel;

/// Descriptor of a table participation in a join graph.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinRelation {
    pub table_name: String,
    pub estimated_rows: u64,
}

/// Optimizes join ordering for multi-way joins based on cost and cardinality estimates.
#[derive(Debug, Clone, Copy)]
pub struct JoinOrderOptimizer;

impl JoinOrderOptimizer {
    /// Determines the optimal join pair ordering (build side vs. probe side).
    /// Places smaller table on the build side to minimize hash table size and build latency.
    #[must_use]
    pub fn order_binary_join(
        left: &JoinRelation,
        right: &JoinRelation,
    ) -> (JoinRelation, JoinRelation) {
        if left.estimated_rows <= right.estimated_rows {
            // Left is smaller -> build left, probe right
            (left.clone(), right.clone())
        } else {
            // Right is smaller -> build right, probe left
            (right.clone(), left.clone())
        }
    }

    /// Orders a set of multiple relations using greedy cardinality ordering.
    #[must_use]
    pub fn order_multi_join(mut relations: Vec<JoinRelation>) -> Vec<JoinRelation> {
        relations.sort_by_key(|r| r.estimated_rows);
        relations
    }

    /// Estimates total join pipeline cost.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]
    pub fn estimate_pipeline_cost(
        cost_model: &CostModel,
        ordered_relations: &[JoinRelation],
    ) -> f64 {
        if ordered_relations.len() < 2 {
            return 0.0;
        }

        let mut total_cost = 0.0;
        let mut cur_intermediate_rows = ordered_relations[0].estimated_rows;

        for next_rel in &ordered_relations[1..] {
            total_cost += cost_model.cost_hash_join(cur_intermediate_rows, next_rel.estimated_rows);
            // Rough intermediate estimation (selectivity factor 0.1)
            cur_intermediate_rows =
                ((cur_intermediate_rows as f64) * (next_rel.estimated_rows as f64) * 0.0001) as u64;
            cur_intermediate_rows = cur_intermediate_rows.max(1);
        }

        total_cost
    }
}
