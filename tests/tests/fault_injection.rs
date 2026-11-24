use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tej_db::Database;
use tej_db_core::{DbError, Page, PageId, PageType, Result, Storage};
use tej_db_pager::{MemStorage, PageCache};

/// Fault-injecting storage wrapper that can corrupt bytes or inject I/O errors.
struct CorruptingStorage {
    inner: MemStorage,
    corrupt_reads: bool,
    fail_writes: Arc<AtomicBool>,
}

impl CorruptingStorage {
    fn new() -> Self {
        Self::with_fail_trigger(Arc::new(AtomicBool::new(false)))
    }

    fn with_fail_trigger(fail_writes: Arc<AtomicBool>) -> Self {
        Self {
            inner: MemStorage::new(),
            corrupt_reads: false,
            fail_writes,
        }
    }
}

impl Storage for CorruptingStorage {
    fn read_page(&self, id: PageId, buf: &mut [u8]) -> Result<()> {
        self.inner.read_page(id, buf)?;
        if self.corrupt_reads && id == PageId(0) {
            // Flip a byte in the payload to simulate torn write / disk corruption
            buf[100] ^= 0xAA;
        }
        Ok(())
    }

    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()> {
        if self.fail_writes.load(Ordering::Acquire) {
            return Err(DbError::Io(std::io::Error::new(
                ErrorKind::StorageFull,
                "No space left on device (simulated ENOSPC)",
            )));
        }
        self.inner.write_page(id, buf)
    }

    fn sync(&self) -> Result<()> {
        self.inner.sync()
    }

    fn page_count(&self) -> u64 {
        self.inner.page_count()
    }

    fn allocate_page(&mut self) -> Result<PageId> {
        if self.fail_writes.load(Ordering::Acquire) {
            return Err(DbError::Io(std::io::Error::new(
                ErrorKind::StorageFull,
                "No space left on device (simulated ENOSPC)",
            )));
        }
        self.inner.allocate_page()
    }
}

#[test]
fn test_checksum_detects_disk_corruption() {
    let mut storage = CorruptingStorage::new();
    let pid = storage.allocate_page().unwrap();
    let page = Page::new(pid, PageType::BTreeLeaf);
    storage.write_page(pid, page.as_slice()).unwrap();

    // Enable simulated disk bit rot / corruption
    storage.corrupt_reads = true;

    let cache = PageCache::new(storage, 10);
    let result = cache.get_page(pid);

    match result {
        Err(DbError::ChecksumMismatch { page_id, .. }) => {
            assert_eq!(page_id, pid);
        }
        other => panic!("Expected ChecksumMismatch, got: {other:?}"),
    }
}

#[test]
fn test_truncated_mid_page_file_detected() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("truncated.db");

    // Write 5000 bytes (1 full 4096B page + 904 torn bytes)
    {
        let mut file = File::create(&db_path).unwrap();
        let buf = vec![0xABu8; 5000];
        file.write_all(&buf).unwrap();
        file.sync_all().unwrap();
    }

    let open_res = Database::open_file(&db_path);
    assert!(open_res.is_err(), "Opening truncated file must fail");
    match open_res {
        Err(DbError::Corruption(msg)) => {
            assert!(msg.contains("not a multiple of PAGE_SIZE"));
        }
        other => panic!("Expected DbError::Corruption, got: {other:?}"),
    }
}

#[test]
fn test_empty_zero_byte_file_auto_initialized() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("empty_init.db");

    // Create a 0-byte file explicitly
    File::create(&db_path).unwrap();
    assert_eq!(fs::metadata(&db_path).unwrap().len(), 0);

    // Open file: engine must auto-initialize meta and root pages
    {
        let db = Database::open_file(&db_path).unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.put(b"init_key", b"init_val").unwrap();
        tx.commit().unwrap();
    }

    // Verify it persists and can be reopened
    {
        let db = Database::open_file(&db_path).unwrap();
        let rx = db.begin_read().unwrap();
        assert_eq!(rx.get(b"init_key").unwrap(), Some(b"init_val".to_vec()));
    }
}

#[test]
fn test_enospc_disk_full_simulation() {
    let fail_writes = Arc::new(AtomicBool::new(false));
    let storage = CorruptingStorage::with_fail_trigger(Arc::clone(&fail_writes));
    let db = Database::open_custom(storage, 10).unwrap();

    // 1. Initial committed state succeeds
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"k1", b"v1").unwrap();
        tx.commit().unwrap();
    }

    // 2. Begin transaction, insert k2, and trigger simulated ENOSPC failure during commit
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"k2", b"v2").unwrap();

        // Inject disk full prior to commit flush
        fail_writes.store(true, Ordering::Release);
        let commit_res = tx.commit();
        assert!(commit_res.is_err(), "Commit must fail when disk is full");
    }

    // 3. Verify database state remains uncorrupted at k1=v1; k2 must NOT be visible
    let rx = db.begin_read().unwrap();
    assert_eq!(rx.get(b"k1").unwrap(), Some(b"v1".to_vec()));
    assert_eq!(rx.get(b"k2").unwrap(), None);

    // 4. Recover from ENOSPC when disk space is cleared
    fail_writes.store(false, Ordering::Release);
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(b"k3", b"v3").unwrap();
        tx.commit().unwrap();
    }

    let rx2 = db.begin_read().unwrap();
    assert_eq!(rx2.get(b"k1").unwrap(), Some(b"v1".to_vec()));
    assert_eq!(rx2.get(b"k2").unwrap(), None);
    assert_eq!(rx2.get(b"k3").unwrap(), Some(b"v3".to_vec()));
}
