use core::cmp::Ordering;
use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

/// Structured JSON representation with fast JSONPath extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonB(pub String);

impl JsonB {
    /// Creates a new `JsonB` from a raw JSON string.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Returns the JSON string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Extracts a JSON sub-element or field using a path syntax (e.g. `foo.bar`, `0`).
    #[must_use]
    pub fn extract_path(&self, path: &str) -> Option<String> {
        let val: serde_json::Value = serde_json::from_str(&self.0).ok()?;
        let mut cur = &val;

        let segments = path.split('.');
        for seg in segments {
            if let Ok(idx) = seg.parse::<usize>() {
                cur = cur.get(idx)?;
            } else {
                cur = cur.get(seg)?;
            }
        }

        match cur {
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }

    /// Encodes into big-endian order-preserving binary bytes for B-tree index keys.
    #[must_use]
    pub fn encode_key(&self) -> Vec<u8> {
        self.0.as_bytes().to_vec()
    }
}

impl FromStr for JsonB {
    type Err = DbError;

    fn from_str(s: &str) -> Result<Self> {
        // Validate JSON syntax
        let _: serde_json::Value = serde_json::from_str(s)
            .map_err(|e| DbError::Custom(format!("Invalid JSON payload: {e}")))?;
        Ok(Self(s.to_string()))
    }
}

impl PartialOrd for JsonB {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JsonB {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl fmt::Display for JsonB {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
