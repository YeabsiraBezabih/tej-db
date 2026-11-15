use crate::expr::evaluate_expr;
use crate::schema::TableSchema;
use crate::value::Value;
use roaring::RoaringBitmap;
use sqlparser::ast::Expr as SqlExpr;
use tej_db_core::{DbError, Result};
use tej_db_vector::simd::{cosine_distance, l2_distance_squared};
use tej_db_vector::{DistanceMetric, SearchCandidate, VectorId};

/// Selectivity threshold below which hybrid search falls back to brute-force SIMD scan.
pub const BRUTE_FORCE_SELECTIVITY_THRESHOLD: f32 = 0.05;

/// Pre-filter execution strategy determined by query selectivity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HybridExecutionStrategy {
    /// High/medium selectivity: use Vamana graph beam search with pre-filter predicate.
    VamanaPreFilter,
    /// Very low selectivity: direct brute-force SIMD scan over filtered rows.
    BruteForceScan,
}

/// Evaluates a candidate row against a SQL filter expression and populates a Roaring Bitmap.
pub fn build_row_id_bitmap<'a>(
    schema: &TableSchema,
    filter: &SqlExpr,
    rows: impl IntoIterator<Item = (u64, &'a [Value])>,
) -> Result<RoaringBitmap> {
    let mut bitmap = RoaringBitmap::new();
    for (row_id, row) in rows {
        let val = evaluate_expr(filter, schema, row)?;
        if matches!(val, Value::Boolean(true)) {
            #[allow(clippy::cast_possible_truncation)]
            bitmap.insert(row_id as u32);
        }
    }
    Ok(bitmap)
}

/// Determines the optimal execution strategy for hybrid search given total table size and matching filter count.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn choose_hybrid_strategy(
    matched_count: u64,
    total_count: u64,
    top_k: usize,
) -> HybridExecutionStrategy {
    if total_count == 0 || matched_count == 0 {
        return HybridExecutionStrategy::BruteForceScan;
    }
    let matched_usize = matched_count as usize;
    if matched_usize <= top_k.saturating_mul(2) {
        return HybridExecutionStrategy::BruteForceScan;
    }
    let selectivity = (matched_count as f32) / (total_count as f32);
    if selectivity < BRUTE_FORCE_SELECTIVITY_THRESHOLD {
        HybridExecutionStrategy::BruteForceScan
    } else {
        HybridExecutionStrategy::VamanaPreFilter
    }
}

/// Performs a brute-force SIMD distance scan over rows that passed the pre-filter.
pub fn brute_force_simd_scan(
    filtered_vectors: &[(VectorId, &[f32])],
    query: &[f32],
    metric: DistanceMetric,
    top_k: usize,
) -> Result<Vec<SearchCandidate>> {
    let mut candidates = Vec::with_capacity(filtered_vectors.len());

    for &(id, vec) in filtered_vectors {
        if vec.len() != query.len() {
            return Err(DbError::Custom(format!(
                "Vector dimension mismatch in SIMD scan: {} vs {}",
                vec.len(),
                query.len()
            )));
        }
        let distance = match metric {
            DistanceMetric::Cosine => cosine_distance(query, vec),
            DistanceMetric::L2 => l2_distance_squared(query, vec).sqrt(),
            DistanceMetric::DotProduct => -tej_db_vector::simd::dot_product(query, vec),
        };
        candidates.push(SearchCandidate {
            page_id: tej_db_core::PageId::INVALID,
            vector_id: id,
            distance,
        });
    }

    candidates.sort_by(|a, b| {
        a.distance
            .partial_cmp(&b.distance)
            .unwrap_or(core::cmp::Ordering::Equal)
    });
    candidates.truncate(top_k);

    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ColumnDef;

    #[test]
    fn test_hybrid_strategy_selection() {
        // Very few matches -> BruteForceScan
        assert_eq!(
            choose_hybrid_strategy(5, 10_000, 10),
            HybridExecutionStrategy::BruteForceScan
        );

        // Low selectivity (< 5%) -> BruteForceScan
        assert_eq!(
            choose_hybrid_strategy(300, 10_000, 10),
            HybridExecutionStrategy::BruteForceScan
        );

        // High selectivity (> 5%) -> VamanaPreFilter
        assert_eq!(
            choose_hybrid_strategy(2_000, 10_000, 10),
            HybridExecutionStrategy::VamanaPreFilter
        );
    }

    #[test]
    fn test_brute_force_simd_scan() {
        let v1 = [1.0f32, 0.0, 0.0];
        let v2 = [0.0f32, 1.0, 0.0];
        let v3 = [0.9f32, 0.1, 0.0];

        let dataset = [
            (VectorId(1), v1.as_slice()),
            (VectorId(2), v2.as_slice()),
            (VectorId(3), v3.as_slice()),
        ];

        let query = [1.0f32, 0.0, 0.0];
        let results = brute_force_simd_scan(&dataset, &query, DistanceMetric::Cosine, 2).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].vector_id, VectorId(1));
        assert!(results[0].distance < 1e-4);
        assert_eq!(results[1].vector_id, VectorId(3));
    }

    #[test]
    fn test_build_row_id_bitmap() {
        let schema = TableSchema::new(
            "test",
            vec![
                ColumnDef {
                    name: "id".into(),
                    data_type: crate::schema::SqlType::Integer,
                    is_primary_key: true,
                    is_nullable: false,
                },
                ColumnDef {
                    name: "active".into(),
                    data_type: crate::schema::SqlType::Boolean,
                    is_primary_key: false,
                    is_nullable: false,
                },
            ],
        );

        let r1 = [Value::Integer(1), Value::Boolean(true)];
        let r2 = [Value::Integer(2), Value::Boolean(false)];
        let r3 = [Value::Integer(3), Value::Boolean(true)];

        let rows = [
            (1u64, r1.as_slice()),
            (2u64, r2.as_slice()),
            (3u64, r3.as_slice()),
        ];

        let filter = SqlExpr::BinaryOp {
            left: Box::new(SqlExpr::Identifier(sqlparser::ast::Ident::new("active"))),
            op: sqlparser::ast::BinaryOperator::Eq,
            right: Box::new(SqlExpr::Value(sqlparser::ast::Value::Boolean(true))),
        };

        let bitmap = build_row_id_bitmap(&schema, &filter, rows).unwrap();
        assert_eq!(bitmap.len(), 2);
        assert!(bitmap.contains(1));
        assert!(!bitmap.contains(2));
        assert!(bitmap.contains(3));
    }
}
