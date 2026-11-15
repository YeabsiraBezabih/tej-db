#![allow(
    clippy::cast_lossless,
    clippy::cast_precision_loss,
    clippy::uninlined_format_args,
    clippy::unwrap_used
)]

use roaring::RoaringBitmap;
use std::collections::HashSet;
use std::time::Instant;
use tej_db::{Database, DistanceMetric, QuantizationPrecision, VectorId};
use tej_db_sql::hybrid::{brute_force_simd_scan, choose_hybrid_strategy};
use tej_db_vector::RaBitQQuantizer;
use tej_db_vector::simd::{
    cosine_distance, l2_distance_squared, rabitq_asymmetric_dot_product, rabitq_hamming_distance,
};

fn main() {
    println!("================================================================================");
    println!(
        "              TejDb Native Vector Engine Benchmark Suite (tej-db-vector)            "
    );
    println!("================================================================================");

    bench_simd_distance_kernels();
    bench_rabitq_quantization();
    bench_delta_buffer_ingest();
    bench_delta_compaction_and_search();
    bench_hybrid_prefiltering();
    run_scale_and_recall_benchmark(
        "--- 6. Scale & Recall@10 Benchmark (10,000 vectors, 128-dim, 2-bit RaBitQ) ---",
        128,
        10_000,
        QuantizationPrecision::TwoBit,
        &[64, 128, 256, 384],
        NormalizationMode::None,
    );
    run_scale_and_recall_benchmark(
        "--- 7. Scale & Recall@10 Benchmark (2,500 vectors, 768-dim, Real Embedding Size, 1-bit RaBitQ) ---",
        768,
        2_500,
        QuantizationPrecision::OneBit,
        &[32, 64, 100, 128],
        NormalizationMode::UnitL2,
    );

    println!("================================================================================");
    println!("                          Vector Benchmark Suite Complete                       ");
    println!("================================================================================");
}

fn bench_simd_distance_kernels() {
    println!("\n--- 1. SIMD Distance Kernel Micro-Benchmarks (100,000 iterations, 768-dim) ---");
    let dim = 768;
    let v1 = vec![0.03f32; dim];
    let v2 = vec![0.04f32; dim];

    let quantizer = RaBitQQuantizer::new(dim, 42);
    let (code1, alpha1, beta1, _) = quantizer.quantize(&v1);
    let (code2, _, _, _) = quantizer.quantize(&v2);
    let rot_q = quantizer.rotate(&v1);

    let n = 100_000;

    // Full f32 L2 Distance
    let start = Instant::now();
    let mut accum_l2 = 0.0f32;
    for _ in 0..n {
        accum_l2 += l2_distance_squared(&v1, &v2);
    }
    let elapsed = start.elapsed();
    let ops = (n as f64) / elapsed.as_secs_f64();
    println!(
        "  Full-Precision f32 L2 Distance:       {:>10.0} ops/s  ({:.2} µs/op, accum: {:.1})",
        ops,
        (elapsed.as_secs_f64() * 1e6) / (n as f64),
        accum_l2
    );

    // Full f32 Cosine Distance
    let start = Instant::now();
    let mut accum_cos = 0.0f32;
    for _ in 0..n {
        accum_cos += cosine_distance(&v1, &v2);
    }
    let elapsed = start.elapsed();
    let ops = (n as f64) / elapsed.as_secs_f64();
    println!(
        "  Full-Precision f32 Cosine Distance:   {:>10.0} ops/s  ({:.2} µs/op, accum: {:.1})",
        ops,
        (elapsed.as_secs_f64() * 1e6) / (n as f64),
        accum_cos
    );

    // RaBitQ Asymmetric Dot Product
    let start = Instant::now();
    let mut accum_asym = 0.0f32;
    for _ in 0..n {
        accum_asym += rabitq_asymmetric_dot_product(&rot_q, &code1, alpha1, beta1);
    }
    let elapsed = start.elapsed();
    let ops = (n as f64) / elapsed.as_secs_f64();
    println!(
        "  RaBitQ Asymmetric Dot Product:        {:>10.0} ops/s  ({:.2} µs/op, accum: {:.1})",
        ops,
        (elapsed.as_secs_f64() * 1e6) / (n as f64),
        accum_asym
    );

    // RaBitQ Binary Popcount Hamming
    let start = Instant::now();
    let mut accum_ham = 0u32;
    for _ in 0..n {
        accum_ham = accum_ham.wrapping_add(rabitq_hamming_distance(&code1, &code2));
    }
    let elapsed = start.elapsed();
    let ops = (n as f64) / elapsed.as_secs_f64();
    println!(
        "  RaBitQ Binary Popcount Hamming:       {:>10.0} ops/s  ({:.2} µs/op, accum: {})",
        ops,
        (elapsed.as_secs_f64() * 1e6) / (n as f64),
        accum_ham
    );
}

fn bench_rabitq_quantization() {
    println!("\n--- 2. RaBitQ 1-Bit Binary Quantization Ingestion Throughput ---");
    let test_dims = [128, 768, 1536];
    let n = 5_000;

    for &dim in &test_dims {
        let quantizer = RaBitQQuantizer::new(dim, 42);
        let sample = vec![0.02f32; dim];

        let start = Instant::now();
        for _ in 0..n {
            let _ = quantizer.quantize(&sample);
        }
        let elapsed = start.elapsed();
        let ops = (n as f64) / elapsed.as_secs_f64();
        let compressed_bytes = dim / 8 + 12; // 1 bit/dim + 3 f32 factors
        let raw_bytes = dim * 4;
        let ratio = (raw_bytes as f64) / (compressed_bytes as f64);
        println!(
            "  Dim {:>4}: {:>9.0} vectors/s | Raw: {:>5} B -> Quantized: {:>4} B ({:.1}× compression)",
            dim, ops, raw_bytes, compressed_bytes, ratio
        );
    }
}

fn bench_delta_buffer_ingest() {
    println!("\n--- 3. FreshDiskANN ACID Delta Buffer Ingest Rate (64-dim, Batch size 100) ---");
    let db = Database::open_memory().unwrap();
    let idx = db.create_vector_index("vectors_delta", 64, DistanceMetric::Cosine);

    let total_vectors = 10_000u64;
    let batch_size = 100u64;
    let sample = vec![0.05f32; 64];

    let start = Instant::now();
    for batch_start in (1..=total_vectors).step_by(batch_size as usize) {
        let mut tx = db.begin_write().unwrap();
        for i in 0..batch_size {
            let id = VectorId(batch_start + i);
            idx.insert(&mut tx, id, &sample).unwrap();
        }
        tx.commit().unwrap();
    }
    let elapsed = start.elapsed();
    let ops = (total_vectors as f64) / elapsed.as_secs_f64();
    println!(
        "  Delta Ingestion Rate:                 {:>10.0} vectors/s  ({:.2} ms total for {} vectors)",
        ops,
        elapsed.as_secs_f64() * 1e3,
        total_vectors
    );
}

fn bench_delta_compaction_and_search() {
    println!("\n--- 4. Vamana Graph Compaction & Beam Search (64-dim, 1,000 vectors) ---");
    let db = Database::open_memory().unwrap();
    let idx = db.create_vector_index("vectors_graph", 64, DistanceMetric::L2);

    let count = 1_000u64;
    {
        let mut tx = db.begin_write().unwrap();
        for i in 1..=count {
            let mut v = vec![0.0f32; 64];
            #[allow(clippy::cast_precision_loss)]
            {
                v[0] = (i % 50) as f32;
                v[1] = ((i / 50) % 50) as f32;
            }
            idx.insert(&mut tx, VectorId(i), &v).unwrap();
        }
        tx.commit().unwrap();
    }

    // Measure compaction time
    let start_compact = Instant::now();
    let root = idx.compact().unwrap();
    let compact_elapsed = start_compact.elapsed();
    assert!(root.is_valid());
    println!(
        "  Vamana Delta Compaction Time:         {:.2} ms  ({:.0} vectors/s)",
        compact_elapsed.as_secs_f64() * 1e3,
        (count as f64) / compact_elapsed.as_secs_f64()
    );

    // Measure beam search QPS at different beam widths
    let rx = db.begin_read().unwrap();
    let query = vec![25.0f32; 64];
    let beam_widths = [16, 32, 64];
    let num_queries = 2_000;

    for &beam in &beam_widths {
        let start_search = Instant::now();
        for _ in 0..num_queries {
            let _ = idx.search(&rx, &query, beam, 10, None).unwrap();
        }
        let search_elapsed = start_search.elapsed();
        let qps = (num_queries as f64) / search_elapsed.as_secs_f64();
        let lat_us = (search_elapsed.as_secs_f64() * 1e6) / (num_queries as f64);
        println!(
            "  Beam Search (beam={:>2}, top_k=10):    {:>10.0} QPS   ({:.2} µs/query)",
            beam, qps, lat_us
        );
    }
}

fn bench_hybrid_prefiltering() {
    println!("\n--- 5. Hybrid Search: Roaring Bitmap Pre-filtering vs Brute-force SIMD Scan ---");
    let total_rows = 10_000;
    let query = vec![1.0f32; 32];
    let sample_vec = vec![0.5f32; 32];

    let dataset: Vec<(VectorId, &[f32])> = (1..=total_rows)
        .map(|i| (VectorId(i), sample_vec.as_slice()))
        .collect();

    // Scenario A: 1% Selectivity (100 matches out of 10,000) -> BruteForceScan
    let mut bitmap_1pct = RoaringBitmap::new();
    for i in (1..=total_rows).step_by(100) {
        #[allow(clippy::cast_possible_truncation)]
        bitmap_1pct.insert(i as u32);
    }
    let strategy_1pct = choose_hybrid_strategy(bitmap_1pct.len(), total_rows, 10);

    let start = Instant::now();
    let iters = 1_000;
    for _ in 0..iters {
        let filtered: Vec<(VectorId, &[f32])> = dataset
            .iter()
            .copied()
            .filter(|(id, _)| bitmap_1pct.contains(id.0 as u32))
            .collect();
        let _ = brute_force_simd_scan(&filtered, &query, DistanceMetric::L2, 10).unwrap();
    }
    let elapsed = start.elapsed();
    let qps_1pct = (iters as f64) / elapsed.as_secs_f64();
    println!(
        "  1% Selectivity (100/10k rows, {:?}):  {:>10.0} QPS  ({:.2} µs/query)",
        strategy_1pct,
        qps_1pct,
        (elapsed.as_secs_f64() * 1e6) / (iters as f64)
    );

    // Scenario B: 50% Selectivity (5,000 matches out of 10,000) -> VamanaPreFilter
    let mut bitmap_50pct = RoaringBitmap::new();
    for i in (1..=total_rows).step_by(2) {
        #[allow(clippy::cast_possible_truncation)]
        bitmap_50pct.insert(i as u32);
    }
    let strategy_50pct = choose_hybrid_strategy(bitmap_50pct.len(), total_rows, 10);

    // Build and compact Vamana graph to benchmark true graph-based pre-filtering
    let db = Database::open_memory().unwrap();
    let idx = db.create_vector_index("hybrid_50pct", 32, DistanceMetric::L2);
    let mut tx = db.begin_write().unwrap();
    for &(id, v) in &dataset {
        idx.insert(&mut tx, id, v).unwrap();
    }
    tx.commit().unwrap();
    let _ = idx.compact().unwrap();

    let rx = db.begin_read().unwrap();
    let filter_fn = |id: VectorId| bitmap_50pct.contains(id.0 as u32);

    let start = Instant::now();
    for _ in 0..iters {
        let _ = idx.search(&rx, &query, 32, 10, Some(&filter_fn)).unwrap();
    }
    let elapsed = start.elapsed();
    let qps_50pct = (iters as f64) / elapsed.as_secs_f64();
    println!(
        "  50% Selectivity (5k/10k rows, {:?}): {:>10.0} QPS  ({:.2} µs/query)",
        strategy_50pct,
        qps_50pct,
        (elapsed.as_secs_f64() * 1e6) / (iters as f64)
    );
}

struct SimpleRng(u64);
impl SimpleRng {
    fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((self.0 >> 33) as f32) / (u32::MAX as f32)
    }

    fn next_gaussian(&mut self, mean: f32, std: f32) -> f32 {
        let u1 = self.next_f32().max(1e-6);
        let u2 = self.next_f32();
        let z0 = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos();
        mean + z0 * std
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NormalizationMode {
    None,
    UnitL2,
}

fn run_scale_and_recall_benchmark(
    section_title: &str,
    dim: usize,
    n_vectors: u64,
    precision: QuantizationPrecision,
    beam_widths: &[usize],
    norm_mode: NormalizationMode,
) {
    println!("\n{}", section_title);
    let n_queries = 100;

    let mut rng = SimpleRng(12345);
    let n_clusters = 50;
    let mut centroids = Vec::with_capacity(n_clusters);
    for _ in 0..n_clusters {
        let mut c = Vec::with_capacity(dim);
        for _ in 0..dim {
            let val = match norm_mode {
                NormalizationMode::UnitL2 => rng.next_f32() * 2.0 - 1.0,
                NormalizationMode::None => rng.next_f32(),
            };
            c.push(val);
        }
        centroids.push(c);
    }

    let noise_std = match norm_mode {
        NormalizationMode::UnitL2 => 0.08,
        NormalizationMode::None => 0.15,
    };
    let mut dataset: Vec<(VectorId, Vec<f32>)> = Vec::with_capacity(n_vectors as usize);
    for i in 1..=n_vectors {
        let centroid = &centroids[(i as usize) % n_clusters];
        let mut v = Vec::with_capacity(dim);
        for &c in centroid {
            v.push(c + rng.next_gaussian(0.0, noise_std));
        }
        if norm_mode == NormalizationMode::UnitL2 {
            let norm = (v.iter().map(|x| x * x).sum::<f32>()).sqrt().max(1e-6);
            for x in &mut v {
                *x /= norm;
            }
        }
        dataset.push((VectorId(i), v));
    }

    let mut queries = Vec::with_capacity(n_queries);
    for i in 0..n_queries {
        let centroid = &centroids[i % n_clusters];
        let mut q = Vec::with_capacity(dim);
        for &c in centroid {
            q.push(c + rng.next_gaussian(0.0, noise_std));
        }
        if norm_mode == NormalizationMode::UnitL2 {
            let norm = (q.iter().map(|x| x * x).sum::<f32>()).sqrt().max(1e-6);
            for x in &mut q {
                *x /= norm;
            }
        }
        queries.push(q);
    }

    // Ingest into DiskANN Delta Buffer
    let db = Database::open_memory().unwrap();
    let idx = db.create_vector_index_with_precision(
        "scale_vec_index",
        dim,
        DistanceMetric::L2,
        precision,
    );

    let batch_size = 500;
    let start_ingest = Instant::now();
    for chunk in dataset.chunks(batch_size) {
        let mut tx = db.begin_write().unwrap();
        for (id, vec) in chunk {
            idx.insert(&mut tx, *id, vec).unwrap();
        }
        tx.commit().unwrap();
    }
    let ingest_elapsed = start_ingest.elapsed();
    println!(
        "  Delta Ingestion:                      {:.2} ms ({:.0} vectors/s)",
        ingest_elapsed.as_secs_f64() * 1e3,
        (n_vectors as f64) / ingest_elapsed.as_secs_f64()
    );

    // Compact into 2-Pass Vamana Graph & Packed Pages
    let start_compact = Instant::now();
    let root = idx.compact().unwrap();
    let compact_elapsed = start_compact.elapsed();
    assert!(root.is_valid());
    println!(
        "  Vamana Graph Compaction (2-Pass):     {:.2} s ({:.0} vectors/s)",
        compact_elapsed.as_secs_f64(),
        (n_vectors as f64) / compact_elapsed.as_secs_f64()
    );

    // Compute exact brute-force ground truth top-10 for each query
    println!(
        "  Computing exact brute-force ground truth for {} queries...",
        n_queries
    );
    let mut ground_truths: Vec<HashSet<VectorId>> = Vec::with_capacity(n_queries);
    for q in &queries {
        let mut scored: Vec<(VectorId, f32)> = dataset
            .iter()
            .map(|(id, v)| (*id, l2_distance_squared(q, v)))
            .collect();
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let top10: HashSet<VectorId> = scored.iter().take(10).map(|(id, _)| *id).collect();
        ground_truths.push(top10);
    }

    // Evaluate Recall@10 and QPS across beam widths with full-precision reranking
    let rx = db.begin_read().unwrap();

    println!(
        "\n  (Note: In-memory f32 reranking over top candidates to isolate pure graph recall fidelity)"
    );
    println!("  +------------+-------------+------------+--------------------+");
    println!("  | Beam Width |  Recall@10  | Search QPS | Latency (µs/query) |");
    println!("  +------------+-------------+------------+--------------------+");

    for &beam in beam_widths {
        let start_eval = Instant::now();
        let mut total_hits = 0;
        let iters_per_query = 5;
        let rerank_k = 300.max(beam);

        for _ in 0..iters_per_query {
            for (q_idx, q) in queries.iter().enumerate() {
                let top = idx
                    .search_and_rerank(&rx, q, beam, 10, rerank_k, None, |id| {
                        let idx = (id.0 - 1) as usize;
                        Ok(dataset.get(idx).map(|item| item.1.as_slice()))
                    })
                    .unwrap();
                let retrieved: HashSet<VectorId> = top.iter().map(|c| c.vector_id).collect();
                total_hits += retrieved.intersection(&ground_truths[q_idx]).count();
            }
        }
        let total_queries = (n_queries * iters_per_query) as f64;
        let elapsed = start_eval.elapsed();
        let recall = (total_hits as f64) / (total_queries * 10.0);
        let qps = total_queries / elapsed.as_secs_f64();
        let lat_us = (elapsed.as_secs_f64() * 1e6) / total_queries;

        println!(
            "  |   L = {:<4} |   {:>6.2}%   | {:>8.0}   | {:>14.2} µs  |",
            beam,
            recall * 100.0,
            qps,
            lat_us
        );
    }
    println!("  +------------+-------------+------------+--------------------+");
}
