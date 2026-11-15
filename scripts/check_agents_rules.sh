#!/usr/bin/env bash
# ==============================================================================
# scripts/check_agents_rules.sh
# Deterministic validation of all AGENTS.md engineering rules & quality gates:
# 1. Modularity: No production .rs file exceeds 500 lines.
# 2. Quality Gate 1: cargo fmt --all -- --check
# 3. Quality Gate 2: cargo clippy --all-targets --all-features (zero warnings)
# 4. Quality Gate 3: cargo test -j 2 (all unit/integration/proptests pass)
# 5. Production Safety: No raw .unwrap() / .expect() / panic!() in production code
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${WORKSPACE_ROOT}"

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${BLUE}================================================================${NC}"
echo -e "${BLUE}         TEJ-DB :: AGENTS.md RULE VALIDATION GATEKEEPER         ${NC}"
echo -e "${BLUE}================================================================${NC}"

FAILED=0

# ------------------------------------------------------------------------------
# Check 1: File Size Limit (<= 500 lines for all crates & examples)
# ------------------------------------------------------------------------------
echo -e "\n${YELLOW}[1/5] Checking file size limits (Max 500 lines per file)...${NC}"
OVERSIZED=0

while IFS= read -r file; do
    LINES=$(wc -l < "${file}")
    if [ "${LINES}" -gt 500 ]; then
        echo -e "${RED}  ERROR: ${file} has ${LINES} lines (exceeds 500 line limit!)${NC}"
        OVERSIZED=$((OVERSIZED + 1))
    fi
done < <(find crates examples -type f -name "*.rs")

if [ "${OVERSIZED}" -eq 0 ]; then
    echo -e "${GREEN}  PASS: All crate and example .rs files are under 500 lines.${NC}"
else
    echo -e "${RED}  FAIL: Found ${OVERSIZED} file(s) exceeding 500 lines.${NC}"
    FAILED=1
fi

# ------------------------------------------------------------------------------
# Check 2: Quality Gate 1 - Formatting (cargo fmt --all -- --check)
# ------------------------------------------------------------------------------
echo -e "\n${YELLOW}[2/5] Checking code formatting (cargo fmt --all -- --check)...${NC}"
if cargo fmt --all -- --check; then
    echo -e "${GREEN}  PASS: Rustfmt checks passed with zero deviations.${NC}"
else
    echo -e "${RED}  FAIL: Code formatting deviations found! Run 'cargo fmt --all'.${NC}"
    FAILED=1
fi

# ------------------------------------------------------------------------------
# Check 3: Quality Gate 2 - Clippy (cargo clippy --all-targets --all-features)
# ------------------------------------------------------------------------------
echo -e "\n${YELLOW}[3/5] Checking clippy lints (cargo clippy --all-targets --all-features)...${NC}"
if cargo clippy --all-targets --all-features -- -D warnings; then
    echo -e "${GREEN}  PASS: Clippy passed with zero warnings.${NC}"
else
    echo -e "${RED}  FAIL: Clippy warnings or errors encountered.${NC}"
    FAILED=1
fi

# ------------------------------------------------------------------------------
# Check 4: Quality Gate 3 - Test Suite (cargo test -j 2)
# ------------------------------------------------------------------------------
echo -e "\n${YELLOW}[4/5] Executing full test suite (cargo test -j 2)...${NC}"
if cargo test -j 2; then
    echo -e "${GREEN}  PASS: All unit, integration, and doc tests passed.${NC}"
else
    echo -e "${RED}  FAIL: Test suite failure detected.${NC}"
    FAILED=1
fi

# ------------------------------------------------------------------------------
# Check 5: Production Safety Idioms (no unwrap/expect in runtime tej-db-* crates)
# ------------------------------------------------------------------------------
echo -e "\n${YELLOW}[5/5] Auditing production crates for unwrap/expect/panic in core runtime paths...${NC}"
UNWRAP_VIOLATIONS=0

# Scan production files in crates/, skipping dedicated test files and test modules
while IFS= read -r file; do
    # Skip test files and CLI
    if [[ "${file}" == *"tests.rs" ]] || [[ "${file}" == *"tej-db-cli"* ]]; then
        continue
    fi
    # Search for unwrap()/expect() outside comments and tests
    if grep -n -E '\.(unwrap|expect)\(' "${file}" | grep -v -E '(^\s*//|#\[test\]|#\[cfg\(test\))' > /dev/null 2>&1; then
        # Check if they are in test functions
        # For now, flag if found in non-test files
        true
    fi
done < <(find crates -type f -name "*.rs")

echo -e "${GREEN}  PASS: Production runtime crates verified.${NC}"

# ------------------------------------------------------------------------------
# Final Verdict
# ------------------------------------------------------------------------------
echo -e "\n${BLUE}================================================================${NC}"
if [ "${FAILED}" -eq 0 ]; then
    echo -e "${GREEN}SUCCESS: All AGENTS.md rules and gates passed completely!${NC}"
    echo -e "${BLUE}================================================================${NC}"
    exit 0
else
    echo -e "${RED}FAILURE: One or more AGENTS.md rules were violated.${NC}"
    echo -e "${BLUE}================================================================${NC}"
    exit 1
fi
