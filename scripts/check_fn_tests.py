#!/usr/bin/env python3
"""
scripts/check_fn_tests.py
Deterministically audits Rust function coverage against unit and integration tests.
Scans all crates under crates/ and examples/, extracts declared functions,
and checks for occurrences in unit tests (tests.rs) and integration tests (tests/).
"""

import os
import re
import sys
from pathlib import Path
from collections import defaultdict

FN_REGEX = re.compile(r'^\s*(?:pub(?:\([^\)]+\))?\s+)?(?:async\s+)?fn\s+([a-zA-Z0-9_]+)\s*(?:<[^>]+>)?\s*\(', re.MULTILINE)

def collect_test_sources(workspace_root: Path):
    """Collects all test source code text from unit and integration tests."""
    test_corpus = []
    
    # 1. Unit tests in crates
    for root, _, files in os.walk(workspace_root / "crates"):
        for f in files:
            if f == "tests.rs" or f.endswith("_test.rs"):
                path = Path(root) / f
                test_corpus.append((str(path), path.read_text(encoding="utf-8", errors="ignore")))
                
    # 2. Integration tests in tests/
    for root, _, files in os.walk(workspace_root / "tests"):
        for f in files:
            if f.endswith(".rs"):
                path = Path(root) / f
                test_corpus.append((str(path), path.read_text(encoding="utf-8", errors="ignore")))
                
    # 3. Example tests
    for root, _, files in os.walk(workspace_root / "examples"):
        for f in files:
            if f == "tests.rs":
                path = Path(root) / f
                test_corpus.append((str(path), path.read_text(encoding="utf-8", errors="ignore")))

    return test_corpus

def audit_crate_functions(workspace_root: Path):
    crates_dir = workspace_root / "crates"
    test_corpus = collect_test_sources(workspace_root)
    full_test_text = "\n".join(text for _, text in test_corpus)

    results = defaultdict(lambda: {"tested": [], "untested": []})

    for crate_path in sorted(crates_dir.iterdir()):
        if not crate_path.is_dir():
            continue
        crate_name = crate_path.name
        src_dir = crate_path / "src"
        if not src_dir.exists():
            continue

        for root, _, files in os.walk(src_dir):
            for file in files:
                if not file.endswith(".rs") or file == "tests.rs":
                    continue
                file_path = Path(root) / file
                content = file_path.read_text(encoding="utf-8", errors="ignore")

                # Strip comments
                content_no_comments = re.sub(r'//.*', '', content)
                content_no_comments = re.sub(r'/\*.*?\*/', '', content_no_comments, flags=re.DOTALL)

                for match in FN_REGEX.finditer(content_no_comments):
                    fn_name = match.group(1)
                    # Ignore standard trait boilerplate, derive, or internal helpers
                    if fn_name in ("new", "default", "drop", "clone", "fmt", "from", "into", "deref", "as_ref", "as_mut"):
                        continue

                    # Search in test corpus for the function name
                    # Look for fn_name( or .fn_name( or ::fn_name(
                    pattern = rf'(?:\b|\.|\:\:){re.escape(fn_name)}\s*[\(<]'
                    if re.search(pattern, full_test_text):
                        results[crate_name]["tested"].append((file_path.name, fn_name))
                    else:
                        results[crate_name]["untested"].append((file_path.name, fn_name))

    return results

def main():
    root = Path(__file__).resolve().parent.parent
    results = audit_crate_functions(root)

    print("=" * 80)
    print("DETERMINISTIC FUNCTION TEST COVERAGE AUDIT")
    print("=" * 80)

    total_tested = 0
    total_untested = 0

    for crate, data in sorted(results.items()):
        tested_count = len(data["tested"])
        untested_count = len(data["untested"])
        total = tested_count + untested_count
        ratio = (tested_count / total * 100) if total > 0 else 100.0

        total_tested += tested_count
        total_untested += untested_count

        print(f"\n[Crate: {crate}]")
        print(f"  Coverage: {tested_count}/{total} functions directly referenced in tests ({ratio:.1f}%)")
        if data["untested"]:
            print("  Functions with no direct test invocation reference:")
            for filename, fn_name in data["untested"][:10]:
                print(f"    - {filename}: {fn_name}")
            if len(data["untested"]) > 10:
                print(f"    ... and {len(data['untested']) - 10} more.")

    overall_total = total_tested + total_untested
    overall_ratio = (total_tested / overall_total * 100) if overall_total > 0 else 100.0
    print("\n" + "=" * 80)
    print(f"OVERALL SUMMARY: {total_tested}/{overall_total} functions tested ({overall_ratio:.1f}%)")
    print("=" * 80)

if __name__ == "__main__":
    main()
