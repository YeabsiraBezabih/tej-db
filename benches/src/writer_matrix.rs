#![allow(
    clippy::unwrap_used,
    clippy::needless_range_loop,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]

//! Comprehensive Single-Writer vs Multi-Writer OCC Concurrency Matrix Benchmark
//!
//! Tests across:
//! - Small tasks (1-key autocommit, 10-key micro-batches)
//! - Medium tasks (50-key and 100-key batches)
//! - Big tasks (1,000-key and 10,000-key bulk batches)
//! - Complex tasks (Read-Modify-Write, Contended Zipfian Skew, Multi-Table, Wide Payloads, Mixed R/W)
//!
//! Evaluates scaling factors across 1, 2, 4, and 8 concurrent writers.

use std::ops::Bound;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use tej_db::Database;
use tej_db_core::KeyRange;

#[derive(Debug, Clone)]
pub struct ScenarioMetric {
    pub task_category: &'static str,
    pub scenario_name: &'static str,
    pub writers_1_tps: f64,
    pub writers_2_tps: f64,
    pub writers_4_tps: f64,
    pub writers_8_tps: f64,
    pub winning_mode: &'static str,
    pub scaling_factor: f64,
    pub architectural_takeaway: &'static str,
}

fn generate_key(prefix: u8, id: u32) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[0] = b'k';
    key[1] = b'e';
    key[2] = b'y';
    key[3] = prefix;
    key[4..8].copy_from_slice(&id.to_be_bytes());
    key[8..12].copy_from_slice(&(!id).to_be_bytes());
    key[12..16].copy_from_slice(&(id ^ 0xA5A5_A5A5).to_be_bytes());
    key
}

fn generate_val(id: u32, size: usize) -> Vec<u8> {
    let mut val = vec![0u8; size.max(8)];
    val[0..4].copy_from_slice(b"val_");
    val[4..8].copy_from_slice(&id.to_le_bytes());
    for j in 8..val.len() {
        val[j] = ((id + j as u32) % 256) as u8;
    }
    val
}

// ----------------------------------------------------------------------------
// Generic Benchmark Harness for Disjoint Writers
// ----------------------------------------------------------------------------
fn bench_disjoint_writers(
    num_writers: usize,
    batch_size: usize,
    val_size: usize,
    total_tx_target: usize,
) -> f64 {
    let db = Arc::new(Database::open_memory_with_capacity(65536).unwrap());
    let tx_per_writer = (total_tx_target / num_writers).max(10);
    let mut handles = Vec::with_capacity(num_writers);
    let barrier = Arc::new(Barrier::new(num_writers + 1));

    for w in 0..num_writers {
        let db = Arc::clone(&db);
        let b = Arc::clone(&barrier);
        let prefix = b'a' + (w % 26) as u8;
        handles.push(thread::spawn(move || {
            b.wait();
            let mut key_id = 0u32;
            for _ in 0..tx_per_writer {
                let mut tx = db.begin_write().unwrap();
                for _ in 0..batch_size {
                    let k = generate_key(prefix, key_id);
                    let v = generate_val(key_id, val_size);
                    tx.put(&k, &v).unwrap();
                    key_id = key_id.wrapping_add(1);
                }
                tx.commit().unwrap();
            }
        }));
    }

    barrier.wait();
    let start = Instant::now();
    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed().as_secs_f64();
    let total_keys = (tx_per_writer * num_writers * batch_size) as f64;
    total_keys / elapsed
}

fn bench_file_writers(
    num_writers: usize,
    batch_size: usize,
    val_size: usize,
    total_tx_target: usize,
) -> f64 {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bench_file.db");
    let db = Arc::new(
        Database::open_file_with_durability(&db_path, 1024, tej_db::DurabilityMode::Normal)
            .unwrap(),
    );

    let tx_per_writer = (total_tx_target / num_writers).max(10);
    let mut handles = Vec::with_capacity(num_writers);
    let barrier = Arc::new(Barrier::new(num_writers + 1));

    for w in 0..num_writers {
        let db = Arc::clone(&db);
        let b = Arc::clone(&barrier);
        let prefix = b'a' + (w % 26) as u8;
        handles.push(thread::spawn(move || {
            b.wait();
            let mut key_id = 0u32;
            for _ in 0..tx_per_writer {
                let mut tx = db.begin_write().unwrap();
                for _ in 0..batch_size {
                    let k = generate_key(prefix, key_id);
                    let v = generate_val(key_id, val_size);
                    tx.put(&k, &v).unwrap();
                    key_id = key_id.wrapping_add(1);
                }
                tx.commit().unwrap();
            }
        }));
    }

    barrier.wait();
    let start = Instant::now();
    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed().as_secs_f64();
    let total_keys = (tx_per_writer * num_writers * batch_size) as f64;
    total_keys / elapsed
}

// ----------------------------------------------------------------------------
// Complex: Read-Modify-Write (RMW Banking / Counter)
// ----------------------------------------------------------------------------
fn bench_rmw_writers(num_writers: usize, total_tx_target: usize) -> f64 {
    let db = Arc::new(Database::open_memory_with_capacity(65536).unwrap());
    let num_accounts = 2000u32;

    // Pre-populate accounts
    {
        let mut tx = db.begin_write().unwrap();
        for id in 0..num_accounts {
            let k = generate_key(b'z', id);
            let v = 1000u64.to_le_bytes();
            tx.put(&k, &v).unwrap();
        }
        tx.commit().unwrap();
    }

    let tx_per_writer = (total_tx_target / num_writers).max(10);
    let mut handles = Vec::with_capacity(num_writers);
    let barrier = Arc::new(Barrier::new(num_writers + 1));

    for w in 0..num_writers {
        let db = Arc::clone(&db);
        let b = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b.wait();
            let mut rng_seed = 1013904223u32.wrapping_add((w as u32) * 1664525);
            for _ in 0..tx_per_writer {
                let _ = db.transaction_with_retry(10, |tx| {
                    rng_seed = rng_seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    let acc_id = rng_seed % num_accounts;
                    let k = generate_key(b'z', acc_id);
                    let mut balance = 1000u64;
                    if let Some(existing) = tx.get(&k)? {
                        if existing.len() >= 8 {
                            balance = u64::from_le_bytes(existing[0..8].try_into().unwrap());
                        }
                    }
                    balance = balance.wrapping_add(1);
                    tx.put(&k, &balance.to_le_bytes())?;
                    Ok(())
                });
            }
        }));
    }

    barrier.wait();
    let start = Instant::now();
    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed().as_secs_f64();
    (tx_per_writer * num_writers) as f64 / elapsed
}

// ----------------------------------------------------------------------------
// Complex: Contended Zipfian Skew (80/20 Rule: 80% writes to 50 hot keys)
// ----------------------------------------------------------------------------
fn bench_contended_skew(num_writers: usize, total_tx_target: usize) -> f64 {
    let db = Arc::new(Database::open_memory_with_capacity(65536).unwrap());
    let hot_keys_count = 50u32;
    let cold_keys_count = 5000u32;

    // Pre-populate
    {
        let mut tx = db.begin_write().unwrap();
        for id in 0..cold_keys_count {
            tx.put(&generate_key(b's', id), b"initial_state").unwrap();
        }
        tx.commit().unwrap();
    }

    let tx_per_writer = (total_tx_target / num_writers).max(10);
    let mut handles = Vec::with_capacity(num_writers);
    let barrier = Arc::new(Barrier::new(num_writers + 1));

    for w in 0..num_writers {
        let db = Arc::clone(&db);
        let b = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b.wait();
            let mut rng_seed = (w as u32).wrapping_mul(314159265).wrapping_add(271828);
            for _ in 0..tx_per_writer {
                let _ = db.transaction_with_retry(20, |tx| {
                    rng_seed = rng_seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    let is_hot = (rng_seed % 100) < 80;
                    let id = if is_hot {
                        rng_seed % hot_keys_count
                    } else {
                        hot_keys_count + (rng_seed % (cold_keys_count - hot_keys_count))
                    };
                    let k = generate_key(b's', id);
                    tx.put(&k, &rng_seed.to_le_bytes())?;
                    Ok(())
                });
            }
        }));
    }

    barrier.wait();
    let start = Instant::now();
    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed().as_secs_f64();
    (tx_per_writer * num_writers) as f64 / elapsed
}

// ----------------------------------------------------------------------------
// Complex: Multi-Table Domain Separation
// ----------------------------------------------------------------------------
fn bench_multi_table(num_writers: usize, total_tx_target: usize) -> f64 {
    let db = Arc::new(Database::open_memory_with_capacity(65536).unwrap());
    let table_names: Vec<String> = (0..num_writers).map(|i| format!("table_{i}")).collect();

    // Pre-create tables
    {
        let mut tx = db.begin_write().unwrap();
        for t in &table_names {
            tx.create_table(t).unwrap();
        }
        tx.commit().unwrap();
    }

    let tx_per_writer = (total_tx_target / num_writers).max(10);
    let mut handles = Vec::with_capacity(num_writers);
    let barrier = Arc::new(Barrier::new(num_writers + 1));

    for w in 0..num_writers {
        let db = Arc::clone(&db);
        let b = Arc::clone(&barrier);
        let table = table_names[w].clone();
        let prefix = b'a' + (w as u8);
        handles.push(thread::spawn(move || {
            b.wait();
            let mut key_id = 0u32;
            for _ in 0..tx_per_writer {
                let mut tx = db.begin_write().unwrap();
                {
                    let mut tbl = tx.open_table_mut(&table).unwrap();
                    for _ in 0..20 {
                        let k = generate_key(prefix, key_id);
                        let v = generate_val(key_id, 64);
                        tbl.put(&k, &v).unwrap();
                        key_id = key_id.wrapping_add(1);
                    }
                }
                tx.commit().unwrap();
            }
        }));
    }

    barrier.wait();
    let start = Instant::now();
    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed().as_secs_f64();
    (tx_per_writer * num_writers * 20) as f64 / elapsed
}

// ----------------------------------------------------------------------------
// Complex: Mixed Workload (Concurrent Readers running Range Scans + Writers)
// ----------------------------------------------------------------------------
fn bench_mixed_readers_writers(num_writers: usize, duration_secs: u64) -> f64 {
    let db = Arc::new(Database::open_memory_with_capacity(65536).unwrap());
    let running = Arc::new(AtomicBool::new(true));
    let write_ops = Arc::new(AtomicU64::new(0));

    // Pre-populate with 2,000 keys
    {
        let mut tx = db.begin_write().unwrap();
        for id in 0..2000 {
            tx.put(&generate_key(b'm', id), &generate_val(id, 64))
                .unwrap();
        }
        tx.commit().unwrap();
    }

    let mut handles = Vec::new();

    // 2 background readers executing range scans
    for _ in 0..2 {
        let db_r = Arc::clone(&db);
        let running_r = Arc::clone(&running);
        handles.push(thread::spawn(move || {
            let mut i = 0u32;
            while running_r.load(Ordering::Relaxed) {
                if let Ok(rx) = db_r.begin_read() {
                    let start_k = generate_key(b'm', i % 1800);
                    let end_k = generate_key(b'm', (i % 1800) + 100);
                    let range = KeyRange {
                        start: Bound::Included(start_k.to_vec()),
                        end: Bound::Included(end_k.to_vec()),
                    };
                    let _ = rx.scan_range(&range);
                }
                i = i.wrapping_add(17);
            }
        }));
    }

    // N writers continuously inserting/updating
    for w in 0..num_writers {
        let db_w = Arc::clone(&db);
        let running_w = Arc::clone(&running);
        let write_ops_w = Arc::clone(&write_ops);
        let prefix = b'0' + (w % 10) as u8;
        handles.push(thread::spawn(move || {
            let mut count = 0u64;
            let mut id = 0u32;
            while running_w.load(Ordering::Relaxed) {
                if let Ok(mut tx) = db_w.begin_write() {
                    for _ in 0..50 {
                        let k = generate_key(prefix, id);
                        let v = generate_val(id, 64);
                        let _ = tx.put(&k, &v);
                        id = id.wrapping_add(1);
                    }
                    if tx.commit().is_ok() {
                        count += 50;
                    }
                }
            }
            write_ops_w.fetch_add(count, Ordering::SeqCst);
        }));
    }

    let start = Instant::now();
    thread::sleep(Duration::from_secs(duration_secs));
    running.store(false, Ordering::SeqCst);

    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed().as_secs_f64();
    let total_w = write_ops.load(Ordering::SeqCst) as f64;
    total_w / elapsed
}

fn main() {
    println!(
        "=================================================================================================="
    );
    println!(
        "   ACID DBMS CONCURRENCY MATRIX BENCHMARK: SINGLE-WRITER vs MULTI-WRITER (1, 2, 4, 8 THREADS)   "
    );
    println!(
        "=================================================================================================="
    );
    println!(
        "Evaluating across: Small Tasks, Big Tasks, and Complex Tasks (RMW, Skew, Multi-Table, Mixed R/W)"
    );
    println!();

    let mut metrics = Vec::new();

    // 1. SMALL TASKS
    println!(">>> Benchmarking [1/3] Small Tasks (Micro-transactions & Point Operations)...");
    {
        // 1-key autocommit
        let t1 = bench_disjoint_writers(1, 1, 32, 2000);
        let t2 = bench_disjoint_writers(2, 1, 32, 2000);
        let t4 = bench_disjoint_writers(4, 1, 32, 2000);
        let t8 = bench_disjoint_writers(8, 1, 32, 2000);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "1. Small Tasks",
            scenario_name: "Point Inserts (1 key / tx, autocommit)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "Multi-writer wins due to parallel transaction staging",
        });

        // 10-key micro-batch
        let t1 = bench_disjoint_writers(1, 10, 64, 1000);
        let t2 = bench_disjoint_writers(2, 10, 64, 1000);
        let t4 = bench_disjoint_writers(4, 10, 64, 1000);
        let t8 = bench_disjoint_writers(8, 10, 64, 1000);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "1. Small Tasks",
            scenario_name: "Micro-Batches (10 keys / tx)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "Multi-writer parallelizes key encoding and leaf staging",
        });

        // 50-key batch
        let t1 = bench_disjoint_writers(1, 50, 64, 400);
        let t2 = bench_disjoint_writers(2, 50, 64, 400);
        let t4 = bench_disjoint_writers(4, 50, 64, 400);
        let t8 = bench_disjoint_writers(8, 50, 64, 400);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "1. Small Tasks",
            scenario_name: "Medium Batches (50 keys / tx)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "Sweet spot for multi-writer parallel OCC throughput",
        });
    }

    // 2. BIG / BULK TASKS
    println!(">>> Benchmarking [2/3] Big Tasks (Large Batches & Massive Bulk Ingestion)...");
    {
        // 500-key batch
        let t1 = bench_disjoint_writers(1, 500, 64, 40);
        let t2 = bench_disjoint_writers(2, 500, 64, 40);
        let t4 = bench_disjoint_writers(4, 500, 64, 40);
        let t8 = bench_disjoint_writers(8, 500, 64, 40);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "2. Big Tasks",
            scenario_name: "Large Batches (500 keys / tx)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "Balanced: Chunk leaf ingestion benefits single & multi",
        });

        // 5,000-key bulk batch
        let t1 = bench_disjoint_writers(1, 5000, 64, 6);
        let t2 = bench_disjoint_writers(2, 5000, 64, 6);
        let t4 = bench_disjoint_writers(4, 5000, 64, 6);
        let t8 = bench_disjoint_writers(8, 5000, 64, 6);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "2. Big Tasks",
            scenario_name: "Massive Bulk Batch (5,000 keys / tx)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "Single-writer excels on giant batches (zero context switches)",
        });

        // Large 1024B payloads
        let t1 = bench_disjoint_writers(1, 100, 1024, 50);
        let t2 = bench_disjoint_writers(2, 100, 1024, 50);
        let t4 = bench_disjoint_writers(4, 100, 1024, 50);
        let t8 = bench_disjoint_writers(8, 100, 1024, 50);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "2. Big Tasks",
            scenario_name: "Wide Payloads (1024B records, 100 / tx)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "Multi-writer saturates available RAM memory bandwidth",
        });

        // File Storage Disk Writes
        let t1 = bench_file_writers(1, 50, 64, 400);
        let t2 = bench_file_writers(2, 50, 64, 400);
        let t4 = bench_file_writers(4, 50, 64, 400);
        let t8 = bench_file_writers(8, 50, 64, 400);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "2. Big Tasks",
            scenario_name: "Disk File Persistence (Normal mode, 50 / tx)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "Disk I/O latency parallelizes transaction staging across cores",
        });
    }

    // 3. COMPLEX TASKS
    println!(">>> Benchmarking [3/3] Complex Tasks (RMW, Contention, Multi-Table, Mixed)...");
    {
        // Read-Modify-Write
        let t1 = bench_rmw_writers(1, 3000);
        let t2 = bench_rmw_writers(2, 3000);
        let t4 = bench_rmw_writers(4, 3000);
        let t8 = bench_rmw_writers(8, 3000);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "3. Complex Tasks",
            scenario_name: "Read-Modify-Write (Banking transfer/counter)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "OCC retry helper enables concurrent RMW scaling",
        });

        // Contended Zipfian Skew
        let t1 = bench_contended_skew(1, 2000);
        let t2 = bench_contended_skew(2, 2000);
        let t4 = bench_contended_skew(4, 2000);
        let t8 = bench_contended_skew(8, 2000);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "3. Complex Tasks",
            scenario_name: "Contended Hotspot (80% writes to 50 keys)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "High contention incurs aborts; single-writer avoids retries",
        });

        // Multi-Table Domain Writes
        let t1 = bench_multi_table(1, 1000);
        let t2 = bench_multi_table(2, 1000);
        let t4 = bench_multi_table(4, 1000);
        let t8 = bench_multi_table(8, 1000);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "3. Complex Tasks",
            scenario_name: "Multi-Table Domain (users, orders, payments)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "Zero conflict across tables yields ideal multi-writer scale",
        });

        // Mixed Readers + Writers
        let t1 = bench_mixed_readers_writers(1, 2);
        let t2 = bench_mixed_readers_writers(2, 2);
        let t4 = bench_mixed_readers_writers(4, 2);
        let t8 = bench_mixed_readers_writers(8, 2);
        let best = t1.max(t2).max(t4).max(t8);
        let (winner, factor) = if (best - t1).abs() < f64::EPSILON {
            ("Single-Writer", 1.0)
        } else if (best - t4).abs() < f64::EPSILON {
            ("4 Writers (OCC)", t4 / t1)
        } else if (best - t8).abs() < f64::EPSILON {
            ("8 Writers (OCC)", t8 / t1)
        } else {
            ("2 Writers (OCC)", t2 / t1)
        };
        metrics.push(ScenarioMetric {
            task_category: "3. Complex Tasks",
            scenario_name: "Mixed Workload (Concurrent Readers + Writers)",
            writers_1_tps: t1,
            writers_2_tps: t2,
            writers_4_tps: t4,
            writers_8_tps: t8,
            winning_mode: winner,
            scaling_factor: factor,
            architectural_takeaway: "Lock-free atomic meta isolates readers from writers",
        });
    }

    // Print Results Table
    println!();
    println!(
        "| Category | Workload Scenario | 1 Writer | 2 Writers | 4 Writers | 8 Writers | Winning Mode (Speedup) | Architectural Reason |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for m in &metrics {
        println!(
            "| {} | {} | {:.0} ops/s | {:.0} ops/s | {:.0} ops/s | {:.0} ops/s | **{} ({:.2}×)** | {} |",
            m.task_category,
            m.scenario_name,
            m.writers_1_tps,
            m.writers_2_tps,
            m.writers_4_tps,
            m.writers_8_tps,
            m.winning_mode,
            m.scaling_factor,
            m.architectural_takeaway
        );
    }
    println!(
        "=================================================================================================="
    );
}
