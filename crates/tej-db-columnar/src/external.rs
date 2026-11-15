use crate::chunk::DataChunk;
use crate::vector::{ColumnVector, PhysicalType};
use tej_db_core::{DbError, Result};

/// Header configuration for CSV reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsvHeader {
    Present,
    Absent,
}

/// Streaming CSV reader parsing tabular text data directly into vectorized DataChunks.
#[derive(Debug, Clone)]
pub struct CsvReader {
    delimiter: u8,
    has_header: bool,
    column_types: Vec<PhysicalType>,
}

impl CsvReader {
    #[must_use]
    pub fn new(delimiter: u8, header: CsvHeader, column_types: Vec<PhysicalType>) -> Self {
        Self {
            delimiter,
            has_header: header == CsvHeader::Present,
            column_types,
        }
    }

    /// Parses CSV content into a DataChunk.
    pub fn parse_str(&self, csv_data: &str, capacity: usize) -> Result<DataChunk> {
        let mut chunk = DataChunk::new(&self.column_types, capacity);
        let mut lines = csv_data.lines();

        if self.has_header {
            let _ = lines.next();
        }

        let mut row_count = 0;
        for line in lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let fields: Vec<&str> = line.split(self.delimiter as char).map(str::trim).collect();

            if fields.len() != self.column_types.len() {
                return Err(DbError::Custom(format!(
                    "CSV field count mismatch: expected {}, got {}",
                    self.column_types.len(),
                    fields.len()
                )));
            }

            for (col_idx, &field) in fields.iter().enumerate() {
                let col = chunk
                    .column_mut(col_idx)
                    .ok_or_else(|| DbError::Custom("Column index out of range".into()))?;

                if field.is_empty() || field.eq_ignore_ascii_case("null") {
                    col.push_null()?;
                } else {
                    match self.column_types[col_idx] {
                        PhysicalType::Int64 => {
                            let val: i64 = field.parse().map_err(|e| {
                                DbError::Custom(format!("Invalid integer '{field}': {e}"))
                            })?;
                            col.push_i64(val)?;
                        }
                        PhysicalType::Float64 => {
                            let val: f64 = field.parse().map_err(|e| {
                                DbError::Custom(format!("Invalid float '{field}': {e}"))
                            })?;
                            col.push_f64(val)?;
                        }
                        PhysicalType::Boolean => {
                            let val = match field {
                                "true" | "1" | "t" | "TRUE" => true,
                                "false" | "0" | "f" | "FALSE" => false,
                                _ => {
                                    return Err(DbError::Custom(format!(
                                        "Invalid boolean '{field}'"
                                    )));
                                }
                            };
                            col.push_bool(val)?;
                        }
                        PhysicalType::String => {
                            let unquoted = if field.starts_with('"')
                                && field.ends_with('"')
                                && field.len() >= 2
                            {
                                &field[1..field.len() - 1]
                            } else {
                                field
                            };
                            col.push_str(unquoted)?;
                        }
                    }
                }
            }

            row_count += 1;
            if row_count >= capacity {
                break;
            }
        }

        chunk.set_num_rows(row_count)?;
        Ok(chunk)
    }
}

/// Arrow C Data Interface view for zero-copy memory export to external analytics engines.
#[derive(Debug, Clone)]
pub struct ArrowArrayView<'a> {
    pub length: usize,
    pub null_count: usize,
    pub offset: usize,
    pub n_buffers: usize,
    pub buffers: Vec<*const u8>,
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> ArrowArrayView<'a> {
    #[must_use]
    pub fn from_i64_vector(vec: &'a ColumnVector) -> Self {
        let slice = vec.as_i64_slice();
        let ptr = slice.as_ptr().cast::<u8>();
        Self {
            length: vec.len(),
            null_count: vec.null_count() as usize,
            offset: 0,
            n_buffers: 2, // validity bitmap + value buffer
            buffers: vec![std::ptr::null(), ptr],
            _phantom: std::marker::PhantomData,
        }
    }
}
