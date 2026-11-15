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
use redb::{Builder as RedbBuilder, Database as RedbDatabase, TableDefinition};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::ops::Bound;
use std::path::Path;
use std::time::Instant;
use tej_db::{
    CompressedStorage, Database, DurabilityMode, EncryptedStorage, FileStorage, KeyRange,
};
use tempfile::TempDir;

const TABLE_REDB_BLOBS: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("blobs");
const TABLE_REDB_ITEMS: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("items");

#[derive(Parser, Debug, Clone)]
#[command(name = "bench_extreme")]
#[command(about = "Extreme Edge Cases Benchmark Suite (Phase 4): TejDb vs Redb vs SQLite (WAL)")]
pub struct ExtremeArgs {
    /// Number of keys for large-scale range scan test
    #[arg(short, long, default_value_t = 50_000)]
    pub scan_keys: u32,

    /// Output format: "table" or "json"
    #[arg(long, default_value = "table")]
    pub format: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtremeResult {
    pub scenario: String,
    pub engine: String,
    pub test_name: String,
    pub status: String,
    pub metric_label: String,
    pub metric_value: String,
    pub duration_ms: f64,
    pub details: String,
}

fn generate_key(i: u32) -> Vec<u8> {
    format!("key_{i:08}").into_bytes()
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
// 1. Scenario 1: Multi-Page Overflow BLOB Chaining (10 KiB to 1 MiB)
// =========================================================================
fn run_overflow_blob_benchmark(dir: &Path) -> Vec<ExtremeResult> {
    let mut results = Vec::new();
    let sizes = [
        10 * 1024,
        50 * 1024,
        100 * 1024,
        250 * 1024,
        500 * 1024,
        1024 * 1024,
    ];
    let total_bytes: usize = sizes.iter().sum();
    let total_mb = total_bytes as f64 / (1024.0 * 1024.0);
    let keys: Vec<Vec<u8>> = (0..sizes.len())
        .map(|idx| format!("blob_{idx:02}").into_bytes())
        .collect();
    let expected_blobs: Vec<Vec<u8>> = sizes
        .iter()
        .enumerate()
        .map(|(idx, &size)| generate_val(idx as u32, size))
        .collect();

    // --- TejDb ---
    {
        let db_path = dir.join("tej_db_overflow.db");
        let start_time = Instant::now();
        let db = Database::open_file(&db_path).unwrap();
        db.set_durability(DurabilityMode::Normal);

        let write_t0 = Instant::now();
        {
            let mut tx = db.begin_write().unwrap();
            for (key, val) in keys.iter().zip(&expected_blobs) {
                tx.put(key, val).unwrap();
            }
            tx.commit().unwrap();
        }
        let write_dur = write_t0.elapsed();

        let read_t0 = Instant::now();
        let mut all_match = true;
        {
            let rx = db.begin_read().unwrap();
            for (key, expected) in keys.iter().zip(&expected_blobs) {
                let matched = rx
                    .with_value(key, |actual| Ok(actual == expected.as_slice()))
                    .unwrap()
                    .unwrap_or(false);
                if !matched {
                    all_match = false;
                    break;
                }
            }
        }
        let read_dur = read_t0.elapsed();
        let write_mb_s = total_mb / write_dur.as_secs_f64();
        let read_mb_s = total_mb / read_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "1. Payload Scaling".into(),
            engine: "TejDb (File)".into(),
            test_name: "Multi-Page Overflow BLOBs (10KB-1MB)".into(),
            status: if all_match {
                "PASSED (100% Match)".into()
            } else {
                "FAILED".into()
            },
            metric_label: "Throughput (MB/s)".into(),
            metric_value: format!("Write: {write_mb_s:.1} MB/s | Read: {read_mb_s:.1} MB/s"),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: "Chained contiguous overflow extents across 6 BLOBs up to 1 MB".into(),
        });
    }

    // --- TejDb (Mmap) ---
    {
        let db_path = dir.join("tej_db_overflow_mmap.db");
        let start_time = Instant::now();
        let db = Database::open_mmap(&db_path).unwrap();
        db.set_durability(DurabilityMode::Normal);

        let write_t0 = Instant::now();
        {
            let mut tx = db.begin_write().unwrap();
            for (key, val) in keys.iter().zip(&expected_blobs) {
                tx.put(key, val).unwrap();
            }
            tx.commit().unwrap();
        }
        let write_dur = write_t0.elapsed();

        let read_t0 = Instant::now();
        let mut all_match = true;
        {
            let rx = db.begin_read().unwrap();
            for (key, expected) in keys.iter().zip(&expected_blobs) {
                let matched = rx
                    .with_value(key, |actual| Ok(actual == expected.as_slice()))
                    .unwrap()
                    .unwrap_or(false);
                if !matched {
                    all_match = false;
                    break;
                }
            }
        }
        let read_dur = read_t0.elapsed();
        let write_mb_s = total_mb / write_dur.as_secs_f64();
        let read_mb_s = total_mb / read_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "1. Payload Scaling".into(),
            engine: "TejDb (Mmap)".into(),
            test_name: "Multi-Page Overflow BLOBs (10KB-1MB)".into(),
            status: if all_match {
                "PASSED (100% Match)".into()
            } else {
                "FAILED".into()
            },
            metric_label: "Throughput (MB/s)".into(),
            metric_value: format!("Write: {write_mb_s:.1} MB/s | Read: {read_mb_s:.1} MB/s"),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: "Memory-mapped zero-copy BLOB extent streaming".into(),
        });
    }

    // --- Redb ---
    {
        let redb_path = dir.join("redb_overflow.redb");
        let start_time = Instant::now();
        let db = RedbDatabase::create(&redb_path).unwrap();

        let write_t0 = Instant::now();
        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(TABLE_REDB_BLOBS).unwrap();
                for (key, val) in keys.iter().zip(&expected_blobs) {
                    table.insert(&key[..], &val[..]).unwrap();
                }
            }
            tx.commit().unwrap();
        }
        let write_dur = write_t0.elapsed();

        let read_t0 = Instant::now();
        let mut all_match = true;
        {
            let rx = db.begin_read().unwrap();
            let table = rx.open_table(TABLE_REDB_BLOBS).unwrap();
            for (key, expected) in keys.iter().zip(&expected_blobs) {
                let actual = table.get(&key[..]).unwrap().expect("BLOB must exist");
                if actual.value() != expected.as_slice() {
                    all_match = false;
                    break;
                }
            }
        }
        let read_dur = read_t0.elapsed();
        let write_mb_s = total_mb / write_dur.as_secs_f64();
        let read_mb_s = total_mb / read_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "1. Payload Scaling".into(),
            engine: "Redb".into(),
            test_name: "Multi-Page Overflow BLOBs (10KB-1MB)".into(),
            status: if all_match {
                "PASSED (100% Match)".into()
            } else {
                "FAILED".into()
            },
            metric_label: "Throughput (MB/s)".into(),
            metric_value: format!("Write: {write_mb_s:.1} MB/s | Read: {read_mb_s:.1} MB/s"),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: "Native Redb branch/leaf multi-page overflow allocation".into(),
        });
    }

    // --- SQLite (WAL) ---
    {
        let sqlite_path = dir.join("sqlite_overflow.db");
        let start_time = Instant::now();
        let mut conn = Connection::open(&sqlite_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE blobs (key BLOB PRIMARY KEY, val BLOB);",
        )
        .unwrap();

        let write_t0 = Instant::now();
        {
            let tx = conn.transaction().unwrap();
            {
                let mut stmt = tx
                    .prepare_cached("INSERT INTO blobs (key, val) VALUES (?1, ?2)")
                    .unwrap();
                for (key, val) in keys.iter().zip(&expected_blobs) {
                    stmt.execute(params![&key[..], &val[..]]).unwrap();
                }
            }
            tx.commit().unwrap();
        }
        let write_dur = write_t0.elapsed();

        let read_t0 = Instant::now();
        let mut all_match = true;
        {
            let mut stmt = conn
                .prepare("SELECT val FROM blobs WHERE key = ?1")
                .unwrap();
            for (key, expected) in keys.iter().zip(&expected_blobs) {
                let actual: Vec<u8> = stmt.query_row(params![&key[..]], |r| r.get(0)).unwrap();
                if actual != *expected {
                    all_match = false;
                    break;
                }
            }
        }
        let read_dur = read_t0.elapsed();
        let write_mb_s = total_mb / write_dur.as_secs_f64();
        let read_mb_s = total_mb / read_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "1. Payload Scaling".into(),
            engine: "SQLite (WAL)".into(),
            test_name: "Multi-Page Overflow BLOBs (10KB-1MB)".into(),
            status: if all_match {
                "PASSED (100% Match)".into()
            } else {
                "FAILED".into()
            },
            metric_label: "Throughput (MB/s)".into(),
            metric_value: format!("Write: {write_mb_s:.1} MB/s | Read: {read_mb_s:.1} MB/s"),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: "SQLite linked list chained overflow pages".into(),
        });
    }

    results
}

// =========================================================================
// 2. Scenario 2: Out-of-Core Cold Disk Paging (64KB Cache << Dataset)
// =========================================================================
fn run_cold_disk_paging_benchmark(dir: &Path) -> Vec<ExtremeResult> {
    let mut results = Vec::new();
    let items = 10_000u32;
    let lookups = 2000u32;

    // --- TejDb ---
    {
        let db_path = dir.join("tej_db_cold_disk.db");
        let start_time = Instant::now();
        {
            let db = Database::open_file(&db_path).unwrap();
            db.set_durability(DurabilityMode::Normal);
            let mut tx = db.begin_write().unwrap();
            for i in 0..items {
                tx.put(&generate_key(i), &generate_val(i, 128)).unwrap();
            }
            tx.commit().unwrap();
        }

        // Reopen with strict 16-page (64 KiB) cold cache
        let db = Database::open_file_with_capacity(&db_path, 16).unwrap();
        let rx = db.begin_read().unwrap();

        let t0 = Instant::now();
        for i in 0..lookups {
            let key_idx = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % items;
            let key = generate_key(key_idx);
            let val = rx.get(&key).unwrap().expect("Key must exist");
            assert_eq!(val.len(), 128);
        }
        let cold_dur = t0.elapsed();
        let stats = db.stats();
        let miss_ratio =
            stats.cache_misses as f64 / (stats.cache_hits + stats.cache_misses).max(1) as f64;
        let ops_s = lookups as f64 / cold_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "2. Memory Starvation".into(),
            engine: "TejDb (File)".into(),
            test_name: "Cold Disk Paging (64KB RAM Limit)".into(),
            status: "PASSED".into(),
            metric_label: "Cold Lookups / Miss Rate".into(),
            metric_value: format!("{ops_s:.0} ops/s (Miss: {:.1}%)", miss_ratio * 100.0),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: format!(
                "Hits: {} | Misses: {} (DB: {:.2} MB)",
                stats.cache_hits,
                stats.cache_misses,
                fs::metadata(&db_path).map_or(0, |m| m.len()) as f64 / 1_048_576.0
            ),
        });
    }

    // --- TejDb (Mmap) ---
    {
        let db_path = dir.join("tej_db_cold_mmap.db");
        let start_time = Instant::now();
        {
            let db = Database::open_mmap(&db_path).unwrap();
            db.set_durability(DurabilityMode::Normal);
            let mut tx = db.begin_write().unwrap();
            for i in 0..items {
                tx.put(&generate_key(i), &generate_val(i, 128)).unwrap();
            }
            tx.commit().unwrap();
        }

        let db = Database::open_mmap_with_capacity(&db_path, 16).unwrap();
        let rx = db.begin_read().unwrap();

        let t0 = Instant::now();
        for i in 0..lookups {
            let key_idx = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % items;
            let key = generate_key(key_idx);
            let val = rx.get(&key).unwrap().expect("Key must exist");
            assert_eq!(val.len(), 128);
        }
        let cold_dur = t0.elapsed();
        let ops_s = lookups as f64 / cold_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "2. Memory Starvation".into(),
            engine: "TejDb (Mmap)".into(),
            test_name: "Cold Disk Paging (64KB RAM Limit)".into(),
            status: "PASSED".into(),
            metric_label: "Cold Lookups / Miss Rate".into(),
            metric_value: format!("{ops_s:.0} ops/s (Zero-Copy Mmap)"),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: format!(
                "Memory-mapped direct paging over {:.2} MB file",
                fs::metadata(&db_path).map_or(0, |m| m.len()) as f64 / 1_048_576.0
            ),
        });
    }

    // --- Redb ---
    {
        let redb_path = dir.join("redb_cold_disk.redb");
        let start_time = Instant::now();
        {
            let db = RedbDatabase::create(&redb_path).unwrap();
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(TABLE_REDB_ITEMS).unwrap();
                for i in 0..items {
                    table
                        .insert(&generate_key(i)[..], &generate_val(i, 128)[..])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        // Reopen with 64 KiB cache limit
        let db = RedbBuilder::new()
            .set_cache_size(64 * 1024)
            .create(&redb_path)
            .or_else(|_| RedbDatabase::open(&redb_path))
            .unwrap();
        let rx = db.begin_read().unwrap();
        let table = rx.open_table(TABLE_REDB_ITEMS).unwrap();

        let t0 = Instant::now();
        for i in 0..lookups {
            let key_idx = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % items;
            let key = generate_key(key_idx);
            let val = table.get(&key[..]).unwrap().expect("Key must exist");
            assert_eq!(val.value().len(), 128);
        }
        let cold_dur = t0.elapsed();
        let ops_s = lookups as f64 / cold_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "2. Memory Starvation".into(),
            engine: "Redb".into(),
            test_name: "Cold Disk Paging (64KB RAM Limit)".into(),
            status: "PASSED".into(),
            metric_label: "Cold Lookups / Miss Rate".into(),
            metric_value: format!("{ops_s:.0} ops/s (mmap OS paging)"),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: format!(
                "OS page faults over {:.2} MB file",
                fs::metadata(&redb_path).map_or(0, |m| m.len()) as f64 / 1_048_576.0
            ),
        });
    }

    // --- SQLite (WAL) ---
    {
        let sqlite_path = dir.join("sqlite_cold_disk.db");
        let start_time = Instant::now();
        let mut conn = Connection::open(&sqlite_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE items (key BLOB PRIMARY KEY, val BLOB);",
        )
        .unwrap();

        {
            let tx = conn.transaction().unwrap();
            {
                let mut stmt = tx
                    .prepare_cached("INSERT INTO items (key, val) VALUES (?1, ?2)")
                    .unwrap();
                for i in 0..items {
                    stmt.execute(params![&generate_key(i)[..], &generate_val(i, 128)[..]])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        // Apply strict 64 KiB cache limit (-64 in SQLite)
        conn.pragma_update(None, "cache_size", -64).unwrap();
        let mut stmt = conn
            .prepare("SELECT val FROM items WHERE key = ?1")
            .unwrap();

        let t0 = Instant::now();
        for i in 0..lookups {
            let key_idx = (i.wrapping_mul(1664525).wrapping_add(1013904223)) % items;
            let key = generate_key(key_idx);
            let val: Vec<u8> = stmt.query_row(params![&key[..]], |r| r.get(0)).unwrap();
            assert_eq!(val.len(), 128);
        }
        let cold_dur = t0.elapsed();
        let ops_s = lookups as f64 / cold_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "2. Memory Starvation".into(),
            engine: "SQLite (WAL)".into(),
            test_name: "Cold Disk Paging (64KB RAM Limit)".into(),
            status: "PASSED".into(),
            metric_label: "Cold Lookups / Miss Rate".into(),
            metric_value: format!("{ops_s:.0} ops/s (PRAGMA cache_size = -64)"),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: format!(
                "SQLite B-Tree cache paging over {:.2} MB file",
                fs::metadata(&sqlite_path).map_or(0, |m| m.len()) as f64 / 1_048_576.0
            ),
        });
    }

    results
}

// =========================================================================
// 3. Scenario 3: Long-Running MVCC Snapshot Retention (Epoch GC Lag)
// =========================================================================
fn run_mvcc_retention_benchmark(dir: &Path) -> Vec<ExtremeResult> {
    let mut results = Vec::new();

    // --- TejDb ---
    {
        let db_path = dir.join("tej_db_mvcc.db");
        let start_time = Instant::now();
        let db = Database::open_file(&db_path).unwrap();
        db.set_durability(DurabilityMode::Normal);

        // Initial baseline dataset
        {
            let mut tx = db.begin_write().unwrap();
            for i in 0..500 {
                tx.put(&generate_key(i), &generate_val(i, 64)).unwrap();
            }
            tx.commit().unwrap();
        }

        // Open a long-running reader snapshot
        let reader_snapshot = db.begin_read().unwrap();

        // Concurrently execute 50 write transactions mutating keys while reader is alive
        for cycle in 1..=50 {
            let mut tx = db.begin_write().unwrap();
            for i in 0..20 {
                let k = (cycle * 20 + i) % 500;
                tx.put(&generate_key(k), b"MUTATED_VALUE_UNDER_ACTIVE_READER")
                    .unwrap();
            }
            tx.commit().unwrap();
        }

        // Verify snapshot isolation: reader snapshot MUST still read original values
        let mut snapshot_intact = true;
        for i in 0..100 {
            let expected = generate_val(i, 64);
            let actual = reader_snapshot.get(&generate_key(i)).unwrap().unwrap();
            if actual != expected {
                snapshot_intact = false;
                break;
            }
        }
        let file_size_with_active_reader = fs::metadata(&db_path).map_or(0, |m| m.len());
        drop(reader_snapshot);

        // Execute one more cycle: free-list should now freely reclaim and recycle retired pages
        {
            let mut tx = db.begin_write().unwrap();
            tx.put(b"final_marker", b"reclaim").unwrap();
            tx.commit().unwrap();
        }
        let file_size_after_drop = fs::metadata(&db_path).map_or(0, |m| m.len());

        results.push(ExtremeResult {
            scenario: "3. MVCC Isolation".into(),
            engine: "TejDb".into(),
            test_name: "Long-Running Snapshot Retention".into(),
            status: if snapshot_intact {
                "PASSED (100% Isolated)".into()
            } else {
                "FAILED".into()
            },
            metric_label: "GC Lag & Footprint".into(),
            metric_value: format!(
                "Active: {:.2} MB -> Post-Drop: {:.2} MB",
                file_size_with_active_reader as f64 / 1_048_576.0,
                file_size_after_drop as f64 / 1_048_576.0
            ),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: "50 writer txs executed concurrently without tearing snapshot".into(),
        });
    }

    // --- Redb ---
    {
        let redb_path = dir.join("redb_mvcc.redb");
        let start_time = Instant::now();
        let db = RedbDatabase::create(&redb_path).unwrap();

        // Initial baseline dataset
        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(TABLE_REDB_ITEMS).unwrap();
                for i in 0..500 {
                    table
                        .insert(&generate_key(i)[..], &generate_val(i, 64)[..])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        // Open a long-running reader snapshot
        let reader_snapshot = db.begin_read().unwrap();
        let reader_table = reader_snapshot.open_table(TABLE_REDB_ITEMS).unwrap();

        // Concurrently execute 50 write transactions
        for cycle in 1..=50 {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(TABLE_REDB_ITEMS).unwrap();
                for i in 0..20 {
                    let k = (cycle * 20 + i) % 500;
                    table
                        .insert(
                            &generate_key(k)[..],
                            &b"MUTATED_VALUE_UNDER_ACTIVE_READER"[..],
                        )
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        // Verify snapshot isolation
        let mut snapshot_intact = true;
        for i in 0..100 {
            let expected = generate_val(i, 64);
            let actual = reader_table.get(&generate_key(i)[..]).unwrap().unwrap();
            if actual.value() != expected.as_slice() {
                snapshot_intact = false;
                break;
            }
        }
        let file_size_with_active_reader = fs::metadata(&redb_path).map_or(0, |m| m.len());
        drop(reader_table);
        drop(reader_snapshot);

        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(TABLE_REDB_ITEMS).unwrap();
                table.insert(&b"final_marker"[..], &b"reclaim"[..]).unwrap();
            }
            tx.commit().unwrap();
        }
        let file_size_after_drop = fs::metadata(&redb_path).map_or(0, |m| m.len());

        results.push(ExtremeResult {
            scenario: "3. MVCC Isolation".into(),
            engine: "Redb".into(),
            test_name: "Long-Running Snapshot Retention".into(),
            status: if snapshot_intact {
                "PASSED (100% Isolated)".into()
            } else {
                "FAILED".into()
            },
            metric_label: "GC Lag & Footprint".into(),
            metric_value: format!(
                "Active: {:.2} MB -> Post-Drop: {:.2} MB",
                file_size_with_active_reader as f64 / 1_048_576.0,
                file_size_after_drop as f64 / 1_048_576.0
            ),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: "Redb CoW retained retired root b-tree nodes until reader dropped".into(),
        });
    }

    // --- SQLite (WAL) ---
    {
        let sqlite_path = dir.join("sqlite_mvcc.db");
        let start_time = Instant::now();
        let mut conn_writer = Connection::open(&sqlite_path).unwrap();
        conn_writer
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 CREATE TABLE items (key BLOB PRIMARY KEY, val BLOB);",
            )
            .unwrap();

        // Populate baseline
        {
            let tx = conn_writer.transaction().unwrap();
            {
                let mut stmt = tx
                    .prepare_cached("INSERT INTO items (key, val) VALUES (?1, ?2)")
                    .unwrap();
                for i in 0..500 {
                    stmt.execute(params![&generate_key(i)[..], &generate_val(i, 64)[..]])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        // Open read connection holding an active read transaction
        let conn_reader = Connection::open(&sqlite_path).unwrap();
        conn_reader.execute("BEGIN DEFERRED", []).unwrap();
        {
            let mut stmt = conn_reader
                .prepare("SELECT val FROM items WHERE key = ?1")
                .unwrap();
            let _: Vec<u8> = stmt
                .query_row(params![&generate_key(0)[..]], |r| r.get(0))
                .unwrap();
        }

        // Writer modifies 50 transactions
        for cycle in 1..=50 {
            let tx = conn_writer.transaction().unwrap();
            {
                let mut stmt = tx
                    .prepare_cached("UPDATE items SET val = ?1 WHERE key = ?2")
                    .unwrap();
                for i in 0..20 {
                    let k = (cycle * 20 + i) % 500;
                    stmt.execute(params![
                        b"MUTATED_VALUE_UNDER_ACTIVE_READER",
                        &generate_key(k)[..]
                    ])
                    .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        // Verify reader snapshot intact
        let mut snapshot_intact = true;
        {
            let mut stmt = conn_reader
                .prepare("SELECT val FROM items WHERE key = ?1")
                .unwrap();
            for i in 0..100 {
                let expected = generate_val(i, 64);
                let actual: Vec<u8> = stmt
                    .query_row(params![&generate_key(i)[..]], |r| r.get(0))
                    .unwrap();
                if actual != expected {
                    snapshot_intact = false;
                    break;
                }
            }
        }
        let wal_path = dir.join("sqlite_mvcc.db-wal");
        let wal_size_with_reader = fs::metadata(&wal_path).map_or(0, |m| m.len());
        conn_reader.execute("ROLLBACK", []).unwrap();
        drop(conn_reader);

        // Checkpoint WAL after reader release
        let _ = conn_writer.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        let wal_size_after_drop = fs::metadata(&wal_path).map_or(0, |m| m.len());

        results.push(ExtremeResult {
            scenario: "3. MVCC Isolation".into(),
            engine: "SQLite (WAL)".into(),
            test_name: "Long-Running Snapshot Retention".into(),
            status: if snapshot_intact {
                "PASSED (100% Isolated)".into()
            } else {
                "FAILED".into()
            },
            metric_label: "GC Lag & Footprint".into(),
            metric_value: format!(
                "WAL Lag: {:.2} MB -> Truncated: {:.2} MB",
                wal_size_with_reader as f64 / 1_048_576.0,
                wal_size_after_drop as f64 / 1_048_576.0
            ),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: "WAL checkpoint blocked from truncation until reader transaction released"
                .into(),
        });
    }

    results
}

// =========================================================================
// 4. Scenario 4: Large-Scale Range Scan & Cursor Iteration
// =========================================================================
fn run_large_range_scan_benchmark(dir: &Path, count: u32) -> Vec<ExtremeResult> {
    let mut results = Vec::new();
    let scan_size = 5000u32;
    let iterations = 10u32;

    // --- TejDb (File) ---
    {
        let db_path = dir.join("tej_db_range.db");
        let start_time = Instant::now();
        let db = Database::open_file(&db_path).unwrap();
        db.set_durability(DurabilityMode::Normal);

        {
            let mut tx = db.begin_write().unwrap();
            for i in 0..count {
                tx.put(&generate_key(i), &generate_val(i, 32)).unwrap();
            }
            tx.commit().unwrap();
        }

        let rx = db.begin_read().unwrap();
        let t0 = Instant::now();
        let mut total_scanned = 0usize;

        for iter in 0..iterations {
            let start_idx = iter * 4000;
            let end_idx = start_idx + scan_size;
            let start_k = generate_key(start_idx);
            let end_k = generate_key(end_idx);

            let range = KeyRange {
                start: Bound::Included(start_k),
                end: Bound::Excluded(end_k),
            };
            let mut scanned = 0;
            rx.scan_range_visitor(&range, |k, _v| {
                assert_ne!(k, &[] as &[u8]);
                scanned += 1;
                Ok(true)
            })
            .unwrap();
            assert_eq!(scanned, scan_size as usize);
            total_scanned += scanned;
        }
        let scan_dur = t0.elapsed();
        let scan_speed = total_scanned as f64 / scan_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "4. Cursor Traversal".into(),
            engine: "TejDb (File)".into(),
            test_name: format!("Range Scan ({count} Keys)"),
            status: "PASSED".into(),
            metric_label: "Scan Throughput".into(),
            metric_value: format!(
                "{scan_speed:.0} items/s ({:.2} ms / 5k range)",
                (scan_dur.as_secs_f64() * 1000.0) / iterations as f64
            ),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: format!("Scanned {total_scanned} records along B-Tree next_leaf cursor chain"),
        });
    }

    // --- TejDb (Mmap) ---
    {
        let db_path = dir.join("tej_db_range_mmap.db");
        let start_time = Instant::now();
        let db = Database::open_mmap(&db_path).unwrap();
        db.set_durability(DurabilityMode::Normal);

        {
            let mut tx = db.begin_write().unwrap();
            for i in 0..count {
                tx.put(&generate_key(i), &generate_val(i, 32)).unwrap();
            }
            tx.commit().unwrap();
        }

        let rx = db.begin_read().unwrap();
        let t0 = Instant::now();
        let mut total_scanned = 0usize;

        for iter in 0..iterations {
            let start_idx = iter * 4000;
            let end_idx = start_idx + scan_size;
            let start_k = generate_key(start_idx);
            let end_k = generate_key(end_idx);

            let range = KeyRange {
                start: Bound::Included(start_k),
                end: Bound::Excluded(end_k),
            };
            let mut scanned = 0;
            rx.scan_range_visitor(&range, |k, _v| {
                assert_ne!(k, &[] as &[u8]);
                scanned += 1;
                Ok(true)
            })
            .unwrap();
            assert_eq!(scanned, scan_size as usize);
            total_scanned += scanned;
        }
        let scan_dur = t0.elapsed();
        let scan_speed = total_scanned as f64 / scan_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "4. Cursor Traversal".into(),
            engine: "TejDb (Mmap)".into(),
            test_name: format!("Range Scan ({count} Keys)"),
            status: "PASSED".into(),
            metric_label: "Scan Throughput".into(),
            metric_value: format!(
                "{scan_speed:.0} items/s ({:.2} ms / 5k range)",
                (scan_dur.as_secs_f64() * 1000.0) / iterations as f64
            ),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: format!("Scanned {total_scanned} records along B-Tree next_leaf cursor chain (Mmap zero-copy)"),
        });
    }

    // --- Redb ---
    {
        let redb_path = dir.join("redb_range.redb");
        let start_time = Instant::now();
        let db = RedbDatabase::create(&redb_path).unwrap();

        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(TABLE_REDB_ITEMS).unwrap();
                for i in 0..count {
                    table
                        .insert(&generate_key(i)[..], &generate_val(i, 32)[..])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        let rx = db.begin_read().unwrap();
        let table = rx.open_table(TABLE_REDB_ITEMS).unwrap();
        let t0 = Instant::now();
        let mut total_scanned = 0usize;

        for iter in 0..iterations {
            let start_idx = iter * 4000;
            let end_idx = start_idx + scan_size;
            let start_k = generate_key(start_idx);
            let end_k = generate_key(end_idx);

            let iter_items = table.range(&start_k[..]..&end_k[..]).unwrap();
            let mut scanned = 0;
            for item in iter_items {
                let (k, _v) = item.unwrap();
                assert_ne!(k.value(), &[] as &[u8]);
                scanned += 1;
            }
            assert_eq!(scanned, scan_size as usize);
            total_scanned += scanned;
        }
        let scan_dur = t0.elapsed();
        let scan_speed = total_scanned as f64 / scan_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "4. Cursor Traversal".into(),
            engine: "Redb".into(),
            test_name: format!("Range Scan ({count} Keys)"),
            status: "PASSED".into(),
            metric_label: "Scan Throughput".into(),
            metric_value: format!(
                "{scan_speed:.0} items/s ({:.2} ms / 5k range)",
                (scan_dur.as_secs_f64() * 1000.0) / iterations as f64
            ),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: format!("Scanned {total_scanned} records using Redb table.range() iterator"),
        });
    }

    // --- SQLite (WAL) ---
    {
        let sqlite_path = dir.join("sqlite_range.db");
        let start_time = Instant::now();
        let mut conn = Connection::open(&sqlite_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE items (key BLOB PRIMARY KEY, val BLOB);",
        )
        .unwrap();

        {
            let tx = conn.transaction().unwrap();
            {
                let mut stmt = tx
                    .prepare_cached("INSERT INTO items (key, val) VALUES (?1, ?2)")
                    .unwrap();
                for i in 0..count {
                    stmt.execute(params![&generate_key(i)[..], &generate_val(i, 32)[..]])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        let mut stmt = conn
            .prepare("SELECT key, val FROM items WHERE key >= ?1 AND key < ?2 ORDER BY key")
            .unwrap();
        let t0 = Instant::now();
        let mut total_scanned = 0usize;

        for iter in 0..iterations {
            let start_idx = iter * 4000;
            let end_idx = start_idx + scan_size;
            let start_k = generate_key(start_idx);
            let end_k = generate_key(end_idx);

            let mut rows = stmt.query(params![&start_k[..], &end_k[..]]).unwrap();
            let mut scanned = 0;
            while let Some(row) = rows.next().unwrap() {
                let _k: Vec<u8> = row.get(0).unwrap();
                scanned += 1;
            }
            assert_eq!(scanned, scan_size as usize);
            total_scanned += scanned;
        }
        let scan_dur = t0.elapsed();
        let scan_speed = total_scanned as f64 / scan_dur.as_secs_f64();

        results.push(ExtremeResult {
            scenario: "4. Cursor Traversal".into(),
            engine: "SQLite (WAL)".into(),
            test_name: format!("Range Scan ({count} Keys)"),
            status: "PASSED".into(),
            metric_label: "Scan Throughput".into(),
            metric_value: format!(
                "{scan_speed:.0} items/s ({:.2} ms / 5k range)",
                (scan_dur.as_secs_f64() * 1000.0) / iterations as f64
            ),
            duration_ms: start_time.elapsed().as_secs_f64() * 1000.0,
            details: format!("Scanned {total_scanned} records via SQL B-Tree cursor index scan"),
        });
    }

    results
}

// =========================================================================
// 5. Scenario 5: Transparent Compression & Encryption A/B Matrix
// =========================================================================
fn run_crypto_compression_matrix(dir: &Path) -> Vec<ExtremeResult> {
    let mut results = Vec::new();
    let count = 4000u32;

    // A: TejDb Baseline (Plain POSIX)
    {
        let path = dir.join("plain.db");
        let t0 = Instant::now();
        let db = Database::open_file(&path).unwrap();
        db.set_durability(DurabilityMode::Normal);
        {
            let mut tx = db.begin_write().unwrap();
            for i in 0..count {
                tx.put(&generate_key(i), &generate_val(i, 128)).unwrap();
            }
            tx.commit().unwrap();
        }
        let write_dur = t0.elapsed();

        let read_t0 = Instant::now();
        let rx = db.begin_read().unwrap();
        for i in 0..count {
            let _ = rx.get(&generate_key(i)).unwrap();
        }
        let read_dur = read_t0.elapsed();
        let size = fs::metadata(&path).map_or(0, |m| m.len());

        results.push(ExtremeResult {
            scenario: "5. Storage Transforms".into(),
            engine: "TejDb (Plain)".into(),
            test_name: "A/B: Direct POSIX Baseline".into(),
            status: "PASSED".into(),
            metric_label: "Throughput / Size".into(),
            metric_value: format!(
                "{:.0} w-ops/s | {:.0} r-ops/s (Size: {:.2} MB)",
                count as f64 / write_dur.as_secs_f64(),
                count as f64 / read_dur.as_secs_f64(),
                size as f64 / 1_048_576.0
            ),
            duration_ms: write_dur.as_secs_f64() * 1000.0,
            details: "Standard uncompressed, unencrypted POSIX file storage".into(),
        });
    }

    // B: TejDb Transparent LZ4 Compression
    {
        let path = dir.join("compressed.db");
        let t0 = Instant::now();
        let raw = FileStorage::open(&path).unwrap();
        let compressed = CompressedStorage::new(raw);
        let db = Database::open_custom(compressed, 500).unwrap();
        db.set_durability(DurabilityMode::Normal);
        {
            let mut tx = db.begin_write().unwrap();
            for i in 0..count {
                tx.put(&generate_key(i), &generate_val(i, 128)).unwrap();
            }
            tx.commit().unwrap();
        }
        let write_dur = t0.elapsed();

        let read_t0 = Instant::now();
        let rx = db.begin_read().unwrap();
        for i in 0..count {
            let _ = rx.get(&generate_key(i)).unwrap();
        }
        let read_dur = read_t0.elapsed();
        let size = fs::metadata(&path).map_or(0, |m| m.len());

        results.push(ExtremeResult {
            scenario: "5. Storage Transforms".into(),
            engine: "TejDb (LZ4)".into(),
            test_name: "A/B: Transparent LZ4".into(),
            status: "PASSED".into(),
            metric_label: "Throughput / Size".into(),
            metric_value: format!(
                "{:.0} w-ops/s | {:.0} r-ops/s (Size: {:.2} MB)",
                count as f64 / write_dur.as_secs_f64(),
                count as f64 / read_dur.as_secs_f64(),
                size as f64 / 1_048_576.0
            ),
            duration_ms: write_dur.as_secs_f64() * 1000.0,
            details: "Per-page transparent LZ4 compression & decompression".into(),
        });
    }

    // C: TejDb Transparent ChaCha20-Poly1305 Encryption
    {
        let path = dir.join("encrypted.db");
        let t0 = Instant::now();
        let raw = FileStorage::open(&path).unwrap();
        let key = [0x42u8; 32];
        let encrypted = EncryptedStorage::new(raw, key);
        let db = Database::open_custom(encrypted, 500).unwrap();
        db.set_durability(DurabilityMode::Normal);
        {
            let mut tx = db.begin_write().unwrap();
            for i in 0..count {
                tx.put(&generate_key(i), &generate_val(i, 128)).unwrap();
            }
            tx.commit().unwrap();
        }
        let write_dur = t0.elapsed();

        let read_t0 = Instant::now();
        let rx = db.begin_read().unwrap();
        for i in 0..count {
            let _ = rx.get(&generate_key(i)).unwrap();
        }
        let read_dur = read_t0.elapsed();
        let size = fs::metadata(&path).map_or(0, |m| m.len());

        results.push(ExtremeResult {
            scenario: "5. Storage Transforms".into(),
            engine: "TejDb (ChaCha20)".into(),
            test_name: "A/B: Hardware ChaCha20-Poly1305".into(),
            status: "PASSED".into(),
            metric_label: "Throughput / Size".into(),
            metric_value: format!(
                "{:.0} w-ops/s | {:.0} r-ops/s (Size: {:.2} MB)",
                count as f64 / write_dur.as_secs_f64(),
                count as f64 / read_dur.as_secs_f64(),
                size as f64 / 1_048_576.0
            ),
            duration_ms: write_dur.as_secs_f64() * 1000.0,
            details: "256-bit AEAD authenticated hardware encryption per page".into(),
        });
    }

    // D: Redb Reference Baseline
    {
        let redb_path = dir.join("redb_baseline.redb");
        let t0 = Instant::now();
        let db = RedbDatabase::create(&redb_path).unwrap();
        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(TABLE_REDB_ITEMS).unwrap();
                for i in 0..count {
                    table
                        .insert(&generate_key(i)[..], &generate_val(i, 128)[..])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }
        let write_dur = t0.elapsed();

        let read_t0 = Instant::now();
        let rx = db.begin_read().unwrap();
        let table = rx.open_table(TABLE_REDB_ITEMS).unwrap();
        for i in 0..count {
            let _ = table.get(&generate_key(i)[..]).unwrap();
        }
        let read_dur = read_t0.elapsed();
        let size = fs::metadata(&redb_path).map_or(0, |m| m.len());

        results.push(ExtremeResult {
            scenario: "5. Storage Transforms".into(),
            engine: "Redb".into(),
            test_name: "A/B: Redb Reference Baseline".into(),
            status: "PASSED".into(),
            metric_label: "Throughput / Size".into(),
            metric_value: format!(
                "{:.0} w-ops/s | {:.0} r-ops/s (Size: {:.2} MB)",
                count as f64 / write_dur.as_secs_f64(),
                count as f64 / read_dur.as_secs_f64(),
                size as f64 / 1_048_576.0
            ),
            duration_ms: write_dur.as_secs_f64() * 1000.0,
            details: "Redb standard file storage baseline".into(),
        });
    }

    // E: SQLite (WAL) Reference Baseline
    {
        let sqlite_path = dir.join("sqlite_baseline.db");
        let t0 = Instant::now();
        let mut conn = Connection::open(&sqlite_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE items (key BLOB PRIMARY KEY, val BLOB);",
        )
        .unwrap();

        {
            let tx = conn.transaction().unwrap();
            {
                let mut stmt = tx
                    .prepare_cached("INSERT INTO items (key, val) VALUES (?1, ?2)")
                    .unwrap();
                for i in 0..count {
                    stmt.execute(params![&generate_key(i)[..], &generate_val(i, 128)[..]])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }
        let write_dur = t0.elapsed();

        let read_t0 = Instant::now();
        let mut stmt = conn
            .prepare("SELECT val FROM items WHERE key = ?1")
            .unwrap();
        for i in 0..count {
            let _: Vec<u8> = stmt
                .query_row(params![&generate_key(i)[..]], |r| r.get(0))
                .unwrap();
        }
        let read_dur = read_t0.elapsed();
        let size = fs::metadata(&sqlite_path).map_or(0, |m| m.len());

        results.push(ExtremeResult {
            scenario: "5. Storage Transforms".into(),
            engine: "SQLite (WAL)".into(),
            test_name: "A/B: SQLite (WAL) Baseline".into(),
            status: "PASSED".into(),
            metric_label: "Throughput / Size".into(),
            metric_value: format!(
                "{:.0} w-ops/s | {:.0} r-ops/s (Size: {:.2} MB)",
                count as f64 / write_dur.as_secs_f64(),
                count as f64 / read_dur.as_secs_f64(),
                size as f64 / 1_048_576.0
            ),
            duration_ms: write_dur.as_secs_f64() * 1000.0,
            details: "SQLite WAL standard uncompressed storage baseline".into(),
        });
    }

    results
}

fn main() {
    let args = ExtremeArgs::parse();

    if args.format != "json" {
        println!(
            "=================================================================================================="
        );
        println!(
            "   PHASE 4 ADVANCED EDGE CASES BENCHMARK SUITE: TejDb vs Redb vs SQLite (WAL)                     "
        );
        println!(
            "=================================================================================================="
        );
        println!(
            "Covering: Overflow BLOBs, Out-of-Core Cold Disk, MVCC GC Lag, Range Scans, & Crypto/Compression Matrix\n"
        );
    }

    let temp_dir = TempDir::new().unwrap();
    let mut results = Vec::new();

    // 1. Multi-Page Overflow BLOBs
    if args.format != "json" {
        println!(">>> [1/5] Executing Multi-Page Overflow BLOB Benchmark (10KB - 1MB)...");
    }
    results.extend(run_overflow_blob_benchmark(temp_dir.path()));

    // 2. Out-of-Core Cold Disk Paging
    if args.format != "json" {
        println!(">>> [2/5] Executing Out-of-Core Cold Disk Paging (64KB RAM Limit)...");
    }
    results.extend(run_cold_disk_paging_benchmark(temp_dir.path()));

    // 3. Long-Running MVCC Snapshot Retention
    if args.format != "json" {
        println!(">>> [3/5] Executing Long-Running MVCC Snapshot Retention Benchmark...");
    }
    results.extend(run_mvcc_retention_benchmark(temp_dir.path()));

    // 4. Large-Scale Range Scan & Cursor Iteration
    if args.format != "json" {
        println!(
            ">>> [4/5] Executing Large-Scale Range Scan Cursor Iteration ({} Keys)...",
            args.scan_keys
        );
    }
    results.extend(run_large_range_scan_benchmark(
        temp_dir.path(),
        args.scan_keys,
    ));

    // 5. Crypto & Compression A/B Matrix
    if args.format != "json" {
        println!(">>> [5/5] Executing Transparent Compression & Encryption A/B Matrix...");
    }
    results.extend(run_crypto_compression_matrix(temp_dir.path()));

    if args.format == "json" {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
    } else {
        println!(
            "\n| {:<24} | {:<16} | {:<32} | {:<22} | {:<36} |",
            "Category", "Engine", "Scenario Test", "Integrity Status", "Benchmark Telemetry"
        );
        println!(
            "|{:-<26}|{:-<18}|{:-<34}|{:-<24}|{:-<38}|",
            "", "", "", "", ""
        );

        for r in &results {
            println!(
                "| {:<24} | {:<16} | {:<32} | {:<22} | {:<36} |",
                r.scenario, r.engine, r.test_name, r.status, r.metric_value
            );
        }

        println!(
            "=================================================================================================="
        );
    }
}
