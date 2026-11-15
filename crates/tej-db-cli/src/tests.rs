use super::*;
use clap::Parser;

#[test]
fn test_cli_arg_parsing() {
    // Test parsing each subcommand
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

    let parsed = Cli::try_parse_from(["tej-db-cli", "create-table", "orders"]).unwrap();
    assert!(matches!(parsed.command, Some(Commands::CreateTable { name }) if name == "orders"));

    let parsed = Cli::try_parse_from(["tej-db-cli", "list-tables"]).unwrap();
    assert!(matches!(parsed.command, Some(Commands::ListTables)));

    let parsed = Cli::try_parse_from(["tej-db-cli", "drop-table", "orders"]).unwrap();
    assert!(matches!(parsed.command, Some(Commands::DropTable { name }) if name == "orders"));

    let parsed = Cli::try_parse_from(["tej-db-cli", "backup", "bak.db"]).unwrap();
    assert!(
        matches!(parsed.command, Some(Commands::Backup { dest }) if dest == std::path::Path::new("bak.db"))
    );

    let parsed = Cli::try_parse_from(["tej-db-cli", "compact", "cmp.db"]).unwrap();
    assert!(
        matches!(parsed.command, Some(Commands::Compact { dest }) if dest == std::path::Path::new("cmp.db"))
    );

    let parsed = Cli::try_parse_from(["tej-db-cli", "sql", "SELECT 1"]).unwrap();
    assert!(matches!(parsed.command, Some(Commands::Sql { query }) if query == "SELECT 1"));
}

#[test]
fn test_cli_execute_crud_lifecycle() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cli_crud.db");

    // 1. Put
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

    // 2. Get
    let get_res = execute_command(
        &db_path,
        Commands::Get {
            table: None,
            key: "site".into(),
        },
    )
    .unwrap();
    assert_eq!(get_res, "github.com");

    // 3. Scan
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

    // 4. Stats
    let stats_res = execute_command(&db_path, Commands::Stats).unwrap();
    assert!(stats_res.contains("Total Storage Pages:"));
    assert!(stats_res.contains("Cache Hit Rate:"));

    // 5. Delete
    let del_res = execute_command(
        &db_path,
        Commands::Delete {
            table: None,
            key: "site".into(),
        },
    )
    .unwrap();
    assert_eq!(del_res, "OK (deleted)");

    // 6. Get non-existent
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

    // 1. Create table
    let create_res = execute_command(
        &db_path,
        Commands::CreateTable {
            name: "employees".into(),
        },
    )
    .unwrap();
    assert!(create_res.contains("table 'employees' created"));

    // 2. List tables
    let list_res = execute_command(&db_path, Commands::ListTables).unwrap();
    assert!(list_res.contains("employees"));

    // 3. Put into table
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

    // 4. Get from table
    let get_tbl = execute_command(
        &db_path,
        Commands::Get {
            table: Some("employees".into()),
            key: "emp_101".into(),
        },
    )
    .unwrap();
    assert_eq!(get_tbl, "Alice");

    // 5. Drop table
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

    // DDL
    let ddl = execute_command(
        &db_path,
        Commands::Sql {
            query: "CREATE TABLE inventory (sku INT PRIMARY KEY, qty INT)".into(),
        },
    )
    .unwrap();
    assert!(ddl.contains("Rows affected: 0"));

    // DML
    let dml = execute_command(
        &db_path,
        Commands::Sql {
            query: "INSERT INTO inventory VALUES (1, 100), (2, 250)".into(),
        },
    )
    .unwrap();
    assert!(dml.contains("Rows affected: 2"));

    // DQL
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

    let cmp_res = execute_command(
        &db_path,
        Commands::Compact {
            dest: cmp_path.clone(),
        },
    )
    .unwrap();
    assert!(cmp_res.contains("compacted database saved"));

    // Verify data in backup
    let get_bak = execute_command(
        &bak_path,
        Commands::Get {
            table: None,
            key: "hello".into(),
        },
    )
    .unwrap();
    assert_eq!(get_bak, "world");

    // Verify data in compacted
    let get_cmp = execute_command(
        &cmp_path,
        Commands::Get {
            table: None,
            key: "hello".into(),
        },
    )
    .unwrap();
    assert_eq!(get_cmp, "world");
}
