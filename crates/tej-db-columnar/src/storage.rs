use crate::row_group::RowGroup;
use tej_db_core::{DbError, Result};

/// Storage serialization utilities for columnar RowGroups.
#[derive(Debug, Clone, Copy)]
pub struct RowGroupStorage;

impl RowGroupStorage {
    /// Serializes a complete RowGroup into a self-contained binary payload.
    pub fn serialize(row_group: &RowGroup) -> Result<Vec<u8>> {
        bincode::serialize(row_group)
            .map_err(|e| DbError::Serialization(format!("RowGroup serialization error: {e}")))
    }

    /// Deserializes a RowGroup from a binary payload.
    pub fn deserialize(bytes: &[u8]) -> Result<RowGroup> {
        bincode::deserialize(bytes)
            .map_err(|e| DbError::Serialization(format!("RowGroup deserialization error: {e}")))
    }
}
