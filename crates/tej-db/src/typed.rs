use core::marker::PhantomData;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tej_db_core::{DbError, Result, Storage};
use tej_db_txn::{TableMut, TableRef};

/// Trait for order-preserving binary key serialization and deserialization.
///
/// Ensures byte-level lexicographical ordering in the B-Tree index matches the natural
/// ordering of keys for strings, raw bytes, and numeric integer types.
pub trait KeyCodec: Sized {
    fn encode_key(&self) -> Result<Vec<u8>>;
    fn decode_key(bytes: &[u8]) -> Result<Self>;
}

impl KeyCodec for String {
    fn encode_key(&self) -> Result<Vec<u8>> {
        Ok(self.as_bytes().to_vec())
    }
    fn decode_key(bytes: &[u8]) -> Result<Self> {
        String::from_utf8(bytes.to_vec())
            .map_err(|e| DbError::Serialization(format!("Invalid UTF-8 key: {e}")))
    }
}

impl KeyCodec for Vec<u8> {
    fn encode_key(&self) -> Result<Vec<u8>> {
        Ok(self.clone())
    }
    fn decode_key(bytes: &[u8]) -> Result<Self> {
        Ok(bytes.to_vec())
    }
}

macro_rules! impl_uint_key_codec {
    ($t:ty) => {
        impl KeyCodec for $t {
            fn encode_key(&self) -> Result<Vec<u8>> {
                Ok(self.to_be_bytes().to_vec())
            }
            fn decode_key(bytes: &[u8]) -> Result<Self> {
                let arr: [u8; core::mem::size_of::<$t>()] = bytes
                    .try_into()
                    .map_err(|_| DbError::Serialization("Key byte length mismatch".into()))?;
                Ok(Self::from_be_bytes(arr))
            }
        }
    };
}

impl_uint_key_codec!(u8);
impl_uint_key_codec!(u16);
impl_uint_key_codec!(u32);
impl_uint_key_codec!(u64);
impl_uint_key_codec!(u128);
impl_uint_key_codec!(usize);

/// Type-safe read-only view of a named table with automatic serde serialization.
#[derive(Debug)]
pub struct TypedTable<'a, S: Storage, K: KeyCodec, V: Serialize + DeserializeOwned> {
    inner: TableRef<'a, S>,
    _marker: PhantomData<(K, V)>,
}

impl<'a, S: Storage, K: KeyCodec, V: Serialize + DeserializeOwned> TypedTable<'a, S, K, V> {
    pub fn new(inner: TableRef<'a, S>) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }

    pub fn name(&self) -> &str {
        self.inner.name()
    }

    pub fn get(&self, key: &K) -> Result<Option<V>> {
        let key_bytes = key.encode_key()?;
        match self.inner.get(&key_bytes)? {
            Some(val_bytes) => {
                let val: V = bincode::deserialize(&val_bytes).map_err(|e| {
                    DbError::Serialization(format!("Value deserialization failed: {e}"))
                })?;
                Ok(Some(val))
            }
            None => Ok(None),
        }
    }

    pub fn scan_all(&self) -> Result<Vec<(K, V)>> {
        let items = self.inner.scan_all()?;
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            let k = K::decode_key(&item.key)?;
            let v: V = bincode::deserialize(&item.value).map_err(|e| {
                DbError::Serialization(format!("Value deserialization failed: {e}"))
            })?;
            results.push((k, v));
        }
        Ok(results)
    }

    pub fn scan_range(&self, start: Option<&K>, end: Option<&K>) -> Result<Vec<(K, V)>> {
        use core::ops::Bound;
        let start_b = match start {
            Some(k) => Bound::Included(k.encode_key()?),
            None => Bound::Unbounded,
        };
        let end_b = match end {
            Some(k) => Bound::Included(k.encode_key()?),
            None => Bound::Unbounded,
        };

        let items = self.inner.scan_range(&tej_db_core::KeyRange {
            start: start_b,
            end: end_b,
        })?;
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            let k = K::decode_key(&item.key)?;
            let v: V = bincode::deserialize(&item.value).map_err(|e| {
                DbError::Serialization(format!("Value deserialization failed: {e}"))
            })?;
            results.push((k, v));
        }
        Ok(results)
    }
}

/// Type-safe mutable view of a named table with automatic serde serialization.
#[derive(Debug)]
pub struct TypedTableMut<'a, 'tx, S: Storage, K: KeyCodec, V: Serialize + DeserializeOwned> {
    inner: TableMut<'a, 'tx, S>,
    _marker: PhantomData<(K, V)>,
}

impl<'a, 'tx, S: Storage, K: KeyCodec, V: Serialize + DeserializeOwned>
    TypedTableMut<'a, 'tx, S, K, V>
{
    pub fn new(inner: TableMut<'a, 'tx, S>) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }

    pub fn name(&self) -> &str {
        self.inner.name()
    }

    pub fn get(&self, key: &K) -> Result<Option<V>> {
        let key_bytes = key.encode_key()?;
        match self.inner.get(&key_bytes)? {
            Some(val_bytes) => {
                let val: V = bincode::deserialize(&val_bytes).map_err(|e| {
                    DbError::Serialization(format!("Value deserialization failed: {e}"))
                })?;
                Ok(Some(val))
            }
            None => Ok(None),
        }
    }

    pub fn put(&mut self, key: &K, val: &V) -> Result<()> {
        let key_bytes = key.encode_key()?;
        let val_bytes = bincode::serialize(val)
            .map_err(|e| DbError::Serialization(format!("Value serialization failed: {e}")))?;
        self.inner.put(&key_bytes, &val_bytes)
    }

    pub fn delete(&mut self, key: &K) -> Result<bool> {
        let key_bytes = key.encode_key()?;
        self.inner.delete(&key_bytes)
    }

    pub fn scan_all(&self) -> Result<Vec<(K, V)>> {
        let items = self.inner.scan_all()?;
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            let k = K::decode_key(&item.key)?;
            let v: V = bincode::deserialize(&item.value).map_err(|e| {
                DbError::Serialization(format!("Value deserialization failed: {e}"))
            })?;
            results.push((k, v));
        }
        Ok(results)
    }

    pub fn scan_range(&self, start: Option<&K>, end: Option<&K>) -> Result<Vec<(K, V)>> {
        use core::ops::Bound;
        let start_b = match start {
            Some(k) => Bound::Included(k.encode_key()?),
            None => Bound::Unbounded,
        };
        let end_b = match end {
            Some(k) => Bound::Included(k.encode_key()?),
            None => Bound::Unbounded,
        };

        let items = self.inner.scan_range(&tej_db_core::KeyRange {
            start: start_b,
            end: end_b,
        })?;
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            let k = K::decode_key(&item.key)?;
            let v: V = bincode::deserialize(&item.value).map_err(|e| {
                DbError::Serialization(format!("Value deserialization failed: {e}"))
            })?;
            results.push((k, v));
        }
        Ok(results)
    }
}
