use tej_db::Database;
use tej_db_core::{DbError, PAGE_SIZE, Page, PageId, PageType, page::SLOTS_START_OFFSET};
use tej_db_pager::{MemStorage, PageCache};
use tej_db_sql::{ColumnDef, SqlEngine, SqlType, TableSchema, Value};

#[test]
fn test_sql_i64_min_pk_lookup() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);
    sql.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)")
        .unwrap();
    sql.execute("INSERT INTO t VALUES (-9223372036854775808, 'min')")
        .unwrap();
    let res = sql
        .execute("SELECT v FROM t WHERE id = -9223372036854775808")
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Text("min".into()));
}

#[test]
fn test_sql_uuid_pk_lookup_coercion() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);
    sql.execute("CREATE TABLE items (id UUID PRIMARY KEY, name TEXT)")
        .unwrap();
    sql.execute("INSERT INTO items VALUES ('550e8400-e29b-41d4-a716-446655440000', 'alpha')")
        .unwrap();
    let res = sql
        .execute("SELECT name FROM items WHERE id = '550e8400-e29b-41d4-a716-446655440000'")
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Text("alpha".into()));
}

#[test]
fn test_sql_rejects_with_fetch_comma_join_and_invalid_limit() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);
    sql.execute("CREATE TABLE a (id INT PRIMARY KEY)").unwrap();
    sql.execute("CREATE TABLE b (id INT PRIMARY KEY)").unwrap();

    assert!(
        sql.execute("WITH cte AS (SELECT id FROM a) SELECT id FROM a")
            .is_err()
    );
    assert!(
        sql.execute("SELECT id FROM a FETCH FIRST 1 ROW ONLY")
            .is_err()
    );
    assert!(sql.execute("SELECT a.id FROM a, b").is_err());
    assert!(sql.execute("SELECT id FROM a LIMIT -1").is_err());
    assert!(sql.execute("SELECT id FROM a LIMIT 1 + 1").is_err());
}

#[test]
fn test_sql_short_circuit_and_or() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);
    sql.execute("CREATE TABLE nums (id INT PRIMARY KEY, x INT)")
        .unwrap();
    sql.execute("INSERT INTO nums VALUES (1, 0), (2, 2)")
        .unwrap();

    let res = sql
        .execute("SELECT id FROM nums WHERE x != 0 AND 10 / x > 2")
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], Value::Integer(2));

    let res_or = sql
        .execute("SELECT id FROM nums WHERE x = 0 OR 10 / x > 2")
        .unwrap();
    assert_eq!(res_or.rows.len(), 2);
}

#[test]
fn test_sql_decode_corrupt_num_cols_no_oom() {
    let schema = TableSchema::new(
        "t",
        vec![ColumnDef {
            name: "id".into(),
            data_type: SqlType::Integer,
            is_nullable: false,
            is_primary_key: true,
        }],
    );
    let mut bad = Vec::new();
    bad.extend_from_slice(&u64::MAX.to_le_bytes());
    bad.extend_from_slice(&[0u8; 16]);
    assert!(schema.decode_row_value(&bad).is_err());
}

#[test]
fn test_corrupt_slotted_page_bounds_no_panic() {
    let mut leaf = Page::new(PageId(1), PageType::BTreeLeaf);
    let mut hdr = leaf.read_header().unwrap();
    hdr.item_count = 1;
    leaf.write_header(&hdr);
    // Offset = PAGE_SIZE - 2 (4094), which would panic without offset + 4 <= PAGE_SIZE
    let off_bytes = ((PAGE_SIZE - 2) as u16).to_le_bytes();
    leaf.data[SLOTS_START_OFFSET] = off_bytes[0];
    leaf.data[SLOTS_START_OFFSET + 1] = off_bytes[1];
    assert!(matches!(leaf.leaf_get_key(0), Err(DbError::Corruption(_))));
    assert!(matches!(leaf.leaf_get_val(0), Err(DbError::Corruption(_))));
    assert!(matches!(
        leaf.leaf_scan_range(0, |_, _, _, _| Ok(true)),
        Err(DbError::Corruption(_))
    ));

    let mut internal = Page::new(PageId(2), PageType::BTreeInternal);
    let mut hdr_int = internal.read_header().unwrap();
    hdr_int.item_count = 1;
    internal.write_header(&hdr_int);
    let off_bytes_int = ((PAGE_SIZE - 4) as u16).to_le_bytes();
    internal.data[SLOTS_START_OFFSET] = off_bytes_int[0];
    internal.data[SLOTS_START_OFFSET + 1] = off_bytes_int[1];
    assert!(matches!(
        internal.internal_get_key(0),
        Err(DbError::Corruption(_))
    ));
}

#[test]
fn test_with_page_mut_err_does_not_dirty() {
    let cache = PageCache::new(MemStorage::new(), 16);
    let pid = cache.allocate_page().unwrap();
    let mut p = Page::new(pid, PageType::BTreeLeaf);
    p.update_checksum();
    cache.write_page(p).unwrap();
    cache.flush().unwrap();

    let res: tej_db_core::Result<()> = cache.with_page_mut(pid, |page| {
        let mut h = page.read_header()?;
        h.item_count = 99;
        page.write_header(&h);
        Err(DbError::Custom("abort mutation".into()))
    });
    assert!(res.is_err());

    let restored = cache.get_page(pid).unwrap();
    assert_eq!(restored.item_count(), 0);
}

#[test]
fn test_table_commit_mixed_default_occ_conflict() {
    let db = Database::open_memory().unwrap();
    {
        let mut tx = db.begin_write().unwrap();
        tx.create_table("users").unwrap();
        tx.put(b"shared_key", b"v0").unwrap();
        tx.commit().unwrap();
    }

    let mut tx1 = db.begin_write().unwrap();
    let mut tx2 = db.begin_write().unwrap();

    tx1.put(b"shared_key", b"v1").unwrap();
    {
        let mut tbl = tx1.open_table_mut("users").unwrap();
        tbl.put(b"u1", b"alice").unwrap();
    }

    tx2.put(b"shared_key", b"v2").unwrap();
    tx2.commit().unwrap();

    let res = tx1.commit();
    assert!(matches!(res, Err(DbError::WriteConflict { .. })));
}

#[test]
fn test_sql_not_null_order_by_offset_distinct_limit0() {
    let db = Database::open_memory().unwrap();
    let sql = SqlEngine::new(&db);
    sql.execute("CREATE TABLE scores (id INT PRIMARY KEY, grp INT NOT NULL)")
        .unwrap();
    assert!(sql.execute("INSERT INTO scores VALUES (1, NULL)").is_err());

    sql.execute("INSERT INTO scores VALUES (1, 10), (2, 30), (3, 10), (4, 20)")
        .unwrap();
    assert!(
        sql.execute("UPDATE scores SET grp = NULL WHERE id = 1")
            .is_err()
    );

    let res = sql
        .execute("SELECT DISTINCT grp FROM scores ORDER BY grp DESC LIMIT 2 OFFSET 1")
        .unwrap();
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0][0], Value::Integer(20));
    assert_eq!(res.rows[1][0], Value::Integer(10));

    let lim0 = sql.execute("SELECT id FROM scores LIMIT 0").unwrap();
    assert_eq!(lim0.rows.len(), 0);
}

#[test]
fn test_database_verify_and_generic_close() {
    let db = Database::open_memory().unwrap();
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"a", b"1").unwrap();
        tx.create_table("items").unwrap();
        {
            let mut tbl = tx.open_table_mut("items").unwrap();
            tbl.put(b"k1", b"v1").unwrap();
        }
        tx.commit().unwrap();
    }
    let (tables, keys) = db.verify().unwrap();
    assert_eq!(tables, 2);
    assert_eq!(keys, 2);
    db.close().unwrap();
}
