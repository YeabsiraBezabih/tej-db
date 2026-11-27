use crate::BTreeIndex;
use core::ops::Bound;
use tej_db_core::{DbError, KeyRange, PageId, PageType, Result, Storage};

impl<S: Storage> BTreeIndex<S> {
    #[allow(dead_code)]
    pub(crate) fn find_leaf_path(&self, key: &[u8]) -> Result<Vec<PageId>> {
        let mut path = Vec::with_capacity(8);
        let mut curr_id = self.root_id();

        loop {
            path.push(curr_id);
            let next_step = self.cache.with_page(curr_id, |page| {
                let header = page.read_header()?;
                match header.page_type {
                    PageType::BTreeLeaf => Ok(None),
                    PageType::BTreeInternal => Ok(Some(page.internal_search(key)?)),
                    _ => Err(DbError::Corruption("Invalid B-Tree page type".into())),
                }
            })?;
            match next_step {
                Some(next_id) => curr_id = next_id,
                None => return Ok(path),
            }
        }
    }

    /// Zero-copy traversal down to the target leaf node with tightest upper key bound.
    pub(crate) fn find_leaf_path_with_bound(
        &self,
        key: &[u8],
    ) -> Result<(Vec<PageId>, Option<Vec<u8>>)> {
        let mut path = Vec::with_capacity(8);
        let mut curr_id = self.root_id();
        let mut tightest_bound = None;

        loop {
            path.push(curr_id);
            let (next_id, bound_opt) = self.cache.with_page(curr_id, |page| {
                let header = page.read_header()?;
                match header.page_type {
                    PageType::BTreeLeaf => Ok((None, None)),
                    PageType::BTreeInternal => {
                        let (child, bound) = page.internal_search_with_bound(key)?;
                        Ok((Some(child), bound.map(<[u8]>::to_vec)))
                    }
                    _ => Err(DbError::Corruption("Invalid B-Tree page type".into())),
                }
            })?;

            if let Some(bound) = bound_opt {
                tightest_bound = Some(bound);
            }

            match next_id {
                Some(next) => curr_id = next,
                None => return Ok((path, tightest_bound)),
            }
        }
    }

    pub(crate) fn find_leaf_path_indexed(&self, key: &[u8]) -> Result<Vec<(PageId, usize)>> {
        let mut path = Vec::with_capacity(8);
        let mut curr_id = self.root_id();

        loop {
            let next_step = self.cache.with_page(curr_id, |page| {
                let header = page.read_header()?;
                match header.page_type {
                    PageType::BTreeLeaf => Ok(None),
                    PageType::BTreeInternal => {
                        let (child_idx, child_id) = page.internal_search_index(key)?;
                        Ok(Some((child_idx, child_id)))
                    }
                    _ => Err(DbError::Corruption("Invalid B-Tree page type".into())),
                }
            })?;
            if let Some((child_idx, next_id)) = next_step {
                path.push((curr_id, child_idx));
                curr_id = next_id;
            } else {
                path.push((curr_id, 0));
                return Ok(path);
            }
        }
    }

    pub(crate) fn advance_to_next_leaf(
        &self,
        path: &mut Vec<(PageId, usize)>,
        tree_depth: usize,
    ) -> Result<Option<PageId>> {
        path.pop();

        while let Some((parent_id, child_idx)) = path.last_mut() {
            *child_idx += 1;
            let next_child = self.cache.with_page(*parent_id, |parent| {
                if *child_idx < parent.internal_child_count() {
                    Ok(Some(parent.internal_child_at(*child_idx)?))
                } else {
                    Ok(None)
                }
            })?;

            if let Some(mut curr_id) = next_child {
                while path.len() + 1 < tree_depth {
                    let next_child_id = self
                        .cache
                        .with_page(curr_id, |page| page.internal_child_at(0))?;
                    path.push((curr_id, 0));
                    curr_id = next_child_id;
                }
                path.push((curr_id, 0));
                return Ok(Some(curr_id));
            }
            path.pop();
        }

        Ok(None)
    }

    /// High-throughput streaming range scan visiting references directly in cached leaves with zero page cloning.
    #[inline]
    pub fn scan_range_visitor(
        &self,
        range: &KeyRange,
        mut visitor: impl FnMut(&[u8], &[u8]) -> Result<bool>,
    ) -> Result<()> {
        let (start_key, start_excluded) = match &range.start {
            Bound::Included(k) => (k.as_slice(), false),
            Bound::Excluded(k) => (k.as_slice(), true),
            Bound::Unbounded => (&[][..], false),
        };
        let (end_key, is_end_inclusive) = match &range.end {
            Bound::Included(k) => (Some(k.as_slice()), true),
            Bound::Excluded(k) => (Some(k.as_slice()), false),
            Bound::Unbounded => (None, false),
        };

        let mut path = self.find_leaf_path_indexed(start_key)?;
        let tree_depth = path.len();
        let mut curr_id = path
            .last()
            .map(|(pid, _)| *pid)
            .ok_or_else(|| DbError::Corruption("Empty path".into()))?;

        let mut is_first_leaf = true;

        'scan: loop {
            let stop = self.cache.with_page(curr_id, |page| {
                let count = page.item_count();
                if count == 0 {
                    return Ok(false);
                }

                let mut start_idx = 0;
                if is_first_leaf && !start_key.is_empty() {
                    if let Ok(Ok(idx) | Err(idx)) = page.leaf_search(start_key) {
                        start_idx = idx;
                    }
                }

                let entire_leaf_in_range = if let Some(end) = end_key {
                    if let Ok(last_k) = page.leaf_get_key(count - 1) {
                        if is_end_inclusive {
                            last_k <= end
                        } else {
                            last_k < end
                        }
                    } else {
                        false
                    }
                } else {
                    true
                };

                let mut stopped = false;
                if entire_leaf_in_range && (!is_first_leaf || !start_excluded) {
                    let scan_res = page.leaf_scan_range(start_idx, |idx, k, v, v_raw| {
                        if (v_raw & tej_db_core::page::OVERFLOW_FLAG) != 0 {
                            let (total_len, first_page) = page.leaf_get_overflow_ref(idx)?;
                            let val = self.read_overflow_chain(first_page, total_len)?;
                            if !visitor(k, &val)? {
                                stopped = true;
                                return Ok(false);
                            }
                        } else if !visitor(k, v)? {
                            stopped = true;
                            return Ok(false);
                        }

                        Ok(true)
                    })?;

                    if !scan_res || stopped {
                        return Ok(true);
                    }
                } else {
                    let scan_res = page.leaf_scan_range(start_idx, |idx, k, v, v_raw| {
                        if is_first_leaf && start_excluded && k == start_key {
                            return Ok(true);
                        }

                        if let Some(end) = end_key {
                            if is_end_inclusive {
                                if k > end {
                                    stopped = true;
                                    return Ok(false);
                                }
                            } else if k >= end {
                                stopped = true;
                                return Ok(false);
                            }
                        }

                        if (v_raw & tej_db_core::page::OVERFLOW_FLAG) != 0 {
                            let (total_len, first_page) = page.leaf_get_overflow_ref(idx)?;
                            let val = self.read_overflow_chain(first_page, total_len)?;
                            if !visitor(k, &val)? {
                                stopped = true;
                                return Ok(false);
                            }
                        } else if !visitor(k, v)? {
                            stopped = true;
                            return Ok(false);
                        }

                        Ok(true)
                    })?;

                    if !scan_res || stopped {
                        return Ok(true);
                    }
                }

                Ok(false)
            })?;

            is_first_leaf = false;
            if stop {
                break 'scan;
            }

            match self.advance_to_next_leaf(&mut path, tree_depth)? {
                Some(next_leaf) => curr_id = next_leaf,
                None => break 'scan,
            }
        }

        Ok(())
    }

    /// Zero-copy point lookup visiting the value slice directly with zero allocations for inline values.
    pub fn with_value<R>(
        &self,
        key: &[u8],
        f: impl FnOnce(&[u8]) -> Result<R>,
    ) -> Result<Option<R>> {
        enum Step<R> {
            Done(Option<R>),
            Overflow(PageId, u64),
            Next(PageId),
        }

        let mut f_opt = Some(f);
        let mut curr_id = self.root_id();
        loop {
            let step = self.cache.with_page(curr_id, |page| {
                let header = page.read_header()?;
                match header.page_type {
                    PageType::BTreeLeaf => {
                        if let Ok(idx) = page.leaf_search(key)? {
                            if page.leaf_is_overflow(idx)? {
                                let (total_len, first_page) = page.leaf_get_overflow_ref(idx)?;
                                Ok(Step::Overflow(first_page, total_len))
                            } else {
                                let val = page.leaf_get_val(idx)?;
                                let f = f_opt.take().ok_or_else(|| {
                                    DbError::Custom("Closure already consumed".into())
                                })?;
                                let res = f(val)?;
                                Ok(Step::Done(Some(res)))
                            }
                        } else {
                            Ok(Step::Done(None))
                        }
                    }
                    PageType::BTreeInternal => {
                        let child = page.internal_search(key)?;
                        Ok(Step::Next(child))
                    }
                    _ => Err(DbError::Corruption("Invalid B-Tree page type".into())),
                }
            })?;

            match step {
                Step::Done(res) => return Ok(res),
                Step::Overflow(first_page, total_len) => {
                    #[allow(clippy::cast_possible_truncation)]
                    let total = total_len as usize;
                    let f = f_opt
                        .take()
                        .ok_or_else(|| DbError::Custom("Closure already consumed".into()))?;
                    return self.cache.with_overflow_payload(first_page, total, f);
                }
                Step::Next(child_id) => curr_id = child_id,
            }
        }
    }
}
