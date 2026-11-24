use serde::{Deserialize, Serialize};

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
