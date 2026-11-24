use super::*;
use tej_db::Database;

#[test]
fn test_sql_ddl_and_dml_full_lifecycle() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    // 1. CREATE TABLE
    let res = sql
        .execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT, active BOOL)")
        .unwrap();
    assert_eq!(res.rows_affected, 0);

    // 2. INSERT INTO
    let res = sql
        .execute(
            "INSERT INTO users (id, name, age, active) VALUES \
                 (1, 'Alice', 30, true), \
                 (2, 'Bob', 25, false), \
                 (3, 'Charlie', 35, true), \
                 (4, 'Diana', 28, true)",
        )
        .unwrap();
    assert_eq!(res.rows_affected, 4);

    // 3. SELECT * (all rows)
    let res = sql.execute("SELECT * FROM users").unwrap();
    assert_eq!(res.columns, vec!["id", "name", "age", "active"]);
    assert_eq!(res.rows.len(), 4);

    // 4. Point Query (Optimized PK Equality: WHERE id = 2)
    let res = sql
        .execute("SELECT name, age FROM users WHERE id = 2")
        .unwrap();
    assert_eq!(res.columns, vec!["name", "age"]);
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Text("Bob".into()));
    assert_eq!(res.rows[0][1], Value::Integer(25));

    // 5. Range Query (Optimized PK Range: WHERE id >= 2 AND id <= 3)
    let res = sql
        .execute("SELECT id, name FROM users WHERE id >= 2 AND id <= 3")
        .unwrap();
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0][0], Value::Integer(2));
    assert_eq!(res.rows[1][0], Value::Integer(3));

    // 6. Filter Query (Non-PK Filter: WHERE age > 28)
    let res = sql
        .execute("SELECT name, age FROM users WHERE age > 28")
        .unwrap();
    assert_eq!(res.rows.len(), 2); // Alice (30) and Charlie (35)
    assert_eq!(res.rows[0][0], Value::Text("Alice".into()));
    assert_eq!(res.rows[1][0], Value::Text("Charlie".into()));

    // 7. UPDATE (WHERE id = 1)
    let res = sql
        .execute("UPDATE users SET age = 31, active = false WHERE id = 1")
        .unwrap();
    assert_eq!(res.rows_affected, 1);

    let res = sql
        .execute("SELECT age, active FROM users WHERE id = 1")
        .unwrap();
    assert_eq!(res.rows[0][0], Value::Integer(31));
    assert_eq!(res.rows[0][1], Value::Boolean(false));

    // 8. DELETE (WHERE id = 3)
    let res = sql.execute("DELETE FROM users WHERE id = 3").unwrap();
    assert_eq!(res.rows_affected, 1);

    let res = sql.execute("SELECT id FROM users WHERE id = 3").unwrap();
    assert_eq!(res.rows.len(), 0);

    // 9. Total remaining rows
    let res = sql.execute("SELECT id FROM users").unwrap();
    assert_eq!(res.rows.len(), 3);
}

#[test]
fn test_sql_projection_and_limit() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE products (sku INT PRIMARY KEY, title TEXT, price REAL)")
        .unwrap();
    sql.execute(
        "INSERT INTO products VALUES \
             (101, 'Mechanical Keyboard', 129.99), \
             (102, 'Wireless Mouse', 49.50), \
             (103, '4K Monitor', 399.00)",
    )
    .unwrap();

    // Project single column with LIMIT
    let res = sql.execute("SELECT title FROM products LIMIT 2").unwrap();
    assert_eq!(res.columns, vec!["title"]);
    assert_eq!(res.rows.len(), 2);
}

#[test]
fn test_sql_tabular_display_formatting() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE inventory (id INT PRIMARY KEY, item TEXT)")
        .unwrap();
    sql.execute("INSERT INTO inventory VALUES (1, 'Widget A'), (2, 'Super Gadget B')")
        .unwrap();

    let res = sql.execute("SELECT * FROM inventory").unwrap();
    let rendered = format!("{res}");
    assert!(rendered.contains("id"));
    assert!(rendered.contains("item"));
    assert!(rendered.contains("Widget A"));
    assert!(rendered.contains("Super Gadget B"));
    assert!(rendered.contains("(2 rows)"));
}

#[test]
fn test_sql_auto_rowid_table() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE logs (message TEXT, level INT)")
        .unwrap();
    sql.execute("INSERT INTO logs VALUES ('Server started', 1), ('Connection error', 3)")
        .unwrap();

    let res = sql.execute("SELECT message, level FROM logs").unwrap();
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0][0], Value::Text("Server started".into()));
    assert_eq!(res.rows[1][0], Value::Text("Connection error".into()));
}

#[test]
fn test_sql_arithmetic_and_null_checks() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE accounts (id INT PRIMARY KEY, balance REAL, note TEXT)")
        .unwrap();
    sql.execute(
        "INSERT INTO accounts VALUES \
             (1, 100.0, 'Regular'), \
             (2, 250.0, NULL), \
             (3, 50.0, 'Overdraft warning')",
    )
    .unwrap();

    // Arithmetic expression in projection
    let res = sql
        .execute("SELECT id, balance + 15.5 FROM accounts WHERE id = 1")
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Integer(1));
    assert_eq!(res.rows[0][1], Value::Real(115.5));

    // IS NULL check
    let res = sql
        .execute("SELECT id FROM accounts WHERE note IS NULL")
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Integer(2));

    // IS NOT NULL check
    let res = sql
        .execute("SELECT id FROM accounts WHERE note IS NOT NULL")
        .unwrap();
    assert_eq!(res.rows.len(), 2);
}

#[test]
fn test_sql_transaction_batching_and_plan_cache() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE products (id INT PRIMARY KEY, name TEXT, price REAL)")
        .unwrap();

    // 1. Batch multiple inserts and an update inside a single WriteTx
    {
        let mut tx = db.begin_write().unwrap();
        for i in 1..=5 {
            let stmt = format!("INSERT INTO products VALUES ({i}, 'Item_{i}', {i}.0)");
            sql.execute_in_tx(&mut tx, &stmt).unwrap();
        }
        sql.execute_in_tx(&mut tx, "UPDATE products SET price = 99.9 WHERE id = 1")
            .unwrap();
        tx.commit().unwrap();
    }

    // Verify committed changes
    let res = sql
        .execute("SELECT price FROM products WHERE id = 1")
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Real(99.9));

    let res_all = sql.execute("SELECT id FROM products").unwrap();
    assert_eq!(res_all.rows.len(), 5);

    // 2. Transaction rollback
    {
        let mut tx = db.begin_write().unwrap();
        sql.execute_in_tx(&mut tx, "INSERT INTO products VALUES (999, 'Ghost', 0.0)")
            .unwrap();
        tx.rollback().unwrap();
    }

    let res_ghost = sql
        .execute("SELECT id FROM products WHERE id = 999")
        .unwrap();
    assert_eq!(res_ghost.rows.len(), 0);

    // 3. Prepared plan execution
    let plan = sql
        .prepare("SELECT name FROM products WHERE id = 2")
        .unwrap();
    let mut tx = db.begin_write().unwrap();
    let res_plan = sql.execute_plan_in_tx(&mut tx, &plan).unwrap();
    assert_eq!(res_plan.rows.len(), 1);
    assert_eq!(res_plan.rows[0][0], Value::Text("Item_2".into()));

    // 4. Parameterized compiled statement execution
    let stmt = sql
        .prepare_statement("UPDATE products SET price = ? WHERE id = ?")
        .unwrap();
    let res_prep = stmt
        .execute_in_tx(&mut tx, &[Value::Real(199.99), Value::Integer(2)])
        .unwrap();
    assert_eq!(res_prep.rows_affected, 1);
    tx.commit().unwrap();

    let res_updated = sql
        .execute("SELECT price FROM products WHERE id = 2")
        .unwrap();
    assert_eq!(res_updated.rows[0][0], Value::Real(199.99));
}

#[test]
fn test_sql_value_operations() {
    let null_val = Value::Null;
    assert!(null_val.is_null());
    assert_eq!(null_val.as_i64(), None);
    assert_eq!(null_val.as_str(), None);

    let int_val = Value::Integer(42);
    assert!(!int_val.is_null());
    assert_eq!(int_val.as_i64(), Some(42));
    assert_eq!(int_val.as_str(), None);

    let str_val = Value::Text("hello".into());
    assert!(!str_val.is_null());
    assert_eq!(str_val.as_i64(), None);
    assert_eq!(str_val.as_str(), Some("hello"));

    let key_bytes = int_val.encode_key();
    assert_ne!(key_bytes.len(), 0);
}

#[test]
fn test_sql_error_handling() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    // Syntax error
    let err = sql.execute("SELECT FROM WHERE");
    assert!(err.is_err());

    // Table not found error
    let err = sql.execute("SELECT * FROM nonexistent_table");
    assert!(err.is_err());
}

#[test]
fn test_sql_vector_column_and_distance_functions() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    // 1. Create table with VECTOR(3) column
    sql.execute("CREATE TABLE embeddings (id INT PRIMARY KEY, emb VECTOR(3), label TEXT)")
        .unwrap();

    // 2. Insert rows with vector literals
    sql.execute("INSERT INTO embeddings VALUES (1, [1.0, 0.0, 0.0], 'x-axis')")
        .unwrap();
    sql.execute("INSERT INTO embeddings VALUES (2, [0.0, 1.0, 0.0], 'y-axis')")
        .unwrap();
    sql.execute("INSERT INTO embeddings VALUES (3, [0.7071, 0.7071, 0.0], 'diagonal')")
        .unwrap();

    // 3. Query with cosine_distance function
    let res = sql
        .execute("SELECT id, label, cosine_distance(emb, [1.0, 0.0, 0.0]) FROM embeddings")
        .unwrap();
    assert_eq!(res.rows.len(), 3);

    // Row 1 (x-axis) cosine distance to [1.0, 0.0, 0.0] should be ~0.0
    if let Value::Real(dist) = res.rows[0][2] {
        assert!(dist < 1e-4, "Expected dist close to 0.0, got {dist}");
    } else {
        panic!("Expected Real distance value");
    }

    // Row 2 (y-axis) cosine distance to [1.0, 0.0, 0.0] should be ~1.0
    if let Value::Real(dist) = res.rows[1][2] {
        assert!(
            (dist - 1.0).abs() < 1e-4,
            "Expected dist close to 1.0, got {dist}"
        );
    } else {
        panic!("Expected Real distance value");
    }

    // 4. Query with l2_distance function
    let res_l2 = sql
        .execute("SELECT id, l2_distance(emb, [1.0, 0.0, 0.0]) FROM embeddings")
        .unwrap();
    assert_eq!(res_l2.rows.len(), 3);
    if let Value::Real(dist) = res_l2.rows[0][1] {
        assert!(dist < 1e-4, "Expected L2 dist close to 0.0, got {dist}");
    } else {
        panic!("Expected Real distance value");
    }
}

#[test]
fn test_schema_row_serialization_fast_path() {
    let schema = TableSchema::new(
        "test_table",
        vec![
            crate::schema::ColumnDef {
                name: "id".into(),
                data_type: crate::schema::SqlType::Integer,
                is_primary_key: true,
                is_nullable: false,
            },
            crate::schema::ColumnDef {
                name: "name".into(),
                data_type: crate::schema::SqlType::Text,
                is_primary_key: false,
                is_nullable: false,
            },
            crate::schema::ColumnDef {
                name: "val".into(),
                data_type: crate::schema::SqlType::Integer,
                is_primary_key: false,
                is_nullable: true,
            },
        ],
    );

    let row = vec![
        Value::Integer(42),
        Value::Text("Antigravity".into()),
        Value::Null,
        Value::Boolean(true),
        Value::Real(std::f64::consts::PI),
    ];

    let mut buf = Vec::new();
    schema.encode_row_value_into(&row, &mut buf).unwrap();
    let bincode_expected = bincode::serialize(&row).unwrap();
    assert_eq!(
        buf, bincode_expected,
        "Direct encoding must match bincode wire format exactly!"
    );

    let decoded = schema.decode_row_value(&buf).unwrap();
    assert_eq!(decoded, row, "Direct decoding must match original values!");
}

#[test]
fn test_sql_overflow_and_security_panics() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);

    sql.execute("CREATE TABLE num_test (id INT PRIMARY KEY, val INT)")
        .unwrap();
    // -9223372036854775807 fits in i64, then subtracting 1 yields exactly i64::MIN
    sql.execute("INSERT INTO num_test VALUES (1, -9223372036854775807)")
        .unwrap();

    // (val - 1) is i64::MIN. (val - 1) / -1 must return error, not crash with SIGFPE
    let div_overflow = sql.execute("SELECT (val - 1) / -1 FROM num_test WHERE id = 1");
    assert!(div_overflow.is_err(), "i64::MIN / -1 must return Error");

    // -(i64::MIN) must return error
    let neg_overflow = sql.execute("SELECT -(val - 1) FROM num_test WHERE id = 1");
    assert!(neg_overflow.is_err(), "-i64::MIN must return Error");

    // NULL arithmetic propagation
    sql.execute("CREATE TABLE null_arith (id INT PRIMARY KEY, a INT, b REAL)")
        .unwrap();
    sql.execute("INSERT INTO null_arith VALUES (1, NULL, 5.0)")
        .unwrap();
    let res_null = sql
        .execute("SELECT a + 10, a * 2, a / 5 FROM null_arith WHERE id = 1")
        .unwrap();
    assert_eq!(res_null.rows[0][0], Value::Null);
    assert_eq!(res_null.rows[0][1], Value::Null);
    assert_eq!(res_null.rows[0][2], Value::Null);

    // 3VL: FALSE AND NULL -> FALSE
    sql.execute("CREATE TABLE logic_test (id INT PRIMARY KEY, flag BOOL)")
        .unwrap();
    sql.execute("INSERT INTO logic_test VALUES (1, NULL)")
        .unwrap();
    let res_and = sql
        .execute("SELECT (flag AND FALSE), (flag OR TRUE) FROM logic_test WHERE id = 1")
        .unwrap();
    assert_eq!(res_and.rows[0][0], Value::Boolean(false));
    assert_eq!(res_and.rows[0][1], Value::Boolean(true));

    // Catalog table isolation: writing into default KV does not corrupt SQL table schema
    let mut write_tx = db.begin_write().unwrap();
    write_tx
        .put(b"__sys:schema:num_test", b"malicious_garbage")
        .unwrap();
    write_tx.commit().unwrap();

    let check_schema = sql.get_schema("num_test").unwrap();
    assert!(check_schema.is_some(), "Schema must remain intact");
    assert_eq!(check_schema.unwrap().name, "num_test");
}
