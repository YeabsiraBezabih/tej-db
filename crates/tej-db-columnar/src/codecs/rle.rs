use tej_db_core::{DbError, Result};

/// Run-Length Encoding (RLE) codec for repeated 64-bit scalar sequences.
#[derive(Debug, Clone, Copy)]
pub struct RleCodec;

impl RleCodec {
    /// Compresses a slice of 64-bit integers using Run-Length Encoding.
    #[must_use]
    pub fn encode(values: &[i64]) -> Vec<u8> {
        if values.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::with_capacity(4 + values.len() * 4);
        out.extend_from_slice(&(values.len() as u32).to_le_bytes());

        let mut current_val = values[0];
        let mut run_len = 1u32;

        for &v in &values[1..] {
            if v == current_val && run_len < u32::MAX {
                run_len += 1;
            } else {
                out.extend_from_slice(&run_len.to_le_bytes());
                out.extend_from_slice(&current_val.to_le_bytes());
                current_val = v;
                run_len = 1;
            }
        }

        out.extend_from_slice(&run_len.to_le_bytes());
        out.extend_from_slice(&current_val.to_le_bytes());
        out
    }

    /// Decompresses Run-Length Encoded bytes back into 64-bit integers.
    pub fn decode(bytes: &[u8]) -> Result<Vec<i64>> {
        let mut out = Vec::new();
        Self::decode_into(bytes, &mut out)?;
        Ok(out)
    }

    /// Decompresses Run-Length Encoded bytes directly into a reusable destination vector,
    /// avoiding intermediate vector allocations using bulk slice resizing.
    pub fn decode_into(bytes: &[u8], out: &mut Vec<i64>) -> Result<()> {
        if bytes.is_empty() {
            out.clear();
            return Ok(());
        }

        if bytes.len() < 4 {
            return Err(DbError::Custom("Corrupt RLE header".into()));
        }

        let total_count = u32::from_le_bytes(bytes[0..4].try_into().unwrap_or([0; 4])) as usize;
        out.clear();
        out.reserve(total_count);

        let mut offset = 4;
        while offset + 12 <= bytes.len() {
            let run_len =
                u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4])) as usize;
            let val =
                i64::from_le_bytes(bytes[offset + 4..offset + 12].try_into().unwrap_or([0; 8]));
            offset += 12;

            out.resize(out.len() + run_len, val);
        }

        if out.len() != total_count {
            return Err(DbError::Custom("RLE decompressed length mismatch".into()));
        }

        Ok(())
    }
}
