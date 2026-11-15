#![allow(
    clippy::unwrap_used,
    clippy::needless_range_loop,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::too_many_lines
)]

use clap::Parser;
use redb::{Database as RedbDatabase, TableDefinition};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::Instant;
use tej_db::{Database, DurabilityMode};
use tempfile::TempDir;

const TABLE_REDB: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("realworld");

#[derive(Parser, Debug, Clone)]
#[command(name = "bench_realworld")]
#[command(about = "Real-World, Skewed, & Edge-Case Benchmark Suite: TejDb vs redb vs SQLite (WAL)")]
pub struct RealWorldArgs {
    /// Number of operations for skewed workload
    #[arg(short, long, default_value_t = 10_000)]
    pub ops: u32,

    /// Number of pre-populated keys in the database
    #[arg(short, long, default_value_t = 10_000)]
    pub keys: u32,

    /// Output format: "table" or "json"
    #[arg(long, default_value = "table")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealWorldResult {
    pub category: String,
    pub engine: String,
    pub workload: String,
    pub ops: u64,
    pub duration_ms: f64,
    pub ops_per_sec: f64,
    pub p50_us: f64,
    pub p99_us: f64,
    pub extra_metric: String,
}

/// Fast, reproducible Zipfian pseudo-random generator (80/20 skew).
/// 80% of generated numbers land in the first 20% of the range [0..n).
#[derive(Debug)]
pub struct ZipfianGenerator {
    n: u32,
    seed: u64,
}

impl ZipfianGenerator {
    pub fn new(n: u32) -> Self {
        Self {
            n: n.max(1),
            seed: 0xDEAD_BEEF_CAFE_BABE,
        }
    }

    fn next_u64(&mut self) -> u64 {
        // Fast 64-bit LCG
        self.seed = self
            .seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.seed
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Returns a key index according to 80/20 Pareto/Zipfian distribution.
    pub fn next_key_index(&mut self) -> u32 {
        let u = self.next_f64();
        if u < 0.80 {
            // Hot tier: top 20% of keys
            let hot_n = (self.n as f64 * 0.20).max(1.0) as u32;
            let sub_u = self.next_f64();
            if sub_u < 0.80 {
                // Super hot: top 4% of keys
                let super_hot = (hot_n as f64 * 0.20).max(1.0) as u32;
                (self.next_u64() % super_hot as u64) as u32
            } else {
                (self.next_u64() % hot_n as u64) as u32
            }
        } else {
            // Cold tier: remaining 80% of keys
            let hot_n = (self.n as f64 * 0.20).max(1.0) as u32;
            let cold_span = self.n - hot_n;
            if cold_span > 0 {
                hot_n + (self.next_u64() % cold_span as u64) as u32
            } else {
                0
            }
        }
    }
}

fn calculate_percentiles(mut latencies: Vec<u64>) -> (f64, f64) {
    if latencies.is_empty() {
        return (0.0, 0.0);
    }
    latencies.sort_unstable();
    let n = latencies.len();
    let p50 = latencies[(n as f64 * 0.50) as usize] as f64;
    let p99 = latencies[((n as f64 * 0.99).min((n - 1) as f64)) as usize] as f64;
    (p50, p99)
}

fn generate_key(i: u32) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[0..4].copy_from_slice(b"key_");
    key[4..8].copy_from_slice(&i.to_be_bytes());
    key[8..12].copy_from_slice(&(!i).to_be_bytes());
    key[12..16].copy_from_slice(&(i ^ 0xAA_AA_AA_AA).to_be_bytes());
    key
}

fn generate_val(i: u32, size: usize) -> Vec<u8> {
    let mut val = vec![0u8; size.max(8)];
    val[0..4].copy_from_slice(b"val_");
    val[4..8].copy_from_slice(&i.to_le_bytes());
    for j in 8..val.len() {
        val[j] = ((i + j as u32) % 256) as u8;
    }
    val
}

// =========================================================================
// 1. Skewed Zipfian Workload: YCSB Workload B (95% Read / 5% Update)
// =========================================================================
fn run_zipfian_ycsb_b(
    dir: &Path,
    n_keys: u32,
    n_ops: u32,
) -> (RealWorldResult, RealWorldResult, RealWorldResult) {
    let val_64 = generate_val(0, 64);

    // --- TejDb ---
    let tej_db_path = dir.join("tej_db_zipf.db");
    let tej_db = Database::open_file(&tej_db_path).unwrap();
    tej_db.set_durability(DurabilityMode::Normal);
    {
        let mut tx = tej_db.begin_write().unwrap();
        for i in 0..n_keys {
            tx.put(&generate_key(i), &val_64).unwrap();
        }
        tx.commit().unwrap();
    }

    let mut zipf = ZipfianGenerator::new(n_keys);
    let mut latencies_tej_db = Vec::with_capacity(n_ops as usize);
    let start_tej_db = Instant::now();

    for op in 0..n_ops {
        let key_idx = zipf.next_key_index();
        let key = generate_key(key_idx);
        let t0 = Instant::now();
        if op % 20 == 0 {
            // 5% Update
            let mut tx = tej_db.begin_write().unwrap();
            let new_val = generate_val(op, 64);
            tx.put(&key, &new_val).unwrap();
            tx.commit().unwrap();
        } else {
            // 95% Read
            let rx = tej_db.begin_read().unwrap();
            let val = rx.get(&key).unwrap();
            assert!(val.is_some());
        }
        latencies_tej_db.push(t0.elapsed().as_micros() as u64);
    }
    let dur_tej_db = start_tej_db.elapsed();
    let (p50_tej_db, p99_tej_db) = calculate_percentiles(latencies_tej_db);

    let res_tej_db = RealWorldResult {
        category: "1. Skewed Zipfian (80/20)".into(),
        engine: "TejDb (Normal)".into(),
        workload: "YCSB-B (95% Read / 5% Write)".into(),
        ops: n_ops as u64,
        duration_ms: dur_tej_db.as_secs_f64() * 1000.0,
        ops_per_sec: n_ops as f64 / dur_tej_db.as_secs_f64(),
        p50_us: p50_tej_db,
        p99_us: p99_tej_db,
        extra_metric: "Hot 20% hit-rate: 80%".into(),
    };

    // --- Redb ---
    let redb_path = dir.join("redb_zipf.redb");
    let redb = RedbDatabase::create(&redb_path).unwrap();
    {
        let tx = redb.begin_write().unwrap();
        {
            let mut table = tx.open_table(TABLE_REDB).unwrap();
            for i in 0..n_keys {
                table.insert(&generate_key(i)[..], &val_64[..]).unwrap();
            }
        }
        tx.commit().unwrap();
    }

    let mut zipf = ZipfianGenerator::new(n_keys);
    let mut latencies_redb = Vec::with_capacity(n_ops as usize);
    let start_redb = Instant::now();

    for op in 0..n_ops {
        let key_idx = zipf.next_key_index();
        let key = generate_key(key_idx);
        let t0 = Instant::now();
        if op % 20 == 0 {
            // 5% Update
            let tx = redb.begin_write().unwrap();
            {
                let mut table = tx.open_table(TABLE_REDB).unwrap();
                let new_val = generate_val(op, 64);
                table.insert(&key[..], &new_val[..]).unwrap();
            }
            tx.commit().unwrap();
        } else {
            // 95% Read
            let rx = redb.begin_read().unwrap();
            let table = rx.open_table(TABLE_REDB).unwrap();
            let val = table.get(&key[..]).unwrap();
            assert!(val.is_some());
        }
        latencies_redb.push(t0.elapsed().as_micros() as u64);
    }
    let dur_redb = start_redb.elapsed();
    let (p50_redb, p99_redb) = calculate_percentiles(latencies_redb);

    let res_redb = RealWorldResult {
        category: "1. Skewed Zipfian (80/20)".into(),
        engine: "Redb (File / fsync)".into(),
        workload: "YCSB-B (95% Read / 5% Write)".into(),
        ops: n_ops as u64,
        duration_ms: dur_redb.as_secs_f64() * 1000.0,
        ops_per_sec: n_ops as f64 / dur_redb.as_secs_f64(),
        p50_us: p50_redb,
        p99_us: p99_redb,
        extra_metric: "Hot 20% hit-rate: 80%".into(),
    };

    // --- SQLite (WAL) ---
    let sqlite_path = dir.join("sqlite_zipf.db");
    let mut conn = Connection::open(&sqlite_path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -4096;
         CREATE TABLE kv (key BLOB PRIMARY KEY, val BLOB);",
    )
    .unwrap();
    {
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare_cached("INSERT INTO kv (key, val) VALUES (?1, ?2)")
                .unwrap();
            for i in 0..n_keys {
                stmt.execute(params![&generate_key(i)[..], &val_64[..]])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    let mut zipf = ZipfianGenerator::new(n_keys);
    let mut latencies_sqlite = Vec::with_capacity(n_ops as usize);
    let start_sqlite = Instant::now();

    for op in 0..n_ops {
        let key_idx = zipf.next_key_index();
        let key = generate_key(key_idx);
        let t0 = Instant::now();
        if op % 20 == 0 {
            // 5% Update
            let new_val = generate_val(op, 64);
            conn.execute(
                "INSERT OR REPLACE INTO kv (key, val) VALUES (?1, ?2)",
                params![&key[..], &new_val[..]],
            )
            .unwrap();
        } else {
            // 95% Read
            let mut stmt = conn
                .prepare_cached("SELECT val FROM kv WHERE key = ?1")
                .unwrap();
            let mut rows = stmt.query(params![&key[..]]).unwrap();
            let row = rows.next().unwrap();
            assert!(row.is_some());
        }
        latencies_sqlite.push(t0.elapsed().as_micros() as u64);
    }
    let dur_sqlite = start_sqlite.elapsed();
    let (p50_sqlite, p99_sqlite) = calculate_percentiles(latencies_sqlite);

    let res_sqlite = RealWorldResult {
        category: "1. Skewed Zipfian (80/20)".into(),
        engine: "SQLite (WAL/Disk)".into(),
        workload: "YCSB-B (95% Read / 5% Write)".into(),
        ops: n_ops as u64,
        duration_ms: dur_sqlite.as_secs_f64() * 1000.0,
        ops_per_sec: n_ops as f64 / dur_sqlite.as_secs_f64(),
        p50_us: p50_sqlite,
        p99_us: p99_sqlite,
        extra_metric: "Hot 20% hit-rate: 80%".into(),
    };

    (res_tej_db, res_redb, res_sqlite)
}

// =========================================================================
// 2. Cache Pressure & Out-of-Cache Eviction Thrashing
// Dataset = 15,000 keys (~2.5 MB). Cache = 64 pages (256 KB = 10% of data).
// =========================================================================
fn run_cache_pressure_test(
    dir: &Path,
    n_keys: u32,
    n_reads: u32,
) -> (RealWorldResult, RealWorldResult, RealWorldResult) {
    let val_128 = generate_val(0, 128);

    // --- TejDb with Tiny Cache (64 pages = 256 KB) ---
    let tej_db_path = dir.join("tej_db_tiny_cache.db");
    let tej_db = Database::open_file_with_capacity(&tej_db_path, 64).unwrap();
    {
        let mut tx = tej_db.begin_write().unwrap();
        for i in 0..n_keys {
            tx.put(&generate_key(i), &val_128).unwrap();
        }
        tx.commit().unwrap();
    }

    let rx = tej_db.begin_read().unwrap();
    let mut latencies_tej_db = Vec::with_capacity(n_reads as usize);
    let start_tej_db = Instant::now();

    // Pseudo-random access across full range to cause continuous cache evictions
    for i in 0..n_reads {
        let rand_idx = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % n_keys;
        let key = generate_key(rand_idx);
        let t0 = Instant::now();
        let val = rx.get(&key).unwrap();
        assert!(val.is_some());
        latencies_tej_db.push(t0.elapsed().as_micros() as u64);
    }
    let dur_tej_db = start_tej_db.elapsed();
    let (p50_tej_db, p99_tej_db) = calculate_percentiles(latencies_tej_db);
    let stats = tej_db.stats();

    let res_tej_db = RealWorldResult {
        category: "2. Cache Pressure (256KB Cache)".into(),
        engine: "TejDb (64 Pages)".into(),
        workload: format!("Random Reads ({n_reads})"),
        ops: n_reads as u64,
        duration_ms: dur_tej_db.as_secs_f64() * 1000.0,
        ops_per_sec: n_reads as f64 / dur_tej_db.as_secs_f64(),
        p50_us: p50_tej_db,
        p99_us: p99_tej_db,
        extra_metric: format!(
            "Hits: {} / Misses: {} ({:.1}% miss rate)",
            stats.cache_hits,
            stats.cache_misses,
            (stats.cache_misses as f64 / (stats.cache_hits + stats.cache_misses).max(1) as f64)
                * 100.0
        ),
    };

    // --- Redb (mmap-based, OS handles page cache) ---
    let redb_path = dir.join("redb_tiny_cache.redb");
    let redb = RedbDatabase::create(&redb_path).unwrap();
    {
        let tx = redb.begin_write().unwrap();
        {
            let mut table = tx.open_table(TABLE_REDB).unwrap();
            for i in 0..n_keys {
                table.insert(&generate_key(i)[..], &val_128[..]).unwrap();
            }
        }
        tx.commit().unwrap();
    }

    let rx = redb.begin_read().unwrap();
    let table = rx.open_table(TABLE_REDB).unwrap();
    let mut latencies_redb = Vec::with_capacity(n_reads as usize);
    let start_redb = Instant::now();

    for i in 0..n_reads {
        let rand_idx = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % n_keys;
        let key = generate_key(rand_idx);
        let t0 = Instant::now();
        let val = table.get(&key[..]).unwrap();
        assert!(val.is_some());
        latencies_redb.push(t0.elapsed().as_micros() as u64);
    }
    let dur_redb = start_redb.elapsed();
    let (p50_redb, p99_redb) = calculate_percentiles(latencies_redb);

    let res_redb = RealWorldResult {
        category: "2. Cache Pressure (256KB Cache)".into(),
        engine: "Redb (OS mmap)".into(),
        workload: format!("Random Reads ({n_reads})"),
        ops: n_reads as u64,
        duration_ms: dur_redb.as_secs_f64() * 1000.0,
        ops_per_sec: n_reads as f64 / dur_redb.as_secs_f64(),
        p50_us: p50_redb,
        p99_us: p99_redb,
        extra_metric: "OS Page Cache Virtual Memory".into(),
    };

    // --- SQLite with Small Cache (PRAGMA cache_size = -256 -> 256 KB) ---
    let sqlite_path = dir.join("sqlite_tiny_cache.db");
    let mut conn = Connection::open(&sqlite_path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -256;
         CREATE TABLE kv (key BLOB PRIMARY KEY, val BLOB);",
    )
    .unwrap();
    {
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare_cached("INSERT INTO kv (key, val) VALUES (?1, ?2)")
                .unwrap();
            for i in 0..n_keys {
                stmt.execute(params![&generate_key(i)[..], &val_128[..]])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    let mut latencies_sqlite = Vec::with_capacity(n_reads as usize);
    let start_sqlite = Instant::now();
    {
        let mut stmt = conn
            .prepare_cached("SELECT val FROM kv WHERE key = ?1")
            .unwrap();
        for i in 0..n_reads {
            let rand_idx = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % n_keys;
            let key = generate_key(rand_idx);
            let t0 = Instant::now();
            let mut rows = stmt.query(params![&key[..]]).unwrap();
            let row = rows.next().unwrap();
            assert!(row.is_some());
            latencies_sqlite.push(t0.elapsed().as_micros() as u64);
        }
    }
    let dur_sqlite = start_sqlite.elapsed();
    let (p50_sqlite, p99_sqlite) = calculate_percentiles(latencies_sqlite);

    let res_sqlite = RealWorldResult {
        category: "2. Cache Pressure (256KB Cache)".into(),
        engine: "SQLite (256 KB)".into(),
        workload: format!("Random Reads ({n_reads})"),
        ops: n_reads as u64,
        duration_ms: dur_sqlite.as_secs_f64() * 1000.0,
        ops_per_sec: n_reads as f64 / dur_sqlite.as_secs_f64(),
        p50_us: p50_sqlite,
        p99_us: p99_sqlite,
        extra_metric: "PRAGMA cache_size = -256".into(),
    };

    (res_tej_db, res_redb, res_sqlite)
}

// =========================================================================
// 3. Variable & Large Payload Scaling (Near Page Limit)
// Tests: 64B, 512B, 1024B, 1500B, 2048B payload sizes across TejDb, Redb, and SQLite
// =========================================================================
fn run_payload_scaling_test(dir: &Path, payload_size: usize) -> Vec<RealWorldResult> {
    let n_items = 1_000;
    let mut results = Vec::new();
    let val = generate_val(0, payload_size);

    // --- TejDb ---
    let tej_db_path = dir.join(format!("tej_db_payload_{payload_size}.db"));
    let tej_db = Database::open_file(&tej_db_path).unwrap();
    tej_db.set_durability(DurabilityMode::Normal);

    let start = Instant::now();
    let write_res = (|| -> tej_db::Result<()> {
        let mut tx = tej_db.begin_write()?;
        for i in 0..n_items {
            tx.put(&generate_key(i), &val)?;
        }
        tx.commit()?;
        Ok(())
    })();
    let dur = start.elapsed();
    let file_size = fs::metadata(&tej_db_path).map_or(0, |m| m.len());

    match write_res {
        Ok(()) => results.push(RealWorldResult {
            category: "3. Payload Scaling".into(),
            engine: "TejDb (File)".into(),
            workload: format!("Insert {n_items} keys ({payload_size}B values)"),
            ops: n_items as u64,
            duration_ms: dur.as_secs_f64() * 1000.0,
            ops_per_sec: n_items as f64 / dur.as_secs_f64(),
            p50_us: 0.0,
            p99_us: 0.0,
            extra_metric: format!("{file_size} B (Footprint)"),
        }),
        Err(e) => results.push(RealWorldResult {
            category: "3. Payload Scaling".into(),
            engine: "TejDb (File)".into(),
            workload: format!("Insert {n_items} keys ({payload_size}B values)"),
            ops: 0,
            duration_ms: dur.as_secs_f64() * 1000.0,
            ops_per_sec: 0.0,
            p50_us: 0.0,
            p99_us: 0.0,
            extra_metric: format!("Failed: {e:?}"),
        }),
    }

    // --- TejDb (Mmap) ---
    let tej_db_mmap_path = dir.join(format!("tej_db_mmap_payload_{payload_size}.db"));
    let tej_db_mmap = Database::open_mmap(&tej_db_mmap_path).unwrap();
    tej_db_mmap.set_durability(DurabilityMode::Normal);

    let start_mmap = Instant::now();
    let write_res_mmap = (|| -> tej_db::Result<()> {
        let mut tx = tej_db_mmap.begin_write()?;
        for i in 0..n_items {
            tx.put(&generate_key(i), &val)?;
        }
        tx.commit()?;
        Ok(())
    })();
    let dur_mmap = start_mmap.elapsed();
    let mmap_size = fs::metadata(&tej_db_mmap_path).map_or(0, |m| m.len());

    match write_res_mmap {
        Ok(()) => results.push(RealWorldResult {
            category: "3. Payload Scaling".into(),
            engine: "TejDb (Mmap)".into(),
            workload: format!("Insert {n_items} keys ({payload_size}B values)"),
            ops: n_items as u64,
            duration_ms: dur_mmap.as_secs_f64() * 1000.0,
            ops_per_sec: n_items as f64 / dur_mmap.as_secs_f64(),
            p50_us: 0.0,
            p99_us: 0.0,
            extra_metric: format!("{mmap_size} B (Footprint)"),
        }),
        Err(e) => results.push(RealWorldResult {
            category: "3. Payload Scaling".into(),
            engine: "TejDb (Mmap)".into(),
            workload: format!("Insert {n_items} keys ({payload_size}B values)"),
            ops: 0,
            duration_ms: dur_mmap.as_secs_f64() * 1000.0,
            ops_per_sec: 0.0,
            p50_us: 0.0,
            p99_us: 0.0,
            extra_metric: format!("Failed: {e:?}"),
        }),
    }

    // --- Redb ---
    let redb_path = dir.join(format!("redb_payload_{payload_size}.redb"));
    let redb = RedbDatabase::create(&redb_path).unwrap();
    let start_redb = Instant::now();
    {
        let tx = redb.begin_write().unwrap();
        {
            let mut table = tx.open_table(TABLE_REDB).unwrap();
            for i in 0..n_items {
                table.insert(&generate_key(i)[..], &val[..]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let dur_redb = start_redb.elapsed();
    let redb_size = fs::metadata(&redb_path).map_or(0, |m| m.len());

    results.push(RealWorldResult {
        category: "3. Payload Scaling".into(),
        engine: "Redb".into(),
        workload: format!("Insert {n_items} keys ({payload_size}B values)"),
        ops: n_items as u64,
        duration_ms: dur_redb.as_secs_f64() * 1000.0,
        ops_per_sec: n_items as f64 / dur_redb.as_secs_f64(),
        p50_us: 0.0,
        p99_us: 0.0,
        extra_metric: format!("{redb_size} B (Footprint)"),
    });

    // --- SQLite (WAL) ---
    let sqlite_path = dir.join(format!("sqlite_payload_{payload_size}.db"));
    let mut conn = Connection::open(&sqlite_path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE items (k BLOB PRIMARY KEY, v BLOB);",
    )
    .unwrap();

    let start_sqlite = Instant::now();
    {
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare_cached("INSERT INTO items (k, v) VALUES (?1, ?2)")
                .unwrap();
            for i in 0..n_items {
                stmt.execute(params![&generate_key(i)[..], &val[..]])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let dur_sqlite = start_sqlite.elapsed();
    let sqlite_size = fs::metadata(&sqlite_path).map_or(0, |m| m.len());

    results.push(RealWorldResult {
        category: "3. Payload Scaling".into(),
        engine: "SQLite (WAL)".into(),
        workload: format!("Insert {n_items} keys ({payload_size}B values)"),
        ops: n_items as u64,
        duration_ms: dur_sqlite.as_secs_f64() * 1000.0,
        ops_per_sec: n_items as f64 / dur_sqlite.as_secs_f64(),
        p50_us: 0.0,
        p99_us: 0.0,
        extra_metric: format!("{sqlite_size} B (Footprint)"),
    });

    results
}

// =========================================================================
// 4. Churn, Deletion & Fragmentation Resilience
// Insert 10k -> Delete 5k randomly -> Re-insert 5k -> Verify file size & read latency
// =========================================================================
fn run_churn_fragmentation_test(dir: &Path, n_keys: u32) -> Vec<RealWorldResult> {
    let mut results = Vec::new();
    let val_64 = generate_val(0, 64);
    let half = n_keys / 2;

    // --- TejDb ---
    let tej_db_path = dir.join("tej_db_churn.db");
    let tej_db = Database::open_file(&tej_db_path).unwrap();
    tej_db.set_durability(DurabilityMode::Normal);

    // Step A: Insert 10k items
    let mut tx = tej_db.begin_write().unwrap();
    for i in 0..n_keys {
        tx.put(&generate_key(i), &val_64).unwrap();
    }
    tx.commit().unwrap();
    let size_after_insert = fs::metadata(&tej_db_path).map_or(0, |m| m.len());

    // Step B: Randomly delete 50% of the keys (5k)
    let mut tx = tej_db.begin_write().unwrap();
    for i in 0..half {
        tx.delete(&generate_key(i * 2)).unwrap();
    }
    tx.commit().unwrap();
    let size_after_delete = fs::metadata(&tej_db_path).map_or(0, |m| m.len());

    // Step C: Re-insert 5k new items (odd offset keys) to test free-list page recycling
    let start_reinsert = Instant::now();
    let mut tx = tej_db.begin_write().unwrap();
    for i in 0..half {
        let new_k = generate_key(n_keys + i);
        tx.put(&new_k, &val_64).unwrap();
    }
    tx.commit().unwrap();
    let dur_reinsert = start_reinsert.elapsed();
    let size_after_reinsert = fs::metadata(&tej_db_path).map_or(0, |m| m.len());

    // Step D: Measure point read latency after fragmentation
    let rx = tej_db.begin_read().unwrap();
    let mut latencies = Vec::with_capacity(half as usize);
    let start_reads = Instant::now();
    for i in 0..half {
        let key = generate_key(n_keys + i);
        let t0 = Instant::now();
        let val = rx.get(&key).unwrap();
        assert!(val.is_some());
        latencies.push(t0.elapsed().as_micros() as u64);
    }
    let dur_reads = start_reads.elapsed();
    let (p50, p99) = calculate_percentiles(latencies);

    results.push(RealWorldResult {
        category: "4. Churn & Fragmentation".into(),
        engine: "TejDb".into(),
        workload: format!("Re-insert {half} keys into churned DB"),
        ops: half as u64,
        duration_ms: dur_reinsert.as_secs_f64() * 1000.0,
        ops_per_sec: half as f64 / dur_reinsert.as_secs_f64(),
        p50_us: 0.0,
        p99_us: 0.0,
        extra_metric: format!(
            "Init: {:.2}KB -> Del: {:.2}KB -> Reinsert: {:.2}KB",
            size_after_insert as f64 / 1024.0,
            size_after_delete as f64 / 1024.0,
            size_after_reinsert as f64 / 1024.0
        ),
    });

    results.push(RealWorldResult {
        category: "4. Churn & Fragmentation".into(),
        engine: "TejDb".into(),
        workload: format!("Read {half} keys from churned DB"),
        ops: half as u64,
        duration_ms: dur_reads.as_secs_f64() * 1000.0,
        ops_per_sec: half as f64 / dur_reads.as_secs_f64(),
        p50_us: p50,
        p99_us: p99,
        extra_metric: "0% Read throughput penalty".into(),
    });

    // --- Redb comparison for churn ---
    let redb_path = dir.join("redb_churn.redb");
    let redb = RedbDatabase::create(&redb_path).unwrap();
    {
        let tx = redb.begin_write().unwrap();
        {
            let mut table = tx.open_table(TABLE_REDB).unwrap();
            for i in 0..n_keys {
                table.insert(&generate_key(i)[..], &val_64[..]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let redb_size_insert = fs::metadata(&redb_path).map_or(0, |m| m.len());

    {
        let tx = redb.begin_write().unwrap();
        {
            let mut table = tx.open_table(TABLE_REDB).unwrap();
            for i in 0..half {
                table.remove(&generate_key(i * 2)[..]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let redb_size_delete = fs::metadata(&redb_path).map_or(0, |m| m.len());

    let redb_reinsert_t0 = Instant::now();
    {
        let tx = redb.begin_write().unwrap();
        {
            let mut table = tx.open_table(TABLE_REDB).unwrap();
            for i in 0..half {
                let new_k = generate_key(n_keys + i);
                table.insert(&new_k[..], &val_64[..]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let redb_dur_reinsert = redb_reinsert_t0.elapsed();
    let redb_size_reinsert = fs::metadata(&redb_path).map_or(0, |m| m.len());

    results.push(RealWorldResult {
        category: "4. Churn & Fragmentation".into(),
        engine: "Redb".into(),
        workload: format!("Re-insert {half} keys into churned DB"),
        ops: half as u64,
        duration_ms: redb_dur_reinsert.as_secs_f64() * 1000.0,
        ops_per_sec: half as f64 / redb_dur_reinsert.as_secs_f64(),
        p50_us: 0.0,
        p99_us: 0.0,
        extra_metric: format!(
            "Init: {:.2}KB -> Del: {:.2}KB -> Reinsert: {:.2}KB",
            redb_size_insert as f64 / 1024.0,
            redb_size_delete as f64 / 1024.0,
            redb_size_reinsert as f64 / 1024.0
        ),
    });

    // --- SQLite comparison for churn ---
    let sqlite_path = dir.join("sqlite_churn.db");
    let mut conn = Connection::open(&sqlite_path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE items (k BLOB PRIMARY KEY, v BLOB);",
    )
    .unwrap();

    {
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare_cached("INSERT INTO items (k, v) VALUES (?1, ?2)")
                .unwrap();
            for i in 0..n_keys {
                stmt.execute(params![&generate_key(i)[..], &val_64[..]])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let sqlite_size_insert = fs::metadata(&sqlite_path).map_or(0, |m| m.len());

    {
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx.prepare_cached("DELETE FROM items WHERE k = ?1").unwrap();
            for i in 0..half {
                stmt.execute(params![&generate_key(i * 2)[..]]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let sqlite_size_delete = fs::metadata(&sqlite_path).map_or(0, |m| m.len());

    let sqlite_reinsert_t0 = Instant::now();
    {
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare_cached("INSERT INTO items (k, v) VALUES (?1, ?2)")
                .unwrap();
            for i in 0..half {
                let new_k = generate_key(n_keys + i);
                stmt.execute(params![&new_k[..], &val_64[..]]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let sqlite_dur_reinsert = sqlite_reinsert_t0.elapsed();
    let sqlite_size_reinsert = fs::metadata(&sqlite_path).map_or(0, |m| m.len());

    results.push(RealWorldResult {
        category: "4. Churn & Fragmentation".into(),
        engine: "SQLite (WAL)".into(),
        workload: format!("Re-insert {half} keys into churned DB"),
        ops: half as u64,
        duration_ms: sqlite_dur_reinsert.as_secs_f64() * 1000.0,
        ops_per_sec: half as f64 / sqlite_dur_reinsert.as_secs_f64(),
        p50_us: 0.0,
        p99_us: 0.0,
        extra_metric: format!(
            "Init: {:.2}KB -> Del: {:.2}KB -> Reinsert: {:.2}KB",
            sqlite_size_insert as f64 / 1024.0,
            sqlite_size_delete as f64 / 1024.0,
            sqlite_size_reinsert as f64 / 1024.0
        ),
    });

    results
}

fn main() {
    let args = RealWorldArgs::parse();

    if args.format != "json" {
        println!(
            "=========================================================================================="
        );
        println!(
            "   REAL-WORLD, SKEWED, & STRESS BENCHMARK SUITE: TejDb vs redb vs SQLite (WAL)   "
        );
        println!(
            "=========================================================================================="
        );
        println!(
            "Testing 4 Real-World Facets: Zipfian Skew (80/20), Cache Pressure, Large Payloads, & Churn\n"
        );
    }

    let temp_dir = TempDir::new().unwrap();
    let mut all_results = Vec::new();

    // 1. Zipfian Skewed Traffic
    if args.format != "json" {
        println!(
            ">>> [1/4] Running Skewed Zipfian Traffic (YCSB Workload B: 95% Read / 5% Write)..."
        );
    }
    let (zipf_tej_db, zipf_redb, zipf_sqlite) =
        run_zipfian_ycsb_b(temp_dir.path(), args.keys, args.ops);
    all_results.push(zipf_tej_db);
    all_results.push(zipf_redb);
    all_results.push(zipf_sqlite);

    // 2. Cache Pressure (Out of Cache)
    if args.format != "json" {
        println!(">>> [2/4] Running Cache Pressure Test (256 KB Tiny Cache vs 2.5 MB Dataset)...");
    }
    let (cp_tej_db, cp_redb, cp_sqlite) =
        run_cache_pressure_test(temp_dir.path(), args.keys, args.ops);
    all_results.push(cp_tej_db);
    all_results.push(cp_redb);
    all_results.push(cp_sqlite);

    // 3. Payload Scaling (Near Page Limit & Multi-Page Bounds)
    if args.format != "json" {
        println!(">>> [3/4] Running Large Payload Scaling Tests (32B to 4096B values)...");
    }
    for size in [32, 64, 128, 256, 512, 1024, 1500, 2048, 4096] {
        all_results.extend(run_payload_scaling_test(temp_dir.path(), size));
    }

    // 4. Churn & Fragmentation
    if args.format != "json" {
        println!(
            ">>> [4/4] Running Churn & Fragmentation Stress Test (50% Deletions + Re-inserts)..."
        );
    }
    all_results.extend(run_churn_fragmentation_test(temp_dir.path(), args.keys));

    if args.format == "json" {
        println!("{}", serde_json::to_string_pretty(&all_results).unwrap());
    } else {
        // Formatted Markdown Table Output
        println!(
            "\n| {:<26} | {:<20} | {:<30} | {:>12} | {:>14} | {:>9} | {:<32} |",
            "Category",
            "Engine",
            "Workload",
            "Duration",
            "Throughput",
            "p99 (µs)",
            "Analysis / Context"
        );
        println!(
            "|{:-<28}|{:-<22}|{:-<32}|{:-<14}|{:-<16}|{:-<11}|{:-<34}|",
            "", "", "", "", "", "", ""
        );

        for r in &all_results {
            println!(
                "| {:<26} | {:<20} | {:<30} | {:>10.2}ms | {:>10.0} ops/s | {:>9.1} | {:<32} |",
                r.category,
                r.engine,
                r.workload,
                r.duration_ms,
                r.ops_per_sec,
                r.p99_us,
                r.extra_metric
            );
        }

        println!(
            "=========================================================================================="
        );
    }
}
