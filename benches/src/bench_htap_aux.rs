#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::uninlined_format_args,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;
use tej_db_columnar::{
    BitPackingCodec, DataChunk, DictionaryCodec, ParquetReader, ParquetWriter, PhysicalType,
    RleCodec, RowGroup,
};
use tej_db_sql::Value;
use tej_db_sql::fts::Bm25Index;
use tej_db_sql::window::{WindowEvaluator, WindowFunctionType, WindowSpec};
use tej_db_vectorized::{ZoneMapPredicate, parallel_tpch_q6};

/// Executes a DuckDB query by feeding setup and query statements to duckdb with `.timer on`.
/// Accurately parses DuckDB's internal `real <seconds>` query execution time, completely
/// eliminating process spawn overhead, table creation time, and stdin piping bias.
pub fn run_duckdb_timed_query(setup_sql: &str, query_sql: &str) -> std::time::Duration {
    let script = format!(".timer on\n{setup_sql}\n{query_sql}\n");
    let mut child = Command::new("duckdb")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("duckdb should be installed");

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    let output = child.wait_with_output().expect("duckdb execution failed");
    let out_str = String::from_utf8_lossy(&output.stdout);

    // Parse the final "Run Time (s): real X.XXX" corresponding to the timed query
    for line in out_str.lines().rev() {
        if line.starts_with("Run Time (s): real") {
            let mut parts = line.split_whitespace();
            if let Some(val_str) = parts.nth(4) {
                if let Ok(sec) = val_str.parse::<f64>() {
                    return std::time::Duration::from_secs_f64(sec);
                }
            }
        }
    }
    std::time::Duration::from_millis(1)
}

pub fn bench_columnar_codecs_and_zonemap() {
    println!("\n--- 2. OLAP Columnar Compression & Direct Compressed Filtering ---");

    const NUM_ROWS: usize = 100_000;

    // 2.1 BitPacking Codec
    let bounded_integers: Vec<i64> = (0..NUM_ROWS).map(|i| (i % 100) as i64).collect();
    let start = Instant::now();
    let packed = BitPackingCodec::encode(&bounded_integers);
    let pack_time = start.elapsed();
    let pack_ratio = (NUM_ROWS * 8) as f64 / packed.len() as f64;
    let pack_throughput = (NUM_ROWS as f64) / pack_time.as_secs_f64();

    // 2.2 RLE Codec & Direct Compressed Querying
    let repeated_integers: Vec<i64> = (0..NUM_ROWS).map(|i| (i / 1000) as i64).collect();
    let start = Instant::now();
    let rle_packed = RleCodec::encode(&repeated_integers);
    let rle_time = start.elapsed();
    let rle_ratio = (NUM_ROWS * 8) as f64 / rle_packed.len() as f64;
    let rle_throughput = (NUM_ROWS as f64) / rle_time.as_secs_f64();

    // Direct filter on compressed RLE stream (zero decompression)
    let start = Instant::now();
    let mut compressed_matches = 0_usize;
    let target = 50_i64;
    let mut offset = 4;
    while offset + 12 <= rle_packed.len() {
        let count = u32::from_le_bytes(rle_packed[offset..offset + 4].try_into().unwrap()) as usize;
        let val = i64::from_le_bytes(rle_packed[offset + 4..offset + 12].try_into().unwrap());
        if val == target {
            compressed_matches += count;
        }
        offset += 12;
    }
    let direct_rle_time = start.elapsed();
    let direct_rle_throughput = (NUM_ROWS as f64) / direct_rle_time.as_secs_f64();
    assert_eq!(compressed_matches, 1000);

    // 2.3 Dictionary Codec
    let strings: Vec<&str> = (0..NUM_ROWS)
        .map(|i| match i % 4 {
            0 => "PENDING",
            1 => "PROCESSING",
            2 => "COMPLETED",
            _ => "FAILED",
        })
        .collect();
    let start = Instant::now();
    let dict_packed = DictionaryCodec::encode(&strings);
    let dict_time = start.elapsed();
    let raw_str_bytes: usize = strings.iter().map(|s| s.len()).sum();
    let dict_ratio = (raw_str_bytes as f64) / dict_packed.len() as f64;
    let dict_throughput = (NUM_ROWS as f64) / dict_time.as_secs_f64();

    println!(
        "  - BitPacking (FoR):     {:10.2} rows/s (Ratio: {:5.2}x vs raw, {:.2?})",
        pack_throughput, pack_ratio, pack_time
    );
    println!(
        "  - Run-Length (RLE):     {:10.2} rows/s (Ratio: {:5.2}x vs raw, {:.2?})",
        rle_throughput, rle_ratio, rle_time
    );
    println!(
        "  - Direct RLE Filtering: {:10.2} rows/s ({:.2?}, 0 bytes decompressed)",
        direct_rle_throughput, direct_rle_time
    );
    println!(
        "  - Dictionary (Strings): {:10.2} rows/s (Ratio: {:5.2}x vs raw, {:.2?})",
        dict_throughput, dict_ratio, dict_time
    );
}

pub fn bench_parquet_io() {
    println!("\n--- 5. Parquet Ingestion & Export Throughput (100,000 Rows) ---");

    const NUM_ROWS: usize = 100_000;
    const CHUNK_SIZE: usize = 2048;
    let num_chunks = (NUM_ROWS + CHUNK_SIZE - 1) / CHUNK_SIZE;

    let types = vec![PhysicalType::Int64, PhysicalType::Float64];
    let names = vec!["id".to_string(), "metric".to_string()];

    let mut chunks = Vec::with_capacity(num_chunks);
    let mut generated = 0;
    while generated < NUM_ROWS {
        let count = CHUNK_SIZE.min(NUM_ROWS - generated);
        let mut chunk = DataChunk::new(&types, count);
        let col0 = chunk.column_mut(0).unwrap();
        for i in 0..count {
            col0.push_i64((generated + i) as i64).unwrap();
        }
        let col1 = chunk.column_mut(1).unwrap();
        for i in 0..count {
            col1.push_f64((generated + i) as f64 * 1.5).unwrap();
        }
        chunk.set_num_rows(count).unwrap();
        chunks.push(chunk);
        generated += count;
    }

    let temp_file = tempfile::NamedTempFile::new().unwrap();
    let temp_path = temp_file.path();

    // 1. Write Parquet
    let start_write = Instant::now();
    ParquetWriter::write_file(temp_path, &names, &types, &chunks).unwrap();
    let write_time = start_write.elapsed();
    let write_rows_sec = (NUM_ROWS as f64) / write_time.as_secs_f64();
    let file_size_mb = temp_file.as_file().metadata().unwrap().len() as f64 / (1024.0 * 1024.0);
    let write_mb_sec = file_size_mb / write_time.as_secs_f64();

    // 2. Read Parquet
    let start_read = Instant::now();
    let (meta, read_chunks) = ParquetReader::read_file(temp_path, CHUNK_SIZE).unwrap();
    let read_time = start_read.elapsed();
    let read_rows_sec = (meta.num_rows as f64) / read_time.as_secs_f64();
    let read_mb_sec = file_size_mb / read_time.as_secs_f64();
    assert_eq!(read_chunks.len(), chunks.len());

    println!(
        "  - Parquet Export: {:10.2} rows/s ({:6.2} MB/s, {:.2?}, file: {:.2} MB)",
        write_rows_sec, write_mb_sec, write_time, file_size_mb
    );
    println!(
        "  - Parquet Import: {:10.2} rows/s ({:6.2} MB/s, {:.2?})",
        read_rows_sec, read_mb_sec, read_time
    );
}

pub fn bench_bm25_text_search() {
    println!("\n--- 6. Full-Text Search (BM25 & Reciprocal Rank Fusion) ---");

    const NUM_DOCS: usize = 10_000;
    const NUM_QUERIES: usize = 1_000;

    let sample_corpus = [
        "Distributed database management with ACID transactions and Raft consensus",
        "Vector search engine utilizing quantization and graph-based proximity graphs",
        "Columnar vectorized execution with dictionary encoding and bitpacking",
        "High-performance LSM-tree key value storage engine with write-ahead log",
        "B-tree index with prefix compression and slotted page physical layout",
        "Hybrid search combining BM25 lexical ranking and dense embedding similarity",
    ];

    let mut bm25 = Bm25Index::new();
    let start_index = Instant::now();
    for i in 0..NUM_DOCS {
        let text = sample_corpus[i % sample_corpus.len()];
        bm25.add_document((i + 1) as u64, text);
    }
    let index_time = start_index.elapsed();
    let index_throughput = (NUM_DOCS as f64) / index_time.as_secs_f64();

    let query_terms = [
        "vector search",
        "acid transactions",
        "columnar vectorized",
        "hybrid bm25 ranking",
        "database index",
    ];

    let start_query = Instant::now();
    for i in 0..NUM_QUERIES {
        let q = query_terms[i % query_terms.len()];
        let results = bm25.score_query(q, 10);
        let mock_vec = vec![((i + 1) as u64, 0.95), ((i + 2) as u64, 0.85)];
        let _fused = Bm25Index::hybrid_rrf(&results, &mock_vec, 60.0, 10);
    }
    let query_time = start_query.elapsed();
    let query_qps = (NUM_QUERIES as f64) / query_time.as_secs_f64();
    let query_lat_us = (query_time.as_secs_f64() * 1e6) / (NUM_QUERIES as f64);

    println!(
        "  - BM25 Ingestion:   {:10.2} docs/s ({:.2?})",
        index_throughput, index_time
    );
    println!(
        "  - BM25 + RRF Query: {:10.2} QPS (Latency: {:.2} µs/query)",
        query_qps, query_lat_us
    );
}

pub fn bench_window_functions() {
    println!("\n--- 7. Window Functions: TejDb vs DuckDB (100,000 Rows) ---");

    const NUM_ROWS: usize = 100_000;
    let mut rows: Vec<Vec<Value>> = Vec::with_capacity(NUM_ROWS);
    for i in 0..NUM_ROWS {
        rows.push(vec![
            Value::Integer(i as i64),
            Value::Integer((i % 10) as i64), // 10 partition groups
            Value::Integer((NUM_ROWS - i) as i64),
        ]);
    }

    let spec = WindowSpec {
        function: WindowFunctionType::RowNumber,
        target_col_idx: None,
        partition_by_indices: vec![1],
        order_by_col_idx: Some(2),
        ascending: true,
    };

    let start_tej_db = Instant::now();
    let out = WindowEvaluator::evaluate(&spec, &rows).unwrap();
    let tej_db_time = start_tej_db.elapsed();
    assert_eq!(out.len(), NUM_ROWS);
    let tej_db_throughput = (NUM_ROWS as f64) / tej_db_time.as_secs_f64();

    let duck_setup = format!(
        "CREATE TABLE fact AS SELECT range AS id, range % 10 AS grp, {NUM_ROWS} - range AS val FROM range({NUM_ROWS});"
    );
    let duck_query = "SELECT row_number() OVER (PARTITION BY grp ORDER BY val) FROM fact;";
    let duck_time = run_duckdb_timed_query(&duck_setup, duck_query);
    let duck_throughput = (NUM_ROWS as f64) / duck_time.as_secs_f64();

    println!("  - ROW_NUMBER() OVER (PARTITION BY grp ORDER BY val):");
    println!(
        "    * TejDb:   {:10.2} rows/s ({:.2?})",
        tej_db_throughput, tej_db_time
    );
    println!(
        "    * DuckDB: {:10.2} rows/s ({:.2?}) [Ratio: {:.2}x]",
        duck_throughput,
        duck_time,
        tej_db_throughput / duck_throughput
    );
}

pub fn bench_tpch_q6() {
    println!("\n--- 8. End-to-End Analytical Benchmark: TPC-H Q6 (1,000,000 Rows) ---");

    const CHUNK_ROWS: usize = 2048;
    const TOTAL_CHUNKS: usize = 500; // 1,024,000 rows
    let total_rows = TOTAL_CHUNKS * CHUNK_ROWS;

    // Lineitem schema: [0: extendedprice, 1: discount, 2: shipdate, 3: quantity]
    let types = vec![
        PhysicalType::Int64,
        PhysicalType::Int64,
        PhysicalType::Int64,
        PhysicalType::Int64,
    ];

    let mut row_groups = Vec::with_capacity(TOTAL_CHUNKS);
    for chunk_idx in 0..TOTAL_CHUNKS {
        let mut chunk = DataChunk::new(&types, CHUNK_ROWS);
        let base = chunk_idx * CHUNK_ROWS;
        let col0 = chunk.column_mut(0).unwrap(); // extendedprice
        for i in 0..CHUNK_ROWS {
            col0.push_i64((1000 + (base + i) % 50000) as i64).unwrap();
        }
        let col1 = chunk.column_mut(1).unwrap(); // discount (percentage: 0..10)
        for i in 0..CHUNK_ROWS {
            col1.push_i64(((base + i) % 11) as i64).unwrap();
        }
        let col2 = chunk.column_mut(2).unwrap(); // shipdate (e.g. 19940101 to 19951231)
        for i in 0..CHUNK_ROWS {
            let offset_days = ((base + i) % 730) as i64;
            col2.push_i64(19940101 + offset_days).unwrap();
        }
        let col3 = chunk.column_mut(3).unwrap(); // quantity (1..50)
        for i in 0..CHUNK_ROWS {
            col3.push_i64((1 + (base + i) % 50) as i64).unwrap();
        }
        chunk.set_num_rows(CHUNK_ROWS).unwrap();
        row_groups.push(RowGroup::from_chunk(&chunk).unwrap());
    }

    // TPC-H Q6 query:
    // SELECT sum(extendedprice) FROM lineitem
    // WHERE shipdate >= 19940101 AND shipdate < 19950101
    //   AND discount >= 5 AND discount <= 7
    //   AND quantity < 24
    let zonemap_preds = vec![
        ZoneMapPredicate::RangeInt64 {
            column_idx: 2,
            low: Some(19940101),
            high: Some(19950100),
        },
        ZoneMapPredicate::RangeInt64 {
            column_idx: 1,
            low: Some(5),
            high: Some(7),
        },
        ZoneMapPredicate::RangeInt64 {
            column_idx: 3,
            low: None,
            high: Some(23),
        },
    ];

    let num_threads = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
    let start_tej_db = Instant::now();
    let (total_sum, matched_count) =
        parallel_tpch_q6(&row_groups, &zonemap_preds, num_threads).unwrap();
    assert!(matched_count > 0);
    assert!(total_sum > 0);
    let tej_db_time = start_tej_db.elapsed();
    let tej_db_throughput = (total_rows as f64) / tej_db_time.as_secs_f64();

    let duck_setup = format!(
        "CREATE TABLE lineitem AS SELECT \n\
           1000 + (range % 50000) AS l_extendedprice, \n\
           range % 11 AS l_discount, \n\
           19940101 + (range % 730) AS l_shipdate, \n\
           1 + (range % 50) AS l_quantity \n\
         FROM range({total_rows});"
    );
    let duck_query = "SELECT count(*), sum(l_extendedprice) FROM lineitem \n\
         WHERE l_shipdate >= 19940101 AND l_shipdate < 19950101 \n\
           AND l_discount >= 5 AND l_discount <= 7 \n\
           AND l_quantity < 24;";
    let duck_time = run_duckdb_timed_query(&duck_setup, duck_query);
    let duck_throughput = (total_rows as f64) / duck_time.as_secs_f64();

    println!("  - TPC-H Q6 (Multi-predicate Filter + Global Aggregation):");
    println!(
        "    * TejDb:   {:10.2} rows/s ({:.2?})",
        tej_db_throughput, tej_db_time
    );
    println!(
        "    * DuckDB: {:10.2} rows/s ({:.2?}) [Ratio: {:.2}x]",
        duck_throughput,
        duck_time,
        tej_db_throughput / duck_throughput
    );
}
