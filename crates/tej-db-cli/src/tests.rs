use super::*;
use clap::Parser;

#[test]
fn test_cli_arg_parsing() {
    let parsed = Cli::try_parse_from(["tej-db-cli", "--db", "custom.db", "get", "mykey"]).unwrap();
    assert_eq!(parsed.db, PathBuf::from("custom.db"));
    assert!(matches!(parsed.command, Some(Commands::Get { key, table: None }) if key == "mykey"));

    let parsed = Cli::try_parse_from(["tej-db-cli", "put", "-t", "users", "alice", "100"]).unwrap();
    assert!(matches!(
        parsed.command,
        Some(Commands::Put { table: Some(tbl), key, value }) if tbl == "users" && key == "alice" && value == "100"
    ));

    let parsed = Cli::try_parse_from(["tej-db-cli", "delete", "tempkey"]).unwrap();
    assert!(matches!(parsed.command, Some(Commands::Delete { key, .. }) if key == "tempkey"));

    let parsed = Cli::try_parse_from(["tej-db-cli", "scan", "--start", "a", "--end", "z"]).unwrap();
    assert!(
        matches!(parsed.command, Some(Commands::Scan { start: Some(s), end: Some(e), .. }) if s == "a" && e == "z")
    );

    let parsed = Cli::try_parse_from(["tej-db-cli", "stats"]).unwrap();
    assert!(matches!(parsed.command, Some(Commands::Stats)));

    let parsed = Cli::try_parse_from(["tej-db-cli", "recover", "--dest", "rec.db"]).unwrap();
    assert!(
        matches!(parsed.command, Some(Commands::Recover { dest: Some(d) }) if d == std::path::Path::new("rec.db"))
    );

    let parsed = Cli::try_parse_from(["tej-db-cli", "verify"]).unwrap();
    assert!(matches!(parsed.command, Some(Commands::Verify)));

    let parsed = Cli::try_parse_from([
        "tej-db-cli",
        "create-vector-index",
        "--table",
        "items",
        "--dim",
        "128",
        "--metric",
        "l2",
        "--precision",
        "1bit",
    ])
    .unwrap();
    assert!(matches!(
        parsed.command,
        Some(Commands::CreateVectorIndex { table, dim, metric, precision })
            if table == "items" && dim == 128 && metric == "l2" && precision == "1bit"
    ));

    let parsed = Cli::try_parse_from([
        "tej-db-cli",
        "vector-insert",
        "--table",
        "items",
        "--id",
        "1",
        "--vector",
        "[1.0, 2.0]",
    ])
    .unwrap();
    assert!(matches!(
        parsed.command,
        Some(Commands::VectorInsert { table, id, vector }) if table == "items" && id == 1 && vector == "[1.0, 2.0]"
    ));

    let parsed = Cli::try_parse_from([
        "tej-db-cli",
        "vector-search",
        "--table",
        "items",
        "--vector",
        "[1.0, 2.0]",
        "-k",
        "5",
    ])
    .unwrap();
    assert!(matches!(
        parsed.command,
        Some(Commands::VectorSearch { table, vector, top_k, .. }) if table == "items" && top_k == 5 && vector == "[1.0, 2.0]"
    ));

    let parsed = Cli::try_parse_from([
        "tej-db-cli",
        "import-csv",
        "--table",
        "logs",
        "--file",
        "data.csv",
    ])
    .unwrap();
    assert!(matches!(
        parsed.command,
        Some(Commands::ImportCsv { table, file, has_header }) if table == "logs" && file == std::path::Path::new("data.csv") && has_header
    ));

    let parsed = Cli::try_parse_from([
        "tej-db-cli",
        "export-parquet",
        "--table",
        "logs",
        "--out",
        "dump.parquet",
    ])
    .unwrap();
    assert!(matches!(
        parsed.command,
        Some(Commands::ExportParquet { table, out }) if table == "logs" && out == std::path::Path::new("dump.parquet")
    ));

    let parsed = Cli::try_parse_from(["tej-db-cli", "explain", "SELECT 1"]).unwrap();
    assert!(matches!(parsed.command, Some(Commands::Explain { query }) if query == "SELECT 1"));
}

#[test]
fn test_cli_execute_crud_lifecycle() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cli_crud.db");

    let put_res = execute_command(
        &db_path,
        Commands::Put {
            table: None,
            key: "site".into(),
            value: "github.com".into(),
        },
    )
    .unwrap();
    assert_eq!(put_res, "OK");

    let get_res = execute_command(
        &db_path,
        Commands::Get {
            table: None,
            key: "site".into(),
        },
    )
    .unwrap();
    assert_eq!(get_res, "github.com");

    let scan_res = execute_command(
        &db_path,
        Commands::Scan {
            table: None,
            start: None,
            end: None,
        },
    )
    .unwrap();
    assert!(scan_res.contains("site => github.com"));

    let stats_res = execute_command(&db_path, Commands::Stats).unwrap();
    assert!(stats_res.contains("Total Storage Pages:"));
    assert!(stats_res.contains("Cache Hit Rate:"));

    let del_res = execute_command(
        &db_path,
        Commands::Delete {
            table: None,
            key: "site".into(),
        },
    )
    .unwrap();
    assert_eq!(del_res, "OK (deleted)");

    let get_after_del = execute_command(
        &db_path,
        Commands::Get {
            table: None,
            key: "site".into(),
        },
    );
    assert!(get_after_del.is_err());
}

#[test]
fn test_cli_execute_table_subcommands() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cli_tables.db");

    let create_res = execute_command(
        &db_path,
        Commands::CreateTable {
            name: "employees".into(),
        },
    )
    .unwrap();
    assert!(create_res.contains("table 'employees' created"));

    let list_res = execute_command(&db_path, Commands::ListTables).unwrap();
    assert!(list_res.contains("employees"));

    let put_tbl = execute_command(
        &db_path,
        Commands::Put {
            table: Some("employees".into()),
            key: "emp_101".into(),
            value: "Alice".into(),
        },
    )
    .unwrap();
    assert_eq!(put_tbl, "OK");

    let get_tbl = execute_command(
        &db_path,
        Commands::Get {
            table: Some("employees".into()),
            key: "emp_101".into(),
        },
    )
    .unwrap();
    assert_eq!(get_tbl, "Alice");

    let drop_res = execute_command(
        &db_path,
        Commands::DropTable {
            name: "employees".into(),
        },
    )
    .unwrap();
    assert!(drop_res.contains("table 'employees' dropped"));
}

#[test]
fn test_cli_execute_sql() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cli_sql.db");

    let ddl = execute_command(
        &db_path,
        Commands::Sql {
            query: "CREATE TABLE inventory (sku INT PRIMARY KEY, qty INT)".into(),
        },
    )
    .unwrap();
    assert!(ddl.contains("Rows affected: 0"));

    let dml = execute_command(
        &db_path,
        Commands::Sql {
            query: "INSERT INTO inventory VALUES (1, 100), (2, 250)".into(),
        },
    )
    .unwrap();
    assert!(dml.contains("Rows affected: 2"));

    let dql = execute_command(
        &db_path,
        Commands::Sql {
            query: "SELECT sku, qty FROM inventory WHERE sku = 1".into(),
        },
    )
    .unwrap();
    assert!(dql.contains("sku"));
    assert!(dql.contains("qty"));
    assert!(dql.contains("100"));
}

#[test]
fn test_cli_backup_and_compact() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cli_source.db");
    let bak_path = temp_dir.path().join("cli_backup.db");
    let cmp_path = temp_dir.path().join("cli_compact.db");

    execute_command(
        &db_path,
        Commands::Put {
            table: None,
            key: "hello".into(),
            value: "world".into(),
        },
    )
    .unwrap();

    let bak_res = execute_command(
        &db_path,
        Commands::Backup {
            dest: bak_path.clone(),
        },
    )
    .unwrap();
    assert!(bak_res.contains("backup saved"));

    let cmp_res = execute_command(&db_path, Commands::Compact { dest: cmp_path }).unwrap();
    assert!(cmp_res.contains("compacted database saved"));

    let get_bak = execute_command(
        &bak_path,
        Commands::Get {
            table: None,
            key: "hello".into(),
        },
    )
    .unwrap();
    assert_eq!(get_bak, "world");
}
