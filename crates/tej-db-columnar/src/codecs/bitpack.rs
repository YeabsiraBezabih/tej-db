use tej_db_core::{DbError, Result};

/// Frame-of-Reference (FOR) bit-packing codec for 64-bit integers.
#[derive(Debug, Clone, Copy)]
pub struct BitPackingCodec;

impl BitPackingCodec {
    /// Compresses a slice of 64-bit integers using Frame of Reference (FOR) bit-packing.
    #[must_use]
    pub fn encode(values: &[i64]) -> Vec<u8> {
        if values.is_empty() {
            return Vec::new();
        }

        let mut min_val = i64::MAX;
        let mut max_val = i64::MIN;
        for &v in values {
            min_val = min_val.min(v);
            max_val = max_val.max(v);
        }

        let span = u64::try_from(i128::from(max_val) - i128::from(min_val)).unwrap_or(0);
        let bit_width = if span == 0 {
            0u8
        } else {
            (64 - span.leading_zeros()) as u8
        };

        let count = values.len();
        let total_bits = count * usize::from(bit_width);
        let byte_len = total_bits.div_ceil(8);

        let mut out = Vec::with_capacity(17 + byte_len);
        out.extend_from_slice(&(count as u32).to_le_bytes());
        out.extend_from_slice(&min_val.to_le_bytes());
        out.push(bit_width);

        if bit_width == 4 {
            let start_offset = out.len();
            out.resize(start_offset + byte_len, 0);
            let dest = &mut out[start_offset..];
            for (i, &v) in values.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let delta = ((i128::from(v) - i128::from(min_val)) as u8) & 0x0F;
                if i % 2 == 0 {
                    dest[i / 2] = delta;
                } else {
                    dest[i / 2] |= delta << 4;
                }
            }
            return out;
        }

        if bit_width == 8 {
            let start_offset = out.len();
            out.resize(start_offset + byte_len, 0);
            let dest = &mut out[start_offset..];
            for (i, &v) in values.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    dest[i] = (i128::from(v) - i128::from(min_val)) as u8;
                }
            }
            return out;
        }

        if bit_width == 16 {
            let start_offset = out.len();
            out.resize(start_offset + byte_len, 0);
            let dest = &mut out[start_offset..];
            for (i, &v) in values.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let delta = (i128::from(v) - i128::from(min_val)) as u16;
                let b = delta.to_le_bytes();
                dest[i * 2] = b[0];
                dest[i * 2 + 1] = b[1];
            }
            return out;
        }

        if bit_width > 0 {
            let start_offset = out.len();
            out.resize(start_offset + byte_len, 0);
            let mut bit_pos = 0;

            for &v in values {
                let delta = u64::try_from(i128::from(v) - i128::from(min_val)).unwrap_or(0);
                for b in 0..bit_width {
                    let bit = ((delta >> b) & 1) as u8;
                    let byte_idx = start_offset + (bit_pos / 8);
                    let bit_idx = bit_pos % 8;
                    out[byte_idx] |= bit << bit_idx;
                    bit_pos += 1;
                }
            }
        }

        out
    }

    /// Decompresses Frame-of-Reference bit-packed bytes back into 64-bit integers.
    pub fn decode(bytes: &[u8]) -> Result<Vec<i64>> {
        let mut out = Vec::new();
        Self::decode_into(bytes, &mut out)?;
        Ok(out)
    }

    /// Decompresses Frame-of-Reference bit-packed bytes directly into a reusable destination vector,
    /// avoiding all intermediate heap allocations with branch-free word-level bit unpacking.
    pub fn decode_into(bytes: &[u8], out: &mut Vec<i64>) -> Result<()> {
        const MAX_BITPACK_COUNT: usize = 10_000_000;

        if bytes.is_empty() {
            out.clear();
            return Ok(());
        }

        if bytes.len() < 13 {
            return Err(DbError::Custom("Corrupt bitpack header".into()));
        }

        let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap_or([0; 4])) as usize;
        if count > MAX_BITPACK_COUNT {
            return Err(DbError::Custom(format!(
                "Bitpack count {count} exceeds maximum allowed limit {MAX_BITPACK_COUNT}"
            )));
        }
        let min_val = i64::from_le_bytes(bytes[4..12].try_into().unwrap_or([0; 8]));
        let bit_width = bytes[12];

        if bit_width > 64 {
            return Err(DbError::Custom(format!(
                "Invalid bitpack bit width: {bit_width}"
            )));
        }

        let expected_payload = count
            .checked_mul(usize::from(bit_width))
            .map(|b| b.div_ceil(8))
            .ok_or_else(|| DbError::Custom("Bitpack bit length integer overflow".into()))?;
        let expected_bytes = 13usize
            .checked_add(expected_payload)
            .ok_or_else(|| DbError::Custom("Bitpack expected bytes integer overflow".into()))?;
        if bytes.len() < expected_bytes {
            return Err(DbError::Custom("Truncated bitpack payload".into()));
        }

        out.clear();
        out.resize(count, 0);

        if bit_width == 0 {
            out.fill(min_val);
            return Ok(());
        }

        let payload = &bytes[13..];

        // Specialized fast paths for common bit widths
        if bit_width == 1 {
            let mut i = 0;
            for &byte in payload.iter().take(count / 8) {
                for b in 0..8 {
                    out[i + b] = min_val + i64::from((byte >> b) & 1);
                }
                i += 8;
            }
            while i < count {
                let byte = payload[i / 8];
                let b = i % 8;
                out[i] = min_val + i64::from((byte >> b) & 1);
                i += 1;
            }
            return Ok(());
        }

        if bit_width == 4 {
            let mut i = 0;
            for &byte in payload.iter().take(count / 2) {
                out[i] = min_val + i64::from(byte & 0x0F);
                out[i + 1] = min_val + i64::from((byte >> 4) & 0x0F);
                i += 2;
            }
            if i < count {
                let byte = payload[count / 2];
                out[i] = min_val + i64::from(byte & 0x0F);
            }
            return Ok(());
        }

        if bit_width == 6 {
            let mut i = 0;
            let mut byte_idx = 0;
            let full_groups = count / 4;
            for _ in 0..full_groups {
                let word = u32::from_le_bytes([
                    payload[byte_idx],
                    payload[byte_idx + 1],
                    payload[byte_idx + 2],
                    0,
                ]);
                out[i] = min_val + i64::from(word & 0x3F);
                out[i + 1] = min_val + i64::from((word >> 6) & 0x3F);
                out[i + 2] = min_val + i64::from((word >> 12) & 0x3F);
                out[i + 3] = min_val + i64::from((word >> 18) & 0x3F);
                i += 4;
                byte_idx += 3;
            }
            while i < count {
                let bit_pos = i * 6;
                let b_idx = bit_pos >> 3;
                let shift = (bit_pos & 7) as u32;
                let mut b = [0u8; 4];
                let rem = (payload.len() - b_idx).min(4);
                b[..rem].copy_from_slice(&payload[b_idx..b_idx + rem]);
                let word = u32::from_le_bytes(b);
                out[i] = min_val + i64::from((word >> shift) & 0x3F);
                i += 1;
            }
            return Ok(());
        }

        if bit_width == 8 {
            for (i, &byte) in payload.iter().take(count).enumerate() {
                out[i] = min_val + i64::from(byte);
            }
            return Ok(());
        }

        if bit_width == 10 {
            let mut i = 0;
            let mut byte_idx = 0;
            let full_groups = count / 4;
            for _ in 0..full_groups {
                let word = u64::from_le_bytes([
                    payload[byte_idx],
                    payload[byte_idx + 1],
                    payload[byte_idx + 2],
                    payload[byte_idx + 3],
                    payload[byte_idx + 4],
                    0,
                    0,
                    0,
                ]);
                #[allow(clippy::cast_possible_wrap)]
                {
                    out[i] = min_val + ((word & 0x3FF) as i64);
                    out[i + 1] = min_val + (((word >> 10) & 0x3FF) as i64);
                    out[i + 2] = min_val + (((word >> 20) & 0x3FF) as i64);
                    out[i + 3] = min_val + (((word >> 30) & 0x3FF) as i64);
                }
                i += 4;
                byte_idx += 5;
            }
            while i < count {
                let bit_pos = i * 10;
                let b_idx = bit_pos >> 3;
                let shift = (bit_pos & 7) as u64;
                let mut b = [0u8; 8];
                let rem = (payload.len() - b_idx).min(8);
                b[..rem].copy_from_slice(&payload[b_idx..b_idx + rem]);
                let word = u64::from_le_bytes(b);
                #[allow(clippy::cast_possible_wrap)]
                {
                    out[i] = min_val + (((word >> shift) & 0x3FF) as i64);
                }
                i += 1;
            }
            return Ok(());
        }

        if bit_width == 16 {
            let chunks = payload[..(count * 2)].chunks_exact(2);
            for (i, chunk) in chunks.enumerate() {
                let delta = u16::from_le_bytes([chunk[0], chunk[1]]);
                out[i] = min_val + i64::from(delta);
            }
            return Ok(());
        }

        if bit_width == 32 {
            let chunks = payload[..(count * 4)].chunks_exact(4);
            for (i, chunk) in chunks.enumerate() {
                let delta = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out[i] = min_val + i64::from(delta);
            }
            return Ok(());
        }

        let mask = if bit_width >= 64 {
            u64::MAX
        } else {
            (1u64 << bit_width) - 1
        };
        let bw = usize::from(bit_width);
        let mut bit_pos = 0usize;
        let payload_len = payload.len();

        for item in out.iter_mut().take(count) {
            let byte_idx = bit_pos >> 3;
            let bit_in_byte = (bit_pos & 7) as u64;

            let delta = if byte_idx + 16 <= payload_len {
                // SAFETY: byte_idx + 16 <= payload_len guarantees 16 contiguous valid bytes within payload.
                // read_unaligned safely loads unaligned 128-bit integers from byte slices.
                let word = unsafe {
                    #[allow(clippy::cast_ptr_alignment)]
                    let ptr = payload.as_ptr().add(byte_idx).cast::<u128>();
                    std::ptr::read_unaligned(ptr).to_le()
                };
                ((word >> bit_in_byte) as u64) & mask
            } else {
                let mut buf = [0u8; 16];
                let rem = payload_len - byte_idx;
                buf[..rem].copy_from_slice(&payload[byte_idx..]);
                let word = u128::from_le_bytes(buf);
                ((word >> bit_in_byte) as u64) & mask
            };

            #[allow(clippy::cast_possible_wrap)]
            {
                *item = (i128::from(min_val) + i128::from(delta)) as i64;
            }
            bit_pos += bw;
        }

        Ok(())
    }
}
