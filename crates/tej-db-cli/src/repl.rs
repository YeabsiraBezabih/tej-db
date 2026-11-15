use anyhow::{Context, Result};
use std::io::{self, Write};
use std::ops::Bound;
use std::path::Path;
use tej_db::Database;
use tej_db_core::KeyRange;

pub(crate) fn run_repl(db_path: &Path) -> Result<()> {
    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
    let sql_engine = tej_db_sql::SqlEngine::new(&db);

    println!("TejDb Interactive REPL Shell v0.1.0");
    println!("Database: {}", db_path.display());
    println!("Type 'help' for commands, run raw SQL, or 'exit' / 'quit' to leave.");

    let stdin = io::stdin();
    let mut current_table: Option<String> = None;

    loop {
        if let Some(ref t) = current_table {
            print!("tej-db [{t}]> ");
        } else {
            print!("tej-db> ");
        }
        io::stdout().flush()?;

        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break; // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let lower = trimmed.to_lowercase();
        let is_sql = lower.starts_with("select ")
            || lower.starts_with("insert ")
            || lower.starts_with("update ")
            || lower.starts_with("delete from ")
            || lower.starts_with("create table ")
            || lower.starts_with("drop table ")
            || lower.starts_with("explain ")
            || lower.starts_with("sql ");

        if is_sql {
            let sql_str = if lower.starts_with("sql ") {
                trimmed[4..].trim()
            } else {
                trimmed
            };
            match sql_engine.execute(sql_str) {
                Ok(res) => println!("{res}"),
                Err(e) => eprintln!("SQL Error: {e}"),
            }
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        match parts[0] {
            "exit" | "quit" | "\\q" => break,
            "help" => {
                println!("Key-Value Commands:");
                println!("  use [table]          Switch active table (or 'default')");
                println!("  tables               List all tables");
                println!("  create <table_name>  Create a table");
                println!("  drop <table_name>    Drop a table");
                println!("  get <key>            Get value for key");
                println!("  put <key> <val>      Put key-value pair");
                println!("  delete <key>         Delete key");
                println!("  scan [start] [end]   Scan range");
                println!("  stats                Display database stats");
                println!("  backup <path>        Online snapshot backup");
                println!("  compact <path>       Compact into target file");
                println!("  exit / quit          Quit the shell");
                println!();
                println!("SQL Commands:");
                println!("  Run standard SQL directly or with 'sql <query>'");
                println!(
                    "  e.g.: CREATE TABLE users (id INT PRIMARY KEY, name TEXT, balance REAL);"
                );
                println!("        INSERT INTO users VALUES (1, 'Alice', 100.50);");
                println!("        SELECT id, name FROM users WHERE id = 1;");
            }
            "use" => {
                if parts.len() < 2 || parts[1] == "default" {
                    current_table = None;
                    println!("Using default keyspace");
                } else {
                    current_table = Some(parts[1].to_string());
                    println!("Using table '{}'", parts[1]);
                }
            }
            "tables" => match db.list_tables() {
                Ok(tables) => {
                    println!("Tables ({}):", tables.len());
                    for t in tables {
                        println!("  - {t}");
                    }
                }
                Err(e) => eprintln!("Error: {e}"),
            },
            "create" => {
                if parts.len() < 2 {
                    eprintln!("Usage: create <table_name>");
                } else if let Err(e) = db.create_table(parts[1]) {
                    eprintln!("Error: {e}");
                } else {
                    println!("OK (created table '{}')", parts[1]);
                }
            }
            "drop" => {
                if parts.len() < 2 {
                    eprintln!("Usage: drop <table_name>");
                } else {
                    match db.drop_table(parts[1]) {
                        Ok(true) => {
                            if current_table.as_deref() == Some(parts[1]) {
                                current_table = None;
                            }
                            println!("OK (dropped table '{}')", parts[1]);
                        }
                        Ok(false) => println!("Table '{}' does not exist", parts[1]),
                        Err(e) => eprintln!("Error: {e}"),
                    }
                }
            }
            "get" => {
                if parts.len() < 2 {
                    eprintln!("Usage: get <key>");
                    continue;
                }
                let key = parts[1];
                let res = (|| -> Result<Option<Vec<u8>>> {
                    let rx = db.begin_read()?;
                    let val = if let Some(ref tbl) = current_table {
                        let t = rx.open_table(tbl)?;
                        t.get(key.as_bytes())?
                    } else {
                        rx.get(key.as_bytes())?
                    };
                    Ok(val)
                })();
                match res {
                    Ok(Some(v)) => println!("{}", String::from_utf8_lossy(&v)),
                    Ok(None) => println!("(nil)"),
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            "put" => {
                if parts.len() < 3 {
                    eprintln!("Usage: put <key> <value>");
                    continue;
                }
                let key = parts[1];
                let val = parts[2..].join(" ");
                let res = (|| -> Result<()> {
                    let mut tx = db.begin_write()?;
                    if let Some(ref tbl) = current_table {
                        let mut t = tx.open_table_mut(tbl)?;
                        t.put(key.as_bytes(), val.as_bytes())?;
                    } else {
                        tx.put(key.as_bytes(), val.as_bytes())?;
                    }
                    tx.commit()?;
                    Ok(())
                })();
                match res {
                    Ok(()) => println!("OK"),
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            "delete" => {
                if parts.len() < 2 {
                    eprintln!("Usage: delete <key>");
                    continue;
                }
                let key = parts[1];
                let res = (|| -> Result<bool> {
                    let mut tx = db.begin_write()?;
                    let deleted = if let Some(ref tbl) = current_table {
                        let mut t = tx.open_table_mut(tbl)?;
                        t.delete(key.as_bytes())?
                    } else {
                        tx.delete(key.as_bytes())?
                    };
                    tx.commit()?;
                    Ok(deleted)
                })();
                match res {
                    Ok(true) => println!("OK (deleted)"),
                    Ok(false) => println!("(did not exist)"),
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            "scan" => {
                let start = parts.get(1).copied();
                let end = parts.get(2).copied();
                let res = (|| -> Result<Vec<tej_db_core::KV>> {
                    let rx = db.begin_read()?;
                    let range = KeyRange {
                        start: match start {
                            Some(s) => Bound::Included(s.as_bytes().to_vec()),
                            None => Bound::Unbounded,
                        },
                        end: match end {
                            Some(e) => Bound::Included(e.as_bytes().to_vec()),
                            None => Bound::Unbounded,
                        },
                    };
                    let items = if let Some(ref tbl) = current_table {
                        let t = rx.open_table(tbl)?;
                        t.scan_range(&range)?
                    } else {
                        rx.scan_range(&range)?
                    };
                    Ok(items)
                })();
                match res {
                    Ok(items) => {
                        println!("Found {} entries:", items.len());
                        for item in items {
                            println!(
                                "  {} => {}",
                                String::from_utf8_lossy(&item.key),
                                String::from_utf8_lossy(&item.value)
                            );
                        }
                    }
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            "stats" => {
                let stats = db.stats();
                println!("Database Engine Statistics:");
                println!("  Total Storage Pages: {}", stats.total_pages);
                println!("  File Size:           {} bytes", stats.file_size_bytes);
                println!("  Free / Reusable:     {} pages", stats.free_pages);
                println!("  Cached Pages:        {}", stats.cached_pages);
                println!("  Cache Hits:          {}", stats.cache_hits);
                println!("  Cache Misses:        {}", stats.cache_misses);
                println!(
                    "  Cache Hit Rate:      {:.2}%",
                    stats.cache_hit_rate * 100.0
                );
                println!("  Active Readers:      {}", stats.active_read_txns);
            }
            "backup" => {
                if parts.len() < 2 {
                    eprintln!("Usage: backup <dest_path>");
                } else if let Err(e) = db.backup(parts[1]) {
                    eprintln!("Error: {e}");
                } else {
                    println!("OK (snapshot backup saved to {})", parts[1]);
                }
            }
            "compact" => {
                if parts.len() < 2 {
                    eprintln!("Usage: compact <dest_path>");
                } else if let Err(e) = db.compact(parts[1]) {
                    eprintln!("Error: {e}");
                } else {
                    println!("OK (compacted database saved to {})", parts[1]);
                }
            }
            unknown => eprintln!("Unknown command: '{unknown}'. Type 'help' for command list."),
        }
    }
    Ok(())
}
