#!/usr/bin/env bash
# ==============================================================================
# scripts/publish_crates.sh
# Publishes all 10 TejDB crates to crates.io in topological dependency order.
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${WORKSPACE_ROOT}"

echo "=================================================================="
echo " Publishing TejDB Crates to crates.io in Dependency Order"
echo "=================================================================="

# Crates ordered topologically by dependency requirements:
CRATES=(
    "tej-db-core"
    "tej-db-pager"
    "tej-db-btree"
    "tej-db-columnar"
    "tej-db-txn"
    "tej-db-vector"
    "tej-db-vectorized"
    "tej-db"
    "tej-db-sql"
    "tej-db-cli"
)

# Optional dry run flag
DRY_RUN=""
if [ "${1:-}" == "--dry-run" ]; then
    DRY_RUN="--dry-run"
    echo ">>> Running in DRY RUN mode..."
fi

for crate in "${CRATES[@]}"; do
    echo ""
    echo "=================================================================="
    echo ">>> Publishing crate: ${crate}"
    echo "=================================================================="
    cargo publish -p "${crate}" ${DRY_RUN}
    
    # Wait for crates.io index propagation between dependent uploads
    if [ -z "${DRY_RUN}" ]; then
        echo "Waiting 20 seconds for crates.io index propagation..."
        sleep 20
    fi
done

echo ""
echo "=================================================================="
echo " All TejDB crates successfully published to crates.io!"
echo "=================================================================="
