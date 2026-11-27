use super::*;
use crate::plan::Plan;
use tej_db_core::KeyRange;

#[test]
#[allow(clippy::cast_sign_loss)]
fn test_hyperloglog_cardinality_estimation() {
    let mut hll = HyperLogLog::new();
    // Insert 500 distinct items (using a simple hash distribution)
    for i in 0..500 {
        let hash = (i as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        hll.add_hash(hash);
    }

    let estimate = hll.estimate();
    // HLL standard error for m=64 is ~13%. Estimate should be within 400..650.
    assert!((350..=700).contains(&estimate), "Estimate was {estimate}");
}

#[test]
fn test_column_histogram_selectivity() {
    let mut hist = ColumnHistogram::new(0, 1000);
    for i in 0..1000 {
        hist.add_value(i);
    }

    // Selectivity for 200..400 should be ~0.20
    let sel = hist.estimate_range_selectivity(Some(200), Some(400));
    assert!((0.15..=0.25).contains(&sel), "Selectivity was {sel}");

    // Selectivity for out-of-range should be 0.0
    let out_sel = hist.estimate_range_selectivity(Some(1200), Some(1500));
    assert_eq!(out_sel, 0.0);
}

#[test]
fn test_join_order_optimizer() {
    let r1 = JoinRelation {
        table_name: "small_dim".into(),
        estimated_rows: 100,
    };
    let r2 = JoinRelation {
        table_name: "large_fact".into(),
        estimated_rows: 1_000_000,
    };

    let (build, probe) = JoinOrderOptimizer::order_binary_join(&r1, &r2);
    assert_eq!(build.table_name, "small_dim");
    assert_eq!(probe.table_name, "large_fact");
}

#[test]
fn test_adaptive_router_path_selection() {
    let router = AdaptiveQueryRouter::default();

    // 1. Point lookup with LIMIT 10 -> VolcanoRowIndex
    let point_plan = Plan::Select {
        table: "users".into(),
        projection: Vec::new(),
        key_range: KeyRange::all(),
        filter: None,
        distinct: false,
        order_by: Vec::new(),
        limit: Some(10),
        offset: None,
    };
    assert_eq!(
        router.route_plan(&point_plan, 10, VectorPredicate::Absent),
        ExecutionPath::VolcanoRowIndex
    );

    // 2. Large analytical scan (100k rows) -> VectorizedColumnar
    let olap_plan = Plan::Select {
        table: "events".into(),
        projection: Vec::new(),
        key_range: KeyRange::all(),
        filter: None,
        distinct: false,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    assert_eq!(
        router.route_plan(&olap_plan, 100_000, VectorPredicate::Absent),
        ExecutionPath::VectorizedColumnar
    );

    // 3. Vector hybrid search -> VectorHybrid
    assert_eq!(
        router.route_plan(&olap_plan, 100_000, VectorPredicate::Present),
        ExecutionPath::VectorHybrid
    );
}
