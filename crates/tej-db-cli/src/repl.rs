use anyhow::{Context, Result};
use std::io::{self, Write};
use std::ops::Bound;
use std::path::Path;
use tej_db::{Database, FileStorage, KeyRange, WriteTx};

pub(crate) fn run_repl(db_path: &Path) -> Result<()> {
    let db = Database::open_file(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
    let sql_engine = tej_db_sql::SqlEngine::new(&db);

    println!("TejDb Interactive REPL Shell v0.1.0");
    println!("Database: {}", db_path.display());
    println!("Type 'help' for commands, run raw SQL, or 'exit' / 'quit' to leave.");

    let stdin = io::stdin();
    let mut current_table: Option<String> = None;
    let mut active_tx: Option<WriteTx<'_, FileStorage>> = None;

    loop {
        // Construct prompt indicating active table and transaction state
        let tx_badge = if active_tx.is_some() { " (in-tx)" } else { "" };
        if let Some(ref t) = current_table {
            print!("tej-db [{t}]{tx_badge}> ");
        } else {
            print!("tej-db{tx_badge}> ");
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

            let res = if let Some(ref mut tx) = active_tx {
                sql_engine.execute_in_tx(tx, sql_str)
            } else {
                sql_engine.execute(sql_str)
            };

            match res {
                Ok(out) => println!("{out}"),
                Err(e) => eprintln!("SQL Error: {e}"),
            }
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        match parts[0] {
            "exit" | "quit" | "\\q" => {
                if let Some(tx) = active_tx.take() {
                    let _ = tx.rollback();
                }
                break;
            }
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
                println!("  verify               Verify integrity and checksums");
                println!("  backup <path>        Online snapshot backup");
                println!("  compact <path>       Compact into target file");
                println!();
                println!("Transaction Commands:");
                println!("  begin                Start an explicit ACID write transaction");
                println!("  commit               Commit active transaction");
                println!("  rollback             Abort active transaction");
                println!("  savepoint <name>     Create named savepoint in transaction");
                println!("  rollback to <name>   Roll back to named savepoint");
                println!();
                println!("Vector & Analytics Commands:");
                println!("  vsearch <vec> [topk] KNN vector search on active table");
                println!("  explain <query>      Analyze query plan and CBO route");
                println!();
                println!("SQL Commands:");
                println!("  Run standard SQL directly or with 'sql <query>'");
            }
            "begin" => {
                if active_tx.is_some() {
                    eprintln!("A transaction is already active. Commit or rollback first.");
                } else {
                    match db.begin_write() {
                        Ok(tx) => {
                            active_tx = Some(tx);
                            println!("OK: Transaction begun.");
                        }
                        Err(e) => eprintln!("Error beginning transaction: {e}"),
                    }
                }
            }
            "commit" => {
                if let Some(tx) = active_tx.take() {
                    match tx.commit() {
                        Ok(()) => println!("OK: Transaction committed."),
                        Err(e) => eprintln!("Error committing transaction: {e}"),
                    }
                } else {
                    eprintln!("No active transaction to commit.");
                }
            }
            "rollback" => {
                if parts.len() >= 3 && parts[1] == "to" {
                    let sp_name = parts[2];
                    if let Some(ref mut tx) = active_tx {
                        match tx.rollback_to_savepoint(sp_name) {
                            Ok(()) => println!("OK: Rolled back to savepoint '{sp_name}'."),
                            Err(e) => eprintln!("Error: {e}"),
                        }
                    } else {
                        eprintln!("No active transaction.");
                    }
                } else if let Some(tx) = active_tx.take() {
                    match tx.rollback() {
                        Ok(()) => println!("OK: Transaction rolled back."),
                        Err(e) => eprintln!("Error rolling back transaction: {e}"),
                    }
                } else {
                    eprintln!("No active transaction to rollback.");
                }
            }
            "savepoint" => {
                if parts.len() < 2 {
                    eprintln!("Usage: savepoint <name>");
                } else if let Some(ref mut tx) = active_tx {
                    tx.savepoint(parts[1]);
                    println!("OK: Savepoint '{}' created.", parts[1]);
                } else {
                    eprintln!("Savepoints require an active transaction. Run 'begin' first.");
                }
            }
            "vsearch" => {
                if parts.len() < 2 {
                    eprintln!("Usage: vsearch <[vector]> [top_k]");
                } else {
                    let vec_literal = parts[1];
                    let top_k = parts
                        .get(2)
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(5);
                    let tbl = current_table.as_deref().unwrap_or("default");
                    match crate::cmd_vector::execute_vector_search(
                        db_path,
                        tbl,
                        vec_literal,
                        top_k,
                        None,
                    ) {
                        Ok(res) => println!("{res}"),
                        Err(e) => eprintln!("Vector search error: {e}"),
                    }
                }
            }
            "explain" => {
                if parts.len() < 2 {
                    eprintln!("Usage: explain <sql_query>");
                } else {
                    let query = parts[1..].join(" ");
                    match crate::cmd_explain::execute_explain(db_path, &query) {
                        Ok(res) => println!("{res}"),
                        Err(e) => eprintln!("Explain error: {e}"),
                    }
                }
            }
            "verify" => match crate::cmd_admin::execute_verify(db_path) {
                Ok(res) => println!("{res}"),
                Err(e) => eprintln!("Verification error: {e}"),
            },
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
                    let val = if let Some(ref mut tx) = active_tx {
                        if let Some(ref tbl) = current_table {
                            let t = tx.open_table_mut(tbl)?;
                            t.get(key.as_bytes())?
                        } else {
                            tx.get(key.as_bytes())?
                        }
                    } else {
                        let rx = db.begin_read()?;
                        if let Some(ref tbl) = current_table {
                            let t = rx.open_table(tbl)?;
                            t.get(key.as_bytes())?
                        } else {
                            rx.get(key.as_bytes())?
                        }
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
                    if let Some(ref mut tx) = active_tx {
                        if let Some(ref tbl) = current_table {
                            let mut t = tx.open_table_mut(tbl)?;
                            t.put(key.as_bytes(), val.as_bytes())?;
                        } else {
                            tx.put(key.as_bytes(), val.as_bytes())?;
                        }
                    } else {
                        let mut tx = db.begin_write()?;
                        if let Some(ref tbl) = current_table {
                            let mut t = tx.open_table_mut(tbl)?;
                            t.put(key.as_bytes(), val.as_bytes())?;
                        } else {
                            tx.put(key.as_bytes(), val.as_bytes())?;
                        }
                        tx.commit()?;
                    }
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
                    if let Some(ref mut tx) = active_tx {
                        let deleted = if let Some(ref tbl) = current_table {
                            let mut t = tx.open_table_mut(tbl)?;
                            t.delete(key.as_bytes())?
                        } else {
                            tx.delete(key.as_bytes())?
                        };
                        Ok(deleted)
                    } else {
                        let mut tx = db.begin_write()?;
                        let deleted = if let Some(ref tbl) = current_table {
                            let mut t = tx.open_table_mut(tbl)?;
                            t.delete(key.as_bytes())?
                        } else {
                            tx.delete(key.as_bytes())?
                        };
                        tx.commit()?;
                        Ok(deleted)
                    }
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
            "stats" => match crate::cmd_admin::execute_stats(db_path) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("Error: {e}"),
            },
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
