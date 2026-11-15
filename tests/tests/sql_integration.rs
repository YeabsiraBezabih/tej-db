use tej_db::Database;
use tej_db_core::DbError;
use tej_db_sql::{SqlEngine, Value};

#[test]
fn test_sql_disk_persistence_across_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("sql_disk_persist.db");

    // Session 1: Create table and insert rows via SQL
    {
        let db = Database::open_file(&db_path).unwrap();
        let sql = SqlEngine::new(&db);

        sql.execute(
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, balance REAL, is_admin BOOL)",
        )
        .unwrap();

        let insert_res = sql
            .execute(
                "INSERT INTO users VALUES \
                 (1, 'Alice', 1250.75, true), \
                 (2, 'Bob', 320.0, false), \
                 (3, 'Charlie', 55.25, false)",
            )
            .unwrap();
        assert_eq!(insert_res.rows_affected, 3);
    }

    // Session 2: Reopen from disk with fresh Database and SqlEngine
    {
        let db = Database::open_file(&db_path).unwrap();
        let sql = SqlEngine::new(&db);

        // 1. Point query by primary key
        let res = sql
            .execute("SELECT id, name, balance, is_admin FROM users WHERE id = 1")
            .unwrap();
        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0][0], Value::Integer(1));
        assert_eq!(res.rows[0][1], Value::Text("Alice".into()));
        assert_eq!(res.rows[0][2], Value::Real(1250.75));
        assert_eq!(res.rows[0][3], Value::Boolean(true));

        // 2. Range query with filter
        let res = sql
            .execute("SELECT name, balance FROM users WHERE balance >= 300.0")
            .unwrap();
        assert_eq!(res.rows.len(), 2);
        assert_eq!(res.rows[0][0], Value::Text("Alice".into()));
        assert_eq!(res.rows[1][0], Value::Text("Bob".into()));

        // 3. Update on disk
        sql.execute("UPDATE users SET balance = 1300.0 WHERE id = 1")
            .unwrap();
    }

    // Session 3: Verify update persisted across another restart
    {
        let db = Database::open_file(&db_path).unwrap();
        let sql = SqlEngine::new(&db);
        let res = sql
            .execute("SELECT balance FROM users WHERE id = 1")
            .unwrap();
        assert_eq!(res.rows[0][0], Value::Real(1300.0));
    }
}

#[test]
fn test_sql_negative_and_error_cases() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    // 1. Syntax Error
    let err = sql.execute("SELECT FROM WHERE BAD_SYNTAX;");
    assert!(err.is_err(), "Invalid syntax must return error");

    // 2. Non-existent Table
    let err = sql.execute("SELECT * FROM non_existent_table;");
    assert!(
        matches!(err, Err(DbError::TableNotFound(_))),
        "Selecting from non-existent table must return TableNotFound"
    );

    // 3. Create Table and Duplicate Primary Key violation
    sql.execute("CREATE TABLE products (sku INT PRIMARY KEY, name TEXT)")
        .unwrap();
    sql.execute("INSERT INTO products VALUES (101, 'Widget A')")
        .unwrap();

    let dup_err = sql.execute("INSERT INTO products VALUES (101, 'Widget Duplicate')");
    assert!(dup_err.is_err(), "Duplicate primary key must be rejected");
    match dup_err {
        Err(DbError::Custom(msg)) => {
            assert!(
                msg.contains("Duplicate primary key constraint violation"),
                "Expected constraint violation message, got: {msg}"
            );
        }
        other => panic!("Expected DbError::Custom with constraint violation, got: {other:?}"),
    }

    // 4. Query Non-Existent Column in Projection
    let col_err = sql.execute("SELECT non_existent_col FROM products");
    assert!(col_err.is_err() || col_err.unwrap().rows[0][0] == Value::Null);
}
