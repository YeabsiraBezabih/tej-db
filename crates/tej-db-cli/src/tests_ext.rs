use super::*;

#[test]
fn test_cli_admin_recover_and_verify() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cli_admin.db");
    let rec_path = temp_dir.path().join("cli_recovered.db");

    execute_command(
        &db_path,
        Commands::Put {
            table: None,
            key: "vital".into(),
            value: "data".into(),
        },
    )
    .unwrap();

    let ver_res = execute_command(&db_path, Commands::Verify).unwrap();
    assert!(ver_res.contains("Status:              PASS"));
    assert!(ver_res.contains("Total Keys Verified: 1"));

    let rec_res = execute_command(
        &db_path,
        Commands::Recover {
            dest: Some(rec_path.clone()),
        },
    )
    .unwrap();
    assert!(rec_res.contains("Successfully recovered database"));

    let get_rec = execute_command(
        &rec_path,
        Commands::Get {
            table: None,
            key: "vital".into(),
        },
    )
    .unwrap();
    assert_eq!(get_rec, "data");
}

#[test]
fn test_cli_vector_operations_lifecycle() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cli_vector.db");

    // 1. Create vector index
    let idx_res = execute_command(
        &db_path,
        Commands::CreateVectorIndex {
            table: "embeddings".into(),
            dim: 3,
            metric: "cosine".into(),
            precision: "2bit".into(),
        },
    )
    .unwrap();
    assert!(idx_res.contains("Created vector index on table 'embeddings'"));

    // 2. Insert vectors
    execute_command(
        &db_path,
        Commands::VectorInsert {
            table: "embeddings".into(),
            id: 1,
            vector: "[1.0, 0.0, 0.0]".into(),
        },
    )
    .unwrap();

    execute_command(
        &db_path,
        Commands::VectorInsert {
            table: "embeddings".into(),
            id: 2,
            vector: "[0.0, 1.0, 0.0]".into(),
        },
    )
    .unwrap();

    execute_command(
        &db_path,
        Commands::VectorInsert {
            table: "embeddings".into(),
            id: 3,
            vector: "[0.9, 0.1, 0.0]".into(),
        },
    )
    .unwrap();

    // 3. Search KNN
    let search_res = execute_command(
        &db_path,
        Commands::VectorSearch {
            table: "embeddings".into(),
            vector: "[1.0, 0.0, 0.0]".into(),
            top_k: 2,
            metric: None,
        },
    )
    .unwrap();
    assert!(search_res.contains("KNN Search Results"));
    assert!(search_res.contains("Vector ID"));
    assert!(search_res.contains('1'));

    // 4. Compact vector index
    let compact_res = execute_command(
        &db_path,
        Commands::VectorCompact {
            table: "embeddings".into(),
            dim: 3,
            metric: "cosine".into(),
        },
    )
    .unwrap();
    assert!(compact_res.contains("Compacted vector index"));
}

#[test]
fn test_cli_data_csv_and_parquet_exchange() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cli_data.db");
    let csv_path = temp_dir.path().join("sample.csv");
    let export_csv_path = temp_dir.path().join("exported.csv");
    let parquet_path = temp_dir.path().join("data.parquet");

    // Write test CSV
    std::fs::write(
        &csv_path,
        "id,name,role\n1,Alice,Engineer\n2,Bob,Architect\n",
    )
    .unwrap();

    // Import CSV
    let imp_res = execute_command(
        &db_path,
        Commands::ImportCsv {
            table: "staff".into(),
            file: csv_path,
            has_header: true,
        },
    )
    .unwrap();
    assert!(imp_res.contains("Successfully imported 2 rows"));

    // Export CSV
    let exp_res = execute_command(
        &db_path,
        Commands::ExportCsv {
            table: "staff".into(),
            out: export_csv_path.clone(),
        },
    )
    .unwrap();
    assert!(exp_res.contains("Exported 2 rows"));
    let exported_text = std::fs::read_to_string(export_csv_path).unwrap();
    assert!(exported_text.contains("Alice"));
    assert!(exported_text.contains("Bob"));

    // Export Parquet
    let exp_parq = execute_command(
        &db_path,
        Commands::ExportParquet {
            table: "staff".into(),
            out: parquet_path.clone(),
        },
    )
    .unwrap();
    assert!(exp_parq.contains("Exported 2 rows"));

    // Import Parquet into another table
    let imp_parq = execute_command(
        &db_path,
        Commands::ImportParquet {
            table: "staff_copy".into(),
            file: parquet_path,
        },
    )
    .unwrap();
    assert!(imp_parq.contains("Successfully imported 2 rows"));
}

#[test]
fn test_cli_explain_query() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cli_explain.db");

    execute_command(
        &db_path,
        Commands::Sql {
            query: "CREATE TABLE metrics (id INT PRIMARY KEY, val REAL)".into(),
        },
    )
    .unwrap();

    let explain_res = execute_command(
        &db_path,
        Commands::Explain {
            query: "SELECT id, val FROM metrics WHERE id = 10".into(),
        },
    )
    .unwrap();

    assert!(explain_res.contains("TejDb SQL Query Execution Plan"));
    assert!(explain_res.contains("Point Lookup on Primary Key"));
    assert!(explain_res.contains("CBO Route:"));
}
