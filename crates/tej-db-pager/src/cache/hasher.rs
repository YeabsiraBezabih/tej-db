#[derive(Debug, Clone, Copy, Default)]
pub struct FastPageIdBuildHasher;

#[derive(Debug, Clone, Copy)]
pub struct FastPageIdHasher(u64);

impl core::hash::Hasher for FastPageIdHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        if bytes.len() == 8 {
            let val = u64::from_ne_bytes(bytes.try_into().unwrap_or([0; 8]));
            self.write_u64(val);
        } else {
            for &b in bytes {
                self.0 = self.0.wrapping_mul(0x100_0000_01b3) ^ u64::from(b);
            }
        }
    }

    #[inline]
    fn write_u64(&mut self, val: u64) {
        // Fast multiply-xorshift permutation with excellent avalanche properties
        let mut x = val ^ (val >> 30);
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        self.0 = x;
    }
}

impl core::hash::BuildHasher for FastPageIdBuildHasher {
    type Hasher = FastPageIdHasher;

    #[inline]
    fn build_hasher(&self) -> Self::Hasher {
        FastPageIdHasher(0)
    }
}
