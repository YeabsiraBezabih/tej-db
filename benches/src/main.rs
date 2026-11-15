#![allow(
    clippy::unwrap_used,
    clippy::needless_range_loop,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]

use clap::Parser;
use redb::{Database as RedbDatabase, TableDefinition};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tej_db::{
    AuditLogHook, ChangeFeedHook, Database, DurabilityMode, KeyRange, TypedTable, TypedTableMut,
};
use tej_db_sql::{SqlEngine, Value};
use tempfile::TempDir;

const TABLE_REDB: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("bench");

#[derive(Parser, Debug, Clone)]
#[command(name = "bench_compare")]
#[command(
    about = "Dynamic Comparative ACID Embedded DBMS Benchmark Harness: TejDb vs redb vs SQLite"
)]
pub struct BenchArgs {
    /// Number of keys to insert/read
    #[arg(short, long, default_value_t = 10_000)]
    pub keys: u32,

    /// Size of payload values in bytes
    #[arg(short, long, default_value_t = 64)]
    pub val_size: usize,

    /// Batch size for batch random writes
    #[arg(short, long, default_value_t = 100)]
    pub batch_size: u32,

    /// Number of concurrent reader threads for concurrency stress test
    #[arg(short, long, default_value_t = 4)]
    pub threads: usize,

    /// Concurrency stress test duration in seconds
    #[arg(short, long, default_value_t = 3)]
    pub duration: u64,

    /// Output format: "table" or "json"
    #[arg(long, default_value = "table")]
    pub format: String,

    /// Specific engine filter: "all", "tej-db", "redb", "sqlite"
    #[arg(short, long, default_value = "all")]
    pub engine: String,

    /// Durability mode filter for TejDb benchmarks: "all", "immediate", "normal", "eventual", "none"
    #[arg(long, default_value = "all")]
    pub durability: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResult {
    pub engine: String,
    pub workload: String,
    pub ops: u64,
    pub duration_ms: f64,
    pub ops_per_sec: f64,
    pub p50_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullReport {
    pub config: BenchReportConfig,
    pub results: Vec<BenchResult>,
    pub storage_bytes: StorageFootprint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReportConfig {
    pub keys: u32,
    pub val_size: usize,
    pub batch_size: u32,
    pub threads: usize,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StorageFootprint {
    pub tej_db_bytes: u64,
    pub redb_bytes: u64,
    pub sqlite_bytes: u64,
}

fn calculate_percentiles(mut latencies: Vec<u64>) -> (f64, f64, f64) {
    if latencies.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    latencies.sort_unstable();
    let n = latencies.len();
    let p50 = latencies[(n as f64 * 0.50) as usize] as f64;
    let p95 = latencies[((n as f64 * 0.95).min((n - 1) as f64)) as usize] as f64;
    let p99 = latencies[((n as f64 * 0.99).min((n - 1) as f64)) as usize] as f64;
    (p50, p95, p99)
}

fn generate_key(i: u32) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[0..4].copy_from_slice(b"key_");
    key[4..8].copy_from_slice(&i.to_be_bytes());
    key[8..12].copy_from_slice(&(!i).to_be_bytes());
    key[12..16].copy_from_slice(&(i ^ 0xAA_AA_AA_AA).to_be_bytes());
    key
}

fn generate_random_key(i: u32, total_keys: u32) -> [u8; 16] {
    let rand_val = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % total_keys;
    generate_key(rand_val)
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

// ==========================================
// 1. TejDb Benchmarks
// ==========================================
fn bench_tej_db_mem(args: &BenchArgs) -> Vec<BenchResult> {
    let mut results = Vec::new();
    let db = Database::open_memory().unwrap();
    let n_keys = args.keys;

    // 1. Bulk Sequential Insert
    let start = Instant::now();
    let mut tx = db.begin_write().unwrap();
    for i in 0..n_keys {
        let k = generate_key(i);
        let v = generate_val(i, args.val_size);
        tx.put(&k, &v).unwrap();
    }
    tx.commit().unwrap();
    let duration = start.elapsed();
    let ops_per_sec = n_keys as f64 / duration.as_secs_f64();
    results.push(BenchResult {
        engine: "TejDb (Memory)".into(),
        workload: format!("Bulk Sequential Write ({n_keys})"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec,
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // 2. Point Reads (Sequential)
    let rx = db.begin_read().unwrap();
    let mut latencies = Vec::with_capacity(n_keys as usize);
    let start = Instant::now();
    for i in 0..n_keys {
        let k = generate_key(i);
        let t0 = Instant::now();
        let val = rx.get(&k).unwrap();
        assert!(val.is_some());
        latencies.push(t0.elapsed().as_micros() as u64);
    }
    let duration = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: "TejDb (Memory)".into(),
        workload: format!("Sequential Point Read ({n_keys})"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // 3. Point Reads (Random)
    let mut latencies = Vec::with_capacity(n_keys as usize);
    let start = Instant::now();
    for i in 0..n_keys {
        let k = generate_random_key(i, n_keys);
        let t0 = Instant::now();
        let val = rx.get(&k).unwrap();
        assert!(val.is_some());
        latencies.push(t0.elapsed().as_micros() as u64);
    }
    let duration = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: "TejDb (Memory)".into(),
        workload: format!("Random Point Read ({n_keys})"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // 4. Range Scans (100 scans)
    let start = Instant::now();
    let mut total_scanned = 0;
    let scans = 100;
    let span = (n_keys / scans).max(1);
    for i in 0..scans {
        let start_k = generate_key(i * (span / 2));
        let end_k = generate_key(i * (span / 2) + span);
        let range = KeyRange {
            start: std::ops::Bound::Included(start_k.to_vec()),
            end: std::ops::Bound::Excluded(end_k.to_vec()),
        };
        let items = rx.scan_range(&range).unwrap();
        total_scanned += items.len();
    }
    let duration = start.elapsed();
    results.push(BenchResult {
        engine: "TejDb (Memory)".into(),
        workload: format!("Range Scans ({scans} scans)"),
        ops: total_scanned as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: total_scanned as f64 / duration.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    results
}

fn bench_tej_db_durability(
    dir: &Path,
    args: &BenchArgs,
    engine_name: &str,
    file_name: &str,
    mode: DurabilityMode,
) -> (Vec<BenchResult>, u64) {
    let mut results = Vec::new();
    let db_path = dir.join(file_name);
    let db = Database::open_file_with_durability(&db_path, 1024, mode).unwrap();
    let n_keys = args.keys;
    let batch_size = args.batch_size;

    // 1. Bulk Sequential Insert
    let start = Instant::now();
    let mut tx = db.begin_write().unwrap();
    for i in 0..n_keys {
        let k = generate_key(i);
        let v = generate_val(i, args.val_size);
        tx.put(&k, &v).unwrap();
    }
    tx.commit().unwrap();
    let duration = start.elapsed();
    results.push(BenchResult {
        engine: engine_name.into(),
        workload: format!("Bulk Sequential Write ({n_keys})"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // 2. Batch Random Write
    let start = Instant::now();
    let num_batches = (n_keys / batch_size).max(1);
    let mut commit_latencies = Vec::with_capacity(num_batches as usize);
    for batch in 0..num_batches {
        let mut tx = db.begin_write().unwrap();
        for item in 0..batch_size {
            let i = batch * batch_size + item;
            if i >= n_keys {
                break;
            }
            let k = generate_random_key(i, n_keys);
            let v = generate_val(i, args.val_size);
            tx.put(&k, &v).unwrap();
        }
        let t_commit = Instant::now();
        tx.commit().unwrap();
        commit_latencies.push(t_commit.elapsed().as_micros() as u64);
    }
    let duration = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(commit_latencies);
    results.push(BenchResult {
        engine: engine_name.into(),
        workload: format!("Batch Random Write ({n_keys}, {num_batches} tx)"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // 3. Point Reads (Random)
    let rx = db.begin_read().unwrap();
    let mut latencies = Vec::with_capacity(n_keys as usize);
    let start = Instant::now();
    for i in 0..n_keys {
        let k = generate_random_key(i, n_keys);
        let t0 = Instant::now();
        let val = rx.get(&k).unwrap();
        assert!(val.is_some());
        latencies.push(t0.elapsed().as_micros() as u64);
    }
    let duration = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: engine_name.into(),
        workload: format!("Random Point Read ({n_keys})"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    let file_size = fs::metadata(&db_path).map_or(0, |m| m.len());
    (results, file_size)
}

fn bench_tej_db_file(dir: &Path, args: &BenchArgs) -> (Vec<BenchResult>, u64) {
    bench_tej_db_durability(
        dir,
        args,
        "TejDb (Immediate/fsync)",
        "tej_db_bench_strict.db",
        DurabilityMode::Immediate,
    )
}

fn bench_tej_db_file_normal(dir: &Path, args: &BenchArgs) -> (Vec<BenchResult>, u64) {
    bench_tej_db_durability(
        dir,
        args,
        "TejDb (Normal/Disk)",
        "tej_db_bench_normal.db",
        DurabilityMode::Normal,
    )
}

fn bench_tej_db_file_eventual(dir: &Path, args: &BenchArgs) -> (Vec<BenchResult>, u64) {
    bench_tej_db_durability(
        dir,
        args,
        "TejDb (Eventual/Sync)",
        "tej_db_bench_eventual.db",
        DurabilityMode::Eventual { interval_ms: 10 },
    )
}

fn bench_tej_db_file_none(dir: &Path, args: &BenchArgs) -> (Vec<BenchResult>, u64) {
    bench_tej_db_durability(
        dir,
        args,
        "TejDb (None/Bypass)",
        "tej_db_bench_none.db",
        DurabilityMode::None,
    )
}

// ==========================================
// 2. Redb Benchmarks
// ==========================================
fn bench_redb(dir: &Path, args: &BenchArgs) -> (Vec<BenchResult>, u64) {
    let mut results = Vec::new();
    let db_path = dir.join("redb_bench.redb");
    let db = RedbDatabase::create(&db_path).unwrap();
    let n_keys = args.keys;
    let batch_size = args.batch_size;

    // 1. Bulk Sequential Insert
    let start = Instant::now();
    let write_txn = db.begin_write().unwrap();
    {
        let mut table = write_txn.open_table(TABLE_REDB).unwrap();
        for i in 0..n_keys {
            let k = generate_key(i);
            let v = generate_val(i, args.val_size);
            table.insert(&k[..], &v[..]).unwrap();
        }
    }
    write_txn.commit().unwrap();
    let duration = start.elapsed();
    results.push(BenchResult {
        engine: "Redb (File/ACID)".into(),
        workload: format!("Bulk Sequential Write ({n_keys})"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // 2. Batch Random Write
    let start = Instant::now();
    let num_batches = (n_keys / batch_size).max(1);
    let mut commit_latencies = Vec::with_capacity(num_batches as usize);
    for batch in 0..num_batches {
        let write_txn = db.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(TABLE_REDB).unwrap();
            for item in 0..batch_size {
                let i = batch * batch_size + item;
                if i >= n_keys {
                    break;
                }
                let k = generate_random_key(i, n_keys);
                let v = generate_val(i, args.val_size);
                table.insert(&k[..], &v[..]).unwrap();
            }
        }
        let t_commit = Instant::now();
        write_txn.commit().unwrap();
        commit_latencies.push(t_commit.elapsed().as_micros() as u64);
    }
    let duration = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(commit_latencies);
    results.push(BenchResult {
        engine: "Redb (File/ACID)".into(),
        workload: format!("Batch Random Write ({n_keys}, {num_batches} tx)"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // 3. Point Reads (Random)
    let read_txn = db.begin_read().unwrap();
    let table = read_txn.open_table(TABLE_REDB).unwrap();
    let mut latencies = Vec::with_capacity(n_keys as usize);
    let start = Instant::now();
    for i in 0..n_keys {
        let k = generate_random_key(i, n_keys);
        let t0 = Instant::now();
        let val = table.get(&k[..]).unwrap();
        assert!(val.is_some());
        latencies.push(t0.elapsed().as_micros() as u64);
    }
    let duration = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: "Redb (File/ACID)".into(),
        workload: format!("Random Point Read ({n_keys})"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    let file_size = fs::metadata(&db_path).map_or(0, |m| m.len());
    (results, file_size)
}

// ==========================================
// 3. SQLite (WAL Mode) Benchmarks
// ==========================================
fn bench_sqlite_mode(
    dir: &Path,
    args: &BenchArgs,
    sync_mode: &str,
    label: &str,
) -> (Vec<BenchResult>, u64) {
    let mut results = Vec::new();
    let file_name = format!("sqlite_{sync_mode}_bench.db");
    let db_path = dir.join(&file_name);
    let mut conn = Connection::open(&db_path).unwrap();
    let n_keys = args.keys;
    let batch_size = args.batch_size;

    conn.execute_batch(&format!(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = {sync_mode};
         PRAGMA cache_size = -4096;
         CREATE TABLE IF NOT EXISTS kv (key BLOB PRIMARY KEY, val BLOB);"
    ))
    .unwrap();

    // 1. Bulk Sequential Insert
    let start = Instant::now();
    {
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare_cached("INSERT OR REPLACE INTO kv (key, val) VALUES (?1, ?2)")
                .unwrap();
            for i in 0..n_keys {
                let k = generate_key(i);
                let v = generate_val(i, args.val_size);
                stmt.execute(params![&k[..], &v[..]]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let duration = start.elapsed();
    results.push(BenchResult {
        engine: label.into(),
        workload: format!("Bulk Sequential Write ({n_keys})"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // 2. Batch Random Write
    let start = Instant::now();
    let num_batches = (n_keys / batch_size).max(1);
    let mut commit_latencies = Vec::with_capacity(num_batches as usize);
    for batch in 0..num_batches {
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare_cached("INSERT OR REPLACE INTO kv (key, val) VALUES (?1, ?2)")
                .unwrap();
            for item in 0..batch_size {
                let i = batch * batch_size + item;
                if i >= n_keys {
                    break;
                }
                let k = generate_random_key(i, n_keys);
                let v = generate_val(i, args.val_size);
                stmt.execute(params![&k[..], &v[..]]).unwrap();
            }
        }
        let t_commit = Instant::now();
        tx.commit().unwrap();
        commit_latencies.push(t_commit.elapsed().as_micros() as u64);
    }
    let duration = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(commit_latencies);
    results.push(BenchResult {
        engine: label.into(),
        workload: format!("Batch Random Write ({n_keys}, {num_batches} tx)"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // 3. Point Reads (Random)
    let mut latencies = Vec::with_capacity(n_keys as usize);
    let start = Instant::now();
    {
        let mut stmt = conn
            .prepare_cached("SELECT val FROM kv WHERE key = ?1")
            .unwrap();
        for i in 0..n_keys {
            let k = generate_random_key(i, n_keys);
            let t0 = Instant::now();
            let mut rows = stmt.query(params![&k[..]]).unwrap();
            let row = rows.next().unwrap();
            assert!(row.is_some());
            latencies.push(t0.elapsed().as_micros() as u64);
        }
    }
    let duration = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: label.into(),
        workload: format!("Random Point Read ({n_keys})"),
        ops: n_keys as u64,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / duration.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    let main_size = fs::metadata(&db_path).map_or(0, |m| m.len());
    let wal_size = fs::metadata(dir.join(format!("{file_name}-wal"))).map_or(0, |m| m.len());
    (results, main_size + wal_size)
}

fn bench_sqlite(dir: &Path, args: &BenchArgs) -> (Vec<BenchResult>, u64) {
    let (res_normal, size_normal) = bench_sqlite_mode(dir, args, "NORMAL", "SQLite (WAL/Normal)");
    let (res_full, size_full) = bench_sqlite_mode(dir, args, "FULL", "SQLite (WAL/fsync FULL)");
    let mut all = res_normal;
    all.extend(res_full);
    (all, size_normal.max(size_full))
}

// ==========================================
// 4. Concurrency Stress Benchmark
// ==========================================
fn bench_tej_db_concurrency(args: &BenchArgs) -> BenchResult {
    let db = Arc::new(Database::open_memory().unwrap());
    let running = Arc::new(AtomicBool::new(true));
    let read_ops = Arc::new(AtomicU64::new(0));
    let write_ops = Arc::new(AtomicU64::new(0));

    // Pre-populate with keys
    {
        let mut tx = db.begin_write().unwrap();
        for i in 0..1000 {
            tx.put(&generate_key(i), &generate_val(i, args.val_size))
                .unwrap();
        }
        tx.commit().unwrap();
    }

    let mut handles = Vec::new();

    // Spawn N concurrent readers
    for r in 0..args.threads {
        let db = Arc::clone(&db);
        let running = Arc::clone(&running);
        let read_ops = Arc::clone(&read_ops);
        handles.push(thread::spawn(move || {
            let mut count = 0u64;
            let mut i = (r as u32) * 100;
            while running.load(Ordering::Relaxed) {
                if let Ok(rx) = db.begin_read() {
                    let k = generate_key(i % 1000);
                    let _ = rx.get(&k);
                    count += 1;
                }
                i = i.wrapping_add(1);
            }
            read_ops.fetch_add(count, Ordering::SeqCst);
        }));
    }

    // Spawn 1 writer
    let db_w = Arc::clone(&db);
    let running_w = Arc::clone(&running);
    let write_ops_w = Arc::clone(&write_ops);
    let val_size = args.val_size;
    let batch_size = args.batch_size;
    let writer_handle = thread::spawn(move || {
        let mut count = 0u64;
        let mut i = 1000u32;
        while running_w.load(Ordering::Relaxed) {
            if let Ok(mut tx) = db_w.begin_write() {
                for _ in 0..batch_size {
                    let k = generate_key(i % 5000);
                    let v = generate_val(i, val_size);
                    let _ = tx.put(&k, &v);
                    i = i.wrapping_add(1);
                }
                if tx.commit().is_ok() {
                    count += batch_size as u64;
                }
            }
        }
        write_ops_w.fetch_add(count, Ordering::SeqCst);
    });

    let start = Instant::now();
    thread::sleep(Duration::from_secs(args.duration));
    running.store(false, Ordering::SeqCst);

    for h in handles {
        h.join().unwrap();
    }
    writer_handle.join().unwrap();
    let elapsed = start.elapsed();

    let total_r = read_ops.load(Ordering::SeqCst);
    let total_w = write_ops.load(Ordering::SeqCst);
    let total_ops = total_r + total_w;

    BenchResult {
        engine: "TejDb (Memory/COW)".into(),
        workload: format!(
            "Concurrent 1W+{}R (reads: {total_r}, writes: {total_w})",
            args.threads
        ),
        ops: total_ops,
        duration_ms: elapsed.as_secs_f64() * 1000.0,
        ops_per_sec: total_ops as f64 / elapsed.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    }
}

fn bench_tej_db_multi_writer(args: &BenchArgs) -> BenchResult {
    let db = Arc::new(Database::open_memory().unwrap());
    let running = Arc::new(AtomicBool::new(true));
    let write_ops = Arc::new(AtomicU64::new(0));
    let num_writers = args.threads.max(4);
    let mut handles = Vec::new();

    let batch_size = args.batch_size;
    let start = Instant::now();
    for w in 0..num_writers {
        let db_w = Arc::clone(&db);
        let running_w = Arc::clone(&running);
        let write_ops_w = Arc::clone(&write_ops);
        let val_size = args.val_size;
        handles.push(thread::spawn(move || {
            let mut count = 0u64;
            let mut i = (w as u32) * 1_000_000;
            while running_w.load(Ordering::Relaxed) {
                if let Ok(mut tx) = db_w.begin_write() {
                    for _ in 0..batch_size {
                        let k = generate_key(i);
                        let v = generate_val(i, val_size);
                        let _ = tx.put(&k, &v);
                        i = i.wrapping_add(1);
                    }
                    if tx.commit().is_ok() {
                        count += batch_size as u64;
                    }
                }
            }
            write_ops_w.fetch_add(count, Ordering::SeqCst);
        }));
    }

    thread::sleep(Duration::from_secs(args.duration));
    running.store(false, Ordering::SeqCst);

    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed();
    let total_w = write_ops.load(Ordering::SeqCst);

    BenchResult {
        engine: "TejDb (Memory/OCC)".into(),
        workload: format!("Multi-Writer OCC ({num_writers} Concurrent Writers)"),
        ops: total_w,
        duration_ms: elapsed.as_secs_f64() * 1000.0,
        ops_per_sec: total_w as f64 / elapsed.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    }
}

// ==========================================
// 5. TejDb-SQL vs SQLite Comparative Benchmarks
// ==========================================
fn bench_tej_db_sql_vs_sqlite(dir: &Path, args: &BenchArgs) -> Vec<BenchResult> {
    let mut results = Vec::new();
    let n_rows = args.keys.min(1000);

    // A. SQLite (WAL)
    let sqlite_path = dir.join("sqlite_sql_bench.db");
    let mut sqlite_conn = Connection::open(&sqlite_path).unwrap();
    sqlite_conn
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER, score REAL);",
        )
        .unwrap();

    // 1. SQLite Bulk Insert
    let start = Instant::now();
    {
        let tx = sqlite_conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare_cached("INSERT INTO users (id, name, age, score) VALUES (?1, ?2, ?3, ?4)")
                .unwrap();
            for i in 0..n_rows {
                let name = format!("User_{i}");
                let age = (i % 80) + 18;
                let score = (i as f64) * 1.5;
                stmt.execute(params![i, name, age, score]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let dur = start.elapsed();
    results.push(BenchResult {
        engine: "SQLite (WAL)".into(),
        workload: format!("SQL Bulk Insert ({n_rows} rows)"),
        ops: n_rows as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_rows as f64 / dur.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // 2. SQLite Point Queries (WHERE id = ?)
    let mut latencies = Vec::with_capacity(n_rows as usize);
    let start = Instant::now();
    {
        let mut stmt = sqlite_conn
            .prepare_cached("SELECT name, score FROM users WHERE id = ?1")
            .unwrap();
        for i in 0..n_rows {
            let rand_id = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % n_rows;
            let t0 = Instant::now();
            let mut rows = stmt.query(params![rand_id]).unwrap();
            let row = rows.next().unwrap();
            assert!(row.is_some());
            latencies.push(t0.elapsed().as_micros() as u64);
        }
    }
    let dur = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: "SQLite (WAL)".into(),
        workload: format!("SQL Point Query PK ({n_rows} ops)"),
        ops: n_rows as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_rows as f64 / dur.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // 3. SQLite Point Updates (WHERE id = ?)
    let mut latencies = Vec::with_capacity(n_rows as usize);
    let start = Instant::now();
    {
        let tx = sqlite_conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare_cached("UPDATE users SET age = ?1 WHERE id = ?2")
                .unwrap();
            for i in 0..n_rows {
                let rand_id = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % n_rows;
                let new_age = (i % 70) + 20;
                let t0 = Instant::now();
                stmt.execute(params![new_age, rand_id]).unwrap();
                latencies.push(t0.elapsed().as_micros() as u64);
            }
        }
        tx.commit().unwrap();
    }
    let dur = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: "SQLite (WAL)".into(),
        workload: format!("SQL Point Update PK ({n_rows} ops)"),
        ops: n_rows as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_rows as f64 / dur.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // B. TejDb-SQL
    let tej_db_sql_path = dir.join("tej_db_sql_bench.db");
    let tej_db_instance = Database::open_file(&tej_db_sql_path).unwrap();
    // Match SQLite WAL with PRAGMA synchronous = NORMAL
    tej_db_instance.set_durability(tej_db::DurabilityMode::Normal);
    let tej_db_sql = SqlEngine::new(&tej_db_instance);
    tej_db_sql
        .execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT, score REAL)")
        .unwrap();

    // 1. TejDb-SQL Bulk Insert (in single transaction matching SQLite)
    let start = Instant::now();
    {
        let mut tx = tej_db_instance.begin_write().unwrap();
        {
            let stmt = tej_db_sql
                .prepare_statement("INSERT INTO users (id, name, age, score) VALUES (?, ?, ?, ?)")
                .unwrap();
            for i in 0..n_rows {
                let name = format!("User_{i}");
                let age = (i % 80) + 18;
                let score = (i as f64) * 1.5;
                stmt.execute_in_tx(
                    &mut tx,
                    &[
                        Value::Integer(i as i64),
                        Value::Text(name),
                        Value::Integer(age as i64),
                        Value::Real(score),
                    ],
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let dur = start.elapsed();
    results.push(BenchResult {
        engine: "TejDb-SQL (Engine)".into(),
        workload: format!("SQL Bulk Insert ({n_rows} rows)"),
        ops: n_rows as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_rows as f64 / dur.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // 2. TejDb-SQL Point Queries (WHERE id = ? with prepared statement matching SQLite)
    let mut latencies = Vec::with_capacity(n_rows as usize);
    let start = Instant::now();
    let stmt = tej_db_sql
        .prepare_statement("SELECT name, score FROM users WHERE id = ?")
        .unwrap();
    for i in 0..n_rows {
        let rand_id = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % n_rows;
        let t0 = Instant::now();
        let res = stmt.execute(&[Value::Integer(rand_id as i64)]).unwrap();
        assert_eq!(res.rows.len(), 1);
        latencies.push(t0.elapsed().as_micros() as u64);
    }
    let dur = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: "TejDb-SQL (Engine)".into(),
        workload: format!("SQL Point Query PK ({n_rows} ops)"),
        ops: n_rows as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_rows as f64 / dur.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // 3. TejDb-SQL Point Updates (WHERE id = ? with prepared statement in tx matching SQLite)
    let mut latencies = Vec::with_capacity(n_rows as usize);
    let start = Instant::now();
    {
        let mut tx = tej_db_instance.begin_write().unwrap();
        let stmt = tej_db_sql
            .prepare_statement("UPDATE users SET age = ? WHERE id = ?")
            .unwrap();
        for i in 0..n_rows {
            let rand_id = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % n_rows;
            let new_age = (i % 70) + 20;
            let t0 = Instant::now();
            let res = stmt
                .execute_in_tx(
                    &mut tx,
                    &[
                        Value::Integer(new_age as i64),
                        Value::Integer(rand_id as i64),
                    ],
                )
                .unwrap();
            assert_eq!(res.rows_affected, 1);
            latencies.push(t0.elapsed().as_micros() as u64);
        }
        tx.commit().unwrap();
    }
    let dur = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: "TejDb-SQL (Engine)".into(),
        workload: format!("SQL Point Update PK ({n_rows} ops)"),
        ops: n_rows as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_rows as f64 / dur.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    results
}

// ==========================================
// 6. Typed API (Table<K, V>) vs Raw Byte KV Benchmarks
// ==========================================
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct BenchRecord {
    pub id: u64,
    pub name: String,
    pub email: String,
    pub active: bool,
    pub score: f64,
}

fn bench_typed_table_vs_raw(args: &BenchArgs) -> Vec<BenchResult> {
    let mut results = Vec::new();
    let n_keys = args.keys;

    // 1. Raw Byte KV
    let raw_db = Database::open_memory().unwrap();
    raw_db.create_table("raw_tbl").unwrap();

    // Raw Write
    let start = Instant::now();
    {
        let mut tx = raw_db.begin_write().unwrap();
        let mut tbl = tx.open_table_mut("raw_tbl").unwrap();
        for i in 0..n_keys {
            let k = (i as u64).to_be_bytes();
            let v = generate_val(i, 64);
            tbl.put(&k, &v).unwrap();
        }
        tx.commit().unwrap();
    }
    let dur = start.elapsed();
    results.push(BenchResult {
        engine: "Raw Byte KV".into(),
        workload: format!("Sequential Write ({n_keys} ops)"),
        ops: n_keys as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / dur.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // Raw Point Read
    let rx = raw_db.begin_read().unwrap();
    let tbl = rx.open_table("raw_tbl").unwrap();
    let mut latencies = Vec::with_capacity(n_keys as usize);
    let start = Instant::now();
    for i in 0..n_keys {
        let rand_i = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % n_keys;
        let k = (rand_i as u64).to_be_bytes();
        let t0 = Instant::now();
        let val = tbl.get(&k).unwrap();
        assert!(val.is_some());
        latencies.push(t0.elapsed().as_micros() as u64);
    }
    let dur = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: "Raw Byte KV".into(),
        workload: format!("Random Point Read ({n_keys} ops)"),
        ops: n_keys as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / dur.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // Raw Range Scan
    let start = Instant::now();
    let range = KeyRange {
        start: std::ops::Bound::Included(0u64.to_be_bytes().to_vec()),
        end: std::ops::Bound::Excluded((n_keys as u64 / 2).to_be_bytes().to_vec()),
    };
    let scanned = tbl.scan_range(&range).unwrap();
    let dur = start.elapsed();
    results.push(BenchResult {
        engine: "Raw Byte KV".into(),
        workload: format!("Range Scan 50% ({n_keys} total)"),
        ops: scanned.len() as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: scanned.len() as f64 / dur.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // 2. Typed Table<u64, BenchRecord>
    let typed_db = Database::open_memory().unwrap();
    typed_db.create_table("typed_tbl").unwrap();

    // Typed Write
    let start = Instant::now();
    {
        let mut tx = typed_db.begin_write().unwrap();
        let tbl_mut = tx.open_table_mut("typed_tbl").unwrap();
        let mut typed_tbl = TypedTableMut::<_, u64, BenchRecord>::new(tbl_mut);
        for i in 0..n_keys {
            let record = BenchRecord {
                id: i as u64,
                name: format!("User_{i}"),
                email: format!("user_{i}@example.com"),
                active: i % 2 == 0,
                score: (i as f64) * 2.5,
            };
            typed_tbl.put(&(i as u64), &record).unwrap();
        }
        tx.commit().unwrap();
    }
    let dur = start.elapsed();
    results.push(BenchResult {
        engine: "Typed Table<K,V>".into(),
        workload: format!("Sequential Write ({n_keys} ops)"),
        ops: n_keys as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / dur.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // Typed Point Read
    let rx = typed_db.begin_read().unwrap();
    let tbl = rx.open_table("typed_tbl").unwrap();
    let typed_tbl = TypedTable::<_, u64, BenchRecord>::new(tbl);
    let mut latencies = Vec::with_capacity(n_keys as usize);
    let start = Instant::now();
    for i in 0..n_keys {
        let rand_i = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % n_keys;
        let t0 = Instant::now();
        let val = typed_tbl.get(&(rand_i as u64)).unwrap();
        assert!(val.is_some());
        latencies.push(t0.elapsed().as_micros() as u64);
    }
    let dur = start.elapsed();
    let (p50, p95, p99) = calculate_percentiles(latencies);
    results.push(BenchResult {
        engine: "Typed Table<K,V>".into(),
        workload: format!("Random Point Read ({n_keys} ops)"),
        ops: n_keys as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: n_keys as f64 / dur.as_secs_f64(),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    });

    // Typed Range Scan
    let start = Instant::now();
    let start_key = 0u64;
    let end_key = (n_keys / 2) as u64;
    let scanned = typed_tbl
        .scan_range(Some(&start_key), Some(&end_key))
        .unwrap();
    let dur = start.elapsed();
    results.push(BenchResult {
        engine: "Typed Table<K,V>".into(),
        workload: format!("Range Scan 50% ({n_keys} total)"),
        ops: scanned.len() as u64,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: scanned.len() as f64 / dur.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    results
}

// ==========================================
// 7. Hook System Overhead Benchmarks
// ==========================================
fn bench_hook_overhead(args: &BenchArgs) -> Vec<BenchResult> {
    let mut results = Vec::new();
    let num_tx = (args.keys / 10).clamp(100, 1000);
    let puts_per_tx = 10;
    let total_ops = num_tx as u64 * puts_per_tx as u64;

    // 1. Baseline: No Hooks
    let db = Database::open_memory().unwrap();
    let start = Instant::now();
    for t in 0..num_tx {
        let mut tx = db.begin_write().unwrap();
        for p in 0..puts_per_tx {
            let i = t * puts_per_tx + p;
            tx.put(&generate_key(i), &generate_val(i, 64)).unwrap();
        }
        tx.commit().unwrap();
    }
    let dur = start.elapsed();
    results.push(BenchResult {
        engine: "TejDb (No Hooks)".into(),
        workload: format!("Batch Writes ({num_tx} tx, {total_ops} puts)"),
        ops: total_ops,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: total_ops as f64 / dur.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // 2. AuditLogHook
    let db = Database::open_memory().unwrap();
    let audit_count = Arc::new(AtomicU64::new(0));
    let audit_count_clone = Arc::clone(&audit_count);
    db.add_hook(Box::new(AuditLogHook::new(move |changes| {
        audit_count_clone.fetch_add(
            (changes.writes.len() + changes.deletes.len()) as u64,
            Ordering::Relaxed,
        );
    })));

    let start = Instant::now();
    for t in 0..num_tx {
        let mut tx = db.begin_write().unwrap();
        for p in 0..puts_per_tx {
            let i = t * puts_per_tx + p;
            tx.put(&generate_key(i), &generate_val(i, 64)).unwrap();
        }
        tx.commit().unwrap();
    }
    let dur = start.elapsed();
    assert_eq!(audit_count.load(Ordering::SeqCst), total_ops);
    results.push(BenchResult {
        engine: "TejDb (AuditLogHook)".into(),
        workload: format!("Batch Writes ({num_tx} tx, {total_ops} puts)"),
        ops: total_ops,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: total_ops as f64 / dur.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    // 3. ChangeFeedHook
    let db = Database::open_memory().unwrap();
    let (feed_tx, feed_rx) = mpsc::channel::<tej_db::ChangeSet>();
    let feed_count = Arc::new(AtomicU64::new(0));
    let feed_count_clone = Arc::clone(&feed_count);
    let drain_handle = thread::spawn(move || {
        while let Ok(cs) = feed_rx.recv() {
            feed_count_clone.fetch_add(
                (cs.writes.len() + cs.deletes.len()) as u64,
                Ordering::Relaxed,
            );
        }
    });

    db.add_hook(Box::new(ChangeFeedHook::new(feed_tx)));

    let start = Instant::now();
    for t in 0..num_tx {
        let mut tx = db.begin_write().unwrap();
        for p in 0..puts_per_tx {
            let i = t * puts_per_tx + p;
            tx.put(&generate_key(i), &generate_val(i, 64)).unwrap();
        }
        tx.commit().unwrap();
    }
    let dur = start.elapsed();
    drop(db);
    let _ = drain_handle.join();
    assert_eq!(feed_count.load(Ordering::SeqCst), total_ops);

    results.push(BenchResult {
        engine: "TejDb (ChangeFeedHook)".into(),
        workload: format!("Batch Writes ({num_tx} tx, {total_ops} puts)"),
        ops: total_ops,
        duration_ms: dur.as_secs_f64() * 1000.0,
        ops_per_sec: total_ops as f64 / dur.as_secs_f64(),
        p50_us: 0.0,
        p95_us: 0.0,
        p99_us: 0.0,
    });

    results
}

// ==========================================
// 8. Large Blob Overflow Performance Benchmarks
// ==========================================
fn bench_large_blob_overflow(dir: &Path, args: &BenchArgs) -> Vec<BenchResult> {
    let mut results = Vec::new();
    let db_path = dir.join("tej_db_blob_overflow.db");
    let db = Database::open_file(&db_path).unwrap();

    let sizes = [
        ("Inline (64 B)", 64, args.keys.min(1000)),
        (
            "Overflow (16 KB / ~4 pages)",
            16 * 1024,
            (args.keys / 10).clamp(50, 200),
        ),
        (
            "Overflow (64 KB / ~16 pages)",
            64 * 1024,
            (args.keys / 20).clamp(20, 100),
        ),
    ];

    for (label, blob_size, count) in sizes {
        let blob_data = vec![0xBB; blob_size];

        // 1. Sequential Write
        let start = Instant::now();
        let mut tx = db.begin_write().unwrap();
        for i in 0..count {
            let k = generate_key(i);
            tx.put(&k, &blob_data).unwrap();
        }
        tx.commit().unwrap();
        let dur = start.elapsed();
        let total_mb = (count as f64 * blob_size as f64) / (1024.0 * 1024.0);
        let mb_per_sec = total_mb / dur.as_secs_f64();
        results.push(BenchResult {
            engine: format!("TejDb Blob {label}"),
            workload: format!("Write {count} blobs ({mb_per_sec:.1} MB/s)"),
            ops: count as u64,
            duration_ms: dur.as_secs_f64() * 1000.0,
            ops_per_sec: count as f64 / dur.as_secs_f64(),
            p50_us: 0.0,
            p95_us: 0.0,
            p99_us: 0.0,
        });

        // 2. Point Reads
        let rx = db.begin_read().unwrap();
        let mut latencies = Vec::with_capacity(count as usize);
        let start = Instant::now();
        for i in 0..count {
            let k = generate_key(i);
            let t0 = Instant::now();
            let val = rx.get(&k).unwrap();
            assert_eq!(val.unwrap().len(), blob_size);
            latencies.push(t0.elapsed().as_micros() as u64);
        }
        let dur = start.elapsed();
        let (p50, p95, p99) = calculate_percentiles(latencies);
        let read_mb_per_sec = total_mb / dur.as_secs_f64();
        results.push(BenchResult {
            engine: format!("TejDb Blob {label}"),
            workload: format!("Read {count} blobs ({read_mb_per_sec:.1} MB/s)"),
            ops: count as u64,
            duration_ms: dur.as_secs_f64() * 1000.0,
            ops_per_sec: count as f64 / dur.as_secs_f64(),
            p50_us: p50,
            p95_us: p95,
            p99_us: p99,
        });
    }

    results
}

fn main() {
    let args = BenchArgs::parse();

    if args.format != "json" {
        println!(
            "=========================================================================================="
        );
        println!(
            "   ACID EMBEDDED DBMS DYNAMIC COMPARATIVE BENCHMARK: TejDb vs redb vs SQLite (WAL)   "
        );
        println!(
            "=========================================================================================="
        );
        println!(
            "Configuration: Keys: {} | Val Size: {}B | Batch Size: {} | Threads: {} | Duration: {}s",
            args.keys, args.val_size, args.batch_size, args.threads, args.duration
        );
        println!("Filter: Engine: '{}'\n", args.engine);
    }

    let temp_dir = TempDir::new().unwrap();
    let mut all_results = Vec::new();
    let mut storage_footprint = StorageFootprint::default();

    let run_all = args.engine == "all";
    let run_tej_db = run_all || args.engine.contains("tej-db");
    let run_redb = run_all || args.engine.contains("redb");
    let run_sqlite = run_all || args.engine.contains("sqlite");
    let run_sql = run_all || args.engine.contains("sql");
    let run_typed = run_all || args.engine.contains("typed");
    let run_hooks = run_all || args.engine.contains("hook");
    let run_blob = run_all || args.engine.contains("blob");

    if run_tej_db {
        if args.format != "json" {
            println!(">>> Running TejDb In-Memory Benchmarks...");
        }
        let tej_db_mem_res = bench_tej_db_mem(&args);
        all_results.extend(tej_db_mem_res);

        let dur_filter = args.durability.to_lowercase();
        let run_all_dur = dur_filter == "all";

        if run_all_dur || dur_filter.contains("immediate") {
            if args.format != "json" {
                println!(">>> Running TejDb File/Disk (Immediate/fsync) Benchmarks...");
            }
            let (tej_db_file_res, tej_db_file_size) = bench_tej_db_file(temp_dir.path(), &args);
            storage_footprint.tej_db_bytes = tej_db_file_size;
            all_results.extend(tej_db_file_res);
        }

        if run_all_dur || dur_filter.contains("normal") {
            if args.format != "json" {
                println!(">>> Running TejDb File/Disk (Normal mode) Benchmarks...");
            }
            let (tej_db_normal_res, _) = bench_tej_db_file_normal(temp_dir.path(), &args);
            all_results.extend(tej_db_normal_res);
        }

        if run_all_dur || dur_filter.contains("eventual") {
            if args.format != "json" {
                println!(">>> Running TejDb File/Disk (Eventual background sync) Benchmarks...");
            }
            let (tej_db_eventual_res, _) = bench_tej_db_file_eventual(temp_dir.path(), &args);
            all_results.extend(tej_db_eventual_res);
        }

        if run_all_dur || dur_filter.contains("none") {
            if args.format != "json" {
                println!(">>> Running TejDb File/Disk (None/Bypass durability) Benchmarks...");
            }
            let (tej_db_none_res, _) = bench_tej_db_file_none(temp_dir.path(), &args);
            all_results.extend(tej_db_none_res);
        }
    }

    if run_redb {
        if args.format != "json" {
            println!(">>> Running Redb Benchmarks...");
        }
        let (redb_res, redb_file_size) = bench_redb(temp_dir.path(), &args);
        storage_footprint.redb_bytes = redb_file_size;
        all_results.extend(redb_res);
    }

    if run_sqlite {
        if args.format != "json" {
            println!(">>> Running SQLite (WAL mode) Benchmarks...");
        }
        let (sqlite_res, sqlite_file_size) = bench_sqlite(temp_dir.path(), &args);
        storage_footprint.sqlite_bytes = sqlite_file_size;
        all_results.extend(sqlite_res);
    }

    if run_tej_db {
        if args.format != "json" {
            println!(">>> Running TejDb Concurrent Readers + Writer Stress Test...");
        }
        let concurrent_res = bench_tej_db_concurrency(&args);
        all_results.push(concurrent_res);

        if args.format != "json" {
            println!(">>> Running TejDb Multi-Writer OCC Stress Test...");
        }
        let multi_writer_res = bench_tej_db_multi_writer(&args);
        all_results.push(multi_writer_res);
    }

    if run_sql {
        if args.format != "json" {
            println!(">>> Running TejDb-SQL vs SQLite Comparative Benchmarks...");
        }
        let sql_res = bench_tej_db_sql_vs_sqlite(temp_dir.path(), &args);
        all_results.extend(sql_res);
    }

    if run_typed {
        if args.format != "json" {
            println!(">>> Running Typed API (Table<K, V>) vs Raw Byte KV Benchmarks...");
        }
        let typed_res = bench_typed_table_vs_raw(&args);
        all_results.extend(typed_res);
    }

    if run_hooks {
        if args.format != "json" {
            println!(">>> Running Hook System Overhead Benchmarks...");
        }
        let hook_res = bench_hook_overhead(&args);
        all_results.extend(hook_res);
    }

    if run_blob {
        if args.format != "json" {
            println!(">>> Running Large Blob Overflow (>4096B Chains) Benchmarks...");
        }
        let blob_res = bench_large_blob_overflow(temp_dir.path(), &args);
        all_results.extend(blob_res);
    }

    if args.format == "json" {
        let report = FullReport {
            config: BenchReportConfig {
                keys: args.keys,
                val_size: args.val_size,
                batch_size: args.batch_size,
                threads: args.threads,
                duration_secs: args.duration,
            },
            results: all_results,
            storage_bytes: storage_footprint,
        };
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        // Print Table
        println!(
            "\n| {:<18} | {:<32} | {:>10} | {:>12} | {:>14} | {:>10} | {:>10} | {:>10} |",
            "Engine",
            "Workload",
            "Total Ops",
            "Elapsed (ms)",
            "Throughput",
            "p50 (µs)",
            "p95 (µs)",
            "p99 (µs)"
        );
        println!(
            "|{:-<20}|{:-<34}|{:-<12}|{:-<14}|{:-<16}|{:-<12}|{:-<12}|{:-<12}|",
            "", "", "", "", "", "", "", ""
        );

        for r in &all_results {
            println!(
                "| {:<18} | {:<32} | {:>10} | {:>10.2}ms | {:>10.0} ops/s | {:>10.1} | {:>10.1} | {:>10.1} |",
                r.engine,
                r.workload,
                r.ops,
                r.duration_ms,
                r.ops_per_sec,
                r.p50_us,
                r.p95_us,
                r.p99_us
            );
        }

        if run_tej_db && run_redb && run_sqlite {
            println!(
                "\n=========================================================================================="
            );
            println!("   STORAGE FOOTPRINT COMPARISON (Disk File Size)");
            println!(
                "=========================================================================================="
            );
            println!(
                "  - TejDb (File Storage): {:>10.2} KB ({:>8} bytes)",
                storage_footprint.tej_db_bytes as f64 / 1024.0,
                storage_footprint.tej_db_bytes
            );
            println!(
                "  - Redb (MVCC B-Tree):  {:>10.2} KB ({:>8} bytes)",
                storage_footprint.redb_bytes as f64 / 1024.0,
                storage_footprint.redb_bytes
            );
            println!(
                "  - SQLite (WAL Mode):   {:>10.2} KB ({:>8} bytes)",
                storage_footprint.sqlite_bytes as f64 / 1024.0,
                storage_footprint.sqlite_bytes
            );
            println!(
                "=========================================================================================="
            );
        }
    }
}
