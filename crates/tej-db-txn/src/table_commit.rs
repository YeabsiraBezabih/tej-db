use crate::conflict::{CommittedTxRecord, validate_tx_conflict};
use crate::meta::MetaPage;
use crate::write_tx::WriteTx;
use smallvec::smallvec;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tej_db_btree::BTreeIndex;
use tej_db_core::{DurabilityMode, PageId, Result, Storage, TxId};

impl<S: Storage> WriteTx<'_, S> {
    pub(crate) fn commit_tables(&mut self) -> Result<()> {
        // 1. Sort and dedup writes for each modified table
        for state in &mut self.table_states {
            if state.is_sorted {
                state.pending_writes.dedup_by(|a, b| a.0 == b.0);
            } else {
                state.pending_writes.reverse();
                state.pending_writes.sort_by(|a, b| a.0.cmp(&b.0));
                state.pending_writes.dedup_by(|a, b| a.0 == b.0);
            }
        }

        // 2. Identify all tables that need locking, sorted to prevent deadlocks
        let mut lock_tables: Vec<String> = self
            .table_states
            .iter()
            .filter(|s| {
                !s.pending_writes.is_empty() || self.created_tables.iter().any(|t| t == &s.name)
            })
            .map(|s| s.name.clone())
            .chain(self.created_tables.iter().cloned())
            .chain(self.dropped_tables.iter().cloned())
            .collect();
        lock_tables.sort();
        lock_tables.dedup();

        // 3. Acquire table-level commit locks in sorted order
        let mut entry_locks = Vec::with_capacity(lock_tables.len());
        let mut fallback_locks = Vec::with_capacity(lock_tables.len());
        for tbl in &lock_tables {
            if let Some(entry) = self.manager.catalog.get_table_entry(tbl) {
                entry_locks.push(entry);
            } else {
                fallback_locks.push(self.manager.table_commit_lock(tbl));
            }
        }
        let _table_guards: (Vec<_>, Vec<_>) = (
            entry_locks.iter().map(|e| e.commit_lock.lock()).collect(),
            fallback_locks.iter().map(|l| l.lock()).collect(),
        );

        // 4. OCC conflict validation against commit history
        let is_single_table = self.table_states.len() == 1
            && self.dropped_tables.is_empty()
            && self.pending_writes.is_empty()
            && (self.created_tables.is_empty()
                || (self.created_tables.len() == 1
                    && self.created_tables[0] == self.table_states[0].name));

        let needs_validation = if is_single_table {
            let state = &self.table_states[0];
            if state.pending_writes.is_empty() {
                false
            } else if let Some(entry) = self.manager.catalog.get_table_entry(&state.name) {
                entry.last_commit_tx.load(Ordering::Acquire) > self.snapshot_tx_id.0
            } else {
                true
            }
        } else {
            let cur_next = self.manager.next_tx_id.load(Ordering::Acquire);
            cur_next > self.snapshot_tx_id.0 + 1
        };

        if needs_validation {
            if is_single_table {
                let state = &self.table_states[0];
                if let Some(entry) = self.manager.catalog.get_table_entry(&state.name) {
                    let history = entry.history.read();
                    validate_tx_conflict(
                        &history,
                        self.snapshot_tx_id,
                        &self.modified_tables,
                        &state.pending_writes,
                    )?;
                }
            } else {
                let history = self.manager.commit_history.read();
                for state in &self.table_states {
                    if !state.pending_writes.is_empty() {
                        validate_tx_conflict(
                            &history,
                            self.snapshot_tx_id,
                            &self.modified_tables,
                            &state.pending_writes,
                        )?;
                    }
                }
            }
        }

        // 5. Commit mutations to each table's B-Tree
        for state in &mut self.table_states {
            if state.pending_writes.is_empty() {
                if self.created_tables.iter().any(|t| t == &state.name) {
                    self.manager
                        .catalog
                        .update_table_root(&state.name, state.snapshot_root);
                }
            } else {
                let cur_root = self
                    .manager
                    .catalog
                    .get_table_root(&state.name)
                    .unwrap_or(state.snapshot_root);
                let mut target_btree = if cur_root == state.snapshot_root {
                    state.btree.clone()
                } else {
                    BTreeIndex::new(Arc::clone(&self.manager.cache), Some(cur_root))?
                };

                target_btree
                    .insert_sorted_batch_raw_in_tx(&state.pending_writes, &mut state.tx_pages)?;
                let new_root = target_btree.root_id();
                self.manager
                    .catalog
                    .update_table_root(&state.name, new_root);
            }
        }

        // 6. Apply table drops
        for tbl in &self.dropped_tables {
            self.manager.catalog.remove_table(tbl);
        }

        // 7. If default table was also mutated, commit default writes
        if !self.pending_writes.is_empty() {
            let default_guard = self.manager.commit_lock.lock();
            let mut pending = std::mem::take(&mut self.pending_writes);
            if self.is_sorted {
                pending.dedup_by(|a, b| a.0 == b.0);
            } else {
                pending.reverse();
                pending.sort_by(|a, b| a.0.cmp(&b.0));
                pending.dedup_by(|a, b| a.0 == b.0);
            }
            let cur_root = PageId(self.manager.active_root.load(Ordering::Acquire));
            let mut target_btree =
                BTreeIndex::new(Arc::clone(&self.manager.cache), Some(cur_root))?;
            target_btree.insert_sorted_batch_raw_in_tx(&pending, &mut self.tx_pages)?;
            let new_default_root = target_btree.root_id();
            self.manager
                .active_root
                .store(new_default_root.0, Ordering::Release);
            drop(default_guard);
        }

        // 8. Assign commit tx_id and record history
        let commit_tx = self.manager.next_tx_id.fetch_add(1, Ordering::SeqCst);
        let new_next = commit_tx + 1;

        if is_single_table {
            let state = &self.table_states[0];
            if let Some(entry) = self.manager.catalog.get_table_entry(&state.name) {
                entry.last_commit_tx.store(commit_tx, Ordering::Release);
                let mut keys = Vec::with_capacity(state.pending_writes.len());
                for item in &state.pending_writes {
                    keys.push(item.0.clone());
                }
                let mut history = entry.history.write();
                history.push_back(CommittedTxRecord {
                    tx_id: TxId(commit_tx),
                    tables: self.modified_tables.clone(),
                    keys: keys.into(),
                });
                if history.len() > 1024 {
                    history.pop_front();
                }
            }
        } else {
            let mut all_keys = Vec::new();
            for state in &self.table_states {
                let mut table_keys = Vec::with_capacity(state.pending_writes.len());
                for item in &state.pending_writes {
                    all_keys.push(item.0.clone());
                    table_keys.push(item.0.clone());
                }
                if let Some(entry) = self.manager.catalog.get_table_entry(&state.name) {
                    entry.last_commit_tx.store(commit_tx, Ordering::Release);
                    let mut history = entry.history.write();
                    history.push_back(CommittedTxRecord {
                        tx_id: TxId(commit_tx),
                        tables: smallvec![state.name.clone()],
                        keys: table_keys.into(),
                    });
                    if history.len() > 1024 {
                        history.pop_front();
                    }
                }
            }
            let mut history = self.manager.commit_history.write();
            history.push_back(CommittedTxRecord {
                tx_id: TxId(commit_tx),
                tables: self.modified_tables.clone(),
                keys: all_keys.into(),
            });
            if history.len() > 2048 {
                history.pop_front();
            }
        }

        // 9. Durability: write MetaPage to alternating superblock and flush
        if self.manager.durability() != DurabilityMode::None {
            let tables = self.manager.catalog.to_meta_entries();
            let active_root = PageId(self.manager.active_root.load(Ordering::Acquire));
            let catalog_root = PageId(self.manager.catalog_root.load(Ordering::Acquire));
            let meta = MetaPage::with_tables(active_root, catalog_root, TxId(new_next), tables);
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

        self.committed = true;

        if self.has_hooks {
            let hooks = self.manager.hooks.read();
            for (_, hook) in hooks.iter() {
                hook.after_commit(&self.changes);
            }
        }

        Ok(())
    }
}
