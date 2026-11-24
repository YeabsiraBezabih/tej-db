use crate::constraint::{CheckConstraint, ForeignKeyConstraint};
use crate::value::Value;
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqlType {
    Integer,
    Real,
    Text,
    Boolean,
    Blob,
    Vector(usize),
    Decimal(u8, u8),
    Uuid,
    Timestamp,
    Interval,
    Json,
    List(Box<SqlType>),
    Struct(Vec<(String, SqlType)>),
    Map(Box<SqlType>, Box<SqlType>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: SqlType,
    pub is_primary_key: bool,
    pub is_nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub pk_indices: Vec<usize>,
    pub foreign_keys: Vec<ForeignKeyConstraint>,
    pub check_constraints: Vec<CheckConstraint>,
}

impl TableSchema {
    pub fn new(name: impl Into<String>, columns: Vec<ColumnDef>) -> Self {
        let name = name.into();
        let mut pk_indices = Vec::new();
        for (i, col) in columns.iter().enumerate() {
            if col.is_primary_key {
                pk_indices.push(i);
            }
        }
        Self {
            name,
            columns,
            pk_indices,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
        }
    }

    pub fn new_with_constraints(
        name: impl Into<String>,
        columns: Vec<ColumnDef>,
        foreign_keys: Vec<ForeignKeyConstraint>,
        check_constraints: Vec<CheckConstraint>,
    ) -> Self {
        let mut s = Self::new(name, columns);
        s.foreign_keys = foreign_keys;
        s.check_constraints = check_constraints;
        s
    }

    #[must_use]
    pub fn column_index(&self, col_name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(col_name))
    }

    /// Extract primary key bytes from a row for the B-tree index key.
    pub fn encode_row_key(&self, row: &[Value], auto_rowid: u64) -> Vec<u8> {
        if self.pk_indices.is_empty() {
            // Implicit rowid primary key
            let mut key = Vec::with_capacity(9);
            key.push(0xFE); // Rowid prefix tag
            key.extend_from_slice(&auto_rowid.to_be_bytes());
            key
        } else if self.pk_indices.len() == 1 {
            // Single PK column
            let pk_idx = self.pk_indices[0];
            row.get(pk_idx)
                .map_or_else(|| vec![0x00], Value::encode_key)
        } else {
            // Compound PK columns: [col_len: 2B][col_bytes]...
            let mut compound = Vec::new();
            for &idx in &self.pk_indices {
                let col_val = row.get(idx).cloned().unwrap_or(Value::Null);
                let encoded = col_val.encode_key();
                compound.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
                compound.extend_from_slice(&encoded);
            }
            compound
        }
    }

    /// Zero-allocation primary key extraction from a row using stack buffers where possible.
    #[allow(clippy::cast_sign_loss)]
    pub fn with_row_key<R>(&self, row: &[Value], auto_rowid: u64, f: impl FnOnce(&[u8]) -> R) -> R {
        if self.pk_indices.is_empty() {
            let mut key = [0u8; 9];
            key[0] = 0xFE;
            key[1..9].copy_from_slice(&auto_rowid.to_be_bytes());
            f(&key)
        } else if self.pk_indices.len() == 1 {
            let pk_idx = self.pk_indices[0];
            match row.get(pk_idx) {
                Some(Value::Integer(i)) => {
                    let mut b = [0u8; 9];
                    b[0] = 0x01;
                    let unsigned = (*i as u64) ^ 0x8000_0000_0000_0000;
                    b[1..9].copy_from_slice(&unsigned.to_be_bytes());
                    f(&b)
                }
                Some(v) => {
                    let enc = v.encode_key();
                    f(&enc)
                }
                None => f(&[0x00]),
            }
        } else {
            let key = self.encode_row_key(row, auto_rowid);
            f(&key)
        }
    }

    /// Encode full row values into binary storage payload.
    pub fn encode_row_value(&self, row: &[Value]) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(32 + row.len() * 16);
        self.encode_row_value_into(row, &mut out)?;
        Ok(out)
    }

    /// Encode row values directly into a reusable destination buffer without heap allocations.
    pub fn encode_row_value_into(&self, row: &[Value], out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        #[allow(clippy::cast_possible_truncation)]
        let num = row.len() as u64;
        out.extend_from_slice(&num.to_le_bytes());
        for v in row {
            match v {
                Value::Integer(i) => {
                    out.extend_from_slice(&1u32.to_le_bytes());
                    out.extend_from_slice(&i.to_le_bytes());
                }
                Value::Text(s) => {
                    out.extend_from_slice(&3u32.to_le_bytes());
                    #[allow(clippy::cast_possible_truncation)]
                    let slen = s.len() as u64;
                    out.extend_from_slice(&slen.to_le_bytes());
                    out.extend_from_slice(s.as_bytes());
                }
                Value::Real(r) => {
                    out.extend_from_slice(&2u32.to_le_bytes());
                    out.extend_from_slice(&r.to_le_bytes());
                }
                Value::Null => {
                    out.extend_from_slice(&0u32.to_le_bytes());
                }
                Value::Boolean(b) => {
                    out.extend_from_slice(&4u32.to_le_bytes());
                    out.push(u8::from(*b));
                }
                Value::Blob(b) => {
                    out.extend_from_slice(&5u32.to_le_bytes());
                    #[allow(clippy::cast_possible_truncation)]
                    let blen = b.len() as u64;
                    out.extend_from_slice(&blen.to_le_bytes());
                    out.extend_from_slice(b);
                }
                _ => {
                    out.clear();
                    return bincode::serialize_into(&mut *out, row).map_err(|e| {
                        DbError::Serialization(format!("Row serialization failed: {e}"))
                    });
                }
            }
        }
        Ok(())
    }

    /// Decode binary storage payload into row values with fast-path deserializer.
    pub fn decode_row_value(&self, bytes: &[u8]) -> Result<Vec<Value>> {
        if bytes.len() < 8 {
            return bincode::deserialize(bytes)
                .map_err(|e| DbError::Serialization(format!("Row deserialization failed: {e}")));
        }

        let num_cols = u64::from_le_bytes(bytes[0..8].try_into().unwrap_or([0; 8])) as usize;
        let mut row = Vec::with_capacity(num_cols);
        let mut pos = 8;
        let total_len = bytes.len();

        for _ in 0..num_cols {
            if pos + 4 > total_len {
                return bincode::deserialize(bytes).map_err(|e| {
                    DbError::Serialization(format!("Row deserialization failed: {e}"))
                });
            }
            let tag = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap_or([0; 4]));
            pos += 4;

            match tag {
                0 => row.push(Value::Null),
                1 => {
                    if pos + 8 > total_len {
                        return bincode::deserialize(bytes).map_err(|e| {
                            DbError::Serialization(format!("Row deserialization failed: {e}"))
                        });
                    }
                    let val = i64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap_or([0; 8]));
                    pos += 8;
                    row.push(Value::Integer(val));
                }
                2 => {
                    if pos + 8 > total_len {
                        return bincode::deserialize(bytes).map_err(|e| {
                            DbError::Serialization(format!("Row deserialization failed: {e}"))
                        });
                    }
                    let val = f64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap_or([0; 8]));
                    pos += 8;
                    row.push(Value::Real(val));
                }
                3 => {
                    if pos + 8 > total_len {
                        return bincode::deserialize(bytes).map_err(|e| {
                            DbError::Serialization(format!("Row deserialization failed: {e}"))
                        });
                    }
                    let slen = u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap_or([0; 8]))
                        as usize;
                    pos += 8;
                    if pos + slen > total_len {
                        return bincode::deserialize(bytes).map_err(|e| {
                            DbError::Serialization(format!("Row deserialization failed: {e}"))
                        });
                    }
                    let s = match std::str::from_utf8(&bytes[pos..pos + slen]) {
                        Ok(valid_str) => valid_str.to_string(),
                        Err(_) => {
                            return bincode::deserialize(bytes).map_err(|e| {
                                DbError::Serialization(format!("Row deserialization failed: {e}"))
                            });
                        }
                    };
                    pos += slen;
                    row.push(Value::Text(s));
                }
                4 => {
                    if pos >= total_len {
                        return bincode::deserialize(bytes).map_err(|e| {
                            DbError::Serialization(format!("Row deserialization failed: {e}"))
                        });
                    }
                    let val = bytes[pos] != 0;
                    pos += 1;
                    row.push(Value::Boolean(val));
                }
                5 => {
                    if pos + 8 > total_len {
                        return bincode::deserialize(bytes).map_err(|e| {
                            DbError::Serialization(format!("Row deserialization failed: {e}"))
                        });
                    }
                    let blen = u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap_or([0; 8]))
                        as usize;
                    pos += 8;
                    if pos + blen > total_len {
                        return bincode::deserialize(bytes).map_err(|e| {
                            DbError::Serialization(format!("Row deserialization failed: {e}"))
                        });
                    }
                    let b = bytes[pos..pos + blen].to_vec();
                    pos += blen;
                    row.push(Value::Blob(b));
                }
                _ => {
                    return bincode::deserialize(bytes).map_err(|e| {
                        DbError::Serialization(format!("Row deserialization failed: {e}"))
                    });
                }
            }
        }

        if pos == total_len {
            Ok(row)
        } else {
            bincode::deserialize(bytes)
                .map_err(|e| DbError::Serialization(format!("Row deserialization failed: {e}")))
        }
    }

    /// Dedicated internal SQL catalog table name, isolated from user KV keyspace.
    pub const SQL_CATALOG_TABLE: &str = "__sys_sql_catalog__";

    /// Schema persistence catalog key inside the dedicated SQL catalog table.
    #[must_use]
    pub fn catalog_key(table_name: &str) -> Vec<u8> {
        table_name.to_ascii_lowercase().into_bytes()
    }
}
