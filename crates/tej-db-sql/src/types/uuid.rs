use core::cmp::Ordering;
use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

/// 16-byte Universally Unique Identifier (UUID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Uuid(pub [u8; 16]);

impl Uuid {
    /// Creates a UUID from raw 16 bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Nil (all zero) UUID.
    #[must_use]
    pub const fn nil() -> Self {
        Self([0u8; 16])
    }

    /// Returns the raw 16-byte buffer.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Encodes into big-endian order-preserving binary bytes for B-tree index keys.
    #[must_use]
    pub fn encode_key(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl FromStr for Uuid {
    type Err = DbError;

    fn from_str(s: &str) -> Result<Self> {
        let cleaned: String = s.chars().filter(|&c| c != '-').collect();
        if cleaned.len() != 32 {
            return Err(DbError::Custom(format!(
                "Invalid UUID length: expected 32 hex characters, got {}",
                cleaned.len()
            )));
        }

        let mut bytes = [0u8; 16];
        for (i, byte) in bytes.iter_mut().enumerate() {
            let hex_slice = &cleaned[i * 2..i * 2 + 2];
            *byte = u8::from_str_radix(hex_slice, 16)
                .map_err(|e| DbError::Custom(format!("Invalid UUID hex character: {e}")))?;
        }

        Ok(Self(bytes))
    }
}

impl PartialOrd for Uuid {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Uuid {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[0],
            b[1],
            b[2],
            b[3],
            b[4],
            b[5],
            b[6],
            b[7],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15]
        )
    }
}
