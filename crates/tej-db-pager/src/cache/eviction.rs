use crate::cache::{CacheShardState, PageCache};
use std::sync::atomic::Ordering;
use tej_db_core::{Result, Storage};

impl<S: Storage> PageCache<S> {
    /// O(1) Clock Eviction (Second-Chance Algorithm) per shard.
    pub(crate) fn evict_if_needed(
        &self,
        shard_idx: usize,
        state: &mut CacheShardState,
    ) -> Result<()> {
        if state.entries.len() < self.shards[shard_idx].capacity || state.clock_ring.is_empty() {
            return Ok(());
        }

        let ring_len = state.clock_ring.len();
        let mut hand = state.clock_hand % ring_len;
        let mut victim_id = None;
        let mut victim_idx = 0;

        for _ in 0..(ring_len * 2) {
            let candidate_id = state.clock_ring[hand];
            if let Some(entry) = state.entries.get(&candidate_id) {
                if entry.referenced.load(Ordering::Relaxed) {
                    entry.referenced.store(false, Ordering::Relaxed);
                } else {
                    victim_id = Some(candidate_id);
                    victim_idx = hand;
                    hand = (hand + 1) % ring_len;
                    break;
                }
            }
            hand = (hand + 1) % ring_len;
        }

        state.clock_hand = hand;

        if let Some(id) = victim_id {
            state.clock_ring.swap_remove(victim_idx);
            if let Some(mut entry) = state.entries.remove(&id) {
                if entry.is_dirty {
                    entry.page.update_checksum();
                    let mut storage = self.storage.write();
                    storage.write_page(id, entry.page.as_slice())?;
                }
            }
        }

        Ok(())
    }
}
