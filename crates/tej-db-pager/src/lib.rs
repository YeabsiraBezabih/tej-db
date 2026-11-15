extern crate alloc;

pub mod cache;
pub mod compression;
pub mod encryption;
pub mod file;
pub mod mem;
#[cfg(feature = "mmap")]
pub mod mmap;
pub mod mmap_view;

pub use cache::{FastPageIdBuildHasher, FastPageIdHasher, PageCache};
#[cfg(feature = "compression")]
pub use compression::{CompressedStorage, compress_page, decompress_page};
#[cfg(feature = "encryption")]
pub use encryption::{EncryptedStorage, decrypt_page_data, encrypt_page_data};
pub use file::FileStorage;
pub use mem::MemStorage;
#[cfg(feature = "mmap")]
pub use mmap::MmapStorage;
pub use mmap_view::{ZeroCopyPageView, view_page_at_offset};

#[cfg(test)]
mod tests;
