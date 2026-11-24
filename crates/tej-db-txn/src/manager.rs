use crate::catalog::TableCatalog;
use crate::conflict::CommittedTxRecord;
use crate::meta::MetaPage;
use crate::read_tx::ReadOnlyTx;
use crate::ring_buffer;
use crate::spin::AdaptiveSpinLock;
use crate::write_tx::WriteTx;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use tej_db_btree::BTreeIndex;
use tej_db_core::{DurabilityMode, Hook, Page, PageId, PageType, Result, Storage, TxId};
use tej_db_pager::PageCache;

pub struct TxManager<S: Storage> {
    pub(crate) cache: Arc<PageCache<S>>,
    pub(crate) active_root: AtomicU64,
    pub(crate) catalog_root: AtomicU64,
    pub(crate) active_superblock: AtomicU64,
    pub(crate) catalog: TableCatalog,
    pub(crate) next_tx_id: AtomicU64,
    durability: RwLock<DurabilityMode>,
    pub(crate) commit_lock: AdaptiveSpinLock,
    pub(crate) table_commit_locks: RwLock<HashMap<String, Arc<AdaptiveSpinLock>>>,
    pub(crate) commit_history: RwLock<VecDeque<CommittedTxRecord>>,
    pub(crate) hooks: RwLock<Vec<(usize, Box<dyn Hook>)>>,
    next_hook_id: AtomicUsize,
    flusher_stop: RwLock<Arc<AtomicBool>>,
    flusher_handle: parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>,
    pub(crate) ring_buffer: ring_buffer::DisruptorRingBuffer,
}

impl<S: Storage> fmt::Debug for TxManager<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TxManager")
            .field("active_root", &self.active_root.load(Ordering::Relaxed))
            .field("catalog_root", &self.catalog_root.load(Ordering::Relaxed))
            .field("next_tx_id", &self.next_tx_id.load(Ordering::Relaxed))
            .field("durability", &self.durability())
            .field("active_readers", &self.active_readers())
            .finish_non_exhaustive()
    }
}

impl<S: Storage> TxManager<S> {
    pub fn open(cache: Arc<PageCache<S>>) -> Result<Self> {
        let (meta, active_sb) = match crate::superblock::load_active_superblock(&cache)? {
            Some((m, slot)) => (m, slot),
            None => Self::init_db(&cache)?,
        };

        let mut all_tables = meta.tables.to_vec();
        if meta.catalog_root.is_valid() {
            if let Ok(cat_entries) =
                crate::catalog_tree::load_catalog_entries(&cache, meta.catalog_root)
            {
                for entry in cat_entries {
                    if !all_tables.iter().any(|t| t.name == entry.name) {
                        all_tables.push(entry);
                    }
                }
            }
        }

        let catalog = TableCatalog::new(&all_tables);

        Ok(Self {
            cache,
            active_root: AtomicU64::new(meta.active_root.0),
            catalog_root: AtomicU64::new(meta.catalog_root.0),
            active_superblock: AtomicU64::new(active_sb.0),
            catalog,
            next_tx_id: AtomicU64::new(meta.next_tx_id.0),
            durability: RwLock::new(DurabilityMode::Immediate),
            commit_lock: AdaptiveSpinLock::new(),
            table_commit_locks: RwLock::new(HashMap::new()),
            commit_history: RwLock::new(VecDeque::with_capacity(256)),
            hooks: RwLock::new(Vec::new()),
            next_hook_id: AtomicUsize::new(1),
            flusher_stop: RwLock::new(Arc::new(AtomicBool::new(false))),
            flusher_handle: parking_lot::Mutex::new(None),
            ring_buffer: ring_buffer::DisruptorRingBuffer::new(),
        })
    }

    pub(crate) fn table_commit_lock(&self, table_name: &str) -> Arc<AdaptiveSpinLock> {
        {
            let r = self.table_commit_locks.read();
            if let Some(lock) = r.get(table_name) {
                return Arc::clone(lock);
            }
        }
        let mut w = self.table_commit_locks.write();
        w.entry(table_name.to_string())
            .or_insert_with(|| Arc::new(AdaptiveSpinLock::new()))
            .clone()
    }

    fn init_db(cache: &Arc<PageCache<S>>) -> Result<(MetaPage, PageId)> {
        let sb_a = cache.allocate_page()?;
        let sb_b = cache.allocate_page()?;
        let root_id = cache.allocate_page()?;

        let leaf_page = Page::new_leaf(root_id, PageId::INVALID);
        cache.write_page(leaf_page)?;

        let btree = BTreeIndex::new(Arc::clone(cache), Some(root_id))?;
        let initial_meta = MetaPage::new(btree.root_id());

        let mut page_a = Page::new(sb_a, PageType::Meta);
        initial_meta.write_to_page(&mut page_a)?;
        cache.write_page(page_a)?;

        let mut page_b = Page::new(sb_b, PageType::Meta);
        initial_meta.write_to_page(&mut page_b)?;
        cache.write_page(page_b)?;

        cache.flush()?;
        Ok((initial_meta, sb_a))
    }

    pub fn add_hook(&self, hook: Box<dyn Hook>) -> usize {
        let id = self.next_hook_id.fetch_add(1, Ordering::SeqCst);
        self.hooks.write().push((id, hook));
        id
    }

    pub fn remove_hook(&self, id: usize) -> bool {
        let mut hooks = self.hooks.write();
        if let Some(pos) = hooks.iter().position(|(hid, _)| *hid == id) {
            hooks.swap_remove(pos);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn cache(&self) -> &Arc<PageCache<S>> {
        &self.cache
    }

    pub fn begin_read(&self) -> Result<ReadOnlyTx<S>> {
        self.cache.acquire_reader();

        let root_id = PageId(self.active_root.load(Ordering::Acquire));
        let tx_id = TxId(self.next_tx_id.load(Ordering::Acquire));
        let btree = BTreeIndex::new(Arc::clone(&self.cache), Some(root_id))?;
        let catalog_snapshot = self.catalog.snapshot();

        Ok(ReadOnlyTx::new(
            tx_id,
            btree,
            Arc::clone(&self.cache),
            catalog_snapshot,
        ))
    }

    pub fn begin_write(&self) -> Result<WriteTx<'_, S>> {
        self.cache.acquire_reader();

        let cur_next = self.next_tx_id.load(Ordering::Acquire);
        let cur_root = PageId(self.active_root.load(Ordering::Acquire));
        let tx_id = TxId(cur_next);
        let snapshot_tx_id = TxId(cur_next.saturating_sub(1));
        let snapshot_root = cur_root;

        let btree = match BTreeIndex::new(Arc::clone(&self.cache), Some(snapshot_root)) {
            Ok(bt) => bt,
            Err(e) => {
                self.cache.release_reader();
                return Err(e);
            }
        };
        Ok(WriteTx::new(
            self,
            tx_id,
            snapshot_root,
            snapshot_tx_id,
            btree,
        ))
    }

    pub(crate) fn has_hooks(&self) -> bool {
        !self.hooks.read().is_empty()
    }

    pub fn set_durability(&self, mode: DurabilityMode) {
        *self.durability.write() = mode;
        if let DurabilityMode::Eventual { interval_ms } = mode {
            self.start_flusher_thread(interval_ms);
        } else {
            self.stop_flusher_thread();
        }
    }

    fn start_flusher_thread(&self, interval_ms: u64) {
        self.stop_flusher_thread();
        let stop_flag = Arc::new(AtomicBool::new(false));
        *self.flusher_stop.write() = Arc::clone(&stop_flag);

        let cache = Arc::clone(&self.cache);
        let duration = std::time::Duration::from_millis(interval_ms.max(1));

        if let Ok(handle) = std::thread::Builder::new()
            .name("tej-db-flusher".into())
            .spawn(move || {
                while !stop_flag.load(Ordering::Relaxed) {
                    let mut elapsed = std::time::Duration::ZERO;
                    while elapsed < duration && !stop_flag.load(Ordering::Relaxed) {
                        let step = std::time::Duration::from_millis(5)
                            .min(duration.saturating_sub(elapsed));
                        std::thread::sleep(step);
                        elapsed += step;
                    }
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = cache.sync();
                }
            })
        {
            *self.flusher_handle.lock() = Some(handle);
        }
    }

    fn stop_flusher_thread(&self) {
        let stop_flag = self.flusher_stop.read();
        stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.flusher_handle.lock().take() {
            let _ = handle.join();
        }
    }

    #[must_use]
    pub fn durability(&self) -> DurabilityMode {
        *self.durability.read()
    }

    #[must_use]
    pub fn active_readers(&self) -> usize {
        self.cache.active_readers()
    }

    #[must_use]
    pub fn active_root(&self) -> PageId {
        PageId(self.active_root.load(Ordering::Acquire))
    }
}

impl<S: Storage> Drop for TxManager<S> {
    fn drop(&mut self) {
        self.stop_flusher_thread();
    }
}
