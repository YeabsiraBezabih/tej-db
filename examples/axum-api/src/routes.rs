use crate::AppState;
pub use axum::http::StatusCode;
use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tej_db::KeyRange;

/// Domain Model for API resources
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub description: String,
    pub price: u64,
}

#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub price: u64,
}

#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub limit: Option<usize>,
    pub start_after: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SqlRequest {
    pub sql: String,
}

#[derive(Debug, Serialize)]
pub struct SqlResponse {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub affected_rows: u64,
}

#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ApiStats {
    pub total_pages: u64,
    pub cached_pages: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_hit_rate: f64,
    pub free_pages: usize,
    pub file_size_bytes: u64,
    pub active_read_txns: usize,
}

/// Helper to encode item keys into table namespace: "item:{id}"
pub fn item_key(id: &str) -> Vec<u8> {
    format!("item:{id}").into_bytes()
}

/// Reusable retry loop for Optimistic Concurrency Control (OCC) conflicts
pub fn execute_with_occ_retry<T, F>(
    max_retries: usize,
    mut operation: F,
) -> Result<T, (StatusCode, String)>
where
    F: FnMut() -> Result<T, (StatusCode, String)>,
{
    let mut retries = 0;
    loop {
        match operation() {
            Ok(val) => return Ok(val),
            Err((StatusCode::CONFLICT, msg)) if msg.contains("Write conflict") => {
                retries += 1;
                if retries >= max_retries {
                    return Err((StatusCode::CONFLICT, format!("{msg}, exceeded max retries")));
                }
                std::thread::sleep(Duration::from_millis(2u64.pow(retries as u32)));
            }
            Err(other) => return Err(other),
        }
    }
}

pub fn value_to_json(val: tej_db_sql::Value) -> serde_json::Value {
    match val {
        tej_db_sql::Value::Null => serde_json::Value::Null,
        tej_db_sql::Value::Boolean(b) => serde_json::Value::Bool(b),
        tej_db_sql::Value::Integer(i) => serde_json::Value::Number(i.into()),
        #[allow(clippy::cast_precision_loss)]
        tej_db_sql::Value::Real(r) => serde_json::Number::from_f64(r)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        tej_db_sql::Value::Text(s) => serde_json::Value::String(s),
        tej_db_sql::Value::Blob(b) => {
            let mut hex_str = String::with_capacity(b.len() * 2);
            for byte in &b {
                use std::fmt::Write;
                let _ = write!(&mut hex_str, "{byte:02x}");
            }
            serde_json::Value::String(hex_str)
        }
        tej_db_sql::Value::Timestamp(t) => serde_json::Value::String(t.to_string()),
        tej_db_sql::Value::Uuid(u) => serde_json::Value::String(u.to_string()),
        tej_db_sql::Value::Decimal(d) => serde_json::Value::String(d.to_string()),
        #[allow(clippy::cast_precision_loss)]
        tej_db_sql::Value::Vector(v) => serde_json::Value::Array(
            v.into_iter()
                .filter_map(|f| {
                    serde_json::Number::from_f64(f as f64).map(serde_json::Value::Number)
                })
                .collect(),
        ),
        other => serde_json::Value::String(format!("{other:?}")),
    }
}

/// Route 1: POST /api/items (Create Item with OCC retry capability)
pub async fn create_item(
    State(state): State<AppState>,
    Json(payload): Json<CreateItemRequest>,
) -> impl IntoResponse {
    let db = Arc::clone(&state.db);
    let item = Item {
        id: payload.id,
        name: payload.name,
        description: payload.description,
        price: payload.price,
    };

    let item_for_task = item.clone();
    let result = tokio::task::spawn_blocking(move || {
        let key = item_key(&item_for_task.id);
        let val_bytes = serde_json::to_vec(&item_for_task)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        execute_with_occ_retry(5, || {
            let mut tx = db
                .begin_write()
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            match tx.get(&key) {
                Ok(Some(_)) => {
                    return Err((
                        StatusCode::CONFLICT,
                        format!("Item with id '{}' already exists", item_for_task.id),
                    ));
                }
                Ok(None) => {}
                Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
            }

            if let Err(e) = tx.put(&key, &val_bytes) {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
            }

            match tx.commit() {
                Ok(()) => Ok(item_for_task.clone()),
                Err(tej_db::DbError::WriteConflict {
                    key: conflict_key,
                    committed_tx,
                }) => Err((
                    StatusCode::CONFLICT,
                    format!(
                        "Write conflict on key {:?} (intervening tx {})",
                        String::from_utf8_lossy(&conflict_key),
                        committed_tx
                    ),
                )),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
            }
        })
    })
    .await;

    match result {
        Ok(Ok(created)) => (
            StatusCode::CREATED,
            Json(ApiResponse {
                success: true,
                data: Some(created),
                error: None,
            }),
        ),
        Ok(Err((status, msg))) => (
            status,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(msg),
            }),
        ),
        Err(join_err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Task execution panicked: {join_err}")),
            }),
        ),
    }
}

/// Route 2: GET /api/items/:id (Point lookup via concurrent MVCC snapshot read)
pub async fn get_item(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let db = Arc::clone(&state.db);
    let id_for_task = id.clone();

    let result = tokio::task::spawn_blocking(move || {
        let rx = db
            .begin_read()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let key = item_key(&id_for_task);
        match rx.get(&key) {
            Ok(Some(bytes)) => match serde_json::from_slice::<Item>(&bytes) {
                Ok(item) => Ok(Some(item)),
                Err(e) => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Deserialization error: {e}"),
                )),
            },
            Ok(None) => Ok(None),
            Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        }
    })
    .await;

    match result {
        Ok(Ok(Some(item))) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                data: Some(item),
                error: None,
            }),
        ),
        Ok(Ok(None)) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Item with id '{id}' not found")),
            }),
        ),
        Ok(Err((status, msg))) => (
            status,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(msg),
            }),
        ),
        Err(join_err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Worker panic: {join_err}")),
            }),
        ),
    }
}

/// Route 3: GET /api/items (Range scan across namespace with pagination)
pub async fn list_items(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let db = Arc::clone(&state.db);
    let limit = params.limit.unwrap_or(50).clamp(1, 500);

    let result = tokio::task::spawn_blocking(move || {
        let rx = db
            .begin_read()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let prefix = b"item:".to_vec();
        let mut end_prefix = prefix.clone();
        if let Some(last) = end_prefix.last_mut() {
            *last = last.saturating_add(1);
        }

        let start_bound = if let Some(after) = params.start_after {
            let mut k = item_key(&after);
            k.push(0);
            std::ops::Bound::Included(k)
        } else {
            std::ops::Bound::Included(prefix)
        };

        let range = KeyRange {
            start: start_bound,
            end: std::ops::Bound::Excluded(end_prefix),
        };

        let mut items = Vec::new();
        rx.scan_range_visitor(&range, |_k, v| {
            if let Ok(item) = serde_json::from_slice::<Item>(v) {
                items.push(item);
                if items.len() >= limit {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        Ok(items)
    })
    .await;

    match result {
        Ok(Ok(items)) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                data: Some(items),
                error: None,
            }),
        ),
        Ok(Err((status, msg))) => (
            status,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(msg),
            }),
        ),
        Err(join_err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Worker panic: {join_err}")),
            }),
        ),
    }
}

/// Route 4: DELETE /api/items/:id (Write transaction deleting item with OCC retry)
pub async fn delete_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let db = Arc::clone(&state.db);
    let id_for_task = id.clone();

    let result = tokio::task::spawn_blocking(move || {
        let key = item_key(&id_for_task);
        execute_with_occ_retry(5, || {
            let mut tx = db
                .begin_write()
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let deleted = tx
                .delete(&key)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            if !deleted {
                return Ok(false);
            }

            match tx.commit() {
                Ok(()) => Ok(true),
                Err(tej_db::DbError::WriteConflict {
                    key: conflict_key,
                    committed_tx,
                }) => Err((
                    StatusCode::CONFLICT,
                    format!(
                        "Write conflict on key {:?} (intervening tx {})",
                        String::from_utf8_lossy(&conflict_key),
                        committed_tx
                    ),
                )),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
            }
        })
    })
    .await;

    match result {
        Ok(Ok(true)) => (
            StatusCode::OK,
            Json(ApiResponse::ok(format!("Item '{id}' successfully deleted"))),
        ),
        Ok(Ok(false)) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::err(format!("Item '{id}' does not exist"))),
        ),
        Ok(Err((status, msg))) => (status, Json(ApiResponse::err(msg))),
        Err(join_err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::err(format!("Worker panic: {join_err}"))),
        ),
    }
}

/// Route 5: GET /api/stats (Database Telemetry)
pub async fn get_stats(State(state): State<AppState>) -> impl IntoResponse {
    let stats = state.db.stats();
    Json(ApiStats {
        total_pages: stats.total_pages,
        cached_pages: stats.cached_pages,
        cache_hits: stats.cache_hits,
        cache_misses: stats.cache_misses,
        cache_hit_rate: stats.cache_hit_rate,
        free_pages: stats.free_pages,
        file_size_bytes: stats.file_size_bytes,
        active_read_txns: stats.active_read_txns,
    })
}

/// Route 6: POST /api/sql (Execute arbitrary SQL queries against the DBMS engine)
pub async fn execute_sql(
    State(state): State<AppState>,
    Json(payload): Json<SqlRequest>,
) -> impl IntoResponse {
    let sql_engine = Arc::clone(&state.sql);
    let run_task = tokio::task::spawn_blocking(move || {
        sql_engine.execute(&payload.sql).map_err(|e| {
            let sanitized = match e {
                tej_db_core::DbError::Io(_) => "Storage I/O error".into(),
                _ => e.to_string(),
            };
            (StatusCode::BAD_REQUEST, sanitized)
        })
    });

    let timeout_res = tokio::time::timeout(std::time::Duration::from_secs(10), run_task).await;
    match timeout_res {
        Ok(Ok(Ok(query_res))) => {
            let rows = query_res
                .rows
                .into_iter()
                .map(|r| r.into_iter().map(value_to_json).collect())
                .collect();
            let data = SqlResponse {
                columns: query_res.columns,
                rows,
                affected_rows: query_res.rows_affected as u64,
            };
            (StatusCode::OK, Json(ApiResponse::ok(data)))
        }
        Ok(Ok(Err((status, msg)))) => (status, Json(ApiResponse::err(msg))),
        Ok(Err(join_err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::err(format!(
                "Execution task failed: {join_err}"
            ))),
        ),
        Err(_) => (
            StatusCode::REQUEST_TIMEOUT,
            Json(ApiResponse::err("Query execution timed out (10s limit)")),
        ),
    }
}
