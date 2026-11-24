use std::collections::HashMap;
use tej_db_core::{DbError, Result};

/// Dictionary encoding codec for string columns with repeated values.
#[derive(Debug, Clone, Copy)]
pub struct DictionaryCodec;

impl DictionaryCodec {
    /// Compresses a slice of string references using dictionary encoding.
    #[must_use]
    pub fn encode(strings: &[&str]) -> Vec<u8> {
        if strings.is_empty() {
            return Vec::new();
        }

        let mut dict_map: HashMap<&str, u32> = HashMap::new();
        let mut dict_entries: Vec<&str> = Vec::new();
        let mut codes: Vec<u32> = Vec::with_capacity(strings.len());

        for &s in strings {
            let next_id = dict_entries.len() as u32;
            let id = *dict_map.entry(s).or_insert_with(|| {
                dict_entries.push(s);
                next_id
            });
            codes.push(id);
        }

        // Serialization format:
        // [total_rows: u32][dict_size: u32]
        // For each dict entry: [len: u16][bytes]
        // For each row code: [code: u32]
        let mut out = Vec::with_capacity(8 + dict_entries.len() * 16 + codes.len() * 4);
        out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        out.extend_from_slice(&(dict_entries.len() as u32).to_le_bytes());

        for entry in &dict_entries {
            let b = entry.as_bytes();
            out.extend_from_slice(&(b.len() as u16).to_le_bytes());
            out.extend_from_slice(b);
        }

        for code in codes {
            out.extend_from_slice(&code.to_le_bytes());
        }

        out
    }

    /// Decompresses dictionary encoded bytes back into owned strings.
    pub fn decode(bytes: &[u8]) -> Result<Vec<String>> {
        if bytes.is_empty() {
            return Ok(Vec::new());
        }

        if bytes.len() < 8 {
            return Err(DbError::Custom("Corrupt dictionary header".into()));
        }

        let total_rows = u32::from_le_bytes(bytes[0..4].try_into().unwrap_or([0; 4])) as usize;
        let dict_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4])) as usize;

        let mut offset = 8;
        let max_possible_entries = (bytes.len() - offset) / 2;
        if dict_size > max_possible_entries {
            return Err(DbError::Custom(format!(
                "Dictionary size {dict_size} exceeds maximum possible entries {max_possible_entries}"
            )));
        }

        let mut dict_entries: Vec<String> = Vec::with_capacity(dict_size);

        for _ in 0..dict_size {
            if offset + 2 > bytes.len() {
                return Err(DbError::Custom("Truncated dictionary entries".into()));
            }
            let len =
                u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap_or([0; 2])) as usize;
            offset += 2;
            if offset + len > bytes.len() {
                return Err(DbError::Custom("Truncated dictionary string".into()));
            }
            let s = core::str::from_utf8(&bytes[offset..offset + len])
                .map_err(|e| DbError::Custom(format!("Invalid UTF-8 in dictionary: {e}")))?;
            dict_entries.push(s.to_string());
            offset += len;
        }

        let needed_code_bytes = total_rows
            .checked_mul(4)
            .ok_or_else(|| DbError::Custom("Total rows integer overflow in dictionary".into()))?;
        if offset + needed_code_bytes > bytes.len() {
            return Err(DbError::Custom("Truncated dictionary code vector".into()));
        }

        let mut out = Vec::with_capacity(total_rows);
        for _ in 0..total_rows {
            let code =
                u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4])) as usize;
            offset += 4;
            if code >= dict_entries.len() {
                return Err(DbError::Custom("Dictionary code out of bounds".into()));
            }
            out.push(dict_entries[code].clone());
        }

        Ok(out)
    }
}
