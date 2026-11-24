mod cmd_admin;
mod cmd_data;
mod cmd_explain;
mod cmd_vector;
mod repl;
mod tui;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::fmt::Write as _;
use std::ops::Bound;
use std::path::PathBuf;
use tej_db::{Database, KeyRange};

#[derive(Parser, Debug)]
#[command(name = "tej-db-cli")]
#[command(
    about = "Interactive TUI and CLI administration shell for Modular ACID Embedded DBMS in Rust",
    long_about = None
)]
struct Cli {
    /// Path to the database file
    #[arg(short, long, default_value = "tej.db")]
    db: PathBuf,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Launch interactive graphical terminal user interface (TUI)
    Tui,
    /// Launch interactive line REPL shell
    Repl,
    /// Get value for a given key
    Get {
        /// Optional table name
        #[arg(short, long)]
        table: Option<String>,
        /// Key to lookup
        key: String,
    },
    /// Insert or update a key-value pair
    Put {
        /// Optional table name
        #[arg(short, long)]
        table: Option<String>,
        /// Key
        key: String,
        /// Value
        value: String,
    },
    /// Delete a key
    Delete {
        /// Optional table name
        #[arg(short, long)]
        table: Option<String>,
        /// Key to delete
        key: String,
    },
    /// Scan key-value pairs in a range
    Scan {
        /// Optional table name
        #[arg(short, long)]
        table: Option<String>,
        /// Optional start key (inclusive)
        #[arg(long)]
        start: Option<String>,
        /// Optional end key (inclusive)
        #[arg(long)]
        end: Option<String>,
    },
    /// Display database cache and transaction statistics
    Stats,
    /// Recover a database file from crashes or torn writes
    Recover {
        /// Optional destination path to write recovered & compacted database
        #[arg(long)]
        dest: Option<PathBuf>,
    },
    /// Verify internal consistency of active tables and page checksums
    Verify,
    /// Create a new named table/keyspace
    CreateTable {
        /// Table name
        name: String,
    },
    /// List all named tables in the database
    ListTables,
    /// Drop a named table
    DropTable {
        /// Table name
        name: String,
    },
    /// Export an online snapshot backup to another file
    Backup {
        /// Destination backup path
        dest: PathBuf,
    },
    /// Compact database into a contiguous new file
    Compact {
        /// Destination compacted path
        dest: PathBuf,
    },
    /// Execute a SQL query (SELECT, INSERT, UPDATE, DELETE, CREATE TABLE, etc.)
    Sql {
        /// SQL statement to execute
        query: String,
    },
    /// Explain the physical execution plan and Cost-Based Optimizer route for a query
    Explain {
        /// SQL query to analyze
        query: String,
    },
    /// Create a high-performance vector index on a table
    CreateVectorIndex {
        /// Table name
        #[arg(short, long)]
        table: String,
        /// Vector dimensions
        #[arg(short, long)]
        dim: usize,
        /// Distance metric (cosine, l2, dot)
        #[arg(short, long, default_value = "cosine")]
        metric: String,
        /// Quantization precision (1bit, 2bit, full)
        #[arg(short, long, default_value = "2bit")]
        precision: String,
    },
    /// Insert a vector into a vector index delta table
    VectorInsert {
        /// Table name
        #[arg(short, long)]
        table: String,
        /// Unique vector ID
        #[arg(short, long)]
        id: u64,
        /// Float vector literal, e.g. "[0.1, 0.2, 0.3]"
        #[arg(short, long)]
        vector: String,
    },
    /// Search for nearest neighbors using SIMD or Vamana graph beam search
    VectorSearch {
        /// Table name
        #[arg(short, long)]
        table: String,
        /// Query vector literal, e.g. "[0.1, 0.2, 0.3]"
        #[arg(short, long)]
        vector: String,
        /// Number of top candidates to return
        #[arg(short = 'k', long, default_value_t = 10)]
        top_k: usize,
        /// Optional distance metric override (cosine, l2, dot)
        #[arg(short, long)]
        metric: Option<String>,
    },
    /// Compact pending delta vectors into an optimized Vamana proximity graph
    VectorCompact {
        /// Table name
        #[arg(short, long)]
        table: String,
        /// Vector dimensions
        #[arg(short, long)]
        dim: usize,
        /// Distance metric (cosine, l2, dot)
        #[arg(short, long, default_value = "cosine")]
        metric: String,
    },
    /// Import tabular CSV file into a database table
    ImportCsv {
        /// Destination table name
        #[arg(short, long)]
        table: String,
        /// CSV file path
        #[arg(short, long)]
        file: PathBuf,
        /// Whether the CSV file contains a header row
        #[arg(long, default_value_t = true)]
        has_header: bool,
    },
    /// Export table contents to a CSV file
    ExportCsv {
        /// Source table name
        #[arg(short, long)]
        table: String,
        /// Output CSV destination path
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Import an Apache Parquet container file into a database table
    ImportParquet {
        /// Destination table name
        #[arg(short, long)]
        table: String,
        /// Parquet file path
        #[arg(short, long)]
        file: PathBuf,
    },
    /// Export table contents to an Apache Parquet container file
    ExportParquet {
        /// Source table name
        #[arg(short, long)]
        table: String,
        /// Output Parquet destination path
        #[arg(short, long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Tui) => {
            tui::run_tui(&cli.db)?;
        }
        Some(Commands::Repl) => {
            repl::run_repl(&cli.db)?;
        }
        Some(cmd) => {
            let output = execute_command(&cli.db, cmd)?;
            if !output.is_empty() {
                println!("{output}");
            }
        }
    }

    Ok(())
}

pub(crate) fn execute_command(db_path: &std::path::Path, cmd: Commands) -> Result<String> {
    match cmd {
        Commands::Tui | Commands::Repl => Ok(String::new()),
        Commands::Get { table, key } => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            let rx = db.begin_read()?;
            let val = if let Some(tbl) = table {
                let t = rx.open_table(&tbl)?;
                t.get(key.as_bytes())?
            } else {
                rx.get(key.as_bytes())?
            };
            match val {
                Some(v) => Ok(String::from_utf8_lossy(&v).to_string()),
                None => anyhow::bail!("Key '{key}' not found"),
            }
        }
        Commands::Put { table, key, value } => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            let mut tx = db.begin_write()?;
            if let Some(tbl) = table {
                let mut t = tx.open_table_mut(&tbl)?;
                t.put(key.as_bytes(), value.as_bytes())?;
            } else {
                tx.put(key.as_bytes(), value.as_bytes())?;
            }
            tx.commit()?;
            Ok("OK".into())
        }
        Commands::Delete { table, key } => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            let mut tx = db.begin_write()?;
            let deleted = if let Some(tbl) = table {
                let mut t = tx.open_table_mut(&tbl)?;
                t.delete(key.as_bytes())?
            } else {
                tx.delete(key.as_bytes())?
            };
            tx.commit()?;
            if deleted {
                Ok("OK (deleted)".into())
            } else {
                Ok("(did not exist)".into())
            }
        }
        Commands::Scan { table, start, end } => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            let rx = db.begin_read()?;
            let range = KeyRange {
                start: match start {
                    Some(s) => Bound::Included(s.into_bytes()),
                    None => Bound::Unbounded,
                },
                end: match end {
                    Some(e) => Bound::Included(e.into_bytes()),
                    None => Bound::Unbounded,
                },
            };
            let items = if let Some(tbl) = table {
                let t = rx.open_table(&tbl)?;
                t.scan_range(&range)?
            } else {
                rx.scan_range(&range)?
            };
            let mut out = format!("Found {} entries:\n", items.len());
            for item in items {
                let k = String::from_utf8_lossy(&item.key);
                let v = String::from_utf8_lossy(&item.value);
                let _ = writeln!(out, "  {k} => {v}");
            }
            Ok(out.trim_end().to_string())
        }
        Commands::Stats => cmd_admin::execute_stats(db_path),
        Commands::Recover { dest } => cmd_admin::execute_recover(db_path, dest.as_deref()),
        Commands::Verify => cmd_admin::execute_verify(db_path),
        Commands::CreateTable { name } => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            db.create_table(&name)?;
            Ok(format!("OK (table '{name}' created)"))
        }
        Commands::ListTables => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            let tables = db.list_tables()?;
            let mut out = format!("Tables ({}):\n", tables.len());
            for t in tables {
                let _ = writeln!(out, "  - {t}");
            }
            Ok(out.trim_end().to_string())
        }
        Commands::DropTable { name } => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            let dropped = db.drop_table(&name)?;
            if dropped {
                Ok(format!("OK (table '{name}' dropped)"))
            } else {
                Ok(format!("Table '{name}' not found"))
            }
        }
        Commands::Backup { dest } => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            db.backup(&dest)?;
            Ok(format!("OK (snapshot backup saved to {})", dest.display()))
        }
        Commands::Compact { dest } => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            db.compact(&dest)?;
            Ok(format!(
                "OK (compacted database saved to {})",
                dest.display()
            ))
        }
        Commands::Sql { query } => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            let engine = tej_db_sql::SqlEngine::new(&db);
            let result = engine.execute(&query)?;
            Ok(format!("{result}"))
        }
        Commands::Explain { query } => cmd_explain::execute_explain(db_path, &query),
        Commands::CreateVectorIndex {
            table,
            dim,
            metric,
            precision,
        } => cmd_vector::execute_create_vector_index(db_path, &table, dim, &metric, &precision),
        Commands::VectorInsert { table, id, vector } => {
            cmd_vector::execute_vector_insert(db_path, &table, id, &vector)
        }
        Commands::VectorSearch {
            table,
            vector,
            top_k,
            metric,
        } => cmd_vector::execute_vector_search(db_path, &table, &vector, top_k, metric.as_deref()),
        Commands::VectorCompact { table, dim, metric } => {
            cmd_vector::execute_vector_compact(db_path, &table, dim, &metric)
        }
        Commands::ImportCsv {
            table,
            file,
            has_header,
        } => {
            let mode = if has_header {
                cmd_data::CsvHeaderMode::WithHeader
            } else {
                cmd_data::CsvHeaderMode::WithoutHeader
            };
            cmd_data::execute_import_csv(db_path, &table, &file, mode)
        }
        Commands::ExportCsv { table, out } => cmd_data::execute_export_csv(db_path, &table, &out),
        Commands::ImportParquet { table, file } => {
            cmd_data::execute_import_parquet(db_path, &table, &file)
        }
        Commands::ExportParquet { table, out } => {
            cmd_data::execute_export_parquet(db_path, &table, &out)
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ext;
