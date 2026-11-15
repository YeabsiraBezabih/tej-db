/// Physical execution costs used by the Cost-Based Optimizer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostModel {
    pub page_io_cost: f64,
    pub cpu_row_eval_cost: f64,
    pub simd_row_eval_cost: f64,
    pub hash_build_cost: f64,
    pub hash_probe_cost: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            page_io_cost: 1.0,
            cpu_row_eval_cost: 0.01,
            simd_row_eval_cost: 0.001, // SIMD vectorized batch is ~10x cheaper per row
            hash_build_cost: 0.02,
            hash_probe_cost: 0.005,
        }
    }
}

#[allow(clippy::cast_precision_loss)]
impl CostModel {
    /// Cost estimate for a B+ tree index point lookup or small range seek.
    #[must_use]
    pub fn cost_index_seek(&self, tree_height: u32, expected_rows: u64) -> f64 {
        let io = f64::from(tree_height) * self.page_io_cost;
        let cpu = (expected_rows as f64) * self.cpu_row_eval_cost;
        io + cpu
    }

    /// Cost estimate for a full row-oriented Volcano table scan.
    #[must_use]
    pub fn cost_row_scan(&self, total_pages: u64, total_rows: u64) -> f64 {
        let io = (total_pages as f64) * self.page_io_cost;
        let cpu = (total_rows as f64) * self.cpu_row_eval_cost;
        io + cpu
    }

    /// Cost estimate for a columnar vectorized scan with projection and zone-map pushdown.
    #[must_use]
    pub fn cost_columnar_scan(
        &self,
        total_pages: u64,
        total_rows: u64,
        projected_cols: usize,
        total_cols: usize,
        compression_ratio: f64,
        zonemap_skip_ratio: f64,
    ) -> f64 {
        let col_ratio = (projected_cols as f64) / (total_cols.max(1) as f64);
        let eff_pages = ((total_pages as f64) * col_ratio / compression_ratio.max(1.0))
            * (1.0 - zonemap_skip_ratio).clamp(0.0, 1.0);
        let io = eff_pages * self.page_io_cost;
        let cpu = (total_rows as f64) * (1.0 - zonemap_skip_ratio) * self.simd_row_eval_cost;
        io + cpu
    }

    /// Cost estimate for a vectorized hash join.
    #[must_use]
    pub fn cost_hash_join(&self, build_rows: u64, probe_rows: u64) -> f64 {
        let build = (build_rows as f64) * self.hash_build_cost;
        let probe = (probe_rows as f64) * self.hash_probe_cost;
        build + probe
    }
}
