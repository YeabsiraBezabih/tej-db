use crate::operator::PhysicalOperator;
use smallvec::SmallVec;
use std::collections::HashMap;
use tej_db_columnar::{DataChunk, PhysicalType};
use tej_db_core::{DbError, Result};

/// Vectorized hash join operator executing equi-joins between two input operator streams.
#[derive(Debug)]
pub struct PhysicalHashJoin {
    build_child: Box<dyn PhysicalOperator>,
    probe_child: Box<dyn PhysicalOperator>,
    build_key_col: usize,
    probe_key_col: usize,
    build_types: Vec<PhysicalType>,
    probe_types: Vec<PhysicalType>,
    build_columns: Vec<Vec<i64>>,
    hash_index: HashMap<i64, SmallVec<[u32; 2]>>,
    direct_build_index: Vec<SmallVec<[u32; 2]>>,
    use_direct_build: bool,
    bloom_filter: u64,
    out_types: Vec<PhysicalType>,
    is_open: bool,
    cur_probe_chunk: Option<DataChunk>,
    probe_row_cursor: usize,
    build_match_cursor: usize,
}

impl PhysicalHashJoin {
    #[must_use]
    pub fn new(
        build_child: Box<dyn PhysicalOperator>,
        probe_child: Box<dyn PhysicalOperator>,
        build_key_col: usize,
        probe_key_col: usize,
        build_types: Vec<PhysicalType>,
        probe_types: Vec<PhysicalType>,
    ) -> Self {
        let mut out_types = Vec::with_capacity(build_types.len() + probe_types.len());
        out_types.extend_from_slice(&build_types);
        out_types.extend_from_slice(&probe_types);

        let num_build_cols = build_types.len();
        let mut build_columns = Vec::with_capacity(num_build_cols);
        for _ in 0..num_build_cols {
            build_columns.push(Vec::new());
        }

        Self {
            build_child,
            probe_child,
            build_key_col,
            probe_key_col,
            build_types,
            probe_types,
            build_columns,
            hash_index: HashMap::new(),
            direct_build_index: Vec::new(),
            use_direct_build: false,
            bloom_filter: 0,
            out_types,
            is_open: false,
            cur_probe_chunk: None,
            probe_row_cursor: 0,
            build_match_cursor: 0,
        }
    }

    /// Probes a single DataChunk against the immutable build table, returning the count of joined output rows.
    pub fn probe_chunk_count(&self, probe_chunk: &DataChunk) -> Result<usize> {
        let probe_key_col = probe_chunk
            .column(self.probe_key_col)
            .ok_or_else(|| DbError::Custom("Probe key column missing".into()))?;
        let probe_keys = probe_key_col.as_i64_slice();

        let mut match_count = 0;
        for &pk in probe_keys {
            #[allow(clippy::cast_sign_loss)]
            let bit = 1u64 << ((pk as u64) & 63);
            if (self.bloom_filter & bit) == 0 {
                continue;
            }

            let matches = if self.use_direct_build {
                #[allow(clippy::cast_sign_loss)]
                if pk >= 0 && (pk as usize) < self.direct_build_index.len() {
                    self.direct_build_index[pk as usize].len()
                } else {
                    0
                }
            } else {
                self.hash_index.get(&pk).map_or(0, smallvec::SmallVec::len)
            };
            match_count += matches;
        }
        Ok(match_count)
    }

    /// Evaluates the probe phase in parallel across `num_threads` worker threads using `MorselScheduler`.
    pub fn probe_row_groups_parallel(
        &self,
        row_groups: &[tej_db_columnar::RowGroup],
        num_threads: usize,
    ) -> Result<usize> {
        let total_rgs = row_groups.len();
        if total_rgs == 0 {
            return Ok(0);
        }
        let rows_per_rg = row_groups
            .iter()
            .map(|rg| rg.metadata.num_rows as usize)
            .max()
            .unwrap_or(1)
            .max(1);
        let scheduler = std::sync::Arc::new(crate::MorselScheduler::new(
            total_rgs,
            rows_per_rg,
            rows_per_rg,
        ));
        let num_threads = num_threads.max(1);

        let bloom_filter = self.bloom_filter;
        let use_direct_build = self.use_direct_build;
        let direct_build_index = &self.direct_build_index;
        let hash_index = &self.hash_index;
        let probe_key_col = self.probe_key_col;

        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(num_threads);
            for _ in 0..num_threads {
                let sched = std::sync::Arc::clone(&scheduler);
                handles.push(s.spawn(move || -> Result<usize> {
                    let mut local_count = 0;
                    let mut pk_buf = Vec::with_capacity(2048);
                    while let Some(morsel) = sched.next_morsel() {
                        let rg = &row_groups[morsel.row_group_idx];
                        rg.decompress_column_into(probe_key_col, &mut pk_buf)?;
                        let probe_keys = pk_buf.as_slice();

                        let chunks_4 = probe_keys.chunks_exact(4);
                        let remainder = chunks_4.remainder();

                        for quad in chunks_4 {
                            for &pk in quad {
                                #[allow(clippy::cast_sign_loss)]
                                let bit = 1u64 << ((pk as u64) & 63);
                                if (bloom_filter & bit) == 0 {
                                    continue;
                                }

                                let matches = if use_direct_build {
                                    #[allow(clippy::cast_sign_loss)]
                                    if pk >= 0 && (pk as usize) < direct_build_index.len() {
                                        direct_build_index[pk as usize].len()
                                    } else {
                                        0
                                    }
                                } else {
                                    hash_index.get(&pk).map_or(0, smallvec::SmallVec::len)
                                };
                                local_count += matches;
                            }
                        }

                        for &pk in remainder {
                            #[allow(clippy::cast_sign_loss)]
                            let bit = 1u64 << ((pk as u64) & 63);
                            if (bloom_filter & bit) == 0 {
                                continue;
                            }

                            let matches = if use_direct_build {
                                #[allow(clippy::cast_sign_loss)]
                                if pk >= 0 && (pk as usize) < direct_build_index.len() {
                                    direct_build_index[pk as usize].len()
                                } else {
                                    0
                                }
                            } else {
                                hash_index.get(&pk).map_or(0, smallvec::SmallVec::len)
                            };
                            local_count += matches;
                        }
                    }
                    Ok(local_count)
                }));
            }

            let mut total = 0;
            for h in handles {
                total += h
                    .join()
                    .map_err(|_| DbError::Custom("Worker thread panicked".into()))??;
            }
            Ok(total)
        })
    }
}

impl PhysicalOperator for PhysicalHashJoin {
    fn open(&mut self) -> Result<()> {
        self.build_child.open()?;
        self.probe_child.open()?;
        self.hash_index.clear();
        self.direct_build_index.clear();
        self.bloom_filter = 0;
        self.use_direct_build = false;
        for col in &mut self.build_columns {
            col.clear();
        }
        self.is_open = true;

        // 1. Build Phase: drain build child into columnar buffers and hash index
        let mut row_counter = 0_u32;
        let mut all_keys = Vec::new();

        while let Some(chunk) = self.build_child.next()? {
            let key_col = chunk
                .column(self.build_key_col)
                .ok_or_else(|| DbError::Custom("Build key column missing".into()))?;
            let keys = key_col.as_i64_slice();

            for (c_idx, col_buf) in self.build_columns.iter_mut().enumerate() {
                if let Some(col) = chunk.column(c_idx) {
                    col_buf.extend_from_slice(col.as_i64_slice());
                }
            }

            for &k in keys {
                all_keys.push(k);
                #[allow(clippy::cast_sign_loss)]
                {
                    self.bloom_filter |= 1u64 << ((k as u64) & 63);
                }
                self.hash_index.entry(k).or_default().push(row_counter);
                row_counter += 1;
            }
        }

        let min_k = all_keys.iter().copied().min().unwrap_or(0);
        let max_k = all_keys.iter().copied().max().unwrap_or(0);
        if !all_keys.is_empty() && min_k >= 0 && max_k < 65536 {
            #[allow(clippy::cast_sign_loss)]
            let cap = (max_k as usize) + 1;
            self.direct_build_index = vec![SmallVec::new(); cap];
            for (r_idx, &k) in all_keys.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                self.direct_build_index[k as usize].push(r_idx as u32);
            }
            self.use_direct_build = true;
        }

        Ok(())
    }

    fn next(&mut self) -> Result<Option<DataChunk>> {
        if !self.is_open {
            return Ok(None);
        }

        let build_slices: Vec<&[i64]> = self.build_columns.iter().map(Vec::as_slice).collect();
        let mut out_chunk = DataChunk::new(&self.out_types, 2048);
        let mut match_count = 0;

        loop {
            if self.cur_probe_chunk.is_none() {
                self.cur_probe_chunk = self.probe_child.next()?;
                self.probe_row_cursor = 0;
                self.build_match_cursor = 0;
                if self.cur_probe_chunk.is_none() {
                    break;
                }
            }

            let probe_chunk = self
                .cur_probe_chunk
                .as_ref()
                .ok_or_else(|| DbError::Custom("Unexpected missing probe chunk in join".into()))?;
            let probe_key_col = probe_chunk
                .column(self.probe_key_col)
                .ok_or_else(|| DbError::Custom("Probe key column missing".into()))?;
            let probe_keys = probe_key_col.as_i64_slice();

            let probe_slices: Vec<&[i64]> = (0..self.probe_types.len())
                .filter_map(|c| {
                    probe_chunk
                        .column(c)
                        .map(tej_db_columnar::ColumnVector::as_i64_slice)
                })
                .collect();

            let num_probe_rows = probe_keys.len();
            while self.probe_row_cursor < num_probe_rows {
                let probe_row_idx = self.probe_row_cursor;
                let pk = probe_keys[probe_row_idx];

                #[allow(clippy::cast_sign_loss)]
                let bit = 1u64 << ((pk as u64) & 63);
                if (self.bloom_filter & bit) == 0 {
                    self.probe_row_cursor += 1;
                    self.build_match_cursor = 0;
                    continue;
                }

                let matching_opt: Option<&[u32]> = if self.use_direct_build {
                    #[allow(clippy::cast_sign_loss)]
                    if pk >= 0 && (pk as usize) < self.direct_build_index.len() {
                        let rows = &self.direct_build_index[pk as usize];
                        if rows.is_empty() {
                            None
                        } else {
                            Some(rows.as_slice())
                        }
                    } else {
                        None
                    }
                } else {
                    self.hash_index.get(&pk).map(SmallVec::as_slice)
                };

                if let Some(matching_build_rows) = matching_opt {
                    while self.build_match_cursor < matching_build_rows.len() {
                        let build_row_idx = matching_build_rows[self.build_match_cursor];
                        let b_idx = build_row_idx as usize;

                        // Emit build columns
                        for (c_idx, col_slice) in build_slices.iter().enumerate() {
                            let val = col_slice.get(b_idx).copied().unwrap_or(0);
                            out_chunk
                                .column_mut(c_idx)
                                .ok_or_else(|| DbError::Custom("Out build col missing".into()))?
                                .push_i64(val)?;
                        }

                        // Emit probe columns via direct slice
                        for (c_idx, col_slice) in probe_slices.iter().enumerate() {
                            let val = col_slice.get(probe_row_idx).copied().unwrap_or(0);
                            out_chunk
                                .column_mut(self.build_types.len() + c_idx)
                                .ok_or_else(|| DbError::Custom("Out probe col missing".into()))?
                                .push_i64(val)?;
                        }

                        match_count += 1;
                        self.build_match_cursor += 1;

                        if match_count >= 2048 {
                            out_chunk.set_num_rows(match_count)?;
                            return Ok(Some(out_chunk));
                        }
                    }
                }

                self.probe_row_cursor += 1;
                self.build_match_cursor = 0;
            }

            self.cur_probe_chunk = None;
        }

        if match_count > 0 {
            out_chunk.set_num_rows(match_count)?;
            return Ok(Some(out_chunk));
        }

        Ok(None)
    }

    fn close(&mut self) -> Result<()> {
        self.is_open = false;
        self.cur_probe_chunk = None;
        self.probe_row_cursor = 0;
        self.build_match_cursor = 0;
        self.build_child.close()?;
        self.probe_child.close()
    }
}
