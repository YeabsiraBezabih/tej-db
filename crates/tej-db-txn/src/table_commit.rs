use crate::conflict::{CommittedTxRecord, validate_tx_conflict};
use crate::meta::{MetaPage, TableMetaEntry};
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
            if !state.is_sorted {
                state.pending_writes.reverse();
                state.pending_writes.sort_by(|a, b| a.0.cmp(&b.0));
                state.pending_writes.dedup_by(|a, b| a.0 == b.0);
            }
        }

        // 2. Identify all tables that need locking, strictly sorted to prevent deadlocks
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

        // 3. Acquire canonical table-level commit locks in sorted order
        let table_locks: Vec<_> = lock_tables
            .iter()
            .map(|tbl| self.manager.table_commit_lock(tbl))
            .collect();
        let _table_guards: Vec<_> = table_locks.iter().map(|l| l.lock()).collect();

        // 4. OCC conflict validation against commit history (including default table writes)
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
            if !self.pending_writes.is_empty() {
                validate_tx_conflict(
                    &history,
                    self.snapshot_tx_id,
                    &self.modified_tables,
                    &self.pending_writes,
                )?;
            }
        }

        // 5. Commit mutations to each table's B-Tree WITHOUT publishing roots yet
        let mut new_roots: Vec<(&str, PageId)> = Vec::with_capacity(self.table_states.len());
        for state in &mut self.table_states {
            if state.pending_writes.is_empty() {
                if self.created_tables.iter().any(|t| t == &state.name) {
                    new_roots.push((&state.name, state.snapshot_root));
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
                new_roots.push((&state.name, target_btree.root_id()));
            }
        }

        // 6. Superblock write, default table mutation, and history recording serialized under global commit_lock
        let commit_guard = self.manager.commit_lock.lock();

        let mut new_default_root = None;
        if !self.pending_writes.is_empty() {
            let mut pending = std::mem::take(&mut self.pending_writes);
            if !self.is_sorted {
                pending.reverse();
                pending.sort_by(|a, b| a.0.cmp(&b.0));
                pending.dedup_by(|a, b| a.0 == b.0);
            }
            let cur_root = PageId(self.manager.active_root.load(Ordering::Acquire));
            let mut target_btree =
                BTreeIndex::new(Arc::clone(&self.manager.cache), Some(cur_root))?;
            target_btree.insert_sorted_batch_raw_in_tx(&pending, &mut self.tx_pages)?;
            new_default_root = Some(target_btree.root_id());
        }

        // 7. Durability Barrier Phase 1: Flush data pages and sync BEFORE publishing roots!
        // If an I/O error occurs here, in-memory catalogs/roots are completely unmutated.
        match self.manager.durability() {
            DurabilityMode::Immediate => {
                self.manager.cache.flush_data_pages()?;
                self.manager.cache.sync()?;
            }
            DurabilityMode::Normal | DurabilityMode::Eventual { .. } => {
                self.manager.cache.flush_data_pages()?;
            }
            DurabilityMode::None => {}
        }

        let commit_tx = self.manager.next_tx_id.load(Ordering::Acquire);
        let new_next = commit_tx + 1;

        // 8. Durability Barrier Phase 2: Write MetaPage to alternating superblock, flush, and sync
        let mut target_sb_opt = None;
        let phase2_res = (|| -> Result<()> {
            if self.manager.durability() != DurabilityMode::None {
                let mut meta_tables = self.manager.catalog.to_meta_entries();
                for (tbl_name, root_id) in &new_roots {
                    if let Some(entry) = meta_tables.iter_mut().find(|e| e.name == *tbl_name) {
                        entry.root_id = *root_id;
                    } else {
                        meta_tables.push(TableMetaEntry {
                            name: (*tbl_name).to_string(),
                            root_id: *root_id,
                        });
                    }
                }
                for tbl in &self.dropped_tables {
                    meta_tables.retain(|e| e.name != *tbl);
                }

                let active_root = new_default_root
                    .unwrap_or_else(|| PageId(self.manager.active_root.load(Ordering::Acquire)));
                let catalog_root = PageId(self.manager.catalog_root.load(Ordering::Acquire));
                let meta =
                    MetaPage::with_tables(active_root, catalog_root, TxId(new_next), meta_tables);
                let cur_sb = PageId(self.manager.active_superblock.load(Ordering::Acquire));
                let target_sb = crate::superblock::choose_commit_superblock(cur_sb, TxId(new_next));
                target_sb_opt = Some(target_sb);
                crate::superblock::write_superblock(&self.manager.cache, target_sb, &meta)?;
                self.manager.cache.flush_page(target_sb)?;
                if self.manager.durability() == DurabilityMode::Immediate {
                    self.manager.cache.sync()?;
                }
                self.manager
                    .active_superblock
                    .store(target_sb.0, Ordering::Release);
            }
            Ok(())
        })();

        if let Err(e) = phase2_res {
            if let Some(target_sb) = target_sb_opt {
                self.manager.cache.discard_page(target_sb);
            }
            return Err(e);
        }

        // Commit state publication strictly after durability succeeds
        self.manager.next_tx_id.store(new_next, Ordering::Release);

        // Publish table roots and drops safely
        for (tbl_name, root_id) in new_roots {
            self.manager.catalog.update_table_root(tbl_name, root_id);
        }
        for tbl in &self.dropped_tables {
            self.manager.catalog.remove_table(tbl);
        }
        if let Some(def_root) = new_default_root {
            self.manager
                .active_root
                .store(def_root.0, Ordering::Release);
        }

        // Record history across table entries and global commit history
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
        all_keys.sort();
        all_keys.dedup();

        let mut history = self.manager.commit_history.write();
        history.push_back(CommittedTxRecord {
            tx_id: TxId(commit_tx),
            tables: self.modified_tables.clone(),
            keys: all_keys.into(),
        });
        if history.len() > 2048 {
            history.pop_front();
        }

        // Safe reclamation: free retired pages after superblock sync
        for pid in self.tx_pages.retired.drain(..) {
            self.manager.cache.free_page(pid);
        }
        for state in &mut self.table_states {
            for pid in state.tx_pages.retired.drain(..) {
                self.manager.cache.free_page(pid);
            }
        }

        drop(commit_guard);

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
