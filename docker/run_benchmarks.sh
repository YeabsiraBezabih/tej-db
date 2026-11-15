#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=========================================================="
echo " Building Docker Benchmark Image for TejDb (Modular ACID DBMS)"
echo "=========================================================="
docker build -t tej-db-benchmark:latest -f "$SCRIPT_DIR/Dockerfile.bench" "$ROOT_DIR"

echo ""
echo "=========================================================="
echo " Running All 7 Benchmarks in Isolated Docker Container"
echo " (Constraints: 4 CPUs, 4GB RAM)"
echo "=========================================================="
docker run --rm \
    --cpus=4 \
    --memory=4g \
    tej-db-benchmark:latest
