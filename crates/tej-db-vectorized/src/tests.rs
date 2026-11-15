use super::*;
use tej_db_columnar::{DataChunk, PhysicalType, RowGroup};

fn create_sample_row_group(start: i64, count: usize) -> RowGroup {
    let types = vec![PhysicalType::Int64, PhysicalType::Int64];
    let mut chunk = DataChunk::new(&types, count);

    let col0 = chunk.column_mut(0).unwrap();
    for i in 0..count {
        let val = start + i64::try_from(i).unwrap_or(0);
        col0.push_i64(val).unwrap();
    }

    let col1 = chunk.column_mut(1).unwrap();
    for i in 0..count {
        let val = (start + i64::try_from(i).unwrap_or(0)) * 10;
        col1.push_i64(val).unwrap();
    }

    chunk.set_num_rows(count).unwrap();
    RowGroup::from_chunk(&chunk).unwrap()
}

#[test]
fn test_vectorized_scan_and_filter() {
    let rg1 = create_sample_row_group(0, 100);
    let rg2 = create_sample_row_group(100, 100);

    let scan = PhysicalColumnScan::new(vec![rg1, rg2], vec![0, 1], Vec::new());
    let filter = PhysicalFilter::new(
        Box::new(scan),
        vec![FilterExpr::CompareInt64 {
            column_idx: 0,
            op: ComparisonOp::Gte,
            val: 150,
        }],
    );

    let mut op: Box<dyn PhysicalOperator> = Box::new(filter);
    op.open().unwrap();

    let mut total_rows = 0;
    while let Some(chunk) = op.next().unwrap() {
        total_rows += chunk.num_rows();
        let col0 = chunk.column(0).unwrap();
        for r in 0..chunk.num_rows() {
            assert!(col0.get_i64(r).unwrap() >= 150);
        }
    }
    op.close().unwrap();

    assert_eq!(total_rows, 50); // 150..200
}

#[test]
fn test_vectorized_zonemap_block_skipping() {
    let rg1 = create_sample_row_group(0, 100); // 0..99
    let rg2 = create_sample_row_group(100, 100); // 100..199
    let rg3 = create_sample_row_group(200, 100); // 200..299

    // Predicate: col 0 == 250 (should skip rg1 and rg2 completely via ZoneMap!)
    let mut scan = PhysicalColumnScan::new(
        vec![rg1, rg2, rg3],
        vec![0],
        vec![ZoneMapPredicate::EqualInt64 {
            column_idx: 0,
            value: 250,
        }],
    );

    scan.open().unwrap();
    let chunk = scan.next().unwrap().expect("Should produce chunk from rg3");
    assert_eq!(chunk.num_rows(), 100);
    assert_eq!(chunk.column(0).unwrap().get_i64(0), Some(200));

    // Next should be None (rg1 and rg2 skipped, rg3 consumed)
    assert!(scan.next().unwrap().is_none());
    scan.close().unwrap();
}

#[test]
fn test_vectorized_hash_aggregation() {
    let types = vec![PhysicalType::Int64, PhysicalType::Int64];
    let mut chunk = DataChunk::new(&types, 100);

    let col0 = chunk.column_mut(0).unwrap();
    for i in 0..100 {
        col0.push_i64(i % 5).unwrap(); // 5 distinct groups: 0, 1, 2, 3, 4 (20 rows each)
    }

    let col1 = chunk.column_mut(1).unwrap();
    for _ in 0..100 {
        col1.push_i64(10).unwrap(); // Sum per group = 20 * 10 = 200
    }

    chunk.set_num_rows(100).unwrap();
    let rg = RowGroup::from_chunk(&chunk).unwrap();

    let scan = PhysicalColumnScan::new(vec![rg], vec![0, 1], Vec::new());
    let mut agg = PhysicalHashAggregate::new(
        Box::new(scan),
        Some(0), // Group by column 0
        vec![
            (1, AggregateFunction::Count),
            (1, AggregateFunction::SumInt64),
        ],
    );

    agg.open().unwrap();
    let res_chunk = agg.next().unwrap().expect("Expected aggregate chunk");
    assert_eq!(res_chunk.num_rows(), 5);

    let key_col = res_chunk.column(0).unwrap();
    let count_col = res_chunk.column(1).unwrap();
    let sum_col = res_chunk.column(2).unwrap();

    for r in 0..5 {
        assert!(key_col.get_i64(r).is_some());
        assert_eq!(count_col.get_i64(r), Some(20));
        assert_eq!(sum_col.get_i64(r), Some(200));
    }

    assert!(agg.next().unwrap().is_none());
    agg.close().unwrap();
}

#[test]
fn test_vectorized_hash_join() {
    // Build table: (user_id: 1..10, role_id: 100)
    let b_types = vec![PhysicalType::Int64, PhysicalType::Int64];
    let mut b_chunk = DataChunk::new(&b_types, 10);
    for i in 1..=10 {
        b_chunk.column_mut(0).unwrap().push_i64(i).unwrap();
        b_chunk.column_mut(1).unwrap().push_i64(100).unwrap();
    }
    b_chunk.set_num_rows(10).unwrap();
    let b_rg = RowGroup::from_chunk(&b_chunk).unwrap();

    // Probe table: (order_id: 1001..1005, user_id: 3)
    let p_types = vec![PhysicalType::Int64, PhysicalType::Int64];
    let mut p_chunk = DataChunk::new(&p_types, 5);
    for i in 1..=5 {
        p_chunk.column_mut(0).unwrap().push_i64(1000 + i).unwrap();
        p_chunk.column_mut(1).unwrap().push_i64(3).unwrap(); // user_id = 3
    }
    p_chunk.set_num_rows(5).unwrap();
    let p_rg = RowGroup::from_chunk(&p_chunk).unwrap();

    let build_scan = PhysicalColumnScan::new(vec![b_rg], vec![0, 1], Vec::new());
    let probe_scan = PhysicalColumnScan::new(vec![p_rg], vec![0, 1], Vec::new());

    let mut join = PhysicalHashJoin::new(
        Box::new(build_scan),
        Box::new(probe_scan),
        0, // build key: user_id
        1, // probe key: user_id
        vec![PhysicalType::Int64, PhysicalType::Int64],
        vec![PhysicalType::Int64, PhysicalType::Int64],
    );

    join.open().unwrap();
    let joined = join.next().unwrap().expect("Expected joined chunk");
    assert_eq!(joined.num_rows(), 5);

    // Joined columns: [b_user_id, b_role_id, p_order_id, p_user_id]
    assert_eq!(joined.column(0).unwrap().get_i64(0), Some(3));
    assert_eq!(joined.column(1).unwrap().get_i64(0), Some(100));
    assert_eq!(joined.column(2).unwrap().get_i64(0), Some(1001));
    assert_eq!(joined.column(3).unwrap().get_i64(0), Some(3));

    assert!(join.next().unwrap().is_none());
    join.close().unwrap();
}

#[test]
fn test_morsel_scheduler_concurrent_dispatch() {
    let scheduler = std::sync::Arc::new(MorselScheduler::new(4, 1000, 250));
    // 4 groups * (1000 / 250 = 4 morsels) = 16 morsels total

    let mut handles = Vec::new();
    for _ in 0..4 {
        let s = std::sync::Arc::clone(&scheduler);
        handles.push(std::thread::spawn(move || {
            let mut count = 0;
            while let Some(m) = s.next_morsel() {
                assert!(m.row_count > 0 && m.row_count <= 250);
                count += 1;
            }
            count
        }));
    }

    let total_claimed: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(total_claimed, 16);
}

#[test]
fn test_spillable_buffer_external_merge_sort() {
    // 100 bytes limit so it spills repeatedly
    let mut buffer = SpillableBuffer::new(100);

    let keys = vec![50, 10, 40, 90, 20, 80, 30, 70, 60, 100];
    for &k in &keys {
        buffer.append(k, vec![k * 10]).unwrap();
    }

    assert!(buffer.num_spilled_runs() > 1);

    let chunks = buffer.merge_all(5).unwrap();
    let mut merged_keys = Vec::new();

    for chunk in chunks {
        let col = chunk.column(0).unwrap();
        for r in 0..chunk.num_rows() {
            merged_keys.push(col.get_i64(r).unwrap());
        }
    }

    let mut expected = keys;
    expected.sort_unstable();
    assert_eq!(merged_keys, expected);
}

#[test]
fn test_parallel_scan_aggregate_and_tpch_q6() {
    use crate::parallel::{parallel_hash_aggregate, parallel_scan_and_filter, parallel_tpch_q6};

    let types = vec![
        PhysicalType::Int64,
        PhysicalType::Int64,
        PhysicalType::Int64,
        PhysicalType::Int64,
    ];
    let mut chunk = DataChunk::new(&types, 100);
    for i in 0..100 {
        chunk
            .column_mut(0)
            .unwrap()
            .push_i64(1000 + i64::from(i))
            .unwrap(); // price
        chunk.column_mut(1).unwrap().push_i64(6).unwrap(); // discount (6 >= 5 && <= 7)
        chunk.column_mut(2).unwrap().push_i64(19940501).unwrap(); // shipdate (1994)
        chunk.column_mut(3).unwrap().push_i64(10).unwrap(); // quantity (10 < 24)
    }
    chunk.set_num_rows(100).unwrap();
    let rg = RowGroup::from_chunk(&chunk).unwrap();
    let row_groups = vec![rg.clone(), rg];

    // 1. Parallel Scan and Filter
    let filter = crate::filter::FilterExpr::CompareInt64 {
        column_idx: 0,
        op: crate::filter::ComparisonOp::Gte,
        val: 1050,
    };
    let count = parallel_scan_and_filter(&row_groups, &[], &[filter], 2).unwrap();
    assert_eq!(count, 100); // 50 per chunk * 2 chunks

    // 2. Parallel Hash Aggregate
    let agg_chunk = parallel_hash_aggregate(
        &row_groups,
        1, // group by discount
        &[
            (0, crate::aggregate::AggregateFunction::Count),
            (0, crate::aggregate::AggregateFunction::SumInt64),
        ],
        2,
    )
    .unwrap();
    assert_eq!(agg_chunk.num_rows(), 1);
    assert_eq!(agg_chunk.column(1).unwrap().get_i64(0).unwrap(), 200); // count = 200

    // 3. Parallel TPC-H Q6
    let (total_sum, matched) = parallel_tpch_q6(&row_groups, &[], 2).unwrap();
    assert_eq!(matched, 200);
    assert!(total_sum > 0);
}

#[test]
fn test_spatial_frustum_and_raycast_culling() {
    use crate::spatial_culling::{
        FrustumPlanes, Ray3D, cull_clusters_morsel, raycast_closest_cluster,
    };
    use roaring::RoaringBitmap;

    // View frustum centered looking down -Z axis with near=1, far=100
    // Frustum planes: left, right, bottom, top, near, far
    let frustum = FrustumPlanes::new([
        [1.0, 0.0, 0.0, 10.0],  // x >= -10
        [-1.0, 0.0, 0.0, 10.0], // x <= 10
        [0.0, 1.0, 0.0, 10.0],  // y >= -10
        [0.0, -1.0, 0.0, 10.0], // y <= 10
        [0.0, 0.0, -1.0, 0.0],  // z <= 0 (looking towards negative z)
        [0.0, 0.0, 1.0, 100.0], // z >= -100
    ]);

    // Construct cluster table: [id, cx, cy, cz, radius, lod_error]
    let types = vec![
        PhysicalType::Int64,
        PhysicalType::Float64,
        PhysicalType::Float64,
        PhysicalType::Float64,
        PhysicalType::Float64,
        PhysicalType::Float64,
    ];
    let mut chunk = DataChunk::new(&types, 3);
    // Cluster 1: In view at (0, 0, -20), radius 2, lod_error 0.05
    chunk.column_mut(0).unwrap().push_i64(1).unwrap();
    chunk.column_mut(1).unwrap().push_f64(0.0).unwrap();
    chunk.column_mut(2).unwrap().push_f64(0.0).unwrap();
    chunk.column_mut(3).unwrap().push_f64(-20.0).unwrap();
    chunk.column_mut(4).unwrap().push_f64(2.0).unwrap();
    chunk.column_mut(5).unwrap().push_f64(0.05).unwrap();

    // Cluster 2: Far outside frustum at (50, 0, -20), radius 2
    chunk.column_mut(0).unwrap().push_i64(2).unwrap();
    chunk.column_mut(1).unwrap().push_f64(50.0).unwrap();
    chunk.column_mut(2).unwrap().push_f64(0.0).unwrap();
    chunk.column_mut(3).unwrap().push_f64(-20.0).unwrap();
    chunk.column_mut(4).unwrap().push_f64(2.0).unwrap();
    chunk.column_mut(5).unwrap().push_f64(0.05).unwrap();

    // Cluster 3: Behind camera at (0, 0, 20), radius 2
    chunk.column_mut(0).unwrap().push_i64(3).unwrap();
    chunk.column_mut(1).unwrap().push_f64(0.0).unwrap();
    chunk.column_mut(2).unwrap().push_f64(0.0).unwrap();
    chunk.column_mut(3).unwrap().push_f64(20.0).unwrap();
    chunk.column_mut(4).unwrap().push_f64(2.0).unwrap();
    chunk.column_mut(5).unwrap().push_f64(0.05).unwrap();

    chunk.set_num_rows(3).unwrap();

    let mut visible = RoaringBitmap::new();
    cull_clusters_morsel(&chunk, &frustum, [0.0, 0.0, 0.0], 1.0, &mut visible);

    assert!(visible.contains(1), "Cluster 1 should be visible");
    assert!(
        !visible.contains(2),
        "Cluster 2 is outside right frustum plane"
    );
    assert!(!visible.contains(3), "Cluster 3 is behind camera");

    // Raycast test: Ray from (0, 0, 0) towards (0, 0, -1)
    let ray = Ray3D::new([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);

    // Construct AABB chunk: [id, min_x, min_y, min_z, max_x, max_y, max_z]
    let aabb_types = vec![
        PhysicalType::Int64,
        PhysicalType::Float64,
        PhysicalType::Float64,
        PhysicalType::Float64,
        PhysicalType::Float64,
        PhysicalType::Float64,
        PhysicalType::Float64,
    ];
    let mut aabb_chunk = DataChunk::new(&aabb_types, 2);
    // Cluster 1 AABB: [-2, -2, -22] to [2, 2, -18]
    aabb_chunk.column_mut(0).unwrap().push_i64(1).unwrap();
    aabb_chunk.column_mut(1).unwrap().push_f64(-2.0).unwrap();
    aabb_chunk.column_mut(2).unwrap().push_f64(-2.0).unwrap();
    aabb_chunk.column_mut(3).unwrap().push_f64(-22.0).unwrap();
    aabb_chunk.column_mut(4).unwrap().push_f64(2.0).unwrap();
    aabb_chunk.column_mut(5).unwrap().push_f64(2.0).unwrap();
    aabb_chunk.column_mut(6).unwrap().push_f64(-18.0).unwrap();

    // Cluster 4 AABB further along ray: [-2, -2, -60] to [2, 2, -50]
    aabb_chunk.column_mut(0).unwrap().push_i64(4).unwrap();
    aabb_chunk.column_mut(1).unwrap().push_f64(-2.0).unwrap();
    aabb_chunk.column_mut(2).unwrap().push_f64(-2.0).unwrap();
    aabb_chunk.column_mut(3).unwrap().push_f64(-60.0).unwrap();
    aabb_chunk.column_mut(4).unwrap().push_f64(2.0).unwrap();
    aabb_chunk.column_mut(5).unwrap().push_f64(2.0).unwrap();
    aabb_chunk.column_mut(6).unwrap().push_f64(-50.0).unwrap();

    aabb_chunk.set_num_rows(2).unwrap();

    let hit = raycast_closest_cluster(&aabb_chunk, &ray, None);
    assert_eq!(hit, Some((1, 18.0)), "Should hit cluster 1 first at t=18.0");
}
