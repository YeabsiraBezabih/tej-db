extern crate alloc;

pub mod cache;
pub mod compression;
pub mod encryption;
#[cfg(not(target_arch = "wasm32"))]
pub mod file;
#[cfg(not(target_arch = "wasm32"))]
pub mod lock;
pub mod mem;
#[cfg(all(feature = "mmap", not(target_arch = "wasm32")))]
pub mod mmap;
#[cfg(not(target_arch = "wasm32"))]
pub mod mmap_view;

pub use cache::{FastPageIdBuildHasher, FastPageIdHasher, PageCache};
#[cfg(feature = "compression")]
pub use compression::{CompressedStorage, compress_page, decompress_page};
#[cfg(feature = "encryption")]
pub use encryption::{EncryptedStorage, decrypt_page_data, encrypt_page_data};
#[cfg(not(target_arch = "wasm32"))]
pub use file::FileStorage;
#[cfg(not(target_arch = "wasm32"))]
pub use lock::FileLock;
pub use mem::MemStorage;
#[cfg(all(feature = "mmap", not(target_arch = "wasm32")))]
pub use mmap::MmapStorage;
#[cfg(not(target_arch = "wasm32"))]
pub use mmap_view::{ZeroCopyPageView, view_page_at_offset};

#[cfg(test)]
mod tests;
