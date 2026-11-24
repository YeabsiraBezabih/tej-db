pub use axum::http::StatusCode;
use axum::{
    Router,
    routing::{get, post},
};
use std::net::SocketAddr;
use std::sync::Arc;
use tej_db::{Database, FileStorage};
use tej_db_sql::SqlEngine;
use tower_http::trace::TraceLayer;
use tracing::info;

pub mod models;
pub mod routes;
pub use models::*;
pub use routes::*;

/// Shared Application State wrapping the embedded DBMS engine
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database<FileStorage>>,
    pub sql: Arc<SqlEngine<FileStorage>>,
}

impl AppState {
    pub fn new(db: Database<FileStorage>) -> Self {
        let sql = Arc::new(SqlEngine::new(&db));
        Self {
            db: Arc::new(db),
            sql,
        }
    }
}

/// Build the Axum router with REST and SQL query routes
pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/api/items", post(create_item).get(list_items))
        .route("/api/items/{id}", get(get_item).delete(delete_item))
        .route("/api/stats", get(get_stats))
        .route("/api/sql", post(execute_sql))
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

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests;
