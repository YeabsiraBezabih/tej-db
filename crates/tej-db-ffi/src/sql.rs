use serde::Serialize;
use std::ffi::{CString, c_char};
use tej_db_core::DbError;

use crate::error::{TEJ_OK, catch_ffi};
use crate::handle::{DbInstance, TejDbHandle, c_str_to_str, check_not_null, check_not_null_mut};

#[derive(Serialize)]
struct SqlJsonResult<'a> {
    columns: &'a [String],
    rows: Vec<Vec<serde_json::Value>>,
    rows_affected: usize,
}

fn value_to_json(v: &tej_db_sql::Value) -> serde_json::Value {
    match v {
        tej_db_sql::Value::Null => serde_json::Value::Null,
        tej_db_sql::Value::Integer(i) => serde_json::json!(i),
        tej_db_sql::Value::Real(f) => serde_json::json!(f),
        tej_db_sql::Value::Text(s) => serde_json::json!(s),
        tej_db_sql::Value::Boolean(b) => serde_json::json!(b),
        tej_db_sql::Value::Blob(b) => serde_json::json!(b),
        tej_db_sql::Value::Decimal(d) => serde_json::json!(d.to_string()),
        tej_db_sql::Value::Uuid(u) => serde_json::json!(u.to_string()),
        tej_db_sql::Value::Timestamp(t) => serde_json::json!(t.to_string()),
        tej_db_sql::Value::Interval(i) => serde_json::json!(i.to_string()),
        tej_db_sql::Value::Json(j) => {
            serde_json::from_str(j.as_str()).unwrap_or_else(|_| serde_json::json!(j.as_str()))
        }
        tej_db_sql::Value::Vector(vec) => serde_json::json!(vec),
        tej_db_sql::Value::List(items) => {
            serde_json::Value::Array(items.iter().map(value_to_json).collect())
        }
        tej_db_sql::Value::Struct(fields) => {
            let mut map = serde_json::Map::new();
            for (k, val) in fields {
                map.insert(k.clone(), value_to_json(val));
            }
            serde_json::Value::Object(map)
        }
        tej_db_sql::Value::Map(entries) => {
            let mut map = serde_json::Map::new();
            for (k, val) in entries {
                map.insert(k.to_string(), value_to_json(val));
            }
            serde_json::Value::Object(map)
        }
    }
}

/// Executes an arbitrary SQL statement and returns the resulting rows or metadata as JSON.
///
/// # Safety
/// `db`, `sql`, and `json_out` must be valid non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_sql_execute(
    db: *mut TejDbHandle,
    sql: *const c_char,
    json_out: *mut *mut c_char,
) -> i32 {
    catch_ffi(|| {
        check_not_null(db, "db")?;
        check_not_null_mut(json_out, "json_out")?;

        // SAFETY: db and sql validated non-null; sql is valid null-terminated UTF-8.
        let sql_str = unsafe { c_str_to_str(sql, "sql")? };
        let handle = unsafe { &*db };

        let result = match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            DbInstance::File { sql, .. } => sql.execute(sql_str)?,
            DbInstance::Mem { sql, .. } => sql.execute(sql_str)?,
        };

        let json_rows: Vec<Vec<serde_json::Value>> = result
            .rows
            .iter()
            .map(|r| r.iter().map(value_to_json).collect())
            .collect();
        let json_payload = SqlJsonResult {
            columns: &result.columns,
            rows: json_rows,
            rows_affected: result.rows_affected,
        };

        let json_str = serde_json::to_string(&json_payload)
            .map_err(|e| DbError::Custom(format!("JSON serialization error: {e}")))?;

        let c_str = CString::new(json_str)
            .map_err(|e| DbError::Custom(format!("CString creation error: {e}")))?;

        // SAFETY: json_out is validated non-null and writable.
        unsafe {
            *json_out = c_str.into_raw();
        }
        Ok(TEJ_OK)
    })
}

/// Frees a C string allocated by `tej-db-ffi`.
///
/// # Safety
/// `str_ptr` must have been returned by an FFI call such as `tej_db_sql_execute`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_free_string(str_ptr: *mut c_char) -> i32 {
    if !str_ptr.is_null() {
        // SAFETY: Pointer is non-null and originated from CString::into_raw.
        unsafe {
            drop(CString::from_raw(str_ptr));
        }
    }
    TEJ_OK
}
