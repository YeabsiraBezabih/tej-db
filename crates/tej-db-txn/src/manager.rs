use crate::catalog::TableCatalog;
use crate::conflict::{
    ApprovedRecord, ApprovedSlotWrites, CommittedTxRecord, raw_and_keys_intersect, tables_overlap,
    validate_tx_conflict,
};
use crate::meta::MetaPage;
use crate::read_tx::ReadOnlyTx;
use crate::ring_buffer;
use crate::spin::{AdaptiveSpinGuard, AdaptiveSpinLock};
use crate::write_tx::WriteTx;
use parking_lot::RwLock;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use tej_db_btree::BTreeIndex;
use tej_db_core::{DbError, DurabilityMode, Hook, Page, PageId, PageType, Result, Storage, TxId};
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

        std::thread::Builder::new()
            .name("tej-db-flusher".into())
            .spawn(move || {
                while !stop_flag.load(Ordering::Relaxed) {
                    std::thread::sleep(duration);
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = cache.sync();
                }
            })
            .ok();
    }

    fn stop_flusher_thread(&self) {
        let stop_flag = self.flusher_stop.read();
        stop_flag.store(true, Ordering::SeqCst);
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

    pub fn drain_and_commit_cohort(&self, _guard: &AdaptiveSpinGuard<'_>) {
        let cur_tail = self.ring_buffer.tail.load(Ordering::Relaxed);
        let cur_head = self.ring_buffer.head.load(Ordering::Acquire);
        if cur_tail == cur_head {
            return;
        }

        let mut scan_tail = cur_tail;
        let mut cohort_tickets: SmallVec<[u64; 64]> = SmallVec::new();
        while scan_tail < cur_head && cohort_tickets.len() < 512 {
            let slot = self.ring_buffer.get_slot(scan_tail);
            let mut sp = 0u32;
            while slot.status.load(Ordering::Acquire) == ring_buffer::SLOT_WRITING && sp < 512 {
                std::hint::spin_loop();
                sp += 1;
            }
            if slot.status.load(Ordering::Acquire) != ring_buffer::SLOT_READY {
                break;
            }
            cohort_tickets.push(scan_tail);
            scan_tail += 1;
        }

        if cohort_tickets.is_empty() {
            return;
        }

        // Phase 1: Validate conflicts in one pass with a single history read lock
        let mut approved_records: SmallVec<[ApprovedRecord; 64]> = SmallVec::new();
        let mut approved_slot_writes: SmallVec<[ApprovedSlotWrites; 64]> = SmallVec::new();

        {
            let history = self.commit_history.read();
            for &ticket in &cohort_tickets {
                let slot = self.ring_buffer.get_slot(ticket);
                let snap_tx = TxId(slot.snapshot_tx_id.load(Ordering::Relaxed));
                let mut writes_guard = slot.writes.lock();
                let tables_guard = slot.modified_tables.lock();

                // Check conflict against committed history
                let conflict_err =
                    validate_tx_conflict(&history, snap_tx, &tables_guard, &writes_guard).err();

                // Check conflict against earlier approved slots in this same cohort
                let mut intra_conflict = false;
                if conflict_err.is_none() {
                    for (_prev_t, prev_tables, prev_keys) in &approved_records {
                        if tables_overlap(&tables_guard, prev_tables)
                            && raw_and_keys_intersect(&writes_guard, prev_keys)
                        {
                            intra_conflict = true;
                            break;
                        }
                    }
                }

                if let Some(err) = conflict_err {
                    let mut p = slot.tx_pages.lock();
                    for &pid in p.iter() {
                        self.cache.free_page(pid);
                    }
                    p.clear();
                    let conf_tx = match err {
                        DbError::WriteConflict { committed_tx, .. } => committed_tx,
                        _ => 0,
                    };
                    slot.conflict_tx_id.store(conf_tx, Ordering::Relaxed);
                    slot.status
                        .store(ring_buffer::SLOT_CONFLICT, Ordering::Release);
                } else if intra_conflict {
                    let mut p = slot.tx_pages.lock();
                    for &pid in p.iter() {
                        self.cache.free_page(pid);
                    }
                    p.clear();
                    slot.conflict_tx_id.store(0, Ordering::Relaxed);
                    slot.status
                        .store(ring_buffer::SLOT_CONFLICT, Ordering::Release);
                } else {
                    let keys: Arc<[tej_db_btree::InlineKey]> =
                        writes_guard.iter().map(|item| item.0.clone()).collect();
                    let tables = tables_guard.clone();
                    let mut p = slot.tx_pages.lock();
                    let slot_pages = std::mem::take(&mut *p);
                    let slot_writes = std::mem::take(&mut *writes_guard);
                    approved_records.push((ticket, tables, keys));
                    approved_slot_writes.push((slot_writes, slot_pages));
                }
            }
        }

        if approved_records.is_empty() {
            self.ring_buffer.tail.store(scan_tail, Ordering::Release);
            return;
        }

        // Phase 2 & 3: Single B-Tree Target with pre-sorted slot batches
        let cur_root = PageId(self.active_root.load(Ordering::Acquire));
        let mut target_btree =
            if let Ok(bt) = BTreeIndex::new(Arc::clone(&self.cache), Some(cur_root)) {
                bt
            } else {
                for (ticket, _, _) in &approved_records {
                    let slot = self.ring_buffer.get_slot(*ticket);
                    slot.status
                        .store(ring_buffer::SLOT_CONFLICT, Ordering::Release);
                }
                self.ring_buffer.tail.store(scan_tail, Ordering::Release);
                return;
            };

        let mut combined_writes = Vec::new();
        let mut combined_pages = tej_db_btree::TxPageSet::default();
        for (slot_writes, slot_pages) in approved_slot_writes {
            combined_writes.extend(slot_writes);
            for pid in slot_pages {
                combined_pages.insert(pid);
            }
        }

        if !combined_writes.is_empty() {
            combined_writes.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            combined_writes.dedup_by(|a, b| a.0 == b.0);
            if target_btree
                .insert_sorted_batch_raw_in_tx(&combined_writes, &mut combined_pages)
                .is_err()
            {
                for &pid in &combined_pages {
                    self.cache.free_page(pid);
                }
            }
        }

        let new_root = target_btree.root_id();

        // Phase 4: Assign TxIDs and record history in single write lock
        let mut cur_next = self.next_tx_id.load(Ordering::Acquire);
        {
            let mut history = self.commit_history.write();
            for (ticket, tables, keys) in approved_records {
                let commit_tx = cur_next;
                cur_next += 1;
                history.push_back(CommittedTxRecord {
                    tx_id: TxId(commit_tx),
                    tables,
                    keys,
                });
                let slot = self.ring_buffer.get_slot(ticket);
                slot.assigned_tx_id.store(commit_tx, Ordering::Relaxed);
                slot.status
                    .store(ring_buffer::SLOT_COMMITTED, Ordering::Release);
            }
            while history.len() > 2048 {
                history.pop_front();
            }
        }

        // Phase 5: Atomic metadata update and durability flush
        self.active_root.store(new_root.0, Ordering::Release);
        self.next_tx_id.store(cur_next, Ordering::Release);

        if self.durability() != DurabilityMode::None {
            let tables = self.catalog.to_meta_entries();
            let catalog_root = PageId(self.catalog_root.load(Ordering::Acquire));
            let meta = MetaPage::with_tables(new_root, catalog_root, TxId(cur_next), tables);
            let cur_sb = PageId(self.active_superblock.load(Ordering::Acquire));
            let target_sb = crate::superblock::choose_commit_superblock(cur_sb, TxId(cur_next));
            let _ = crate::superblock::write_superblock(&self.cache, target_sb, &meta);
            self.active_superblock.store(target_sb.0, Ordering::Release);
        }

        match self.durability() {
            DurabilityMode::Immediate => {
                let _ = self.cache.flush();
            }
            DurabilityMode::Normal | DurabilityMode::Eventual { .. } => {
                let _ = self.cache.flush_to_storage();
            }
            DurabilityMode::None => {}
        }

        // Phase 6: Advance Ring Buffer Tail
        self.ring_buffer.tail.store(scan_tail, Ordering::Release);
    }
}

impl<S: Storage> Drop for TxManager<S> {
    fn drop(&mut self) {
        self.stop_flusher_thread();
    }
}
