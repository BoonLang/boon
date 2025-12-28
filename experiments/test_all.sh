#!/bin/bash
# Test script to verify PLAN.md implementation completeness
# Exit on first failure
set -e

echo "=== Boon v3 Engine Prototype Completeness Check ==="
echo ""

cd "$(dirname "$0")"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

pass() { echo -e "${GREEN}✓ $1${NC}"; }
fail() { echo -e "${RED}✗ $1${NC}"; exit 1; }
warn() { echo -e "${YELLOW}⚠ $1${NC}"; }

echo "--- Phase 1: Directory Structure ---"

# Check path_a structure
[ -f path_a/Cargo.toml ] || fail "path_a/Cargo.toml missing"
[ -f path_a/src/lib.rs ] || fail "path_a/src/lib.rs missing"
[ -f path_a/src/arena.rs ] || fail "path_a/src/arena.rs missing"
[ -f path_a/src/node.rs ] || fail "path_a/src/node.rs missing"
[ -f path_a/src/template.rs ] || fail "path_a/src/template.rs missing"
[ -f path_a/src/evaluator.rs ] || fail "path_a/src/evaluator.rs missing"
[ -f path_a/src/engine.rs ] || fail "path_a/src/engine.rs missing"
[ -f path_a/src/value.rs ] || fail "path_a/src/value.rs missing"
[ -f path_a/src/ledger.rs ] || fail "path_a/src/ledger.rs missing"
pass "path_a/ structure complete"

# Check path_b structure
[ -f path_b/Cargo.toml ] || fail "path_b/Cargo.toml missing"
[ -f path_b/src/lib.rs ] || fail "path_b/src/lib.rs missing"
[ -f path_b/src/tick.rs ] || fail "path_b/src/tick.rs missing"
[ -f path_b/src/slot.rs ] || fail "path_b/src/slot.rs missing"
[ -f path_b/src/scope.rs ] || fail "path_b/src/scope.rs missing"
[ -f path_b/src/cell.rs ] || fail "path_b/src/cell.rs missing"
[ -f path_b/src/cache.rs ] || fail "path_b/src/cache.rs missing"
[ -f path_b/src/runtime.rs ] || fail "path_b/src/runtime.rs missing"
[ -f path_b/src/evaluator.rs ] || fail "path_b/src/evaluator.rs missing"
[ -f path_b/src/value.rs ] || fail "path_b/src/value.rs missing"
[ -f path_b/src/diagnostics.rs ] || fail "path_b/src/diagnostics.rs missing"
pass "path_b/ structure complete"

# Check shared structure
[ -f shared/Cargo.toml ] || fail "shared/Cargo.toml missing"
[ -f shared/src/lib.rs ] || fail "shared/src/lib.rs missing"
[ -f shared/src/test_harness.rs ] || fail "shared/src/test_harness.rs missing"
[ -f shared/src/ast.rs ] || fail "shared/src/ast.rs missing"
[ -f shared/src/examples.rs ] || fail "shared/src/examples.rs missing"
pass "shared/ structure complete"

# Check bench structure
[ -f bench/Cargo.toml ] || fail "bench/Cargo.toml missing"
[ -f bench/src/main.rs ] || fail "bench/src/main.rs missing"
pass "bench/ structure complete"

# Check boon examples
[ -d shared/boon_examples ] || fail "shared/boon_examples/ missing"
[ -f shared/boon_examples/counter.bn ] || fail "shared/boon_examples/counter.bn missing"
[ -f shared/boon_examples/todo_mvc.bn ] || fail "shared/boon_examples/todo_mvc.bn missing"
pass "boon examples present"

# Check test files
[ -f path_a/tests/counter.rs ] || fail "path_a/tests/counter.rs missing"
[ -f path_a/tests/toggle_all.rs ] || fail "path_a/tests/toggle_all.rs missing"
[ -f path_b/tests/counter.rs ] || fail "path_b/tests/counter.rs missing"
[ -f path_b/tests/toggle_all.rs ] || fail "path_b/tests/toggle_all.rs missing"
[ -f path_b/tests/diagnostics.rs ] || fail "path_b/tests/diagnostics.rs missing"
pass "test files present"

echo ""
echo "--- Phase 2-3: Compilation ---"

cargo check --all 2>&1 || fail "cargo check failed"
pass "all crates compile"

echo ""
echo "--- Phase 2-3: Tests ---"

cargo test --all 2>&1 || fail "cargo test failed"
pass "all tests pass"

# Verify critical toggle_all test exists and runs
echo ""
echo "--- Critical Test: toggle_all_affects_new_items ---"

cargo test -p path_a toggle_all_affects_new_items 2>&1 || fail "path_a toggle_all test failed"
pass "path_a: toggle_all_affects_new_items passes"

cargo test -p path_b toggle_all_affects_new_items 2>&1 || fail "path_b toggle_all test failed"
pass "path_b: toggle_all_affects_new_items passes"

echo ""
echo "--- Phase 4: Benchmarks ---"

# Just verify benchmarks compile (full run is slow)
cargo build -p bench --release 2>&1 || fail "bench crate failed to build"
pass "benchmarks compile"

# Optional: run benchmarks if --bench flag provided
if [ "$1" = "--bench" ]; then
    echo ""
    echo "Running benchmarks (this may take a while)..."
    cd bench && cargo bench
    pass "benchmarks complete"
fi

echo ""
echo "--- Phase 5: Decision Documentation ---"

if [ -f DECISION.md ] || [ -f FINDINGS.md ] || [ -f RESULTS.md ]; then
    pass "decision documentation exists"
else
    warn "No DECISION.md/FINDINGS.md/RESULTS.md found (Phase 5 incomplete)"
fi

echo ""
echo "=== Summary ==="
echo -e "${GREEN}All implementation checks passed!${NC}"
echo ""
echo "To run full benchmarks: $0 --bench"
