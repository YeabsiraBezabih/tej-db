use tej_db::Database;
use tej_db_sql::{Decimal, SqlEngine, Timestamp, Uuid, Value};

#[test]
fn test_sql_advanced_types_parity() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    // 1. DECIMAL
    sql.execute("CREATE TABLE ledger (id INT PRIMARY KEY, balance DECIMAL(18, 4))")
        .unwrap();
    sql.execute("INSERT INTO ledger VALUES (1, '1250.7500')")
        .unwrap();
    sql.execute("INSERT INTO ledger VALUES (2, '320.1250')")
        .unwrap();

    let res = sql
        .execute("SELECT id, balance FROM ledger WHERE id = 1")
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Integer(1));
    assert_eq!(res.rows[0][1], Value::Decimal(Decimal::new(12_507_500, 4)));

    // 2. UUID
    sql.execute("CREATE TABLE entities (id UUID PRIMARY KEY, name TEXT)")
        .unwrap();
    let uuid_str = "a1a2a3a4-b1b2-c1c2-d1d2-d3d4d5d6d7d8";
    sql.execute(&format!(
        "INSERT INTO entities VALUES ('{uuid_str}', 'Node A')"
    ))
    .unwrap();

    let res = sql.execute("SELECT id, name FROM entities").unwrap();
    assert_eq!(res.rows.len(), 1);
    let expected_uuid = uuid_str.parse::<Uuid>().unwrap();
    assert_eq!(res.rows[0][0], Value::Uuid(expected_uuid));
    assert_eq!(res.rows[0][1], Value::Text("Node A".into()));

    // 3. TIMESTAMP
    sql.execute("CREATE TABLE events (id INT PRIMARY KEY, ts TIMESTAMP)")
        .unwrap();
    sql.execute("INSERT INTO events VALUES (1, '2024-01-15T12:00:00Z')")
        .unwrap();

    let res = sql.execute("SELECT id, ts FROM events").unwrap();
    assert_eq!(res.rows.len(), 1);
    let expected_ts = "2024-01-15T12:00:00Z".parse::<Timestamp>().unwrap();
    assert_eq!(res.rows[0][1], Value::Timestamp(expected_ts));
}

#[test]
fn test_sql_check_constraints() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE accounts (id INT PRIMARY KEY, balance INT, CHECK (balance >= 0))")
        .unwrap();

    // Valid insert
    let ok = sql.execute("INSERT INTO accounts VALUES (1, 100)");
    assert!(ok.is_ok());

    // Invalid insert violating CHECK
    let err = sql.execute("INSERT INTO accounts VALUES (2, -50)");
    assert!(err.is_err());
    let err_msg = err.unwrap_err().to_string();
    assert!(err_msg.contains("CHECK constraint violation"));

    // Invalid update violating CHECK
    let update_err = sql.execute("UPDATE accounts SET balance = -10 WHERE id = 1");
    assert!(update_err.is_err());
    assert!(
        update_err
            .unwrap_err()
            .to_string()
            .contains("CHECK constraint violation")
    );

    // Verify balance unchanged
    let res = sql
        .execute("SELECT balance FROM accounts WHERE id = 1")
        .unwrap();
    assert_eq!(res.rows[0][0], Value::Integer(100));
}

#[test]
fn test_sql_foreign_key_constraints() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT)")
        .unwrap();
    sql.execute(
        "CREATE TABLE orders (order_id INT PRIMARY KEY, user_id INT, FOREIGN KEY (user_id) REFERENCES users(id))",
    )
    .unwrap();

    sql.execute("INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')")
        .unwrap();

    // Valid FK insert
    let ok = sql.execute("INSERT INTO orders VALUES (101, 1)");
    assert!(ok.is_ok());

    // Invalid FK insert
    let err = sql.execute("INSERT INTO orders VALUES (102, 999)");
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("FOREIGN KEY constraint violation")
    );

    // NULL FK insert (allowed by standard SQL)
    let null_ok = sql.execute("INSERT INTO orders VALUES (103, NULL)");
    assert!(null_ok.is_ok());

    let res = sql.execute("SELECT order_id FROM orders").unwrap();
    assert_eq!(res.rows.len(), 2);
}

#[test]
fn test_sql_upsert_on_conflict() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE counters (id INT PRIMARY KEY, val INT)")
        .unwrap();
    sql.execute("INSERT INTO counters VALUES (1, 10)").unwrap();

    // ON CONFLICT DO NOTHING
    sql.execute("INSERT INTO counters VALUES (1, 99) ON CONFLICT (id) DO NOTHING")
        .unwrap();
    let res = sql
        .execute("SELECT val FROM counters WHERE id = 1")
        .unwrap();
    assert_eq!(res.rows[0][0], Value::Integer(10));

    // ON CONFLICT DO UPDATE
    sql.execute("INSERT INTO counters VALUES (1, 20) ON CONFLICT (id) DO UPDATE SET val = 20")
        .unwrap();
    let res = sql
        .execute("SELECT val FROM counters WHERE id = 1")
        .unwrap();
    assert_eq!(res.rows[0][0], Value::Integer(20));
}

#[test]
fn test_sql_returning_clause() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE items (id INT PRIMARY KEY, title TEXT, price INT)")
        .unwrap();

    // 1. INSERT RETURNING
    let res = sql
        .execute("INSERT INTO items VALUES (1, 'Mechanical Keyboard', 120) RETURNING id, title")
        .unwrap();
    assert_eq!(res.columns, vec!["id", "title"]);
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Integer(1));
    assert_eq!(res.rows[0][1], Value::Text("Mechanical Keyboard".into()));

    // 2. UPDATE RETURNING
    let res = sql
        .execute("UPDATE items SET price = 140 WHERE id = 1 RETURNING price")
        .unwrap();
    assert_eq!(res.columns, vec!["price"]);
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Integer(140));

    // 3. DELETE RETURNING
    let res = sql
        .execute("DELETE FROM items WHERE id = 1 RETURNING title")
        .unwrap();
    assert_eq!(res.columns, vec!["title"]);
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Text("Mechanical Keyboard".into()));

    // Verify table is now empty
    let res = sql.execute("SELECT * FROM items").unwrap();
    assert_eq!(res.rows.len(), 0);
}

#[test]
fn test_sql_savepoint_rollback_and_release() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE logs (id INT PRIMARY KEY, message TEXT)")
        .unwrap();

    let mut tx = db.begin_write().unwrap();

    sql.execute_in_tx(&mut tx, "INSERT INTO logs VALUES (1, 'initial')")
        .unwrap();
    sql.execute_in_tx(&mut tx, "SAVEPOINT sp1").unwrap();

    sql.execute_in_tx(&mut tx, "INSERT INTO logs VALUES (2, 'step 2')")
        .unwrap();
    sql.execute_in_tx(&mut tx, "SAVEPOINT sp2").unwrap();

    sql.execute_in_tx(
        &mut tx,
        "INSERT INTO logs VALUES (3, 'step 3 to be undone')",
    )
    .unwrap();

    // Rollback to sp2 (should undo step 3)
    sql.execute_in_tx(&mut tx, "ROLLBACK TO sp2").unwrap();

    // Release sp1
    sql.execute_in_tx(&mut tx, "RELEASE SAVEPOINT sp1").unwrap();

    tx.commit().unwrap();

    // Verify on a fresh read
    let res = sql.execute("SELECT id, message FROM logs").unwrap();
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0][0], Value::Integer(1));
    assert_eq!(res.rows[1][0], Value::Integer(2));
}
