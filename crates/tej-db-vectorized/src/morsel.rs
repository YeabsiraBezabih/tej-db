use std::sync::atomic::{AtomicUsize, Ordering};

/// A morsel representing an execution chunk allocated to a worker thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Morsel {
    pub row_group_idx: usize,
    pub start_row: usize,
    pub row_count: usize,
}

/// Morsel work-stealing scheduler dispensing chunks to worker threads lock-free.
#[derive(Debug)]
pub struct MorselScheduler {
    total_row_groups: usize,
    rows_per_group: usize,
    morsel_size: usize,
    current_morsel_idx: AtomicUsize,
    total_morsels: usize,
    morsels_per_group: usize,
}

impl MorselScheduler {
    #[must_use]
    pub fn new(total_row_groups: usize, rows_per_group: usize, morsel_size: usize) -> Self {
        let morsel_size = morsel_size.max(1);
        let morsels_per_group = if rows_per_group == 0 {
            0
        } else {
            rows_per_group.div_ceil(morsel_size)
        };
        let total_morsels = total_row_groups * morsels_per_group;

        Self {
            total_row_groups,
            rows_per_group,
            morsel_size,
            current_morsel_idx: AtomicUsize::new(0),
            total_morsels,
            morsels_per_group,
        }
    }

    /// Atomically claims the next available morsel for the calling worker thread.
    #[must_use]
    pub fn next_morsel(&self) -> Option<Morsel> {
        if self.morsels_per_group == 0 || self.total_morsels == 0 {
            return None;
        }

        loop {
            let idx = self.current_morsel_idx.fetch_add(1, Ordering::SeqCst);
            if idx >= self.total_morsels {
                return None;
            }

            let rg_idx = idx / self.morsels_per_group;
            let morsel_in_group = idx % self.morsels_per_group;
            let start_row = morsel_in_group * self.morsel_size;
            let remaining_in_group = self.rows_per_group.saturating_sub(start_row);
            let row_count = self.morsel_size.min(remaining_in_group);

            if rg_idx < self.total_row_groups {
                if row_count > 0 {
                    return Some(Morsel {
                        row_group_idx: rg_idx,
                        start_row,
                        row_count,
                    });
                }
                continue;
            }
            return None;
        }
    }

    /// Resets the scheduler for a new query pass.
    pub fn reset(&self) {
        self.current_morsel_idx.store(0, Ordering::SeqCst);
    }
}
