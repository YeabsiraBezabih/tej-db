#![allow(
    clippy::unwrap_used,
    clippy::needless_range_loop,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_possible_wrap
)]

//! Comprehensive TejDb Extended Benchmark Suite
//!
//! Benchmarks:
//! 1. SQL Complex Clauses (ORDER BY + OFFSET + LIMIT, DISTINCT, NOT NULL, i64::MIN PK)
//! 2. SQL Point Update PK (TejDb-SQL vs SQLite WAL)
//! 3. Database::verify() (Live fsck integrity scan throughput)
//! 4. Vector Multi-Metric (L2, Cosine, DotProduct) + SpatialViewportManager SIMD Culling & Raycast
//! 5. Apples-to-Apples Immediate Durability Parity (TejDb Immediate vs Redb Immediate vs SQLite WAL/FULL)

use std::sync::Arc;
use std::time::Instant;
use tej_db::spatial_viewport::{ClusterMetadata, FrustumPlanes, Ray3D, RaycastScope};
use tej_db::{Database, DistanceMetric, DurabilityMode, SpatialViewportManager};
use tej_db_core::PageId;
use tej_db_core::spatial::{Aabb3D, BoundingSphere};
use tej_db_sql::{SqlEngine, Value};
use tempfile::tempdir;

fn main() {
    println!("================================================================================");
    println!("       TejDb Extended Comprehensive Benchmark Suite (SQL + Spatial + fsck)      ");
    println!("================================================================================\n");

    bench_sql_complex_and_update();
    bench_database_verify();
    bench_vector_multimetric_and_spatial();
    bench_immediate_durability_parity();

    println!("\n================================================================================");
    println!("                    Extended Comprehensive Benchmark Complete                   ");
    println!("================================================================================");
}

fn bench_sql_complex_and_update() {
    println!("--- 1. SQL Complex Clauses & Point Update PK (TejDb-SQL vs SQLite WAL) ---");
    let dir = tempdir().unwrap();
    let tej_path = dir.path().join("tej_sql.db");
    let sq_path = dir.path().join("sqlite.db");

    let n_rows = 5_000usize;

    // Setup TejDb-SQL
    let db = Database::open_file_with_durability(&tej_path, 4096, DurabilityMode::Normal).unwrap();
    let sql = SqlEngine::new(&db);
    sql.execute(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, category TEXT NOT NULL, score INTEGER NOT NULL, tag TEXT)",
    )
    .unwrap();

    let insert_stmt = sql
        .prepare_statement("INSERT INTO items VALUES (?, ?, ?, ?)")
        .unwrap();
    let start_ins = Instant::now();
    {
        let mut tx = db.begin_write().unwrap();
        for i in 0..n_rows {
            let id = if i == 0 { i64::MIN } else { i as i64 };
            let cat = format!("cat_{}", i % 25);
            let tag = format!("tag_{}", i % 10);
            insert_stmt
                .execute_in_tx(
                    &mut tx,
                    &[
                        Value::Integer(id),
                        Value::Text(cat),
                        Value::Integer((n_rows - i) as i64),
                        Value::Text(tag),
                    ],
                )
                .unwrap();
        }
        tx.commit().unwrap();
    }
    let ins_dur = start_ins.elapsed();

    // Setup SQLite
    let conn = rusqlite::Connection::open(&sq_path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
         CREATE TABLE items (id INTEGER PRIMARY KEY, category TEXT NOT NULL, score INTEGER NOT NULL, tag TEXT);",
    )
    .unwrap();
    let sq_ins_start = Instant::now();
    {
        let tx = conn.unchecked_transaction().unwrap();
        let mut stmt = tx
            .prepare("INSERT INTO items VALUES (?1, ?2, ?3, ?4)")
            .unwrap();
        for i in 0..n_rows {
            let id = if i == 0 { i64::MIN } else { i as i64 };
            let cat = format!("cat_{}", i % 25);
            let tag = format!("tag_{}", i % 10);
            stmt.execute(rusqlite::params![id, cat, (n_rows - i) as i64, tag])
                .unwrap();
        }
        drop(stmt);
        tx.commit().unwrap();
    }
    let sq_ins_dur = sq_ins_start.elapsed();

    println!(
        "  Bulk Insert ({n_rows} rows, NOT NULL):   TejDb {:>9.0} ops/s ({:.2}ms) | SQLite {:>9.0} ops/s ({:.2}ms) [{:.2}x]",
        n_rows as f64 / ins_dur.as_secs_f64(),
        ins_dur.as_secs_f64() * 1000.0,
        n_rows as f64 / sq_ins_dur.as_secs_f64(),
        sq_ins_dur.as_secs_f64() * 1000.0,
        sq_ins_dur.as_secs_f64() / ins_dur.as_secs_f64(),
    );

    // Point Update PK (2,000 updates)
    let n_upd = 2_000usize;
    let upd_stmt = sql
        .prepare_statement("UPDATE items SET score = ? WHERE id = ?")
        .unwrap();
    let start_upd = Instant::now();
    {
        let mut tx = db.begin_write().unwrap();
        for i in 1..=n_upd {
            let id = ((i * 7919) % (n_rows - 1) + 1) as i64;
            upd_stmt
                .execute_in_tx(
                    &mut tx,
                    &[Value::Integer((i * 13) as i64), Value::Integer(id)],
                )
                .unwrap();
        }
        tx.commit().unwrap();
    }
    let upd_dur = start_upd.elapsed();

    let sq_upd_start = Instant::now();
    {
        let tx = conn.unchecked_transaction().unwrap();
        let mut stmt = tx
            .prepare("UPDATE items SET score = ?1 WHERE id = ?2")
            .unwrap();
        for i in 1..=n_upd {
            let id = ((i * 7919) % (n_rows - 1) + 1) as i64;
            stmt.execute(rusqlite::params![(i * 13) as i64, id])
                .unwrap();
        }
        drop(stmt);
        tx.commit().unwrap();
    }
    let sq_upd_dur = sq_upd_start.elapsed();

    println!(
        "  Point Update PK ({n_upd} ops):          TejDb {:>9.0} ops/s ({:.2}ms) | SQLite {:>9.0} ops/s ({:.2}ms) [{:.2}x]",
        n_upd as f64 / upd_dur.as_secs_f64(),
        upd_dur.as_secs_f64() * 1000.0,
        n_upd as f64 / sq_upd_dur.as_secs_f64(),
        sq_upd_dur.as_secs_f64() * 1000.0,
        sq_upd_dur.as_secs_f64() / upd_dur.as_secs_f64(),
    );

    // Complex SQL: ORDER BY + OFFSET + LIMIT + DISTINCT + i64::MIN PK
    let min_res = sql
        .execute("SELECT id, category FROM items WHERE id = -9223372036854775808")
        .unwrap();
    assert_eq!(min_res.rows.len(), 1);

    let start_order = Instant::now();
    for _ in 0..50 {
        let res = sql
            .execute("SELECT id, score FROM items ORDER BY score DESC LIMIT 20 OFFSET 100")
            .unwrap();
        assert_eq!(res.rows.len(), 20);
    }
    let order_dur = start_order.elapsed();

    let start_distinct = Instant::now();
    for _ in 0..50 {
        let res = sql
            .execute("SELECT DISTINCT category FROM items ORDER BY category ASC")
            .unwrap();
        assert_eq!(res.rows.len(), 25);
    }
    let distinct_dur = start_distinct.elapsed();

    println!(
        "  ORDER BY + OFFSET + LIMIT (50 runs):  {:.2} ms total ({:.2} ms/query)",
        order_dur.as_secs_f64() * 1000.0,
        order_dur.as_secs_f64() * 1000.0 / 50.0,
    );
    println!(
        "  DISTINCT + ORDER BY (50 runs):        {:.2} ms total ({:.2} ms/query)\n",
        distinct_dur.as_secs_f64() * 1000.0,
        distinct_dur.as_secs_f64() * 1000.0 / 50.0,
    );
}

fn bench_database_verify() {
    println!("--- 2. Live Storage Integrity Verification (Database::verify / fsck) ---");
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("verify.db");
    let db = Database::open_file_with_durability(&db_path, 4096, DurabilityMode::Normal).unwrap();

    let mut tx = db.begin_write().unwrap();
    for t in 0..4 {
        let tbl_name = format!("tbl_{t}");
        let mut tbl = tx.open_table_mut(&tbl_name).unwrap();
        for k in 0..2_500u32 {
            tbl.put(&k.to_be_bytes(), &[0xAB; 128]).unwrap();
        }
    }
    tx.commit().unwrap();

    let start = Instant::now();
    let (pages_checked, kv_checked) = db.verify().unwrap();
    let dur = start.elapsed();

    println!(
        "  Verified {} pages and {} KV entries across 4 tables in {:.2} ms ({:.0} pages/s)\n",
        pages_checked,
        kv_checked,
        dur.as_secs_f64() * 1000.0,
        pages_checked as f64 / dur.as_secs_f64(),
    );
}

fn bench_vector_multimetric_and_spatial() {
    println!("--- 3. Vector Multi-Metric (L2/Cosine/Dot) & SpatialViewportManager ---");
    let db = Database::open_memory_with_capacity(16384).unwrap();
    let dim = 128;
    let n_vecs = 1_000usize;

    for (name, metric) in [
        ("vec_l2", DistanceMetric::L2),
        ("vec_cos", DistanceMetric::Cosine),
        ("vec_dot", DistanceMetric::DotProduct),
    ] {
        let idx = db.create_vector_index(name, dim, metric);
        let mut tx = db.begin_write().unwrap();
        for i in 0..n_vecs {
            let v: Vec<f32> = (0..dim)
                .map(|d| ((i * 31 + d) % 100) as f32 / 100.0)
                .collect();
            idx.insert(&mut tx, tej_db::VectorId(i as u64 + 1), &v)
                .unwrap();
        }
        tx.commit().unwrap();
        idx.compact().unwrap();

        let rx = db.begin_read().unwrap();
        let query: Vec<f32> = (0..dim).map(|d| (d % 10) as f32 / 10.0).collect();
        let start = Instant::now();
        for _ in 0..200 {
            let res = idx.search(&rx, &query, 32, 10, None).unwrap();
            assert_eq!(res.len(), 10);
        }
        let dur = start.elapsed();
        println!(
            "  Vector {:?} (1k x 128d, L=32): {:>7.0} QPS ({:.1} µs/query)",
            metric,
            200.0 / dur.as_secs_f64(),
            dur.as_secs_f64() * 1e6 / 200.0,
        );
    }

    // SpatialViewportManager SIMD frustum culling + raycast benchmark
    let sp_db = Database::open_memory_with_capacity(8192).unwrap();
    let mut mgr = SpatialViewportManager::new(
        Arc::clone(sp_db.tx_manager()),
        "scene",
        Aabb3D {
            min: [-1000.0, -1000.0, -1000.0],
            max: [1000.0, 1000.0, 1000.0],
        },
    )
    .unwrap();
    for i in 0..2_000u32 {
        let x = ((i * 73) % 1800) as f32 - 900.0;
        let y = ((i * 137) % 1800) as f32 - 900.0;
        let z = ((i * 251) % 1800) as f32 - 900.0;
        mgr.register_cluster(ClusterMetadata {
            cluster_id: i + 1,
            lod_level: 0,
            lod_error: 0.5,
            bounds: Aabb3D {
                min: [x - 2.0, y - 2.0, z - 2.0],
                max: [x + 2.0, y + 2.0, z + 2.0],
            },
            sphere: BoundingSphere {
                center: [x, y, z],
                radius: 3.5,
            },
            page_id: PageId::INVALID,
        })
        .unwrap();
    }

    // 6-plane box frustum [-300, 300]^3
    let frustum = FrustumPlanes {
        planes: [
            [1.0, 0.0, 0.0, 300.0],
            [-1.0, 0.0, 0.0, 300.0],
            [0.0, 1.0, 0.0, 300.0],
            [0.0, -1.0, 0.0, 300.0],
            [0.0, 0.0, 1.0, 300.0],
            [0.0, 0.0, -1.0, 300.0],
        ],
    };

    let start_cull = Instant::now();
    let mut visible_total = 0usize;
    for _ in 0..500 {
        visible_total += mgr.cull_frustum_simd(&frustum, [0.0, 0.0, 0.0], 0.01);
    }
    let cull_dur = start_cull.elapsed();

    let ray = Ray3D::new([0.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
    let start_ray = Instant::now();
    for _ in 0..500 {
        let _ = mgr.pick_ray(&ray, RaycastScope::AllClusters);
    }
    let ray_dur = start_ray.elapsed();

    println!(
        "  Spatial SIMD Frustum Cull (2k clusters): {:>7.0} QPS ({:.1} µs/frame, avg {} visible)",
        500.0 / cull_dur.as_secs_f64(),
        cull_dur.as_secs_f64() * 1e6 / 500.0,
        visible_total / 500,
    );
    println!(
        "  Spatial Raycast Pick (2k clusters):      {:>7.0} QPS ({:.1} µs/pick)\n",
        500.0 / ray_dur.as_secs_f64(),
        ray_dur.as_secs_f64() * 1e6 / 500.0,
    );
}

fn bench_immediate_durability_parity() {
    println!("--- 4. Apples-to-Apples Immediate Durability Parity (20 tx x 100 keys) ---");
    let dir = tempdir().unwrap();
    let n_tx = 20usize;
    let batch = 100usize;

    // TejDb Immediate
    let tej_p = dir.path().join("tej_imm.db");
    let db = Database::open_file_with_durability(&tej_p, 4096, DurabilityMode::Immediate).unwrap();
    let start_t = Instant::now();
    for t in 0..n_tx {
        let mut tx = db.begin_write().unwrap();
        for i in 0..batch {
            let k = ((t * batch + i) as u64).to_be_bytes();
            tx.put(&k, &[0x55; 64]).unwrap();
        }
        tx.commit().unwrap();
    }
    let dur_t = start_t.elapsed();

    // Redb Immediate
    let redb_p = dir.path().join("redb_imm.db");
    let rdb = redb::Database::create(&redb_p).unwrap();
    let tbl_def: redb::TableDefinition<'_, &[u8], &[u8]> = redb::TableDefinition::new("data");
    let start_r = Instant::now();
    for t in 0..n_tx {
        let mut tx = rdb.begin_write().unwrap();
        tx.set_durability(redb::Durability::Immediate);
        {
            let mut tbl = tx.open_table(tbl_def).unwrap();
            for i in 0..batch {
                let k = ((t * batch + i) as u64).to_be_bytes();
                tbl.insert(&k[..], &[0x55; 64][..]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let dur_r = start_r.elapsed();

    // SQLite WAL/FULL
    let sq_p = dir.path().join("sq_imm.db");
    let conn = rusqlite::Connection::open(&sq_p).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
         CREATE TABLE data (k BLOB PRIMARY KEY, v BLOB);",
    )
    .unwrap();
    let start_s = Instant::now();
    for t in 0..n_tx {
        let tx = conn.unchecked_transaction().unwrap();
        {
            let mut stmt = tx
                .prepare("INSERT OR REPLACE INTO data VALUES (?1, ?2)")
                .unwrap();
            for i in 0..batch {
                let k = ((t * batch + i) as u64).to_be_bytes();
                stmt.execute(rusqlite::params![&k[..], &[0x55u8; 64][..]])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let dur_s = start_s.elapsed();

    let total = (n_tx * batch) as f64;
    println!(
        "  TejDb (Immediate): {:>8.0} ops/s ({:.2} ms/tx) | Redb: {:>8.0} ops/s ({:.2} ms/tx) | SQLite FULL: {:>8.0} ops/s ({:.2} ms/tx)",
        total / dur_t.as_secs_f64(),
        dur_t.as_secs_f64() * 1000.0 / n_tx as f64,
        total / dur_r.as_secs_f64(),
        dur_r.as_secs_f64() * 1000.0 / n_tx as f64,
        total / dur_s.as_secs_f64(),
        dur_s.as_secs_f64() * 1000.0 / n_tx as f64,
    );
}
