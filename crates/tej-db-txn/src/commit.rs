use crate::conflict::{
    ApprovedRecord, ApprovedSlotWrites, CommittedTxRecord, raw_and_keys_intersect, tables_overlap,
    validate_tx_conflict,
};
use crate::meta::MetaPage;
use crate::ring_buffer;
use crate::spin::AdaptiveSpinGuard;
use crate::write_tx::WriteTx;
use smallvec::SmallVec;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tej_db_btree::{BTreeIndex, RawBatchItem};
use tej_db_core::{DbError, DurabilityMode, PageId, Result, Storage, TxId};

impl<S: Storage> WriteTx<'_, S> {
    pub(crate) fn commit_single(
        &mut self,
        _guard: &AdaptiveSpinGuard<'_>,
        pending: &[RawBatchItem],
    ) -> Result<TxId> {
        let cur_next = self.manager.next_tx_id.load(Ordering::Acquire);
        let cur_root = PageId(self.manager.active_root.load(Ordering::Acquire));
        let mut target_btree = if cur_next == self.snapshot_tx_id.0 + 1 {
            self.btree.clone()
        } else {
            let history = self.manager.commit_history.read();
            validate_tx_conflict(
                &history,
                self.snapshot_tx_id,
                &self.modified_tables,
                pending,
            )?;
            BTreeIndex::new(Arc::clone(&self.manager.cache), Some(cur_root))?
        };

        target_btree.insert_sorted_batch_raw_in_tx(pending, &mut self.tx_pages)?;
        let new_root = target_btree.root_id();

        let commit_tx_id = cur_next;
        let new_next = commit_tx_id + 1;

        let leader_keys: Arc<[tej_db_btree::InlineKey]> =
            pending.iter().map(|item| item.0.clone()).collect();
        {
            let mut history = self.manager.commit_history.write();
            history.push_back(CommittedTxRecord {
                tx_id: TxId(commit_tx_id),
                tables: self.modified_tables.clone(),
                keys: leader_keys,
            });
            if history.len() > 2048 {
                history.pop_front();
            }
        }

        if self.manager.durability() != DurabilityMode::None {
            let tables = self.manager.catalog.to_meta_entries();
            let catalog_root = PageId(self.manager.catalog_root.load(Ordering::Acquire));
            let meta = MetaPage::with_tables(new_root, catalog_root, TxId(new_next), tables);
            let cur_sb = PageId(self.manager.active_superblock.load(Ordering::Acquire));
            let target_sb = crate::superblock::choose_commit_superblock(cur_sb, TxId(new_next));
            crate::superblock::write_superblock(&self.manager.cache, target_sb, &meta)?;
            self.manager
                .active_superblock
                .store(target_sb.0, Ordering::Release);
        }

        match self.manager.durability() {
            DurabilityMode::Immediate => {
                self.manager.cache.flush()?;
            }
            DurabilityMode::Normal | DurabilityMode::Eventual { .. } => {
                self.manager.cache.flush_to_storage()?;
            }
            DurabilityMode::None => {}
        }

        self.manager
            .active_root
            .store(new_root.0, Ordering::Release);
        self.manager.next_tx_id.store(new_next, Ordering::Release);

        Ok(TxId(commit_tx_id))
    }

    pub(crate) fn commit_leader_and_cohort(
        &mut self,
        guard: &AdaptiveSpinGuard<'_>,
        pending: &[RawBatchItem],
    ) -> Result<TxId> {
        let cur_tail = self.manager.ring_buffer.tail.load(Ordering::Relaxed);
        let cur_head = self.manager.ring_buffer.head.load(Ordering::Acquire);

        // Fast path for isolated single-writer without waiting followers
        if cur_tail == cur_head {
            return self.commit_single(guard, pending);
        }

        let cur_next = self.manager.next_tx_id.load(Ordering::Acquire);
        let cur_root = PageId(self.manager.active_root.load(Ordering::Acquire));

        let mut target_btree = if cur_next == self.snapshot_tx_id.0 + 1 {
            self.btree.clone()
        } else {
            let history = self.manager.commit_history.read();
            validate_tx_conflict(
                &history,
                self.snapshot_tx_id,
                &self.modified_tables,
                pending,
            )?;
            BTreeIndex::new(Arc::clone(&self.manager.cache), Some(cur_root))?
        };

        target_btree.insert_sorted_batch_raw_in_tx(pending, &mut self.tx_pages)?;

        let leader_keys: Arc<[tej_db_btree::InlineKey]> =
            pending.iter().map(|item| item.0.clone()).collect();
        let leader_commit_tx = cur_next;
        let mut next_assign_tx = leader_commit_tx + 1;

        let mut scan_tail = cur_tail;
        let mut cohort_tickets: SmallVec<[u64; 64]> = SmallVec::new();
        while scan_tail < cur_head && cohort_tickets.len() < 512 {
            let slot = self.manager.ring_buffer.get_slot(scan_tail);
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

        let mut approved_records: SmallVec<[ApprovedRecord; 64]> = SmallVec::new();
        let mut approved_slot_writes: SmallVec<[ApprovedSlotWrites; 64]> = SmallVec::new();

        if !cohort_tickets.is_empty() {
            let history = self.manager.commit_history.read();
            for &ticket in &cohort_tickets {
                let slot = self.manager.ring_buffer.get_slot(ticket);
                let snap_tx = TxId(slot.snapshot_tx_id.load(Ordering::Relaxed));
                let mut writes_guard = slot.writes.lock();
                let tables_guard = slot.modified_tables.lock();

                let conflict_err =
                    validate_tx_conflict(&history, snap_tx, &tables_guard, &writes_guard).err();

                let mut intra_conflict = false;
                if conflict_err.is_none() {
                    // Check against the leader's writes first
                    if tables_overlap(&tables_guard, &self.modified_tables)
                        && raw_and_keys_intersect(&writes_guard, &leader_keys)
                    {
                        intra_conflict = true;
                    } else {
                        // Check against earlier approved slots in this cohort
                        for (_prev_t, prev_tables, prev_keys) in &approved_records {
                            if tables_overlap(&tables_guard, prev_tables)
                                && raw_and_keys_intersect(&writes_guard, prev_keys)
                            {
                                intra_conflict = true;
                                break;
                            }
                        }
                    }
                }

                if let Some(err) = conflict_err {
                    let mut p = slot.tx_pages.lock();
                    for &pid in p.iter() {
                        self.manager.cache.free_page(pid);
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
                        self.manager.cache.free_page(pid);
                    }
                    p.clear();
                    slot.conflict_tx_id
                        .store(leader_commit_tx, Ordering::Relaxed);
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
                    self.manager.cache.free_page(pid);
                }
            }
        }

        let new_root = target_btree.root_id();

        {
            let mut history = self.manager.commit_history.write();
            history.push_back(CommittedTxRecord {
                tx_id: TxId(leader_commit_tx),
                tables: self.modified_tables.clone(),
                keys: leader_keys,
            });

            for (ticket, tables, keys) in approved_records {
                let commit_tx = next_assign_tx;
                next_assign_tx += 1;
                history.push_back(CommittedTxRecord {
                    tx_id: TxId(commit_tx),
                    tables,
                    keys,
                });
                let slot = self.manager.ring_buffer.get_slot(ticket);
                slot.assigned_tx_id.store(commit_tx, Ordering::Relaxed);
                slot.status
                    .store(ring_buffer::SLOT_COMMITTED, Ordering::Release);
            }
            while history.len() > 2048 {
                history.pop_front();
            }
        }

        if !cohort_tickets.is_empty() {
            self.manager
                .ring_buffer
                .tail
                .store(scan_tail, Ordering::Release);
        }

        if self.manager.durability() != DurabilityMode::None {
            let tables = self.manager.catalog.to_meta_entries();
            let catalog_root = PageId(self.manager.catalog_root.load(Ordering::Acquire));
            let meta = MetaPage::with_tables(new_root, catalog_root, TxId(next_assign_tx), tables);
            let cur_sb = PageId(self.manager.active_superblock.load(Ordering::Acquire));
            let target_sb =
                crate::superblock::choose_commit_superblock(cur_sb, TxId(next_assign_tx));
            crate::superblock::write_superblock(&self.manager.cache, target_sb, &meta)?;
            self.manager
                .active_superblock
                .store(target_sb.0, Ordering::Release);
        }

        match self.manager.durability() {
            DurabilityMode::Immediate => {
                self.manager.cache.flush()?;
            }
            DurabilityMode::Normal | DurabilityMode::Eventual { .. } => {
                self.manager.cache.flush_to_storage()?;
            }
            DurabilityMode::None => {}
        }

        self.manager
            .active_root
            .store(new_root.0, Ordering::Release);
        self.manager
            .next_tx_id
            .store(next_assign_tx, Ordering::Release);

        Ok(TxId(leader_commit_tx))
    }

    pub fn commit(mut self) -> Result<()> {
        // 1. Run before_commit hooks
        if self.has_hooks {
            let hooks = self.manager.hooks.read();
            for (_, hook) in hooks.iter() {
                hook.before_commit(&self.changes)?;
            }
        }

        // 2. Route named table / partitioned mutations
        if !self.table_states.is_empty()
            || !self.created_tables.is_empty()
            || !self.dropped_tables.is_empty()
        {
            return self.commit_tables();
        }

        // 3. Sort pending writes in contiguous memory and deduplicate
        let mut pending = std::mem::take(&mut self.pending_writes);
        if self.is_sorted {
            pending.dedup_by(|a, b| a.0 == b.0);
        } else {
            pending.reverse();
            pending.sort_by(|a, b| a.0.cmp(&b.0));
            pending.dedup_by(|a, b| a.0 == b.0);
        }

        if pending.is_empty() {
            self.committed = true;
            return Ok(());
        }

        // Fast path: try to become Leader immediately
        if let Some(commit_guard) = self.manager.commit_lock.try_lock() {
            let res = self.commit_leader_and_cohort(&commit_guard, &pending);
            if res.is_err() {
                self.manager.drain_and_commit_cohort(&commit_guard);
                drop(commit_guard);
                return res.map(|_| ());
            }
            if self.manager.ring_buffer.head.load(Ordering::Acquire)
                > self.manager.ring_buffer.tail.load(Ordering::Relaxed)
            {
                self.manager.drain_and_commit_cohort(&commit_guard);
            }
            self.committed = true;
            drop(commit_guard);

            if self.has_hooks {
                let hooks = self.manager.hooks.read();
                for (_, hook) in hooks.iter() {
                    hook.after_commit(&self.changes);
                }
            }
            return Ok(());
        }

        // Follower path: zero-allocation atomic slot reservation
        let ticket = self
            .manager
            .ring_buffer
            .head
            .fetch_add(1, Ordering::Relaxed);
        let slot = self.manager.ring_buffer.get_slot(ticket);

        // Spin until slot is empty from any previous ring wrap-around
        let mut spins = 0u32;
        while slot.status.load(Ordering::Acquire) != ring_buffer::SLOT_EMPTY {
            spins = spins.wrapping_add(1);
            if spins < 32 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }

        slot.status
            .store(ring_buffer::SLOT_WRITING, Ordering::Relaxed);
        slot.ticket.store(ticket, Ordering::Relaxed);
        slot.snapshot_tx_id
            .store(self.snapshot_tx_id.0, Ordering::Relaxed);

        {
            let mut w = slot.writes.lock();
            w.clear();
            w.append(&mut pending);
        }
        {
            let mut t = slot.modified_tables.lock();
            t.clear();
            t.extend(self.modified_tables.drain(..));
        }
        {
            let mut p = slot.tx_pages.lock();
            *p = std::mem::take(&mut self.tx_pages);
        }

        slot.status
            .store(ring_buffer::SLOT_READY, Ordering::Release);

        // Spin on slot.status awaiting completion
        spins = 0;
        loop {
            let s = slot.status.load(Ordering::Acquire);
            if s == ring_buffer::SLOT_COMMITTED {
                self.committed = true;
                slot.status
                    .store(ring_buffer::SLOT_EMPTY, Ordering::Release);
                if self.has_hooks {
                    let hooks = self.manager.hooks.read();
                    for (_, hook) in hooks.iter() {
                        hook.after_commit(&self.changes);
                    }
                }
                return Ok(());
            }
            if s == ring_buffer::SLOT_CONFLICT {
                let conf_tx = slot.conflict_tx_id.load(Ordering::Relaxed);
                slot.status
                    .store(ring_buffer::SLOT_EMPTY, Ordering::Release);
                return Err(DbError::WriteConflict {
                    key: Vec::new(),
                    committed_tx: conf_tx,
                });
            }

            if spins < 32 {
                std::hint::spin_loop();
                spins += 1;
                continue;
            }

            // Check if we can become Leader to drain the cohort
            if let Some(commit_guard) = self.manager.commit_lock.try_lock() {
                self.manager.drain_and_commit_cohort(&commit_guard);
                drop(commit_guard);
                continue;
            }

            std::thread::yield_now();
            spins = spins.saturating_add(1);
        }
    }
}
