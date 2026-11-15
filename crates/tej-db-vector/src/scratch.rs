use std::cell::RefCell;

/// High-performance reusable scratchpad for zero-allocation beam search.
///
/// Replaces `HashSet<u64>` with a fast bitset where only touched 64-bit words
/// are cleared between queries in $O(\text{visited})$ time instead of zeroing the entire bitset.
#[derive(Debug, Clone)]
pub struct SearchScratch {
    words: Vec<u64>,
    touched: Vec<usize>,
}

impl Default for SearchScratch {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl SearchScratch {
    /// Creates a new scratchpad with initial bit capacity.
    #[must_use]
    pub fn new(initial_bits: usize) -> Self {
        let num_words = initial_bits.div_ceil(64).max(16);
        Self {
            words: vec![0u64; num_words],
            touched: Vec::with_capacity(128),
        }
    }

    /// Ensures the bitset can accommodate an element with index `bit_idx`.
    pub fn ensure_capacity(&mut self, bit_idx: usize) {
        let word_idx = bit_idx / 64;
        if word_idx >= self.words.len() {
            let new_len = (word_idx + 1).next_power_of_two().max(self.words.len() * 2);
            self.words.resize(new_len, 0);
        }
    }

    /// Checks if `bit_idx` is marked as visited without modifying state.
    #[inline]
    #[must_use]
    pub fn is_visited(&self, bit_idx: usize) -> bool {
        let word_idx = bit_idx / 64;
        if word_idx >= self.words.len() {
            return false;
        }
        let bit = 1u64 << (bit_idx % 64);
        (self.words[word_idx] & bit) != 0
    }

    /// Marks `bit_idx` as visited.
    ///
    /// Returns `true` if `bit_idx` was not previously visited (newly marked),
    /// or `false` if it was already marked.
    #[inline]
    pub fn mark_visited(&mut self, bit_idx: usize) -> bool {
        self.ensure_capacity(bit_idx);
        let word_idx = bit_idx / 64;
        let bit = 1u64 << (bit_idx % 64);
        let was_set = (self.words[word_idx] & bit) != 0;
        if was_set {
            false
        } else {
            if self.words[word_idx] == 0 {
                self.touched.push(word_idx);
            }
            self.words[word_idx] |= bit;
            true
        }
    }

    /// Resets all visited bits in $O(\text{touched})$ time by zeroing only the modified words.
    #[inline]
    pub fn clear(&mut self) {
        for word_idx in self.touched.drain(..) {
            self.words[word_idx] = 0;
        }
    }
}

thread_local! {
    static LOCAL_SCRATCH: RefCell<SearchScratch> = RefCell::new(SearchScratch::new(65536));
}

/// Executes a closure with a thread-local zero-allocation `SearchScratch`.
/// Automatically clears touched words upon completion.
pub fn with_thread_scratch<R>(f: impl FnOnce(&mut SearchScratch) -> R) -> R {
    LOCAL_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        let res = f(&mut scratch);
        scratch.clear();
        res
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scratch_bitset_lifecycle() {
        let mut scratch = SearchScratch::new(64);
        assert!(!scratch.is_visited(42));
        assert!(scratch.mark_visited(42));
        assert!(scratch.is_visited(42));
        assert!(!scratch.mark_visited(42)); // already visited

        assert!(!scratch.is_visited(1000));
        assert!(scratch.mark_visited(1000));
        assert!(scratch.is_visited(1000));

        scratch.clear();
        assert!(!scratch.is_visited(42));
        assert!(!scratch.is_visited(1000));
    }
}
