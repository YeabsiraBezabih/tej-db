use tej_db::{Database, Result};
use tej_db_sql::{SqlEngine, Value};
use tempfile::NamedTempFile;

#[test]
fn test_persistent_auto_rowid_across_reopen() -> Result<()> {
    let tmp = NamedTempFile::new().map_err(tej_db_core::DbError::Io)?;
    let path = tmp.path().to_path_buf();

    // 1. Initial database session: insert rows into implicit rowid table
    {
        let db = Database::open_file(&path)?;
        let sql = SqlEngine::new(&db);
        sql.execute("CREATE TABLE logs (msg TEXT)")?;
        for i in 1..=5 {
            sql.execute(&format!("INSERT INTO logs VALUES ('message_{i}')"))?;
        }

        let res = sql.execute("SELECT * FROM logs")?;
        assert_eq!(res.rows.len(), 5);
    }

    // 2. Reopen session: ensure auto_rowid starts after max existing rowid
    {
        let db = Database::open_file(&path)?;
        let sql = SqlEngine::new(&db);

        // Insert a new row after reopen
        sql.execute("INSERT INTO logs VALUES ('message_6')")?;

        let res = sql.execute("SELECT * FROM logs")?;
        assert_eq!(res.rows.len(), 6);

        // Verify all 6 rows are preserved without any overwritten keys
        let messages: Vec<String> = res
            .rows
            .iter()
            .map(|r| match &r[0] {
                Value::Text(s) => s.clone(),
                other => panic!("Unexpected value type: {other:?}"),
            })
            .collect();
        assert_eq!(
            messages,
            vec![
                "message_1",
                "message_2",
                "message_3",
                "message_4",
                "message_5",
                "message_6"
            ]
        );
    }

    Ok(())
}

#[test]
fn test_prepared_point_insert_auto_rowid() -> Result<()> {
    let tmp = NamedTempFile::new().map_err(tej_db_core::DbError::Io)?;
    let db = Database::open_file(tmp.path())?;
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE telemetry (val INT)")?;

    // Prepare point insert with placeholder
    let prep = sql.prepare_statement("INSERT INTO telemetry VALUES (?)")?;
    for i in 1..=10 {
        prep.execute(&[Value::Integer(i * 10)])?;
    }

    let res = sql.execute("SELECT * FROM telemetry")?;
    assert_eq!(
        res.rows.len(),
        10,
        "Every prepared insert must receive a unique auto_rowid"
    );

    Ok(())
}

#[test]
fn test_update_primary_key_column() -> Result<()> {
    let tmp = NamedTempFile::new().map_err(tej_db_core::DbError::Io)?;
    let db = Database::open_file(tmp.path())?;
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT)")?;
    sql.execute("INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')")?;

    // 1. Update PK of Alice from 1 to 10
    let res = sql.execute("UPDATE users SET id = 10 WHERE id = 1")?;
    assert_eq!(res.rows_affected, 1);

    // Old PK should be gone
    let old_res = sql.execute("SELECT * FROM users WHERE id = 1")?;
    assert_eq!(old_res.rows.len(), 0);

    // New PK should exist
    let new_res = sql.execute("SELECT name FROM users WHERE id = 10")?;
    assert_eq!(new_res.rows.len(), 1);
    assert_eq!(new_res.rows[0][0], Value::Text("Alice".into()));

    // 2. Colliding PK update should fail with uniqueness violation
    let collision_err = sql.execute("UPDATE users SET id = 2 WHERE id = 10");
    assert!(
        collision_err.is_err(),
        "Updating PK to an existing PK must return an error"
    );

    Ok(())
}

#[test]
fn test_prepared_general_statement_parameter_substitution() -> Result<()> {
    let tmp = NamedTempFile::new().map_err(tej_db_core::DbError::Io)?;
    let db = Database::open_file(tmp.path())?;
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE items (id INT PRIMARY KEY, category TEXT, price INT)")?;
    sql.execute("INSERT INTO items VALUES (1, 'book', 20), (2, 'book', 45), (3, 'pen', 5)")?;

    // Prepare general select statement with multiple parameter placeholders
    let prep =
        sql.prepare_statement("SELECT id, price FROM items WHERE category = ? AND price > ?")?;

    let res = prep.execute(&[Value::Text("book".into()), Value::Integer(25)])?;
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Integer(2));
    assert_eq!(res.rows[0][1], Value::Integer(45));

    Ok(())
}

#[test]
fn test_prepared_fast_path_check_constraint_validation() -> Result<()> {
    let tmp = NamedTempFile::new().map_err(tej_db_core::DbError::Io)?;
    let db = Database::open_file(tmp.path())?;
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE accounts (id INT PRIMARY KEY, balance INT CHECK (balance >= 0))")?;

    // 1. PointInsert with violating CHECK constraint
    let prep_insert = sql.prepare_statement("INSERT INTO accounts VALUES (?, ?)")?;
    let err = prep_insert.execute(&[Value::Integer(1), Value::Integer(-100)]);
    assert!(
        err.is_err(),
        "Prepared point insert must validate CHECK constraints"
    );

    // Valid insert
    prep_insert.execute(&[Value::Integer(1), Value::Integer(100)])?;

    // 2. PointUpdate with violating CHECK constraint
    let prep_update = sql.prepare_statement("UPDATE accounts SET balance = ? WHERE id = ?")?;
    let err_update = prep_update.execute(&[Value::Integer(-50), Value::Integer(1)]);
    assert!(
        err_update.is_err(),
        "Prepared point update must validate CHECK constraints"
    );

    // Valid update
    prep_update.execute(&[Value::Integer(200), Value::Integer(1)])?;
    let res = sql.execute("SELECT balance FROM accounts WHERE id = 1")?;
    assert_eq!(res.rows[0][0], Value::Integer(200));

    Ok(())
}

#[test]
fn test_sql_null_three_valued_logic_and_reversed_bounds() -> Result<()> {
    let tmp = NamedTempFile::new().map_err(tej_db_core::DbError::Io)?;
    let db = Database::open_file(tmp.path())?;
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE products (id INT PRIMARY KEY, price INT)")?;
    sql.execute("INSERT INTO products VALUES (1, 100), (2, 200), (3, 300)")?;

    // id > NULL and NULL < id must evaluate to UNKNOWN and return 0 rows
    let res1 = sql.execute("SELECT * FROM products WHERE id > NULL")?;
    assert_eq!(res1.rows.len(), 0, "id > NULL must return 0 rows");

    let res2 = sql.execute("SELECT * FROM products WHERE NULL < id")?;
    assert_eq!(res2.rows.len(), 0, "NULL < id must return 0 rows");

    let res3 = sql.execute("SELECT * FROM products WHERE id = NULL")?;
    assert_eq!(res3.rows.len(), 0, "id = NULL must return 0 rows");

    // Reversed bounds: 1 < id <=> id > 1
    let res4 = sql.execute("SELECT id FROM products WHERE 1 < id")?;
    assert_eq!(res4.rows.len(), 2, "1 < id must return 2 rows (2, 3)");

    // Reversed bounds with AND range: 1 < id AND id < 3
    let res5 = sql.execute("SELECT id FROM products WHERE 1 < id AND id < 3")?;
    assert_eq!(res5.rows.len(), 1, "1 < id AND id < 3 must return row 2");
    assert_eq!(res5.rows[0][0], Value::Integer(2));

    Ok(())
}

#[test]
fn test_update_pkless_table_row_preservation() -> Result<()> {
    let tmp = NamedTempFile::new().map_err(tej_db_core::DbError::Io)?;
    let db = Database::open_file(tmp.path())?;
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE items (val INT)")?;
    sql.execute("INSERT INTO items VALUES (10), (20), (30)")?;

    // Update val from 20 to 25 without primary key
    let res = sql.execute("UPDATE items SET val = 25 WHERE val = 20")?;
    assert_eq!(res.rows_affected, 1);

    let rows = sql.execute("SELECT val FROM items")?;
    assert_eq!(rows.rows.len(), 3);
    let mut values: Vec<i64> = rows
        .rows
        .iter()
        .map(|r| match r[0] {
            Value::Integer(i) => i,
            _ => panic!("Expected int"),
        })
        .collect();
    values.sort_unstable();
    assert_eq!(values, vec![10, 25, 30]);

    Ok(())
}
