#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::uninlined_format_args,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

mod bench_htap_aux;

use bench_htap_aux::{
    bench_bm25_text_search, bench_columnar_codecs_and_zonemap, bench_parquet_io, bench_tpch_q6,
    bench_window_functions, run_duckdb_timed_query,
};
use rusqlite::Connection;
use std::sync::Arc;
use std::time::Instant;
use tej_db::Database;
use tej_db_columnar::{DataChunk, PhysicalType, RowGroup};
use tej_db_sql::{SqlEngine, Value};
use tej_db_vectorized::{
    AggregateFunction, ComparisonOp, FilterExpr, PhysicalColumnScan, PhysicalHashJoin,
    PhysicalOperator, ZoneMapPredicate, parallel_hash_aggregate, parallel_scan_and_filter,
};

fn main() {
    println!("==========================================================================");
    println!("  TejDb HTAP + Vector Unified Engine Benchmark (OLTP + OLAP + Vector)      ");
    println!("==========================================================================\n");

    bench_oltp_sqlite_comparison();
    bench_columnar_codecs_and_zonemap();
    bench_vectorized_olap_pipelines();
    bench_unified_htap_hybrid();
    bench_parquet_io();
    bench_bm25_text_search();
    bench_window_functions();
    bench_tpch_q6();

    println!("\n==========================================================================");
    println!("  All HTAP Benchmarks Complete Successfully                               ");
    println!("==========================================================================");
}

fn bench_oltp_sqlite_comparison() {
    println!("--- 1. OLTP Benchmark: TejDb vs SQLite (Prepared Statements) ---");

    const NUM_ROWS: usize = 10_000;

    // 1.1 TejDb Point Inserts (Compiled PreparedStatement)
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);
    sql.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT, balance INT)")
        .unwrap();

    let stmt = sql
        .prepare_statement("INSERT INTO users (id, name, balance) VALUES (?, ?, ?)")
        .unwrap();

    let start = Instant::now();
    let mut tx = db.begin_write().unwrap();
    {
        let mut tbl = tx.open_table_mut("users").unwrap();
        tbl.reserve(NUM_ROWS);
        let mut row_buf = Vec::with_capacity(128);
        for i in 0..NUM_ROWS {
            stmt.execute_in_table_buf(
                &mut tbl,
                &[
                    Value::Integer(i as i64),
                    Value::Text(format!("User_{i}")),
                    Value::Integer((i * 10) as i64),
                ],
                &mut row_buf,
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
    let tej_db_insert_time = start.elapsed();
    let tej_db_insert_ops = (NUM_ROWS as f64) / tej_db_insert_time.as_secs_f64();

    // 1.2 SQLite Point Inserts (Prepared Statement)
    let mut sqlite = Connection::open_in_memory().unwrap();
    sqlite
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, balance INTEGER)",
            [],
        )
        .unwrap();

    let tx_sqlite = sqlite.transaction().unwrap();
    let mut stmt_insert = tx_sqlite
        .prepare("INSERT INTO users VALUES (?, ?, ?)")
        .unwrap();

    let start = Instant::now();
    for i in 0..NUM_ROWS {
        stmt_insert
            .execute(rusqlite::params![i, format!("User_{i}"), i * 10])
            .unwrap();
    }
    drop(stmt_insert);
    tx_sqlite.commit().unwrap();
    let sqlite_insert_time = start.elapsed();
    let sqlite_insert_ops = (NUM_ROWS as f64) / sqlite_insert_time.as_secs_f64();

    // 1.3 TejDb Point Lookups (Compiled PreparedStatement)
    let query_keys = [100, 500, 1000, 2500, 5000, 7500, 9000, 9999];
    let stmt_select = sql
        .prepare_statement("SELECT id, name, balance FROM users WHERE id = ?")
        .unwrap();

    let rx = db.begin_read().unwrap();
    let tbl = rx.open_table("users").unwrap();
    let start = Instant::now();
    for _ in 0..1000 {
        for &k in &query_keys {
            let res = stmt_select
                .query_row_in_table(&tbl, &[Value::Integer(k as i64)])
                .unwrap();
            assert!(res.is_some());
        }
    }
    let tej_db_point_time = start.elapsed();
    let tej_db_point_ops = 8000.0 / tej_db_point_time.as_secs_f64();

    // 1.4 SQLite Point Lookups (Prepared Statement)
    let tx_read = sqlite.transaction().unwrap();
    let mut stmt_lookup = tx_read
        .prepare("SELECT id, name, balance FROM users WHERE id = ?")
        .unwrap();

    let start = Instant::now();
    for _ in 0..1000 {
        for &k in &query_keys {
            let mut rows = stmt_lookup.query(rusqlite::params![k]).unwrap();
            assert!(rows.next().unwrap().is_some());
        }
    }
    let sqlite_point_time = start.elapsed();
    let sqlite_point_ops = 8000.0 / sqlite_point_time.as_secs_f64();
    drop(stmt_lookup);
    tx_read.commit().unwrap();

    println!("  - Point Insert (10k rows tx):");
    println!(
        "    * TejDb:   {:8.2} ops/sec ({:.2?})",
        tej_db_insert_ops, tej_db_insert_time
    );
    println!(
        "    * SQLite: {:8.2} ops/sec ({:.2?}) [Ratio: {:.2}x]",
        sqlite_insert_ops,
        sqlite_insert_time,
        tej_db_insert_ops / sqlite_insert_ops
    );
    println!("  - Point Lookups (PK Equality):");
    println!(
        "    * TejDb:   {:8.2} ops/sec ({:.2?})",
        tej_db_point_ops, tej_db_point_time
    );
    println!(
        "    * SQLite: {:8.2} ops/sec ({:.2?}) [Ratio: {:.2}x]",
        sqlite_point_ops,
        sqlite_point_time,
        tej_db_point_ops / sqlite_point_ops
    );
}

fn bench_vectorized_olap_pipelines() {
    println!("\n--- 3. Vectorized OLAP Execution: TejDb vs DuckDB (1,000,000 Rows) ---");

    const CHUNK_ROWS: usize = 2048;
    const TOTAL_CHUNKS: usize = 500; // 1,024,000 rows
    let types = vec![PhysicalType::Int64, PhysicalType::Int64];

    let mut row_groups = Vec::with_capacity(TOTAL_CHUNKS);
    for chunk_idx in 0..TOTAL_CHUNKS {
        let mut chunk = DataChunk::new(&types, CHUNK_ROWS);
        let col0 = chunk.column_mut(0).unwrap();
        let base = (chunk_idx * CHUNK_ROWS) as i64;
        for i in 0..CHUNK_ROWS {
            col0.push_i64(base + (i as i64)).unwrap();
        }
        let col1 = chunk.column_mut(1).unwrap();
        for i in 0..CHUNK_ROWS {
            col1.push_i64((i % 10) as i64).unwrap(); // 10 distinct groups
        }
        chunk.set_num_rows(CHUNK_ROWS).unwrap();
        row_groups.push(RowGroup::from_chunk(&chunk).unwrap());
    }

    let total_rows = TOTAL_CHUNKS * CHUNK_ROWS;
    let row_groups: Arc<[RowGroup]> = row_groups.into();
    let num_threads = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);

    // 3.1 Vectorized Scan & Filter (Parallel Morsels + ZoneMap Pruning)
    let filter_exprs = vec![FilterExpr::CompareInt64 {
        column_idx: 0,
        op: ComparisonOp::Gte,
        val: (total_rows / 2) as i64,
    }];
    let zonemap_preds = vec![ZoneMapPredicate::RangeInt64 {
        column_idx: 0,
        low: Some((total_rows / 2) as i64),
        high: None,
    }];

    let start = Instant::now();
    let scanned_rows =
        parallel_scan_and_filter(&row_groups, &zonemap_preds, &filter_exprs, num_threads).unwrap();
    assert!(scanned_rows > 0);
    let tej_db_filter_time = start.elapsed();
    let tej_db_filter_throughput = (total_rows as f64) / tej_db_filter_time.as_secs_f64();

    // DuckDB Scan & Filter comparison
    let duck_setup = format!(
        "CREATE TABLE fact AS SELECT range AS id, range % 10 AS grp FROM range({total_rows});"
    );
    let duck_query = format!("SELECT count(*) FROM fact WHERE id >= {};", total_rows / 2);
    let duck_filter_time = run_duckdb_timed_query(&duck_setup, &duck_query);
    let duck_filter_throughput = (total_rows as f64) / duck_filter_time.as_secs_f64();

    println!("  - Scan + Filter (1M rows, id >= 500k):");
    println!(
        "    * TejDb:   {:10.2} rows/s ({:.2?})",
        tej_db_filter_throughput, tej_db_filter_time
    );
    println!(
        "    * DuckDB: {:10.2} rows/s ({:.2?}) [Ratio: {:.2}x]",
        duck_filter_throughput,
        duck_filter_time,
        tej_db_filter_throughput / duck_filter_throughput
    );

    // 3.2 Vectorized Hash Aggregation (GROUP BY - Parallel Morsels)
    let agg_specs = vec![
        (0, AggregateFunction::Count),
        (0, AggregateFunction::SumInt64),
    ];
    let start = Instant::now();
    let agg_res = parallel_hash_aggregate(&row_groups, 1, &agg_specs, num_threads).unwrap();
    assert_eq!(agg_res.num_rows(), 10);
    let tej_db_agg_time = start.elapsed();
    let tej_db_agg_throughput = (total_rows as f64) / tej_db_agg_time.as_secs_f64();

    // DuckDB Aggregation comparison
    let duck_setup = format!(
        "CREATE TABLE fact AS SELECT range AS id, range % 10 AS grp FROM range({total_rows});"
    );
    let duck_query = "SELECT grp, count(*), sum(id) FROM fact GROUP BY grp;";
    let duck_agg_time = run_duckdb_timed_query(&duck_setup, duck_query);
    let duck_agg_throughput = (total_rows as f64) / duck_agg_time.as_secs_f64();

    println!("  - Hash Aggregation (GROUP BY 10 groups, COUNT + SUM):");
    println!(
        "    * TejDb:   {:10.2} rows/s ({:.2?})",
        tej_db_agg_throughput, tej_db_agg_time
    );
    println!(
        "    * DuckDB: {:10.2} rows/s ({:.2?}) [Ratio: {:.2}x]",
        duck_agg_throughput,
        duck_agg_time,
        tej_db_agg_throughput / duck_agg_throughput
    );

    // 3.3 Vectorized Hash Join
    let mut small_chunk = DataChunk::new(&types, 10);
    for i in 0..10 {
        small_chunk.column_mut(0).unwrap().push_i64(i).unwrap();
        small_chunk
            .column_mut(1)
            .unwrap()
            .push_i64(i * 100)
            .unwrap();
    }
    small_chunk.set_num_rows(10).unwrap();
    let small_rg = RowGroup::from_chunk(&small_chunk).unwrap();

    let build_scan = PhysicalColumnScan::new(vec![small_rg], vec![0, 1], Vec::new());
    let probe_scan = PhysicalColumnScan::new(Arc::clone(&row_groups), vec![1, 0], Vec::new());
    let mut join = PhysicalHashJoin::new(
        Box::new(build_scan),
        Box::new(probe_scan),
        0,
        0,
        vec![PhysicalType::Int64, PhysicalType::Int64],
        vec![PhysicalType::Int64, PhysicalType::Int64],
    );
    join.open().unwrap();

    let start = Instant::now();
    let joined_count = join
        .probe_row_groups_parallel(&row_groups, num_threads)
        .unwrap();
    let tej_db_join_time = start.elapsed();
    join.close().unwrap();
    assert!(joined_count > 0);
    let tej_db_join_throughput = (total_rows as f64) / tej_db_join_time.as_secs_f64();

    // DuckDB Hash Join comparison
    let duck_setup = format!(
        "CREATE TABLE dim AS SELECT range AS id, range * 100 AS val FROM range(10);\n\
         CREATE TABLE fact AS SELECT range AS id, range % 10 AS grp FROM range({total_rows});"
    );
    let duck_query = "SELECT count(*) FROM fact JOIN dim ON fact.grp = dim.id;";
    let duck_join_time = run_duckdb_timed_query(&duck_setup, duck_query);
    let duck_join_throughput = (total_rows as f64) / duck_join_time.as_secs_f64();

    println!("  - Hash Join (1M fact rows x 10 dim rows):");
    println!(
        "    * TejDb:   {:10.2} rows/s ({:.2?})",
        tej_db_join_throughput, tej_db_join_time
    );
    println!(
        "    * DuckDB: {:10.2} rows/s ({:.2?}) [Ratio: {:.2}x]",
        duck_join_throughput,
        duck_join_time,
        tej_db_join_throughput / duck_join_throughput
    );
}

fn bench_unified_htap_hybrid() {
    println!("\n--- 4. Unified HTAP + Vector Hybrid Benchmark ---");

    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute(
        "CREATE TABLE hybrid_items (id INT PRIMARY KEY, category TEXT, price DECIMAL(10, 2))",
    )
    .unwrap();

    let mut tx = db.begin_write().unwrap();
    for i in 0..1000 {
        let cat = if i % 2 == 0 { "Electronics" } else { "Books" };
        let cents = 1000 + (i % 500);
        let dollars = cents / 100;
        let rem = cents % 100;
        sql.execute_in_tx(
            &mut tx,
            &format!("INSERT INTO hybrid_items VALUES ({i}, '{cat}', '{dollars}.{rem:02}')"),
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let start = Instant::now();
    let res = sql
        .execute("SELECT id, category, price FROM hybrid_items WHERE id < 100")
        .unwrap();
    let elapsed = start.elapsed();
    let qps = 1.0 / elapsed.as_secs_f64();

    println!(
        "  - Hybrid Relational + Advanced Type Query: {} rows retrieved in {:.2?} ({:.2} QPS)",
        res.rows.len(),
        elapsed,
        qps
    );
}
