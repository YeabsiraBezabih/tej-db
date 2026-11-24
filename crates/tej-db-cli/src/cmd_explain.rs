use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::Path;
use tej_db::Database;
use tej_db_sql::SqlEngine;
use tej_db_sql::cbo::{AdaptiveQueryRouter, CostModel, ExecutionPath, VectorPredicate};
use tej_db_sql::plan::Plan;

/// Analyzes and explains the query execution plan and CBO routing decision.
pub fn execute_explain(db_path: &Path, query: &str) -> Result<String> {
    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
    let sql_engine = SqlEngine::new(&db);

    let plan = sql_engine
        .prepare(query)
        .with_context(|| format!("Failed to plan query '{query}'"))?;

    let mut out = String::new();
    let _ = writeln!(out, "TejDb SQL Query Execution Plan & CBO Route:");
    let _ = writeln!(out, "--------------------------------------------");
    let _ = writeln!(out, "Input SQL:   {query}");

    match &plan {
        Plan::Select {
            table,
            projection,
            key_range,
            filter,
            limit,
        } => {
            let _ = writeln!(out, "Statement:   SELECT (DQL)");
            let _ = writeln!(out, "Target Table: {table}");
            let _ = writeln!(out, "Projections: {} items", projection.len());

            let is_point = matches!(
                (&key_range.start, &key_range.end),
                (std::ops::Bound::Included(k1), std::ops::Bound::Included(k2)) if k1 == k2
            );

            if is_point {
                let _ = writeln!(out, "Access Path: Point Lookup on Primary Key (O(log N))");
            } else if key_range.start != std::ops::Bound::Unbounded
                || key_range.end != std::ops::Bound::Unbounded
            {
                let _ = writeln!(out, "Access Path: Range Scan on Primary Key Index");
            } else {
                let _ = writeln!(out, "Access Path: Full Table Scan");
            }

            if let Some(f) = filter {
                let _ = writeln!(out, "Filter Pred: {f}");
            } else {
                let _ = writeln!(out, "Filter Pred: None");
            }

            if let Some(l) = limit {
                let _ = writeln!(out, "Limit:       {l} rows");
            }

            // Inspect Table row count from engine
            let row_count = (|| -> Result<u64> {
                let rx = db.begin_read()?;
                let range = tej_db::KeyRange::all();
                let tbl = rx.open_table(table)?;
                let items = tbl.scan_range(&range)?;
                Ok(items.len() as u64)
            })()
            .unwrap_or(100);

            let filter_str = filter.as_ref().map(|f| format!("{f}")).unwrap_or_default();
            let has_vector = filter_str.contains("cosine_distance")
                || filter_str.contains("l2_distance")
                || filter_str.contains("vector");

            let vec_pred = if has_vector {
                VectorPredicate::Present
            } else {
                VectorPredicate::Absent
            };

            let router = AdaptiveQueryRouter::default();
            let cost_model = CostModel::default();
            let path = router.route_plan(&plan, row_count, vec_pred);

            let (path_name, rationale) = match path {
                ExecutionPath::VolcanoRowIndex => (
                    "VolcanoRowIndex (Row-at-a-time B+ Tree Iterator)",
                    "Selected for point lookup / low cardinality (<= 1,000 rows)",
                ),
                ExecutionPath::VectorizedColumnar => (
                    "VectorizedColumnar (SIMD Columnar Morsel Engine)",
                    "Selected for high-throughput OLAP analytical batch scan (> 1,000 rows)",
                ),
                ExecutionPath::VectorHybrid => (
                    "VectorHybrid (RoaringBitmap Pre-Filter + Vamana/SIMD)",
                    "Selected due to vector distance search predicates in query",
                ),
            };

            let _ = writeln!(out, "\nCBO Route:   {path_name}");
            let _ = writeln!(out, "Rationale:   {rationale}");
            let _ = writeln!(out, "Est. Rows:   {row_count}");

            let seek_cost = cost_model.cost_index_seek(3, row_count.min(100));
            let scan_cost = cost_model.cost_row_scan(row_count / 30 + 1, row_count);
            let _ = writeln!(
                out,
                "Cost Model:  Index Seek = {seek_cost:.4}, Table Scan = {scan_cost:.4}"
            );
        }
        Plan::Insert { table, rows, .. } => {
            let _ = writeln!(out, "Statement:   INSERT INTO {table} (DML)");
            let _ = writeln!(out, "Rows to Add: {}", rows.len());
            let _ = writeln!(
                out,
                "Route:       ACID Write Transaction -> B+ Tree Leaf In-Place Insert"
            );
        }
        Plan::Update {
            table, assignments, ..
        } => {
            let _ = writeln!(out, "Statement:   UPDATE {table} (DML)");
            let _ = writeln!(out, "Assignments: {} columns", assignments.len());
            let _ = writeln!(
                out,
                "Route:       ACID Write Transaction -> Seek, In-Place Mutate, Re-index"
            );
        }
        Plan::Delete { table, .. } => {
            let _ = writeln!(out, "Statement:   DELETE FROM {table} (DML)");
            let _ = writeln!(
                out,
                "Route:       ACID Write Transaction -> Slotted Cell Tombstone & Page Rebalance"
            );
        }
        Plan::CreateTable { schema } => {
            let _ = writeln!(out, "Statement:   CREATE TABLE {} (DDL)", schema.name);
            let _ = writeln!(out, "Columns:     {} defined", schema.columns.len());
            let _ = writeln!(
                out,
                "Route:       Catalog Table Allocation & Schema Metadata Persistence"
            );
        }
        other => {
            let _ = writeln!(out, "Statement:   {other:?}");
            let _ = writeln!(out, "Route:       Direct Transactional Execution");
        }
    }

    Ok(out.trim_end().to_string())
}
