pub mod bitpack;
pub mod dict;
pub mod rle;

#[cfg(test)]
mod tests;

pub use bitpack::BitPackingCodec;
pub use dict::DictionaryCodec;
pub use rle::RleCodec;
use serde::{Deserialize, Serialize};

/// Supported columnar compression formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionType {
    Uncompressed,
    BitPacking,
    Rle,
    Dictionary,
}
