use anyhow::{Context, Result};
use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use tej_db::Database;
use tej_db_columnar::chunk::DataChunk;
use tej_db_columnar::parquet::{ParquetReader, ParquetWriter};
use tej_db_columnar::vector::PhysicalType;
use tej_db_sql::SqlEngine;

/// Specifies whether a CSV input file contains a header row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsvHeaderMode {
    WithHeader,
    WithoutHeader,
}

/// Imports tabular data from a CSV file into a database table.
pub fn execute_import_csv(
    db_path: &Path,
    table: &str,
    file_path: &Path,
    header_mode: CsvHeaderMode,
) -> Result<String> {
    let has_header = header_mode == CsvHeaderMode::WithHeader;
    let content = std::fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read CSV file from {}", file_path.display()))?;

    let mut lines = content.lines().filter(|l| !l.trim().is_empty());

    let header_line = if has_header {
        lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("CSV file is empty"))?
    } else {
        ""
    };

    let first_data_line = if has_header {
        lines.clone().next()
    } else {
        content.lines().find(|l| !l.trim().is_empty())
    };

    let col_count = if let Some(first) = first_data_line {
        first.split(',').count()
    } else {
        0
    };

    if col_count == 0 {
        return Ok(format!(
            "CSV file {} has no data rows.",
            file_path.display()
        ));
    }

    let col_names: Vec<String> = if has_header && !header_line.is_empty() {
        header_line
            .split(',')
            .map(|s| s.trim().replace(['\"', '\''], ""))
            .collect()
    } else {
        (0..col_count).map(|i| format!("col_{i}")).collect()
    };

    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
    let sql_engine = SqlEngine::new(&db);

    // Create table if it doesn't already exist
    let col_defs: Vec<String> = col_names.iter().map(|n| format!("{n} TEXT")).collect();
    let ddl = format!(
        "CREATE TABLE IF NOT EXISTS {table} ({})",
        col_defs.join(", ")
    );
    let _ = sql_engine.execute(&ddl);

    let mut tx = db.begin_write()?;
    let mut imported_count = 0;

    let row_lines: Vec<&str> = if has_header {
        content
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .collect()
    } else {
        content.lines().filter(|l| !l.trim().is_empty()).collect()
    };

    for line in row_lines {
        let fields: Vec<String> = line
            .split(',')
            .map(|f| {
                let trimmed = f.trim().replace('\'', "''");
                format!("'{trimmed}'")
            })
            .collect();

        if fields.len() == col_names.len() {
            let insert_sql = format!("INSERT INTO {table} VALUES ({})", fields.join(", "));
            sql_engine.execute_in_tx(&mut tx, &insert_sql)?;
            imported_count += 1;
        }
    }

    tx.commit()?;

    Ok(format!(
        "OK: Successfully imported {imported_count} rows from '{}' into table '{table}'",
        file_path.display()
    ))
}

/// Exports table data to a CSV file.
pub fn execute_export_csv(db_path: &Path, table: &str, out_path: &Path) -> Result<String> {
    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
    let sql_engine = SqlEngine::new(&db);

    let res = sql_engine.execute(&format!("SELECT * FROM {table}"))?;

    let mut file = File::create(out_path)
        .with_context(|| format!("Failed to create output file at {}", out_path.display()))?;

    // Write header
    writeln!(file, "{}", res.columns.join(","))?;

    // Write rows
    let row_count = res.rows.len();
    for row in res.rows {
        let line: Vec<String> = row.iter().map(|v| format!("{v}")).collect();
        writeln!(file, "{}", line.join(","))?;
    }

    Ok(format!(
        "OK: Exported {row_count} rows from table '{table}' to CSV '{}'",
        out_path.display()
    ))
}

/// Exports a database table into an Apache Parquet file container.
pub fn execute_export_parquet(db_path: &Path, table: &str, out_path: &Path) -> Result<String> {
    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
    let sql_engine = SqlEngine::new(&db);

    let res = sql_engine.execute(&format!("SELECT * FROM {table}"))?;
    let row_count = res.rows.len();

    if res.columns.is_empty() {
        return Err(anyhow::anyhow!(
            "Table '{table}' is empty or has no columns"
        ));
    }

    // Default physical types to String or inferred types
    let physical_types = vec![PhysicalType::String; res.columns.len()];
    let mut chunk = DataChunk::new(&physical_types, row_count.max(1));

    for row in &res.rows {
        for (col_idx, val) in row.iter().enumerate() {
            if let Some(col) = chunk.column_mut(col_idx) {
                let s = format!("{val}");
                col.push_str(&s)?;
            }
        }
    }
    chunk.set_num_rows(row_count)?;

    ParquetWriter::write_file(out_path, &res.columns, &physical_types, &[chunk])?;

    Ok(format!(
        "OK: Exported {row_count} rows from table '{table}' into Parquet container '{}'",
        out_path.display()
    ))
}

/// Imports data from an Apache Parquet file into a database table.
pub fn execute_import_parquet(db_path: &Path, table: &str, file_path: &Path) -> Result<String> {
    let (metadata, chunks) = ParquetReader::read_file(file_path, 2048)
        .with_context(|| format!("Failed to read Parquet container {}", file_path.display()))?;

    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
    let sql_engine = SqlEngine::new(&db);

    // Create table if needed
    let col_defs: Vec<String> = metadata
        .column_names
        .iter()
        .map(|n| format!("{n} TEXT"))
        .collect();

    let ddl = format!(
        "CREATE TABLE IF NOT EXISTS {table} ({})",
        col_defs.join(", ")
    );
    let _ = sql_engine.execute(&ddl);

    let mut tx = db.begin_write()?;
    let mut total_rows = 0;

    for chunk in &chunks {
        let n_rows = chunk.num_rows();
        for r in 0..n_rows {
            let mut row_vals = Vec::with_capacity(metadata.column_names.len());
            for (c_idx, &pt) in metadata.physical_types.iter().enumerate() {
                if let Some(col) = chunk.column(c_idx) {
                    let val_str = match pt {
                        PhysicalType::Int64 => col
                            .as_i64_slice()
                            .get(r)
                            .map_or_else(|| "0".to_string(), ToString::to_string),
                        PhysicalType::Float64 => col
                            .as_f64_slice()
                            .get(r)
                            .map_or_else(|| "0.0".to_string(), ToString::to_string),
                        PhysicalType::Boolean => {
                            let b = col.get_bool(r).unwrap_or(false);
                            b.to_string()
                        }
                        PhysicalType::String => col.get_str(r).unwrap_or("").replace('\'', "''"),
                    };
                    row_vals.push(format!("'{val_str}'"));
                }
            }

            let insert_sql = format!("INSERT INTO {table} VALUES ({})", row_vals.join(", "));
            sql_engine.execute_in_tx(&mut tx, &insert_sql)?;
            total_rows += 1;
        }
    }

    tx.commit()?;

    Ok(format!(
        "OK: Successfully imported {total_rows} rows from Parquet file '{}' into table '{table}'",
        file_path.display()
    ))
}
