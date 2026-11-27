use crate::operator::PhysicalOperator;
use smallvec::SmallVec;
use std::collections::HashMap;
use tej_db_columnar::{DataChunk, PhysicalType};
use tej_db_core::{DbError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateFunction {
    Count,
    SumInt64,
    AvgFloat64,
    MinInt64,
    MaxInt64,
}

#[derive(Debug, Clone)]
enum AggState {
    Count(u64),
    SumInt64(i64),
    AvgFloat64 { sum: f64, count: u64 },
    MinInt64(i64),
    MaxInt64(i64),
}

impl AggState {
    fn new(func: AggregateFunction) -> Self {
        match func {
            AggregateFunction::Count => Self::Count(0),
            AggregateFunction::SumInt64 => Self::SumInt64(0),
            AggregateFunction::AvgFloat64 => Self::AvgFloat64 { sum: 0.0, count: 0 },
            AggregateFunction::MinInt64 => Self::MinInt64(i64::MAX),
            AggregateFunction::MaxInt64 => Self::MaxInt64(i64::MIN),
        }
    }

    #[inline]
    fn update_scalar_i64(&mut self, val: i64) {
        match self {
            Self::Count(c) => *c += 1,
            Self::SumInt64(s) => *s += val,
            Self::AvgFloat64 { sum, count } => {
                #[allow(clippy::cast_precision_loss)]
                {
                    *sum += val as f64;
                }
                *count += 1;
            }
            Self::MinInt64(m) => *m = (*m).min(val),
            Self::MaxInt64(m) => *m = (*m).max(val),
        }
    }
}

#[derive(Debug, Clone)]
struct DirectGroupSlot {
    key: i64,
    initialized: bool,
    states: SmallVec<[AggState; 4]>,
}

/// Physical hash aggregation operator supporting both grouped and scalar aggregations.
#[derive(Debug)]
pub struct PhysicalHashAggregate {
    child: Box<dyn PhysicalOperator>,
    group_col_idx: Option<usize>,
    aggregates: Vec<(usize, AggregateFunction)>,
    emitted: bool,
    groups: HashMap<i64, Vec<AggState>>,
    scalar_states: Vec<AggState>,
    direct_slots: Vec<DirectGroupSlot>,
    use_direct_table: bool,
}

impl PhysicalHashAggregate {
    #[must_use]
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        group_col_idx: Option<usize>,
        aggregates: Vec<(usize, AggregateFunction)>,
    ) -> Self {
        let scalar_states = aggregates.iter().map(|(_, f)| AggState::new(*f)).collect();
        Self {
            child,
            group_col_idx,
            aggregates,
            emitted: false,
            groups: HashMap::new(),
            scalar_states,
            direct_slots: Vec::new(),
            use_direct_table: true,
        }
    }

    fn process_scalar_chunk(&mut self, chunk: &DataChunk) {
        for (agg_idx, &(c_idx, func)) in self.aggregates.iter().enumerate() {
            let col = match chunk.column(c_idx) {
                Some(c) => c,
                None => continue,
            };

            let state = &mut self.scalar_states[agg_idx];
            match func {
                AggregateFunction::Count => {
                    let null_cnt = u64::from(col.null_count());
                    let valid_cnt = (col.len() as u64).saturating_sub(null_cnt);
                    if let AggState::Count(c) = state {
                        *c += valid_cnt;
                    }
                }
                AggregateFunction::SumInt64 => {
                    let slice = col.as_i64_slice();
                    let sum: i64 = if col.null_count() == 0 {
                        slice.iter().sum()
                    } else {
                        slice
                            .iter()
                            .enumerate()
                            .filter(|&(i, _)| !col.is_null(i))
                            .map(|(_, &v)| v)
                            .sum()
                    };
                    if let AggState::SumInt64(s) = state {
                        *s += sum;
                    }
                }
                AggregateFunction::AvgFloat64 => {
                    let slice = col.as_f64_slice();
                    let sum: f64 = if col.null_count() == 0 {
                        slice.iter().sum()
                    } else {
                        slice
                            .iter()
                            .enumerate()
                            .filter(|&(i, _)| !col.is_null(i))
                            .map(|(_, &v)| v)
                            .sum()
                    };
                    let null_cnt = u64::from(col.null_count());
                    let cnt = (col.len() as u64).saturating_sub(null_cnt);
                    if let AggState::AvgFloat64 {
                        sum: s,
                        count: count_ref,
                    } = state
                    {
                        *s += sum;
                        *count_ref += cnt;
                    }
                }
                AggregateFunction::MinInt64 => {
                    let slice = col.as_i64_slice();
                    let min_opt = if col.null_count() == 0 {
                        slice.iter().copied().min()
                    } else {
                        slice
                            .iter()
                            .enumerate()
                            .filter(|&(i, _)| !col.is_null(i))
                            .map(|(_, &v)| v)
                            .min()
                    };
                    if let Some(min_val) = min_opt {
                        if let AggState::MinInt64(m) = state {
                            *m = (*m).min(min_val);
                        }
                    }
                }
                AggregateFunction::MaxInt64 => {
                    let slice = col.as_i64_slice();
                    let max_opt = if col.null_count() == 0 {
                        slice.iter().copied().max()
                    } else {
                        slice
                            .iter()
                            .enumerate()
                            .filter(|&(i, _)| !col.is_null(i))
                            .map(|(_, &v)| v)
                            .max()
                    };
                    if let Some(max_val) = max_opt {
                        if let AggState::MaxInt64(m) = state {
                            *m = (*m).max(max_val);
                        }
                    }
                }
            }
        }
    }

    fn process_grouped_chunk(&mut self, chunk: &DataChunk, g_idx: usize) {
        let group_col = match chunk.column(g_idx) {
            Some(c) => c,
            None => return,
        };
        let keys = group_col.as_i64_slice();

        if self.use_direct_table {
            let mut can_use_direct = true;
            let cap = self.direct_slots.len();
            for &k in keys {
                #[allow(clippy::cast_sign_loss)]
                if k < 0 || (k as usize) >= cap {
                    can_use_direct = false;
                    break;
                }
            }

            if can_use_direct {
                for (agg_idx, &(c_idx, _)) in self.aggregates.iter().enumerate() {
                    if let Some(val_col) = chunk.column(c_idx) {
                        let vals = val_col.as_i64_slice();
                        let n = keys.len().min(vals.len());
                        for i in 0..n {
                            #[allow(clippy::cast_sign_loss)]
                            let k = keys[i] as usize;
                            let slot = &mut self.direct_slots[k];
                            if !slot.initialized {
                                slot.initialized = true;
                                slot.key = keys[i];
                            }
                            slot.states[agg_idx].update_scalar_i64(vals[i]);
                        }
                    }
                }
                return;
            }

            let max_k = keys.iter().copied().max().unwrap_or(0);
            let min_k = keys.iter().copied().min().unwrap_or(0);
            if min_k >= 0 && max_k < 65536 {
                #[allow(clippy::cast_sign_loss)]
                let new_cap = (max_k as usize + 1)
                    .max(self.direct_slots.len() * 2)
                    .min(65536);
                let default_states: SmallVec<[AggState; 4]> = self
                    .aggregates
                    .iter()
                    .map(|(_, f)| AggState::new(*f))
                    .collect();
                self.direct_slots.resize(
                    new_cap,
                    DirectGroupSlot {
                        key: 0,
                        initialized: false,
                        states: default_states,
                    },
                );

                for (agg_idx, &(c_idx, _)) in self.aggregates.iter().enumerate() {
                    if let Some(val_col) = chunk.column(c_idx) {
                        let vals = val_col.as_i64_slice();
                        let n = keys.len().min(vals.len());
                        for i in 0..n {
                            #[allow(clippy::cast_sign_loss)]
                            let k = keys[i] as usize;
                            let slot = &mut self.direct_slots[k];
                            if !slot.initialized {
                                slot.initialized = true;
                                slot.key = keys[i];
                            }
                            slot.states[agg_idx].update_scalar_i64(vals[i]);
                        }
                    }
                }
                return;
            }

            self.use_direct_table = false;
            for slot in &self.direct_slots {
                if slot.initialized {
                    self.groups.insert(slot.key, slot.states.to_vec());
                }
            }
        }

        // Fallback for large or negative keys
        for (agg_idx, &(c_idx, _)) in self.aggregates.iter().enumerate() {
            let val_col = match chunk.column(c_idx) {
                Some(c) => c,
                None => continue,
            };
            let vals = val_col.as_i64_slice();

            for (&k, &v) in keys.iter().zip(vals.iter()) {
                let states = self.groups.entry(k).or_insert_with(|| {
                    self.aggregates
                        .iter()
                        .map(|(_, f)| AggState::new(*f))
                        .collect()
                });
                states[agg_idx].update_scalar_i64(v);
            }
        }
    }
}

impl PhysicalOperator for PhysicalHashAggregate {
    fn open(&mut self) -> Result<()> {
        self.child.open()?;
        self.emitted = false;
        self.groups.clear();
        self.use_direct_table = true;
        let default_states: SmallVec<[AggState; 4]> = self
            .aggregates
            .iter()
            .map(|(_, f)| AggState::new(*f))
            .collect();
        self.direct_slots = vec![
            DirectGroupSlot {
                key: 0,
                initialized: false,
                states: default_states,
            };
            1024
        ];

        for s in &mut self.scalar_states {
            *s = AggState::new(match s {
                AggState::Count(_) => AggregateFunction::Count,
                AggState::SumInt64(_) => AggregateFunction::SumInt64,
                AggState::AvgFloat64 { .. } => AggregateFunction::AvgFloat64,
                AggState::MinInt64(_) => AggregateFunction::MinInt64,
                AggState::MaxInt64(_) => AggregateFunction::MaxInt64,
            });
        }

        while let Some(chunk) = self.child.next()? {
            if let Some(g_idx) = self.group_col_idx {
                self.process_grouped_chunk(&chunk, g_idx);
            } else {
                self.process_scalar_chunk(&chunk);
            }
        }

        Ok(())
    }

    fn next(&mut self) -> Result<Option<DataChunk>> {
        if self.emitted {
            return Ok(None);
        }
        self.emitted = true;

        if self.group_col_idx.is_some() {
            let mut types = vec![PhysicalType::Int64];
            for (_, func) in &self.aggregates {
                match func {
                    AggregateFunction::Count
                    | AggregateFunction::SumInt64
                    | AggregateFunction::MinInt64
                    | AggregateFunction::MaxInt64 => {
                        types.push(PhysicalType::Int64);
                    }
                    AggregateFunction::AvgFloat64 => types.push(PhysicalType::Float64),
                }
            }

            let num_direct = self.direct_slots.iter().filter(|s| s.initialized).count();
            let num_groups = if self.use_direct_table {
                num_direct
            } else {
                self.groups.len()
            };
            let mut out = DataChunk::new(&types, num_groups.max(1));

            let emit_row = |out: &mut DataChunk, k: i64, states: &[AggState]| -> Result<()> {
                out.column_mut(0)
                    .ok_or_else(|| DbError::Custom("Group key missing".into()))?
                    .push_i64(k)?;

                for (idx, state) in states.iter().enumerate() {
                    let col = out
                        .column_mut(idx + 1)
                        .ok_or_else(|| DbError::Custom("Agg column missing".into()))?;
                    match state {
                        #[allow(clippy::cast_possible_wrap)]
                        AggState::Count(c) => col.push_i64(*c as i64)?,
                        AggState::SumInt64(s) => col.push_i64(*s)?,
                        AggState::AvgFloat64 { sum, count } => {
                            #[allow(clippy::cast_precision_loss)]
                            let avg = if *count > 0 {
                                *sum / (*count as f64)
                            } else {
                                0.0
                            };
                            col.push_f64(avg)?;
                        }
                        AggState::MinInt64(m) | AggState::MaxInt64(m) => col.push_i64(*m)?,
                    }
                }
                Ok(())
            };

            if self.use_direct_table {
                for slot in &self.direct_slots {
                    if slot.initialized {
                        emit_row(&mut out, slot.key, &slot.states)?;
                    }
                }
            } else {
                for (&k, states) in &self.groups {
                    emit_row(&mut out, k, states)?;
                }
            }

            out.set_num_rows(num_groups)?;
            Ok(Some(out))
        } else {
            let mut types = Vec::with_capacity(self.aggregates.len());
            for (_, func) in &self.aggregates {
                match func {
                    AggregateFunction::Count
                    | AggregateFunction::SumInt64
                    | AggregateFunction::MinInt64
                    | AggregateFunction::MaxInt64 => {
                        types.push(PhysicalType::Int64);
                    }
                    AggregateFunction::AvgFloat64 => types.push(PhysicalType::Float64),
                }
            }

            let mut out = DataChunk::new(&types, 1);
            for (idx, state) in self.scalar_states.iter().enumerate() {
                let col = out
                    .column_mut(idx)
                    .ok_or_else(|| DbError::Custom("Agg column missing".into()))?;
                match state {
                    #[allow(clippy::cast_possible_wrap)]
                    AggState::Count(c) => col.push_i64(*c as i64)?,
                    AggState::SumInt64(s) => col.push_i64(*s)?,
                    AggState::AvgFloat64 { sum, count } => {
                        #[allow(clippy::cast_precision_loss)]
                        let avg = if *count > 0 {
                            *sum / (*count as f64)
                        } else {
                            0.0
                        };
                        col.push_f64(avg)?;
                    }
                    AggState::MinInt64(m) | AggState::MaxInt64(m) => col.push_i64(*m)?,
                }
            }
            out.set_num_rows(1)?;
            Ok(Some(out))
        }
    }

    fn close(&mut self) -> Result<()> {
        self.child.close()?;
        self.groups.clear();
        self.direct_slots.clear();
        Ok(())
    }
}
