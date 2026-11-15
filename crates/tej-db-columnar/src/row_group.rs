use crate::chunk::DataChunk;
use crate::codecs::{BitPackingCodec, CompressionType, DictionaryCodec, RleCodec};
use crate::vector::{ColumnVector, PhysicalType};
use crate::zonemap::ZoneMap;
use serde::{Deserialize, Serialize};
use tej_db_core::{DbError, Result};

/// Metadata for a single column chunk within a row group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnChunkMetadata {
    pub column_idx: usize,
    pub physical_type: PhysicalType,
    pub compression: CompressionType,
    pub zonemap: ZoneMap,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
}

/// Metadata header describing the contents and statistics of a RowGroup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowGroupMetadata {
    pub num_rows: u32,
    pub columns: Vec<ColumnChunkMetadata>,
}

/// In-memory representation of a serialized columnar row-group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowGroup {
    pub metadata: RowGroupMetadata,
    pub column_data: Vec<Vec<u8>>,
}

impl RowGroup {
    /// Compresses a DataChunk into a RowGroup, automatically selecting the optimal codec per column.
    pub fn from_chunk(chunk: &DataChunk) -> Result<Self> {
        let num_rows = u32::try_from(chunk.num_rows())
            .map_err(|_| DbError::Custom("RowGroup row count exceeds u32 limit".into()))?;

        let mut columns_meta = Vec::with_capacity(chunk.num_columns());
        let mut column_data = Vec::with_capacity(chunk.num_columns());

        for (col_idx, col) in chunk.columns().iter().enumerate() {
            let zonemap = ZoneMap::build(col);
            let uncompressed_bytes = bincode::serialize(col)
                .map_err(|e| DbError::Serialization(format!("Vector serialization error: {e}")))?;
            let uncompressed_size = u32::try_from(uncompressed_bytes.len()).unwrap_or(u32::MAX);

            let (compression, compressed_bytes) = Self::compress_column(col, &uncompressed_bytes);
            let compressed_size = u32::try_from(compressed_bytes.len()).unwrap_or(u32::MAX);

            columns_meta.push(ColumnChunkMetadata {
                column_idx: col_idx,
                physical_type: col.physical_type(),
                compression,
                zonemap,
                compressed_size,
                uncompressed_size,
            });
            column_data.push(compressed_bytes);
        }

        Ok(Self {
            metadata: RowGroupMetadata {
                num_rows,
                columns: columns_meta,
            },
            column_data,
        })
    }

    /// Automatically chooses the highest compression ratio codec for the given column.
    fn compress_column(col: &ColumnVector, fallback: &[u8]) -> (CompressionType, Vec<u8>) {
        match col.physical_type() {
            PhysicalType::Int64 if col.null_count() == 0 => {
                let slice = col.as_i64_slice();
                let rle = RleCodec::encode(slice);
                let bitpack = BitPackingCodec::encode(slice);

                if rle.len() < bitpack.len() && rle.len() < fallback.len() {
                    (CompressionType::Rle, rle)
                } else if bitpack.len() < fallback.len() {
                    (CompressionType::BitPacking, bitpack)
                } else {
                    (CompressionType::Uncompressed, fallback.to_vec())
                }
            }
            PhysicalType::String if col.null_count() == 0 => {
                let mut refs = Vec::with_capacity(col.len());
                for i in 0..col.len() {
                    if let Some(s) = col.get_str(i) {
                        refs.push(s);
                    }
                }
                let dict = DictionaryCodec::encode(&refs);
                if dict.len() < fallback.len() {
                    (CompressionType::Dictionary, dict)
                } else {
                    (CompressionType::Uncompressed, fallback.to_vec())
                }
            }
            _ => (CompressionType::Uncompressed, fallback.to_vec()),
        }
    }

    /// Decompresses this RowGroup back into a DataChunk for vectorized query processing.
    pub fn decompress_to_chunk(&self) -> Result<DataChunk> {
        let types: Vec<PhysicalType> = self
            .metadata
            .columns
            .iter()
            .map(|c| c.physical_type)
            .collect();
        let mut chunk = DataChunk::new(&types, self.metadata.num_rows as usize);

        for (col_idx, meta) in self.metadata.columns.iter().enumerate() {
            let raw_data = self
                .column_data
                .get(col_idx)
                .ok_or_else(|| DbError::Custom("Missing column payload".into()))?;

            let col_vec = match meta.compression {
                CompressionType::Uncompressed => bincode::deserialize(raw_data)
                    .map_err(|e| DbError::Serialization(format!("Deserialization error: {e}")))?,
                CompressionType::BitPacking => {
                    let ints = BitPackingCodec::decode(raw_data)?;
                    ColumnVector::from_i64_vec(ints)
                }
                CompressionType::Rle => {
                    let ints = RleCodec::decode(raw_data)?;
                    ColumnVector::from_i64_vec(ints)
                }
                CompressionType::Dictionary => {
                    let strings = DictionaryCodec::decode(raw_data)?;
                    let mut vec = ColumnVector::new(PhysicalType::String, strings.len());
                    for s in &strings {
                        vec.push_str(s)?;
                    }
                    vec
                }
            };

            let dest_col = chunk
                .column_mut(col_idx)
                .ok_or_else(|| DbError::Custom("Destination column missing".into()))?;
            *dest_col = col_vec;
        }

        chunk.set_num_rows(self.metadata.num_rows as usize)?;
        Ok(chunk)
    }

    /// Decompresses a single column directly from the RowGroup without decompressing other columns.
    pub fn decompress_column(&self, col_idx: usize) -> Result<ColumnVector> {
        let meta = self
            .metadata
            .columns
            .get(col_idx)
            .ok_or_else(|| DbError::Custom(format!("Missing column idx {col_idx}")))?;
        let raw_data = self
            .column_data
            .get(col_idx)
            .ok_or_else(|| DbError::Custom(format!("Missing column payload {col_idx}")))?;

        match meta.compression {
            CompressionType::Uncompressed => bincode::deserialize(raw_data)
                .map_err(|e| DbError::Serialization(format!("Deserialization error: {e}"))),
            CompressionType::BitPacking => {
                let ints = BitPackingCodec::decode(raw_data)?;
                Ok(ColumnVector::from_i64_vec(ints))
            }
            CompressionType::Rle => {
                let ints = RleCodec::decode(raw_data)?;
                Ok(ColumnVector::from_i64_vec(ints))
            }
            CompressionType::Dictionary => {
                let strings = DictionaryCodec::decode(raw_data)?;
                let mut vec = ColumnVector::new(PhysicalType::String, strings.len());
                for s in &strings {
                    vec.push_str(s)?;
                }
                Ok(vec)
            }
        }
    }

    /// Decompresses an Int64 column directly into a reusable destination vector, avoiding all heap allocations.
    pub fn decompress_column_into(&self, col_idx: usize, out: &mut Vec<i64>) -> Result<()> {
        let meta = self
            .metadata
            .columns
            .get(col_idx)
            .ok_or_else(|| DbError::Custom(format!("Missing column idx {col_idx}")))?;
        let raw_data = self
            .column_data
            .get(col_idx)
            .ok_or_else(|| DbError::Custom(format!("Missing column payload {col_idx}")))?;

        out.clear();
        match meta.compression {
            CompressionType::Uncompressed => {
                let vec: ColumnVector = bincode::deserialize(raw_data)
                    .map_err(|e| DbError::Serialization(format!("Deserialization error: {e}")))?;
                out.extend_from_slice(vec.as_i64_slice());
                Ok(())
            }
            CompressionType::BitPacking => BitPackingCodec::decode_into(raw_data, out),
            CompressionType::Rle => RleCodec::decode_into(raw_data, out),
            CompressionType::Dictionary => Err(DbError::Custom("Expected Int64 column".into())),
        }
    }
}
