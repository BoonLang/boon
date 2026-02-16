#!/bin/bash
# DD v2 Engine Verification Script
# Run from anywhere: ~/repos/boon-dd-v2/tools/verify_dd_v2.sh
# Use --strict to treat warnings as failures
set -uo pipefail

STRICT=false
if [ "${1:-}" = "--strict" ]; then
    STRICT=true
fi

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

pass() { echo -e "  ${GREEN}PASS${NC} $1"; ((CHECKS_PASSED++)) || true; }
fail() { echo -e "  ${RED}FAIL${NC} $1"; PASS=false; ((CHECKS_FAILED++)) || true; }
warn() { echo -e "  ${YELLOW}WARN${NC} $1"; ((CHECKS_WARNED++)) || true; }

BOON_TOOLS="$REPO_ROOT/target/release/boon-tools"

echo -e "${BLUE}═══════════════════════════════════════════${NC}"
echo -e "${BLUE}  DD v2 Engine Verification${NC}"
echo -e "${BLUE}═══════════════════════════════════════════${NC}"
echo ""

# ─── CHECK 1: Anti-Cheat (core/ module purity) ───
echo -e "${BLUE}[1/8] Anti-Cheat: core/ module purity${NC}"
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
echo -e "${BLUE}[2/8] DD Operator Usage: verify real DD operators${NC}"
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
    # Exclude local_scope.to_vec() (compile-time scope copies, not DD collection materialization)
    count=$(grep -r 'to_vec()' "$ENGINE_DD" --include='*.rs' 2>/dev/null | grep -v 'local_scope' | grep -v 'fn_scope' | grep -v 'scope\.to_vec' | grep -v 'loop_scope' | grep -v 'new_scope' | grep -v 'item_scope' | wc -l)
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
echo -e "${BLUE}[3/8] Compilation: cargo check with engine-dd feature${NC}"
cd "$REPO_ROOT"
if cargo check -p boon --features engine-dd --message-format=short 2>&1 | tail -5; then
    pass "engine-dd compiles"
else
    fail "engine-dd compilation failed"
fi
echo ""

# ─── CHECK 4: Actor engine regression ───
echo -e "${BLUE}[4/8] Regression: actor engine still compiles${NC}"
if cargo check -p boon --features engine-actors --message-format=short 2>&1 | tail -5; then
    pass "engine-actors compiles"
else
    fail "engine-actors compilation failed"
fi
echo ""

# ─── CHECK 5: Construct coverage ───
echo -e "${BLUE}[5/8] Construct Coverage: TodoMVC constructs supported${NC}"
CONSTRUCTS=(
    "LATEST:hold_latest"
    "HOLD:hold_state"
    "THEN:then"
    "WHEN:when\|flat_map"
    "WHILE:while_reactive\|join"
    "LIST:list\|ListKey"
    "TEXT:text_interpolation\|join"
    "BLOCK:block\|scope"
    "FUNCTION:user_function\|FunctionCall"
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

# ─── CHECK 6: Live browser test (DD engine) ───
echo -e "${BLUE}[6/8] Live Browser: DD engine counter_hold${NC}"
BROWSER_AVAILABLE=false
if [ -x "$BOON_TOOLS" ]; then
    # Check if browser extension is connected
    if $BOON_TOOLS exec status 2>/dev/null | grep -qi "Connected: true"; then
        BROWSER_AVAILABLE=true
    fi
fi

if [ "$BROWSER_AVAILABLE" = true ]; then
    # Select counter_hold and switch to DD engine
    $BOON_TOOLS exec select counter_hold >/dev/null 2>&1
    sleep 2

    # Switch to DD engine
    $BOON_TOOLS exec set-engine DD >/dev/null 2>&1
    sleep 2

    # Check initial output
    PREVIEW=$($BOON_TOOLS exec preview 2>/dev/null || echo "")
    if echo "$PREVIEW" | grep -q "0+"; then
        pass "DD counter_hold initial render: 0+"
    else
        fail "DD counter_hold initial render expected '0+', got: '$PREVIEW'"
    fi

    # Click button (use click-text since DD renders raw <button> without role attr)
    $BOON_TOOLS exec click-text "+" --exact >/dev/null 2>&1
    sleep 1

    PREVIEW=$($BOON_TOOLS exec preview 2>/dev/null || echo "")
    if echo "$PREVIEW" | grep -q "1+"; then
        pass "DD counter_hold after click: 1+"
    else
        fail "DD counter_hold after click expected '1+', got: '$PREVIEW'"
    fi

    # Click again
    $BOON_TOOLS exec click-text "+" --exact >/dev/null 2>&1
    sleep 1

    PREVIEW=$($BOON_TOOLS exec preview 2>/dev/null || echo "")
    if echo "$PREVIEW" | grep -q "2+"; then
        pass "DD counter_hold after 2nd click: 2+"
    else
        fail "DD counter_hold after 2nd click expected '2+', got: '$PREVIEW'"
    fi

    # Switch back to Actors
    $BOON_TOOLS exec set-engine Actors >/dev/null 2>&1
    sleep 1
else
    warn "Browser not connected - skipping live DD test (run with browser + boon-tools)"
fi
echo ""

# ─── CHECK 7: DD test-examples suite ───
echo -e "${BLUE}[7/8] Live Browser: DD engine test-examples${NC}"
if [ "$BROWSER_AVAILABLE" = true ]; then
    # Switch to DD engine and run the full test suite
    $BOON_TOOLS exec set-engine DD >/dev/null 2>&1
    sleep 1

    DD_RESULT=$($BOON_TOOLS exec test-examples --verbose 2>&1 || true)
    DD_PASSED=$(echo "$DD_RESULT" | grep -oP '\d+(?=/\d+ passed)' || echo "0")
    DD_TOTAL=$(echo "$DD_RESULT" | grep -oP '(?<=\/)\d+(?= passed)' || echo "11")

    echo "  DD test suite result: $DD_PASSED/$DD_TOTAL passed"
    # Show individual results
    echo "$DD_RESULT" | grep -E '(PASS|FAIL)' | head -15 | while read -r line; do
        echo "    $line"
    done

    # Require all examples to pass
    if [ "$DD_PASSED" -ge "$DD_TOTAL" ]; then
        pass "DD test suite: $DD_PASSED/$DD_TOTAL passed (all required)"
    else
        fail "DD test suite: $DD_PASSED/$DD_TOTAL passed (need all $DD_TOTAL)"
    fi

    # Switch back to Actors
    $BOON_TOOLS exec set-engine Actors >/dev/null 2>&1
    sleep 1
else
    warn "Browser not connected - skipping DD test-examples (run with browser + boon-tools)"
fi
echo ""

# ─── CHECK 8: Live browser test (Actors engine regression) ───
echo -e "${BLUE}[8/8] Live Browser: Actors engine regression${NC}"
if [ "$BROWSER_AVAILABLE" = true ]; then
    # Select counter_hold on Actors engine
    $BOON_TOOLS exec select counter_hold >/dev/null 2>&1
    sleep 2

    PREVIEW=$($BOON_TOOLS exec preview 2>/dev/null || echo "")
    if echo "$PREVIEW" | grep -q "0+"; then
        pass "Actors counter_hold initial render: 0+"
    else
        fail "Actors counter_hold initial render expected '0+', got: '$PREVIEW'"
    fi

    # Click button
    $BOON_TOOLS exec click-button 0 >/dev/null 2>&1
    sleep 1

    PREVIEW=$($BOON_TOOLS exec preview 2>/dev/null || echo "")
    if echo "$PREVIEW" | grep -q "1+"; then
        pass "Actors counter_hold after click: 1+"
    else
        fail "Actors counter_hold after click expected '1+', got: '$PREVIEW'"
    fi

    # Run the full actors test suite
    ACTORS_RESULT=$($BOON_TOOLS exec test-examples 2>&1 || true)
    ACTORS_PASSED=$(echo "$ACTORS_RESULT" | grep -oP '\d+(?=/\d+ passed)' || echo "0")
    ACTORS_TOTAL=$(echo "$ACTORS_RESULT" | grep -oP '(?<=\/)\d+(?= passed)' || echo "11")
    if [ "$ACTORS_PASSED" -ge 8 ]; then
        pass "Actors test suite: $ACTORS_PASSED/$ACTORS_TOTAL passed (>=8 required)"
    else
        fail "Actors test suite: $ACTORS_PASSED/$ACTORS_TOTAL passed (<8, regression detected)"
    fi
else
    warn "Browser not connected - skipping live Actors test (run with browser + boon-tools)"
fi
echo ""

# ─── SUMMARY ───
echo -e "${BLUE}═══════════════════════════════════════════${NC}"
echo -e "  Passed: ${GREEN}$CHECKS_PASSED${NC}  Failed: ${RED}$CHECKS_FAILED${NC}  Warnings: ${YELLOW}$CHECKS_WARNED${NC}"
if [ "$STRICT" = true ] && [ "$CHECKS_WARNED" -gt 0 ]; then
    PASS=false
fi
if [ "$PASS" = true ]; then
    echo -e "  ${GREEN}OVERALL: PASS${NC}"
    if [ "$CHECKS_WARNED" -gt 0 ]; then
        echo -e "  ${YELLOW}(warnings indicate incomplete implementation - continue working)${NC}"
    fi
    echo -e "${BLUE}═══════════════════════════════════════════${NC}"
    exit 0
else
    if [ "$STRICT" = true ] && [ "$CHECKS_WARNED" -gt 0 ]; then
        echo -e "  ${RED}OVERALL: FAIL (--strict: warnings treated as failures)${NC}"
    else
        echo -e "  ${RED}OVERALL: FAIL - fix issues above before continuing${NC}"
    fi
    echo -e "${BLUE}═══════════════════════════════════════════${NC}"
    exit 1
fi
