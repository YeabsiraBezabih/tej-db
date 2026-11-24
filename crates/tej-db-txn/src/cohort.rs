use crate::conflict::{
    ApprovedRecord, ApprovedSlotWrites, CommittedTxRecord, raw_and_keys_intersect, tables_overlap,
    validate_tx_conflict,
};
use crate::manager::TxManager;
use crate::meta::MetaPage;
use crate::ring_buffer;
use crate::spin::AdaptiveSpinGuard;
use smallvec::SmallVec;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tej_db_btree::BTreeIndex;
use tej_db_core::{DbError, DurabilityMode, PageId, Result, Storage, TxId};

impl<S: Storage> TxManager<S> {
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
                for (_, slot_pages) in approved_slot_writes {
                    for pid in slot_pages {
                        self.cache.free_page(pid);
                    }
                }
                for (ticket, _, _) in &approved_records {
                    let slot = self.ring_buffer.get_slot(*ticket);
                    slot.status
                        .store(ring_buffer::SLOT_ABORTED, Ordering::Release);
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
            if let Err(e) =
                target_btree.insert_sorted_batch_raw_in_tx(&combined_writes, &mut combined_pages)
            {
                for &pid in &combined_pages {
                    self.cache.free_page(pid);
                }
                for (ticket, _, _) in &approved_records {
                    let slot = self.ring_buffer.get_slot(*ticket);
                    slot.status
                        .store(ring_buffer::SLOT_ABORTED, Ordering::Release);
                }
                self.ring_buffer.tail.store(scan_tail, Ordering::Release);
                tracing::error!("Error committing cohort batch: {e}");
                return;
            }
        }

        let new_root = target_btree.root_id();

        // Phase 4: Prepare TxIDs
        let cur_next = self.next_tx_id.load(Ordering::Acquire);
        let final_next = cur_next + approved_records.len() as u64;

        // Phase 5: Two-Phase Durability Flush Barrier
        let mut target_sb_opt = None;
        let durability_res = (|| -> Result<()> {
            match self.durability() {
                DurabilityMode::Immediate => {
                    self.cache.flush_data_pages()?;
                    self.cache.sync()?;
                }
                DurabilityMode::Normal | DurabilityMode::Eventual { .. } => {
                    self.cache.flush_data_pages()?;
                }
                DurabilityMode::None => {}
            }

            if self.durability() != DurabilityMode::None {
                let tables = self.catalog.to_meta_entries();
                let catalog_root = PageId(self.catalog_root.load(Ordering::Acquire));
                let meta = MetaPage::with_tables(new_root, catalog_root, TxId(final_next), tables);
                let cur_sb = PageId(self.active_superblock.load(Ordering::Acquire));
                let target_sb =
                    crate::superblock::choose_commit_superblock(cur_sb, TxId(final_next));
                target_sb_opt = Some(target_sb);
                crate::superblock::write_superblock(&self.cache, target_sb, &meta)?;
                self.cache.flush_page(target_sb)?;
                if self.durability() == DurabilityMode::Immediate {
                    self.cache.sync()?;
                }
                self.active_superblock.store(target_sb.0, Ordering::Release);
            }
            Ok(())
        })();

        if let Err(e) = durability_res {
            if let Some(target_sb) = target_sb_opt {
                self.cache.discard_page(target_sb);
            }
            for &pid in &combined_pages.allocated {
                self.cache.free_page(pid);
            }
            for (ticket, _, _) in &approved_records {
                let slot = self.ring_buffer.get_slot(*ticket);
                slot.status
                    .store(ring_buffer::SLOT_ABORTED, Ordering::Release);
            }
            self.ring_buffer.tail.store(scan_tail, Ordering::Release);
            tracing::error!("Error committing cohort durability barrier: {e}");
            return;
        }

        // Phase 6: Commit state publication strictly after durability succeeds
        self.active_root.store(new_root.0, Ordering::Release);
        self.next_tx_id.store(final_next, Ordering::Release);

        let mut committed_slots = Vec::with_capacity(approved_records.len());
        {
            let mut history = self.commit_history.write();
            for (commit_tx, (ticket, tables, keys)) in (cur_next..).zip(approved_records) {
                history.push_back(CommittedTxRecord {
                    tx_id: TxId(commit_tx),
                    tables,
                    keys,
                });
                committed_slots.push((ticket, commit_tx));
            }
            while history.len() > 2048 {
                history.pop_front();
            }
        }

        // Phase 7: Advance Ring Buffer Tail
        self.ring_buffer.tail.store(scan_tail, Ordering::Release);

        // Phase 8: Safe reclamation of retired pages after persistence
        for pid in combined_pages.retired {
            self.cache.free_page(pid);
        }

        // Phase 9: Notify approved followers only after persistence
        for (ticket, commit_tx) in committed_slots {
            let slot = self.ring_buffer.get_slot(ticket);
            slot.assigned_tx_id.store(commit_tx, Ordering::Relaxed);
            slot.status
                .store(ring_buffer::SLOT_COMMITTED, Ordering::Release);
        }
    }
}
