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
    about = "Interactive TUI and CLI demo shell for Modular ACID Embedded DBMS in Rust",
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
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Tui) => {
            // Launch interactive TUI
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
        Commands::Stats => {
            let db = Database::open_file(db_path)
                .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
            let stats = db.stats();
            let mut out = String::new();
            out.push_str("TejDb Database Engine Statistics:\n");
            let _ = writeln!(out, "  Total Storage Pages: {}", stats.total_pages);
            let _ = writeln!(
                out,
                "  File Size (approx):  {} bytes",
                stats.file_size_bytes
            );
            let _ = writeln!(out, "  Free / Reusable:     {} pages", stats.free_pages);
            let _ = writeln!(out, "  Cached Pages:        {}", stats.cached_pages);
            let _ = writeln!(out, "  Cache Hits:          {}", stats.cache_hits);
            let _ = writeln!(out, "  Cache Misses:        {}", stats.cache_misses);
            let _ = writeln!(
                out,
                "  Cache Hit Rate:      {:.2}%",
                stats.cache_hit_rate * 100.0
            );
            let _ = write!(out, "  Active Readers:      {}", stats.active_read_txns);
            Ok(out)
        }
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
    }
}

#[cfg(test)]
mod tests;
