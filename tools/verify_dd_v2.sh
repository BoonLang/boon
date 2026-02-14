#!/bin/bash
# DD v2 Engine Verification Script
# Run from anywhere: ~/repos/boon-dd-v2/tools/verify_dd_v2.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DD="$REPO_ROOT/crates/boon/src/platform/browser/engine_dd"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[1;34m'
NC='\033[0m'

PASS=true
CHECKS_PASSED=0
CHECKS_FAILED=0
CHECKS_WARNED=0

pass() { echo -e "  ${GREEN}PASS${NC} $1"; ((CHECKS_PASSED++)); }
fail() { echo -e "  ${RED}FAIL${NC} $1"; PASS=false; ((CHECKS_FAILED++)); }
warn() { echo -e "  ${YELLOW}WARN${NC} $1"; ((CHECKS_WARNED++)); }

echo -e "${BLUE}═══════════════════════════════════════════${NC}"
echo -e "${BLUE}  DD v2 Engine Verification${NC}"
echo -e "${BLUE}═══════════════════════════════════════════${NC}"
echo ""

# ─── CHECK 1: Anti-Cheat (core/ module purity) ───
echo -e "${BLUE}[1/5] Anti-Cheat: core/ module purity${NC}"
CORE_DIR="$ENGINE_DD/core"
if [ -d "$CORE_DIR" ]; then
    for pattern in 'RefCell' 'Mutable<' 'thread_local' 'use zoon' 'use web_sys' \
                   'Arc<Mutex' 'RwLock' '.lock()' '.borrow()' '.borrow_mut()'; do
        count=$(grep -r "$pattern" "$CORE_DIR" --include='*.rs' 2>/dev/null | \
                grep -v '// ALLOWED:' | grep -v '//!' | wc -l)
        if [ "$count" -gt 0 ]; then
            fail "Found '$pattern' in core/ ($count occurrences)"
            grep -rn "$pattern" "$CORE_DIR" --include='*.rs' | grep -v '// ALLOWED:' | head -3
        else
            pass "No '$pattern' in core/"
        fi
    done
    # Check no .get() on CollectionHandle
    count=$(grep -r '\.get()' "$CORE_DIR" --include='*.rs' 2>/dev/null | \
            grep -v '// ALLOWED:' | grep -v 'BTreeMap' | grep -v 'HashMap' | grep -v 'IndexMap' | wc -l)
    if [ "$count" -gt 0 ]; then
        warn "Found .get() in core/ ($count) - verify these are map lookups, not collection reads"
    else
        pass "No suspicious .get() in core/"
    fi
else
    warn "core/ directory not found yet (not implemented)"
fi
echo ""

# ─── CHECK 2: DD Operator Usage ───
echo -e "${BLUE}[2/5] DD Operator Usage: verify real DD operators${NC}"
if [ -d "$ENGINE_DD" ]; then
    for op in 'join' 'arrange' 'concat' 'filter' 'map(' 'flat_map' 'reduce' 'count'; do
        count=$(grep -r "\.$op" "$ENGINE_DD" --include='*.rs' 2>/dev/null | wc -l)
        if [ "$count" -gt 0 ]; then
            pass "DD operator .$op used ($count occurrences)"
        else
            warn "DD operator .$op not found yet"
        fi
    done
    # Anti-pattern: Vec<Value> with to_vec() (DD v1 failure mode)
    count=$(grep -r 'to_vec()' "$ENGINE_DD" --include='*.rs' 2>/dev/null | wc -l)
    if [ "$count" -gt 0 ]; then
        fail "Found to_vec() in engine_dd/ ($count) - DD v1 anti-pattern!"
    else
        pass "No to_vec() anti-pattern"
    fi
    count=$(grep -r 'Arc<Vec<Value>>' "$ENGINE_DD" --include='*.rs' 2>/dev/null | wc -l)
    if [ "$count" -gt 0 ]; then
        fail "Found Arc<Vec<Value>> in engine_dd/ ($count) - DD v1 anti-pattern!"
    else
        pass "No Arc<Vec<Value>> anti-pattern"
    fi
else
    warn "engine_dd/ directory not found yet (not implemented)"
fi
echo ""

# ─── CHECK 3: Compilation ───
echo -e "${BLUE}[3/5] Compilation: cargo check with engine-dd feature${NC}"
cd "$REPO_ROOT"
if cargo check -p boon --features engine-dd --message-format=short 2>&1 | tail -5; then
    pass "engine-dd compiles"
else
    fail "engine-dd compilation failed"
fi
echo ""

# ─── CHECK 4: Actor engine regression ───
echo -e "${BLUE}[4/5] Regression: actor engine still compiles${NC}"
if cargo check -p boon --features engine-actors --message-format=short 2>&1 | tail -5; then
    pass "engine-actors compiles"
else
    fail "engine-actors compilation failed"
fi
echo ""

# ─── CHECK 5: Construct coverage ───
echo -e "${BLUE}[5/5] Construct Coverage: TodoMVC constructs supported${NC}"
CONSTRUCTS=(
    "LATEST:hold_latest"
    "HOLD:hold_state"
    "THEN:then"
    "WHEN:when\|flat_map"
    "WHILE:while_reactive\|join"
    "LIST:list\|ListKey"
    "TEXT:text_interpolation\|join"
    "BLOCK:block\|scope"
    "FUNCTION:function\|inline"
    "PASS/PASSED:pass\|passed\|context"
    "SKIP:skip\|filter"
    "LINK:link\|Input"
    "List/retain+WHILE:join.*retain\|retain.*join"
    "Router:router\|route"
    "Spread:spread\|\.\.\."
    "element.hovered:hovered\|hover"
    "NoElement:no_element\|NoElement"
)
if [ -d "$ENGINE_DD" ]; then
    for construct_pair in "${CONSTRUCTS[@]}"; do
        construct="${construct_pair%%:*}"
        patterns="${construct_pair##*:}"
        found=false
        for pat in $(echo "$patterns" | tr '|' ' '); do
            if grep -rqi "$pat" "$ENGINE_DD" --include='*.rs' 2>/dev/null; then
                found=true; break
            fi
        done
        if $found; then
            pass "$construct"
        else
            warn "$construct not implemented yet"
        fi
    done
else
    warn "engine_dd/ not found - skipping construct coverage"
fi
echo ""

# ─── SUMMARY ───
echo -e "${BLUE}═══════════════════════════════════════════${NC}"
echo -e "  Passed: ${GREEN}$CHECKS_PASSED${NC}  Failed: ${RED}$CHECKS_FAILED${NC}  Warnings: ${YELLOW}$CHECKS_WARNED${NC}"
if [ "$PASS" = true ]; then
    echo -e "  ${GREEN}OVERALL: PASS${NC}"
    if [ "$CHECKS_WARNED" -gt 0 ]; then
        echo -e "  ${YELLOW}(warnings indicate incomplete implementation - continue working)${NC}"
    fi
    echo -e "${BLUE}═══════════════════════════════════════════${NC}"
    exit 0
else
    echo -e "  ${RED}OVERALL: FAIL - fix issues above before continuing${NC}"
    echo -e "${BLUE}═══════════════════════════════════════════${NC}"
    exit 1
fi
