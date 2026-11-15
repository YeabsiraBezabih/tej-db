use crate::schema::SqlType;
use crate::types::{Decimal, Interval, JsonB, Timestamp, Uuid};
use core::cmp::Ordering;
use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

/// Dynamically typed SQL value.
#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Boolean(bool),
    Blob(Vec<u8>),
    Vector(Vec<f32>),
    Decimal(Decimal),
    Uuid(Uuid),
    Timestamp(Timestamp),
    Interval(Interval),
    Json(JsonB),
    List(Vec<Value>),
    Struct(Vec<(String, Value)>),
    Map(Vec<(Value, Value)>),
}

impl Value {
    pub fn coerce(&self, target_type: &SqlType) -> Result<Self> {
        match (self, target_type) {
            (Self::Null, _) => Ok(Self::Null),
            (v, SqlType::Decimal(_p, s)) => match v {
                Self::Decimal(d) => Ok(Self::Decimal(d.rescale(*s))),
                Self::Text(text) => {
                    let d = Decimal::from_str(text).map_err(|e| {
                        DbError::Custom(format!("Invalid decimal literal '{text}': {e}"))
                    })?;
                    Ok(Self::Decimal(d.rescale(*s)))
                }
                Self::Integer(i) => {
                    let d = Decimal::new(i128::from(*i), 0);
                    Ok(Self::Decimal(d.rescale(*s)))
                }
                Self::Real(r) => {
                    let text = format!("{r}");
                    let d = Decimal::from_str(&text).map_err(|e| {
                        DbError::Custom(format!("Invalid decimal conversion '{r}': {e}"))
                    })?;
                    Ok(Self::Decimal(d.rescale(*s)))
                }
                _ => Ok(self.clone()),
            },
            (Self::Text(text), SqlType::Uuid) => {
                let u = Uuid::from_str(text)
                    .map_err(|e| DbError::Custom(format!("Invalid UUID literal '{text}': {e}")))?;
                Ok(Self::Uuid(u))
            }
            (Self::Text(text), SqlType::Timestamp) => {
                let ts = Timestamp::from_str(text).map_err(|e| {
                    DbError::Custom(format!("Invalid timestamp literal '{text}': {e}"))
                })?;
                Ok(Self::Timestamp(ts))
            }
            (Self::Text(text), SqlType::Interval) => {
                let iv = Interval::from_str(text).map_err(|e| {
                    DbError::Custom(format!("Invalid interval literal '{text}': {e}"))
                })?;
                Ok(Self::Interval(iv))
            }
            (Self::Text(text), SqlType::Json) => Ok(Self::Json(JsonB::new(text.clone()))),
            _ => Ok(self.clone()),
        }
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(i) => Some(*i),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            Self::Json(j) => Some(j.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_vector(&self) -> Option<&[f32]> {
        match self {
            Self::Vector(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// Zero-allocation key byte extraction for primary keys using stack buffers.
    #[allow(clippy::cast_sign_loss)]
    pub fn with_key_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        match self {
            Self::Null => f(&[0x00]),
            Self::Integer(i) => {
                let mut b = [0u8; 9];
                b[0] = 0x01;
                let unsigned = (*i as u64) ^ 0x8000_0000_0000_0000;
                b[1..9].copy_from_slice(&unsigned.to_be_bytes());
                f(&b)
            }
            Self::Real(r) => {
                let mut b = [0u8; 9];
                b[0] = 0x02;
                let bits = r.to_bits();
                let encoded = if *r >= 0.0 {
                    bits ^ 0x8000_0000_0000_0000
                } else {
                    !bits
                };
                b[1..9].copy_from_slice(&encoded.to_be_bytes());
                f(&b)
            }
            Self::Boolean(b) => f(&[0x04, u8::from(*b)]),
            Self::Text(s) if s.len() <= 64 => {
                let mut b = [0u8; 65];
                b[0] = 0x03;
                b[1..=s.len()].copy_from_slice(s.as_bytes());
                f(&b[..=s.len()])
            }
            _ => {
                let key = self.encode_key();
                f(&key)
            }
        }
    }

    /// Encode value into big-endian order-preserving binary bytes for B-tree index keys.
    #[must_use]
    #[allow(clippy::cast_sign_loss)]
    pub fn encode_key(&self) -> Vec<u8> {
        match self {
            Self::Null => vec![0x00],
            Self::Integer(i) => {
                let mut b = Vec::with_capacity(9);
                b.push(0x01);
                // Flip sign bit for correct 2's complement ordering
                let unsigned = (*i as u64) ^ 0x8000_0000_0000_0000;
                b.extend_from_slice(&unsigned.to_be_bytes());
                b
            }
            Self::Real(f) => {
                let mut b = Vec::with_capacity(9);
                b.push(0x02);
                let bits = f.to_bits();
                let encoded = if *f >= 0.0 {
                    bits ^ 0x8000_0000_0000_0000
                } else {
                    !bits
                };
                b.extend_from_slice(&encoded.to_be_bytes());
                b
            }
            Self::Text(s) => {
                let mut b = Vec::with_capacity(1 + s.len());
                b.push(0x03);
                b.extend_from_slice(s.as_bytes());
                b
            }
            Self::Boolean(v) => {
                vec![0x04, u8::from(*v)]
            }
            Self::Blob(bytes) => {
                let mut b = Vec::with_capacity(1 + bytes.len());
                b.push(0x05);
                b.extend_from_slice(bytes);
                b
            }
            Self::Vector(vec) => {
                let mut b = Vec::with_capacity(1 + vec.len() * 4);
                b.push(0x06);
                for &f in vec {
                    b.extend_from_slice(&f.to_bits().to_be_bytes());
                }
                b
            }
            Self::Decimal(d) => {
                let mut b = Vec::with_capacity(18);
                b.push(0x07);
                b.extend_from_slice(&d.encode_key());
                b
            }
            Self::Uuid(u) => {
                let mut b = Vec::with_capacity(17);
                b.push(0x08);
                b.extend_from_slice(&u.encode_key());
                b
            }
            Self::Timestamp(ts) => {
                let mut b = Vec::with_capacity(9);
                b.push(0x09);
                b.extend_from_slice(&ts.encode_key());
                b
            }
            Self::Interval(inv) => {
                let mut b = Vec::with_capacity(17);
                b.push(0x0A);
                b.extend_from_slice(&inv.encode_key());
                b
            }
            Self::Json(j) => {
                let mut b = Vec::with_capacity(1 + j.as_str().len());
                b.push(0x0B);
                b.extend_from_slice(&j.encode_key());
                b
            }
            Self::List(list) => {
                let mut b = vec![0x0C];
                for item in list {
                    let enc = item.encode_key();
                    #[allow(clippy::cast_possible_truncation)]
                    b.extend_from_slice(&(enc.len() as u16).to_be_bytes());
                    b.extend_from_slice(&enc);
                }
                b
            }
            Self::Struct(fields) => {
                let mut b = vec![0x0D];
                for (name, val) in fields {
                    b.extend_from_slice(&(name.len() as u16).to_be_bytes());
                    b.extend_from_slice(name.as_bytes());
                    let enc = val.encode_key();
                    #[allow(clippy::cast_possible_truncation)]
                    b.extend_from_slice(&(enc.len() as u16).to_be_bytes());
                    b.extend_from_slice(&enc);
                }
                b
            }
            Self::Map(entries) => {
                let mut b = vec![0x0E];
                for (k, v) in entries {
                    let k_enc = k.encode_key();
                    let v_enc = v.encode_key();
                    #[allow(clippy::cast_possible_truncation)]
                    b.extend_from_slice(&(k_enc.len() as u16).to_be_bytes());
                    b.extend_from_slice(&k_enc);
                    #[allow(clippy::cast_possible_truncation)]
                    b.extend_from_slice(&(v_enc.len() as u16).to_be_bytes());
                    b.extend_from_slice(&v_enc);
                }
                b
            }
        }
    }

    /// Evaluate SQL comparison between two values.
    #[must_use]
    #[allow(clippy::cast_precision_loss, clippy::match_same_arms)]
    pub fn sql_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Null, _) | (_, Self::Null) => None,
            (Self::Integer(a), Self::Integer(b)) => Some(a.cmp(b)),
            (Self::Real(a), Self::Real(b)) => a.partial_cmp(b),
            (Self::Integer(a), Self::Real(b)) => (*a as f64).partial_cmp(b),
            (Self::Real(a), Self::Integer(b)) => a.partial_cmp(&(*b as f64)),
            (Self::Text(a), Self::Text(b)) => Some(a.cmp(b)),
            (Self::Boolean(a), Self::Boolean(b)) => Some(a.cmp(b)),
            (Self::Blob(a), Self::Blob(b)) => Some(a.cmp(b)),
            (Self::Vector(a), Self::Vector(b)) => a.partial_cmp(b),
            (Self::Decimal(a), Self::Decimal(b)) => Some(a.cmp(b)),
            (Self::Integer(a), Self::Decimal(b)) => Some(Decimal::new(i128::from(*a), 0).cmp(b)),
            (Self::Decimal(a), Self::Integer(b)) => Some(a.cmp(&Decimal::new(i128::from(*b), 0))),
            (Self::Uuid(a), Self::Uuid(b)) => Some(a.cmp(b)),
            (Self::Timestamp(a), Self::Timestamp(b)) => Some(a.cmp(b)),
            (Self::Interval(a), Self::Interval(b)) => Some(a.cmp(b)),
            (Self::Json(a), Self::Json(b)) => Some(a.cmp(b)),
            (Self::List(a), Self::List(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "NULL"),
            Self::Integer(i) => write!(f, "{i}"),
            Self::Real(r) => write!(f, "{r}"),
            Self::Text(s) => write!(f, "{s}"),
            Self::Boolean(b) => write!(f, "{b}"),
            Self::Blob(bytes) => write!(f, "x'{}'", hex_encode(bytes)),
            Self::Vector(vec) => {
                write!(f, "[")?;
                for (i, v) in vec.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Self::Decimal(d) => write!(f, "{d}"),
            Self::Uuid(u) => write!(f, "{u}"),
            Self::Timestamp(ts) => write!(f, "{ts}"),
            Self::Interval(inv) => write!(f, "{inv}"),
            Self::Json(j) => write!(f, "{j}"),
            Self::List(list) => {
                write!(f, "[")?;
                for (i, v) in list.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Self::Struct(fields) => {
                write!(f, "{{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "\"{k}\": {v}")?;
                }
                write!(f, "}}")
            }
            Self::Map(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
        }
    }
}

fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        use fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}
