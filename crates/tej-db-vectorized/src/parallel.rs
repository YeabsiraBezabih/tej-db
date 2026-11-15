use crate::aggregate::AggregateFunction;
use crate::filter::FilterExpr;
use crate::morsel::MorselScheduler;
use crate::scan::ZoneMapPredicate;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::Arc;
use tej_db_columnar::{ColumnVector, DataChunk, PhysicalType, RowGroup};
use tej_db_core::{DbError, Result};

/// Evaluates vectorized scan and filter in parallel across worker threads using `MorselScheduler`.
///
/// Applies ZoneMap pruning to skip entire RowGroups before decompression and evaluates
/// filters in-place using thread-local selection vectors.
pub fn parallel_scan_and_filter(
    row_groups: &[RowGroup],
    predicates: &[ZoneMapPredicate],
    filter_exprs: &[FilterExpr],
    num_threads: usize,
) -> Result<usize> {
    let total_rgs = row_groups.len();
    if total_rgs == 0 {
        return Ok(0);
    }
    let rows_per_rg = row_groups[0].metadata.num_rows as usize;
    let scheduler = Arc::new(MorselScheduler::new(total_rgs, rows_per_rg, rows_per_rg));
    let num_threads = num_threads.max(1);

    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(num_threads);
        for _ in 0..num_threads {
            let sched = Arc::clone(&scheduler);
            handles.push(s.spawn(move || -> Result<usize> {
                let mut local_count = 0;
                let mut sel_buf1 = Vec::with_capacity(2048);
                let mut sel_buf2 = Vec::with_capacity(2048);

                while let Some(morsel) = sched.next_morsel() {
                    let rg = &row_groups[morsel.row_group_idx];

                    // 1. ZoneMap block skipping: if any predicate cannot match, skip entire RowGroup!
                    let can_match_all = predicates.iter().all(|pred| pred.can_match(rg));
                    if !can_match_all {
                        continue;
                    }

                    // 2. ZoneMap full acceptance: if all predicates match all rows, accept in O(1) without decompression!
                    if !predicates.is_empty() && predicates.iter().all(|pred| pred.all_match(rg)) {
                        local_count += rg.metadata.num_rows as usize;
                        continue;
                    }

                    // 3. Decompress row-group into chunk
                    let chunk = rg.decompress_to_chunk()?;
                    if filter_exprs.is_empty() {
                        local_count += chunk.num_rows();
                        continue;
                    }

                    // 4. Filter vector evaluation
                    filter_exprs[0].filter_vector(&chunk, None, &mut sel_buf1);
                    let mut current_sel = &sel_buf1;

                    for (f_idx, filter) in filter_exprs.iter().enumerate().skip(1) {
                        if current_sel.is_empty() {
                            break;
                        }
                        let (curr, next) = if f_idx % 2 == 1 {
                            (&sel_buf1, &mut sel_buf2)
                        } else {
                            (&sel_buf2, &mut sel_buf1)
                        };
                        filter.filter_vector(&chunk, Some(curr), next);
                        current_sel = next;
                    }

                    local_count += current_sel.len();
                }
                Ok(local_count)
            }));
        }

        let mut total_matches = 0;
        for h in handles {
            total_matches += h
                .join()
                .map_err(|_| DbError::Custom("Worker thread panicked".into()))??;
        }
        Ok(total_matches)
    })
}

#[derive(Debug, Clone)]
enum ThreadAggState {
    Count(u64),
    SumInt64(i64),
    MinInt64(i64),
    MaxInt64(i64),
    AvgFloat64 { sum: f64, count: u64 },
}

impl ThreadAggState {
    fn new(func: AggregateFunction) -> Self {
        match func {
            AggregateFunction::Count => Self::Count(0),
            AggregateFunction::SumInt64 => Self::SumInt64(0),
            AggregateFunction::MinInt64 => Self::MinInt64(i64::MAX),
            AggregateFunction::MaxInt64 => Self::MaxInt64(i64::MIN),
            AggregateFunction::AvgFloat64 => Self::AvgFloat64 { sum: 0.0, count: 0 },
        }
    }

    #[inline]
    fn update_i64(&mut self, val: i64) {
        match self {
            Self::Count(c) => *c += 1,
            Self::SumInt64(s) => *s += val,
            Self::MinInt64(m) => *m = (*m).min(val),
            Self::MaxInt64(m) => *m = (*m).max(val),
            Self::AvgFloat64 { sum, count } => {
                #[allow(clippy::cast_precision_loss)]
                {
                    *sum += val as f64;
                }
                *count += 1;
            }
        }
    }

    fn merge(&mut self, other: &Self) {
        match (self, other) {
            (Self::Count(c1), Self::Count(c2)) => *c1 += c2,
            (Self::SumInt64(s1), Self::SumInt64(s2)) => *s1 += s2,
            (Self::MinInt64(m1), Self::MinInt64(m2)) => *m1 = (*m1).min(*m2),
            (Self::MaxInt64(m1), Self::MaxInt64(m2)) => *m1 = (*m1).max(*m2),
            (Self::AvgFloat64 { sum: s1, count: c1 }, Self::AvgFloat64 { sum: s2, count: c2 }) => {
                *s1 += s2;
                *c1 += c2;
            }
            _ => {}
        }
    }
}

const DENSE_CAP: usize = 64;

/// Evaluates vectorized hash aggregation across worker threads using `MorselScheduler`.
///
/// Each worker thread aggregates into thread-local hash or direct tables before
/// merging into a final result chunk.
pub fn parallel_hash_aggregate(
    row_groups: &[RowGroup],
    group_col_idx: usize,
    aggregates: &[(usize, AggregateFunction)],
    num_threads: usize,
) -> Result<DataChunk> {
    let total_rgs = row_groups.len();
    if total_rgs == 0 {
        return Ok(DataChunk::new(&[], 0));
    }
    let rows_per_rg = row_groups[0].metadata.num_rows as usize;
    let scheduler = Arc::new(MorselScheduler::new(total_rgs, rows_per_rg, rows_per_rg));
    let num_threads = num_threads.max(1);

    let is_count_sum = aggregates.len() == 2
        && aggregates[0].1 == AggregateFunction::Count
        && aggregates[1].1 == AggregateFunction::SumInt64;
    let sum_col_idx = aggregates.get(1).map_or(0, |a| a.0);

    let per_thread_results = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(num_threads);
        for _ in 0..num_threads {
            let sched = Arc::clone(&scheduler);
            handles.push(s.spawn(
                move || -> Result<HashMap<i64, SmallVec<[ThreadAggState; 4]>>> {
                    let mut local_groups: HashMap<i64, SmallVec<[ThreadAggState; 4]>> =
                        HashMap::new();
                    let mut fast_slots = [(0u64, 0i64); DENSE_CAP];
                    let mut grp_buf = Vec::with_capacity(2048);
                    let mut val_buf = Vec::with_capacity(2048);

                    while let Some(morsel) = sched.next_morsel() {
                        let rg = &row_groups[morsel.row_group_idx];

                        if is_count_sum {
                            rg.decompress_column_into(group_col_idx, &mut grp_buf)?;
                            rg.decompress_column_into(sum_col_idx, &mut val_buf)?;
                            let keys = grp_buf.as_slice();
                            let vals = val_buf.as_slice();

                            let len = keys.len().min(vals.len());
                            let mut i = 0;
                            while i + 4 <= len {
                                #[allow(clippy::cast_sign_loss)]
                                let k0 = keys[i] as usize;
                                if k0 < DENSE_CAP {
                                    fast_slots[k0].0 += 1;
                                    fast_slots[k0].1 += vals[i];
                                }
                                #[allow(clippy::cast_sign_loss)]
                                let k1 = keys[i + 1] as usize;
                                if k1 < DENSE_CAP {
                                    fast_slots[k1].0 += 1;
                                    fast_slots[k1].1 += vals[i + 1];
                                }
                                #[allow(clippy::cast_sign_loss)]
                                let k2 = keys[i + 2] as usize;
                                if k2 < DENSE_CAP {
                                    fast_slots[k2].0 += 1;
                                    fast_slots[k2].1 += vals[i + 2];
                                }
                                #[allow(clippy::cast_sign_loss)]
                                let k3 = keys[i + 3] as usize;
                                if k3 < DENSE_CAP {
                                    fast_slots[k3].0 += 1;
                                    fast_slots[k3].1 += vals[i + 3];
                                }
                                i += 4;
                            }
                            while i < len {
                                #[allow(clippy::cast_sign_loss)]
                                let k = keys[i] as usize;
                                if k < DENSE_CAP {
                                    fast_slots[k].0 += 1;
                                    fast_slots[k].1 += vals[i];
                                }
                                i += 1;
                            }
                        } else {
                            let chunk = rg.decompress_to_chunk()?;
                            let group_col = chunk
                                .column(group_col_idx)
                                .ok_or_else(|| DbError::Custom("Group column missing".into()))?;
                            let group_keys = group_col.as_i64_slice();

                            let agg_slices: Vec<(usize, AggregateFunction, &[i64])> = aggregates
                                .iter()
                                .map(|&(c_idx, func)| {
                                    let slice = chunk
                                        .column(c_idx)
                                        .map_or(&[] as &[i64], ColumnVector::as_i64_slice);
                                    (c_idx, func, slice)
                                })
                                .collect();

                            for (row_idx, &key) in group_keys.iter().enumerate() {
                                let states = local_groups.entry(key).or_insert_with(|| {
                                    let mut v = SmallVec::new();
                                    for &(_, func) in aggregates {
                                        v.push(ThreadAggState::new(func));
                                    }
                                    v
                                });

                                for (agg_idx, &(_, _, slice)) in agg_slices.iter().enumerate() {
                                    let val = slice.get(row_idx).copied().unwrap_or(0);
                                    states[agg_idx].update_i64(val);
                                }
                            }
                        }
                    }

                    if is_count_sum {
                        for (k_idx, slot) in fast_slots.iter().enumerate() {
                            if slot.0 > 0 {
                                #[allow(clippy::cast_possible_wrap)]
                                local_groups.insert(
                                    k_idx as i64,
                                    smallvec::smallvec![
                                        ThreadAggState::Count(slot.0),
                                        ThreadAggState::SumInt64(slot.1),
                                    ],
                                );
                            }
                        }
                    }

                    Ok(local_groups)
                },
            ));
        }

        let mut all = Vec::with_capacity(num_threads);
        for h in handles {
            all.push(
                h.join()
                    .map_err(|_| DbError::Custom("Worker panicked".into()))??,
            );
        }
        Ok::<_, DbError>(all)
    })?;

    // Merge thread-local aggregate hash tables
    let mut final_groups: HashMap<i64, SmallVec<[ThreadAggState; 4]>> = HashMap::new();
    for thread_map in per_thread_results {
        for (key, states) in thread_map {
            let entry = final_groups.entry(key).or_insert_with(|| {
                let mut v = SmallVec::new();
                for &(_, func) in aggregates {
                    v.push(ThreadAggState::new(func));
                }
                v
            });
            for (agg_idx, st) in states.iter().enumerate() {
                entry[agg_idx].merge(st);
            }
        }
    }

    // Build output chunk: [group_col, agg_0, agg_1, ...]
    let mut out_types = Vec::with_capacity(1 + aggregates.len());
    out_types.push(PhysicalType::Int64);
    for _ in aggregates {
        out_types.push(PhysicalType::Int64);
    }

    let mut out_chunk = DataChunk::new(&out_types, final_groups.len());
    for (key, states) in &final_groups {
        out_chunk
            .column_mut(0)
            .ok_or_else(|| DbError::Custom("Col 0 missing".into()))?
            .push_i64(*key)?;
        for (agg_idx, st) in states.iter().enumerate() {
            let col = out_chunk
                .column_mut(1 + agg_idx)
                .ok_or_else(|| DbError::Custom("Agg col missing".into()))?;
            match st {
                #[allow(clippy::cast_possible_wrap)]
                ThreadAggState::Count(c) => col.push_i64(*c as i64)?,
                ThreadAggState::SumInt64(s) => col.push_i64(*s)?,
                ThreadAggState::MinInt64(m) | ThreadAggState::MaxInt64(m) => col.push_i64(*m)?,
                ThreadAggState::AvgFloat64 { .. } => col.push_i64(0)?,
            }
        }
    }
    out_chunk.set_num_rows(final_groups.len())?;
    Ok(out_chunk)
}

/// Evaluates TPC-H Q6 in parallel with fused multi-predicate filtering and scalar aggregation:
/// `SELECT sum(extendedprice) FROM lineitem WHERE shipdate >= 19940101 AND shipdate < 19950101 AND discount >= 5 AND discount <= 7 AND quantity < 24;`
#[allow(clippy::needless_bitwise_bool)]
pub fn parallel_tpch_q6(
    row_groups: &[RowGroup],
    predicates: &[ZoneMapPredicate],
    num_threads: usize,
) -> Result<(i64, usize)> {
    let total_rgs = row_groups.len();
    if total_rgs == 0 {
        return Ok((0, 0));
    }
    let rows_per_rg = row_groups[0].metadata.num_rows as usize;
    let scheduler = Arc::new(MorselScheduler::new(total_rgs, rows_per_rg, rows_per_rg));
    let num_threads = num_threads.max(1);

    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(num_threads);
        for _ in 0..num_threads {
            let sched = Arc::clone(&scheduler);
            handles.push(s.spawn(move || -> Result<(i64, usize)> {
                let mut local_sum = 0i64;
                let mut local_count = 0usize;
                let mut prices_buf = Vec::with_capacity(2048);
                let mut disc_buf = Vec::with_capacity(2048);
                let mut ship_buf = Vec::with_capacity(2048);
                let mut qty_buf = Vec::with_capacity(2048);
                let mut sel_buf: Vec<u16> = vec![0u16; 2048];

                while let Some(morsel) = sched.next_morsel() {
                    let rg = &row_groups[morsel.row_group_idx];

                    // 1. ZoneMap block skipping
                    let can_match = predicates.iter().all(|p| p.can_match(rg));
                    if !can_match {
                        continue;
                    }

                    // 2. Decompress filter columns directly into reusable thread-local buffers
                    rg.decompress_column_into(2, &mut ship_buf)?;
                    rg.decompress_column_into(1, &mut disc_buf)?;
                    rg.decompress_column_into(3, &mut qty_buf)?;

                    let shipdates = ship_buf.as_slice();
                    let discounts = disc_buf.as_slice();
                    let quantities = qty_buf.as_slice();
                    let num_rows = shipdates.len().min(discounts.len()).min(quantities.len());

                    let mut cnt = 0usize;
                    let mut i = 0;
                    while i + 4 <= num_rows {
                        #[allow(clippy::cast_sign_loss)]
                        let m0 = ((shipdates[i].wrapping_sub(19940101) as u64) < 10000)
                            & (((discounts[i].wrapping_sub(5)) as u64) <= 2)
                            & ((quantities[i] as u64) < 24);
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            sel_buf[cnt] = i as u16;
                        }
                        cnt += usize::from(m0);

                        #[allow(clippy::cast_sign_loss)]
                        let m1 = ((shipdates[i + 1].wrapping_sub(19940101) as u64) < 10000)
                            & (((discounts[i + 1].wrapping_sub(5)) as u64) <= 2)
                            & ((quantities[i + 1] as u64) < 24);
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            sel_buf[cnt] = (i + 1) as u16;
                        }
                        cnt += usize::from(m1);

                        #[allow(clippy::cast_sign_loss)]
                        let m2 = ((shipdates[i + 2].wrapping_sub(19940101) as u64) < 10000)
                            & (((discounts[i + 2].wrapping_sub(5)) as u64) <= 2)
                            & ((quantities[i + 2] as u64) < 24);
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            sel_buf[cnt] = (i + 2) as u16;
                        }
                        cnt += usize::from(m2);

                        #[allow(clippy::cast_sign_loss)]
                        let m3 = ((shipdates[i + 3].wrapping_sub(19940101) as u64) < 10000)
                            & (((discounts[i + 3].wrapping_sub(5)) as u64) <= 2)
                            & ((quantities[i + 3] as u64) < 24);
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            sel_buf[cnt] = (i + 3) as u16;
                        }
                        cnt += usize::from(m3);

                        i += 4;
                    }
                    while i < num_rows {
                        #[allow(clippy::cast_sign_loss)]
                        let m = ((shipdates[i].wrapping_sub(19940101) as u64) < 10000)
                            & (((discounts[i].wrapping_sub(5)) as u64) <= 2)
                            & ((quantities[i] as u64) < 24);
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            sel_buf[cnt] = i as u16;
                        }
                        cnt += usize::from(m);
                        i += 1;
                    }

                    if cnt == 0 {
                        continue;
                    }

                    // 3. Decompress extendedprice ONLY when matching rows exist
                    rg.decompress_column_into(0, &mut prices_buf)?;
                    let prices = prices_buf.as_slice();
                    local_count += cnt;
                    for &idx in &sel_buf[..cnt] {
                        local_sum += prices[idx as usize];
                    }
                }

                Ok((local_sum, local_count))
            }));
        }

        let mut total_sum = 0i64;
        let mut total_count = 0usize;
        for h in handles {
            let (sum, count) = h
                .join()
                .map_err(|_| DbError::Custom("Worker panicked".into()))??;
            total_sum += sum;
            total_count += count;
        }
        Ok((total_sum, total_count))
    })
}
