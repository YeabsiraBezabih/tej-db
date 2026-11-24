use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;
use tej_db::Database;
use tej_db_vector::{
    DistanceMetric, QuantizationPrecision, VectorId, insert_vector_delta, scan_vector_delta,
};

/// Parse a string representing a float vector, such as `"[0.1, 0.2, 0.3]"` or `"0.1, 0.2, 0.3"`.
pub fn parse_vector_literal(s: &str) -> Result<Vec<f32>> {
    let trimmed = s.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    trimmed
        .split(',')
        .map(|token| {
            token
                .trim()
                .parse::<f32>()
                .map_err(|e| anyhow::anyhow!("Invalid vector float element '{token}': {e}"))
        })
        .collect()
}

/// Create a new vector index on a table.
pub fn execute_create_vector_index(
    db_path: &Path,
    table: &str,
    dim: usize,
    metric_str: &str,
    precision_str: &str,
) -> Result<String> {
    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;

    let metric = match metric_str.to_ascii_lowercase().as_str() {
        "l2" => DistanceMetric::L2,
        "dot" | "dotproduct" => DistanceMetric::DotProduct,
        _ => DistanceMetric::Cosine,
    };

    let precision = match precision_str.to_ascii_lowercase().as_str() {
        "1bit" | "1" => QuantizationPrecision::OneBit,
        _ => QuantizationPrecision::TwoBit,
    };

    // Initialize index structure
    let _ = db.create_vector_index_with_precision(table, dim, metric, precision);

    // Persist vector index metadata in database
    let meta_key = format!("__sys_vec_meta_{table}");
    let meta_val = format!("{dim}:{metric_str}:{precision_str}");
    let mut tx = db.begin_write()?;
    tx.put(meta_key.as_bytes(), meta_val.as_bytes())?;
    tx.commit()?;

    Ok(format!(
        "OK: Created vector index on table '{table}' (dim={dim}, metric={metric_str}, precision={precision_str})"
    ))
}

/// Insert a vector into a vector index delta table.
pub fn execute_vector_insert(
    db_path: &Path,
    table: &str,
    id: u64,
    vec_str: &str,
) -> Result<String> {
    let vector = parse_vector_literal(vec_str)?;
    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;

    let mut tx = db.begin_write()?;
    insert_vector_delta(&mut tx, table, VectorId(id), &vector)?;
    tx.commit()?;

    Ok(format!(
        "OK: Inserted vector ID {id} (dimension {}) into '{table}'",
        vector.len()
    ))
}

/// Search for nearest neighbors using SIMD / Vamana similarity search.
pub fn execute_vector_search(
    db_path: &Path,
    table: &str,
    vec_str: &str,
    top_k: usize,
    metric_override: Option<&str>,
) -> Result<String> {
    let query = parse_vector_literal(vec_str)?;
    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;

    let rx = db.begin_read()?;

    // Read index metadata if available
    let meta_key = format!("__sys_vec_meta_{table}");
    let metric_str = if let Some(m) = metric_override {
        m.to_string()
    } else if let Ok(Some(bytes)) = rx.get(meta_key.as_bytes()) {
        let meta_str = String::from_utf8_lossy(&bytes);
        let parts: Vec<&str> = meta_str.split(':').collect();
        parts.get(1).unwrap_or(&"cosine").to_string()
    } else {
        "cosine".to_string()
    };

    let metric = match metric_str.to_ascii_lowercase().as_str() {
        "l2" => DistanceMetric::L2,
        "dot" | "dotproduct" => DistanceMetric::DotProduct,
        _ => DistanceMetric::Cosine,
    };

    let start = Instant::now();
    let deltas = scan_vector_delta(&rx, table)?;

    if deltas.is_empty() {
        return Ok(format!(
            "Vector table '{table}' is empty (0 vectors indexed)."
        ));
    }

    // Convert deltas to format expected by SIMD scan (skipping tombstone deletions)
    let dataset: Vec<(VectorId, &[f32])> = deltas
        .iter()
        .filter_map(|item| item.vector.as_deref().map(|v| (item.vector_id, v)))
        .collect();

    let candidates = tej_db_sql::hybrid::brute_force_simd_scan(&dataset, &query, metric, top_k)?;

    let elapsed_micros = start.elapsed().as_micros();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "KNN Search Results for table '{table}' (metric={metric_str}, corpus_size={}, query_time={elapsed_micros} µs):",
        deltas.len()
    );
    let _ = writeln!(out, "Rank | Vector ID | Distance Score");
    let _ = writeln!(out, "-----+-----------+---------------");

    for (rank, cand) in candidates.iter().enumerate() {
        let _ = writeln!(
            out,
            "{:<4} | {:<9} | {:.6}",
            rank + 1,
            cand.vector_id.0,
            cand.distance
        );
    }

    Ok(out.trim_end().to_string())
}

/// Compact pending delta vectors into an optimized Vamana proximity graph.
pub fn execute_vector_compact(
    db_path: &Path,
    table: &str,
    dim: usize,
    metric_str: &str,
) -> Result<String> {
    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;

    let metric = match metric_str.to_ascii_lowercase().as_str() {
        "l2" => DistanceMetric::L2,
        "dot" | "dotproduct" => DistanceMetric::DotProduct,
        _ => DistanceMetric::Cosine,
    };

    let index = db.create_vector_index(table, dim, metric);
    let root = index.compact()?;

    Ok(format!(
        "OK: Compacted vector index for table '{table}' into Vamana graph (root page ID: {})",
        root.0
    ))
}
