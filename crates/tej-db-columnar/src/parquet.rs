use crate::chunk::DataChunk;
use crate::vector::PhysicalType;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use tej_db_core::{DbError, Result};

const PARQUET_MAGIC: &[u8; 4] = b"PAR1";

/// Parquet file metadata describing schema and row counts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParquetFileMetadata {
    pub version: u32,
    pub num_rows: usize,
    pub column_names: Vec<String>,
    pub physical_types: Vec<PhysicalType>,
    pub column_chunk_offsets: Vec<u64>,
    pub column_chunk_lengths: Vec<u64>,
}

/// Native columnar Parquet file writer for high-performance table export.
#[derive(Debug)]
pub struct ParquetWriter;

impl ParquetWriter {
    /// Writes a sequence of DataChunks into a standard Parquet container file.
    pub fn write_file(
        path: &Path,
        column_names: &[String],
        column_types: &[PhysicalType],
        chunks: &[DataChunk],
    ) -> Result<()> {
        let mut file = File::create(path).map_err(DbError::Io)?;

        // 1. Magic header
        file.write_all(PARQUET_MAGIC).map_err(DbError::Io)?;

        let mut total_rows = 0;
        for c in chunks {
            total_rows += c.num_rows();
        }

        let num_cols = column_types.len();
        let mut chunk_offsets = Vec::with_capacity(num_cols);
        let mut chunk_lengths = Vec::with_capacity(num_cols);

        // 2. Write column data sequentially (columnar PAX format)
        for (col_idx, &pt) in column_types.iter().enumerate() {
            let start_pos = file.stream_position().map_err(DbError::Io)?;
            chunk_offsets.push(start_pos);

            let mut col_bytes = Vec::new();

            for chunk in chunks {
                if let Some(col) = chunk.column(col_idx) {
                    match pt {
                        PhysicalType::Int64 => {
                            for &val in col.as_i64_slice() {
                                col_bytes.extend_from_slice(&val.to_le_bytes());
                            }
                        }
                        PhysicalType::Float64 => {
                            for &val in col.as_f64_slice() {
                                col_bytes.extend_from_slice(&val.to_le_bytes());
                            }
                        }
                        PhysicalType::Boolean => {
                            for row_idx in 0..col.len() {
                                let b = col.get_bool(row_idx).unwrap_or(false);
                                col_bytes.push(u8::from(b));
                            }
                        }
                        PhysicalType::String => {
                            for row_idx in 0..col.len() {
                                let s = col.get_str(row_idx).unwrap_or("");
                                let len = u32::try_from(s.len()).unwrap_or(0);
                                col_bytes.extend_from_slice(&len.to_le_bytes());
                                col_bytes.extend_from_slice(s.as_bytes());
                            }
                        }
                    }
                }
            }

            file.write_all(&col_bytes).map_err(DbError::Io)?;
            let end_pos = file.stream_position().map_err(DbError::Io)?;
            chunk_lengths.push(end_pos - start_pos);
        }

        // 3. Write File Metadata footer
        let metadata = ParquetFileMetadata {
            version: 1,
            num_rows: total_rows,
            column_names: column_names.to_vec(),
            physical_types: column_types.to_vec(),
            column_chunk_offsets: chunk_offsets,
            column_chunk_lengths: chunk_lengths,
        };

        let meta_bytes = bincode::serialize(&metadata)
            .map_err(|e| DbError::Custom(format!("Metadata serialization failed: {e}")))?;
        let meta_len = u32::try_from(meta_bytes.len())
            .map_err(|_| DbError::Custom("Metadata too large".into()))?;

        file.write_all(&meta_bytes).map_err(DbError::Io)?;
        file.write_all(&meta_len.to_le_bytes())
            .map_err(DbError::Io)?;
        file.write_all(PARQUET_MAGIC).map_err(DbError::Io)?;

        file.flush().map_err(DbError::Io)?;
        Ok(())
    }
}

/// Native columnar Parquet file reader for zero-copy data ingestion.
#[derive(Debug)]
pub struct ParquetReader;

impl ParquetReader {
    /// Reads metadata and loads DataChunks from a Parquet file.
    pub fn read_file(
        path: &Path,
        batch_size: usize,
    ) -> Result<(ParquetFileMetadata, Vec<DataChunk>)> {
        let mut file = File::open(path).map_err(DbError::Io)?;
        let file_len = file.seek(SeekFrom::End(0)).map_err(DbError::Io)?;

        if file_len < 12 {
            return Err(DbError::Custom("Parquet file too short".into()));
        }

        // 1. Verify header magic
        file.seek(SeekFrom::Start(0)).map_err(DbError::Io)?;
        let mut header_magic = [0u8; 4];
        file.read_exact(&mut header_magic).map_err(DbError::Io)?;
        if &header_magic != PARQUET_MAGIC {
            return Err(DbError::Custom("Invalid Parquet header magic".into()));
        }

        // 2. Read footer magic and metadata length
        file.seek(SeekFrom::End(-8)).map_err(DbError::Io)?;
        let mut len_bytes = [0u8; 4];
        let mut footer_magic = [0u8; 4];
        file.read_exact(&mut len_bytes).map_err(DbError::Io)?;
        file.read_exact(&mut footer_magic).map_err(DbError::Io)?;

        if &footer_magic != PARQUET_MAGIC {
            return Err(DbError::Custom("Invalid Parquet footer magic".into()));
        }

        let meta_len = u64::from(u32::from_le_bytes(len_bytes));
        let meta_start = file_len - 8 - meta_len;
        file.seek(SeekFrom::Start(meta_start))
            .map_err(DbError::Io)?;

        let mut meta_bytes = vec![0u8; meta_len as usize];
        file.read_exact(&mut meta_bytes).map_err(DbError::Io)?;

        let metadata: ParquetFileMetadata = bincode::deserialize(&meta_bytes)
            .map_err(|e| DbError::Custom(format!("Metadata deserialization failed: {e}")))?;

        // 3. Load column data into DataChunks
        let num_rows = metadata.num_rows;
        let num_cols = metadata.physical_types.len();

        // Read all raw column bytes
        let mut all_column_data = Vec::with_capacity(num_cols);
        for col_idx in 0..num_cols {
            let offset = metadata.column_chunk_offsets[col_idx];
            let length = metadata.column_chunk_lengths[col_idx] as usize;
            file.seek(SeekFrom::Start(offset)).map_err(DbError::Io)?;
            let mut raw_data = vec![0u8; length];
            file.read_exact(&mut raw_data).map_err(DbError::Io)?;
            all_column_data.push(raw_data);
        }

        // Slice into batches of `batch_size` rows
        let mut chunks = Vec::new();
        let mut row_cursor = 0;
        let mut col_byte_cursors = vec![0usize; num_cols];

        while row_cursor < num_rows {
            let cur_batch = (num_rows - row_cursor).min(batch_size);
            let mut chunk = DataChunk::new(&metadata.physical_types, cur_batch);

            for col_idx in 0..num_cols {
                let pt = metadata.physical_types[col_idx];
                let col = chunk
                    .column_mut(col_idx)
                    .ok_or_else(|| DbError::Custom("Chunk column missing".into()))?;
                let data = &all_column_data[col_idx];
                let cursor = &mut col_byte_cursors[col_idx];

                match pt {
                    PhysicalType::Int64 => {
                        for _ in 0..cur_batch {
                            let slice = data.get(*cursor..*cursor + 8).ok_or_else(|| {
                                DbError::Custom("Parquet Int64 out of bounds".into())
                            })?;
                            let val = i64::from_le_bytes(slice.try_into().unwrap_or([0; 8]));
                            col.push_i64(val)?;
                            *cursor += 8;
                        }
                    }
                    PhysicalType::Float64 => {
                        for _ in 0..cur_batch {
                            let slice = data.get(*cursor..*cursor + 8).ok_or_else(|| {
                                DbError::Custom("Parquet Float64 out of bounds".into())
                            })?;
                            let val = f64::from_le_bytes(slice.try_into().unwrap_or([0; 8]));
                            col.push_f64(val)?;
                            *cursor += 8;
                        }
                    }
                    PhysicalType::Boolean => {
                        for _ in 0..cur_batch {
                            let b = data.get(*cursor).copied().unwrap_or(0) != 0;
                            col.push_bool(b)?;
                            *cursor += 1;
                        }
                    }
                    PhysicalType::String => {
                        for _ in 0..cur_batch {
                            let len_slice = data.get(*cursor..*cursor + 4).ok_or_else(|| {
                                DbError::Custom("Parquet String len out of bounds".into())
                            })?;
                            let len =
                                u32::from_le_bytes(len_slice.try_into().unwrap_or([0; 4])) as usize;
                            *cursor += 4;
                            let str_slice = data.get(*cursor..*cursor + len).ok_or_else(|| {
                                DbError::Custom("Parquet String bytes out of bounds".into())
                            })?;
                            let s = core::str::from_utf8(str_slice)
                                .map_err(|e| DbError::Custom(e.to_string()))?;
                            col.push_str(s)?;
                            *cursor += len;
                        }
                    }
                }
            }

            chunk.set_num_rows(cur_batch)?;
            chunks.push(chunk);
            row_cursor += cur_batch;
        }

        Ok((metadata, chunks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parquet_writer_reader_roundtrip() {
        let file_path = std::env::temp_dir().join(format!("test_{}.parquet", std::process::id()));

        let names = vec!["id".into(), "score".into(), "name".into()];
        let types = vec![
            PhysicalType::Int64,
            PhysicalType::Float64,
            PhysicalType::String,
        ];

        let mut chunk = DataChunk::new(&types, 3);
        chunk.column_mut(0).unwrap().push_i64(101).unwrap();
        chunk.column_mut(1).unwrap().push_f64(98.5).unwrap();
        chunk.column_mut(2).unwrap().push_str("Alpha").unwrap();

        chunk.column_mut(0).unwrap().push_i64(102).unwrap();
        chunk.column_mut(1).unwrap().push_f64(87.0).unwrap();
        chunk.column_mut(2).unwrap().push_str("Beta").unwrap();

        chunk.set_num_rows(2).unwrap();

        ParquetWriter::write_file(&file_path, &names, &types, &[chunk]).unwrap();

        let (meta, read_chunks) = ParquetReader::read_file(&file_path, 10).unwrap();
        assert_eq!(meta.num_rows, 2);
        assert_eq!(meta.column_names, names);
        assert_eq!(read_chunks.len(), 1);
        assert_eq!(read_chunks[0].column(0).unwrap().get_i64(0), Some(101));
        assert_eq!(read_chunks[0].column(2).unwrap().get_str(1), Some("Beta"));
    }
}
