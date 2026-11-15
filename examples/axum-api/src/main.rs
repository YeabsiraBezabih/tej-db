use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tej_db::{Database, FileStorage, KeyRange};
use tower_http::trace::TraceLayer;
use tracing::info;

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

#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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

/// Shared Application State wrapping the embedded DBMS engine
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database<FileStorage>>,
}

impl AppState {
    pub fn new(db: Database<FileStorage>) -> Self {
        Self { db: Arc::new(db) }
    }
}

/// Helper to encode item keys into table namespace: "item:{id}"
fn item_key(id: &str) -> Vec<u8> {
    format!("item:{id}").into_bytes()
}

// ---------------------------------------------------------------------------
// 4 REST API Route Handlers
// ---------------------------------------------------------------------------

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

    // Offload synchronous DBMS file I/O to Tokio's blocking threadpool
    let item_for_task = item.clone();
    let result = tokio::task::spawn_blocking(move || {
        let key = item_key(&item_for_task.id);
        let val_bytes = serde_json::to_vec(&item_for_task)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        // Retry loop for Optimistic Concurrency Control (OCC) conflicts
        const MAX_RETRIES: usize = 5;
        let mut retries = 0;

        loop {
            let mut tx = match db.begin_write() {
                Ok(tx) => tx,
                Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
            };

            // Check if key already exists (Snapshot read within transaction)
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
                Ok(()) => {
                    return Ok(item_for_task);
                }
                Err(tej_db::DbError::WriteConflict { key: conflict_key, committed_tx }) => {
                    retries += 1;
                    if retries >= MAX_RETRIES {
                        return Err((
                            StatusCode::CONFLICT,
                            format!(
                                "Write conflict on key {:?} (intervening tx {}), exceeded max retries",
                                String::from_utf8_lossy(&conflict_key),
                                committed_tx
                            ),
                        ));
                    }
                    // Exponential backoff before OCC retry
                    std::thread::sleep(Duration::from_millis(2u64.pow(retries as u32)));
                }
                Err(e) => {
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
                }
            }
        }
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
            k.push(0); // Exclude the starting key itself
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
                    return Ok(false); // Halt scan once limit reached
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

/// Route 4: DELETE /api/items/:id (Write transaction deleting item)
pub async fn delete_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let db = Arc::clone(&state.db);
    let id_for_task = id.clone();

    let result = tokio::task::spawn_blocking(move || {
        let key = item_key(&id_for_task);
        let mut tx = db
            .begin_write()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let deleted = tx
            .delete(&key)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if !deleted {
            return Ok(false);
        }

        tx.commit()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        Ok(true)
    })
    .await;

    match result {
        Ok(Ok(true)) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                data: Some(format!("Item '{id}' successfully deleted")),
                error: None,
            }),
        ),
        Ok(Ok(false)) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Item '{id}' does not exist")),
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

/// Optional Route 5: GET /api/stats (Database Telemetry)
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

/// Build the Axum router with the 4 routes
pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/api/items", post(create_item).get(list_items))
        .route("/api/items/{id}", get(get_item).delete(delete_item))
        .route("/api/stats", get(get_stats))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let db_path = std::env::var("TEJ_DB_PATH").unwrap_or_else(|_| "tej_axum_data.db".to_string());
    info!("Opening database at {}", db_path);

    let db = Database::open_file(&db_path)?;
    let state = AppState::new(db);
    let app = create_router(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests;
