#!/bin/bash
set -e

echo "===================================================================="
echo " RUNNING ALL 7 UNIFIED BENCHMARKS INSIDE ISOLATED DOCKER CONTAINER"
echo " (OLTP + ACID + HTAP + Vector + Edge Cases + Disaster Simulation)"
echo "===================================================================="
echo ""
echo ">>> [1/7] Running Phase 1: Comparative Head-to-Head (TejDb vs Redb vs SQLite WAL)..."
/bench/bench_compare
echo ""
echo ">>> [2/7] Running Phase 2: Real-World, Skewed & Stress Workloads..."
/bench/bench_realworld
echo ""
echo ">>> [3/7] Running Phase 3: Production Hardening & Hardware Disaster Simulation..."
/bench/bench_production
echo ""
echo ">>> [4/7] Running Phase 4: Extreme Edge Cases & Storage Transforms..."
/bench/bench_extreme
echo ""
echo ">>> [5/7] Running Phase 5: Concurrency Scaling Matrix (1, 2, 4, 8 Writers)..."
/bench/bench_writer_matrix
echo ""
echo ">>> [6/7] Running Phase 6: Native Vector Engine (RaBitQ, FreshDiskANN, Vamana Recall@10)..."
/bench/bench_vector
echo ""
echo ">>> [7/7] Running Phase 7: Embedded HTAP & Vectorized Execution (DuckDB & SQLite Parity)..."
/bench/bench_htap
echo ""
echo "===================================================================="
echo " ALL 7 DOCKER BENCHMARKS COMPLETED SUCCESSFULLY!"
echo "===================================================================="
