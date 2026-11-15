#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod error;
pub mod page;
pub mod spatial;
pub mod traits;
pub mod types;

pub use error::{DbError, Result};
pub use page::{
    MAX_INLINE_VAL_SIZE, OVERFLOW_FLAG, PAGE_HEADER_SIZE, PAGE_MAGIC, PAGE_SIZE, Page, PageHeader,
    PageType,
};
pub use spatial::{
    Aabb3D, BoundingSphere, GAUSSIAN_SPLAT_HEADER_SIZE, GAUSSIAN_SPLAT_MAGIC,
    GaussianSplatPageHeader, MESHLET_HEADER_SIZE, MESHLET_MAGIC, MeshletPageHeader,
    decode_spatial_key, dequantize_position, encode_spatial_key, quantize_position,
};
pub use traits::{Durability, Hook, Index, Storage};
pub use types::{
    BatchOp, ChangeSet, DbStats, DurabilityMode, KV, KeyRange, PageId, TxId, WriteBatch,
};

#[cfg(test)]
mod tests;
