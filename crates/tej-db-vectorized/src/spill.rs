use std::cmp::Reverse;
use std::collections::BinaryHeap;
use tej_db_columnar::{DataChunk, PhysicalType};
use tej_db_core::{DbError, Result};

/// Manages an out-of-core temporary spilled run of rows.
#[derive(Debug, Clone, PartialEq)]
pub struct SpilledRun {
    pub rows: Vec<(i64, Vec<i64>)>, // (sort_key, payload_columns)
}

/// Out-of-core spill operator for memory-capped analytical processing.
#[derive(Debug)]
pub struct SpillableBuffer {
    max_memory_bytes: usize,
    current_memory_bytes: usize,
    in_memory_rows: Vec<(i64, Vec<i64>)>,
    spilled_runs: Vec<SpilledRun>,
}

impl SpillableBuffer {
    #[must_use]
    pub fn new(max_memory_bytes: usize) -> Self {
        Self {
            max_memory_bytes,
            current_memory_bytes: 0,
            in_memory_rows: Vec::new(),
            spilled_runs: Vec::new(),
        }
    }

    /// Appends a row, spilling to a sorted run if memory limit is breached.
    pub fn append(&mut self, key: i64, payload: Vec<i64>) -> Result<()> {
        let row_bytes = 8 + payload.len() * 8 + 32;
        self.current_memory_bytes += row_bytes;
        self.in_memory_rows.push((key, payload));

        if self.current_memory_bytes >= self.max_memory_bytes {
            self.spill_current_run()?;
        }

        Ok(())
    }

    /// Spills active in-memory rows into a sorted run.
    pub fn spill_current_run(&mut self) -> Result<()> {
        if self.in_memory_rows.is_empty() {
            return Ok(());
        }

        self.in_memory_rows.sort_by_key(|r| r.0);
        let run = SpilledRun {
            rows: std::mem::take(&mut self.in_memory_rows),
        };
        self.spilled_runs.push(run);
        self.current_memory_bytes = 0;
        Ok(())
    }

    #[must_use]
    pub fn num_spilled_runs(&self) -> usize {
        self.spilled_runs.len()
    }

    /// Executes an out-of-core k-way merge across all spilled runs into sorted DataChunks.
    pub fn merge_all(mut self, batch_size: usize) -> Result<Vec<DataChunk>> {
        if !self.in_memory_rows.is_empty() {
            self.spill_current_run()?;
        }

        if self.spilled_runs.is_empty() {
            return Ok(Vec::new());
        }

        let num_runs = self.spilled_runs.len();
        let mut cursors = vec![0usize; num_runs];
        let mut min_heap: BinaryHeap<Reverse<(i64, usize)>> = BinaryHeap::new();

        for (run_idx, run) in self.spilled_runs.iter().enumerate() {
            if let Some(first) = run.rows.first() {
                min_heap.push(Reverse((first.0, run_idx)));
            }
        }

        let payload_len = self.spilled_runs[0].rows.first().map_or(0, |r| r.1.len());

        let mut types = vec![PhysicalType::Int64]; // key
        for _ in 0..payload_len {
            types.push(PhysicalType::Int64);
        }

        let mut out_chunks = Vec::new();
        let mut cur_chunk = DataChunk::new(&types, batch_size);

        while let Some(Reverse((_, run_idx))) = min_heap.pop() {
            let row_idx = cursors[run_idx];
            let (key, payload) = &self.spilled_runs[run_idx].rows[row_idx];

            cur_chunk
                .column_mut(0)
                .ok_or_else(|| DbError::Custom("Key column missing".into()))?
                .push_i64(*key)?;

            for (p_idx, &val) in payload.iter().enumerate() {
                cur_chunk
                    .column_mut(p_idx + 1)
                    .ok_or_else(|| DbError::Custom("Payload column missing".into()))?
                    .push_i64(val)?;
            }

            let cur_len = cur_chunk
                .column(0)
                .map_or(0, tej_db_columnar::ColumnVector::len);
            if cur_len == batch_size {
                let mut full_chunk = DataChunk::new(&types, batch_size);
                std::mem::swap(&mut cur_chunk, &mut full_chunk);
                full_chunk.set_num_rows(batch_size)?;
                out_chunks.push(full_chunk);
            }

            cursors[run_idx] += 1;
            if cursors[run_idx] < self.spilled_runs[run_idx].rows.len() {
                let next_key = self.spilled_runs[run_idx].rows[cursors[run_idx]].0;
                min_heap.push(Reverse((next_key, run_idx)));
            }
        }

        if cur_chunk.column(0).is_some_and(|c| !c.is_empty()) {
            let row_count = cur_chunk
                .column(0)
                .map_or(0, tej_db_columnar::ColumnVector::len);
            cur_chunk.set_num_rows(row_count)?;
            out_chunks.push(cur_chunk);
        }

        Ok(out_chunks)
    }
}
