#!/usr/bin/env bash
# verify_7guis_complete.sh — Verify 7GUIs examples across all engines
#
# Runs behavioral tests (via boon-tools test-examples) for each engine,
# plus static checks for persistence toggle, actor instrumentation, etc.
#
# Prerequisites:
#   - Playground running (cd playground && makers mzoon start)
#   - WS server running (cd tools && cargo run --release -- server start --watch ./extension)
#   - Chrome extension loaded and connected
#
# Usage: ./verify_7guis_complete.sh [--static-only]
#   --static-only  Skip live browser tests, only run static checks

set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TOOLS_DIR="$(dirname "$SCRIPT_DIR")"
REPO_DIR="$(dirname "$TOOLS_DIR")"
BT="$REPO_DIR/target/release/boon-tools"
EXAMPLES_DIR="$REPO_DIR/playground/frontend/src/examples"
PASS=0; FAIL=0; SKIP=0
STATIC_ONLY=false

for arg in "$@"; do
    case "$arg" in
        --static-only) STATIC_ONLY=true ;;
    esac
done

ok()   { PASS=$((PASS+1)); echo "  [PASS] $1"; }
fail() { FAIL=$((FAIL+1)); echo "  [FAIL] $1"; }
skip() { SKIP=$((SKIP+1)); echo "  [SKIP] $1"; }

EXAMPLES="temperature_converter flight_booker timer crud circle_drawer cells"
# Run the current three public engines. Actual per-example support is still
# encoded in each `.expected` file via `skip_engines`.
ENGINES="Actors DD Wasm"

# ── Section 1: Build boon-tools ──
echo "=== 7GUIs Verification ==="
echo ""
echo "1. Build check"
if [ -f "$BT" ]; then
    ok "boon-tools binary exists"
else
    echo "  Building boon-tools..."
    if (cd "$TOOLS_DIR" && cargo build --release --target-dir "$REPO_DIR/target" 2>/dev/null); then
        ok "boon-tools built successfully"
    else
        fail "boon-tools build failed"
        echo ""
        echo "Cannot run tests without boon-tools. Aborting."
        exit 1
    fi
fi

# ── Section 2: Static artifact checks ──
echo ""
echo "2. Test files and references"

for ex in $EXAMPLES; do
    EXPECTED="$EXAMPLES_DIR/$ex/$ex.expected"
    if [ -f "$EXPECTED" ]; then
        SEQ_COUNT=$(grep -c '\[\[sequence\]\]' "$EXPECTED" || echo 0)
        if [ "$SEQ_COUNT" -gt 0 ]; then
            ok "$ex.expected ($SEQ_COUNT sequences)"
        else
            fail "$ex.expected has no [[sequence]] sections"
        fi
    else
        fail "$ex.expected missing"
    fi
done

# Reference images
for ex in $EXAMPLES; do
    REF="$EXAMPLES_DIR/$ex/reference_700x700_(1400x1400).png"
    if [ -f "$REF" ]; then
        SIZE=$(stat -c%s "$REF" 2>/dev/null || stat -f%z "$REF" 2>/dev/null || echo 0)
        if [ "$SIZE" -gt 1000 ]; then
            ok "$ex reference image (${SIZE} bytes)"
        else
            fail "$ex reference image too small (${SIZE} bytes)"
        fi
    else
        skip "$ex reference image not found"
    fi
done

# Reference metadata
METADATA="$EXAMPLES_DIR/reference_metadata.json"
if [ -f "$METADATA" ]; then
    ok "reference_metadata.json exists"
else
    skip "reference_metadata.json not found"
fi

# ── Section 3: Code integrity checks ──
echo ""
echo "3. Code integrity"

# Persistence toggle
if grep -q "persistence_enabled" "$REPO_DIR/playground/frontend/src/main.rs"; then
    ok "Persistence toggle in frontend"
else
    fail "Persistence toggle missing from frontend"
fi

# Actor count instrumentation
ENGINE_RS="$REPO_DIR/crates/boon/src/platform/browser/engine_actors/engine.rs"
if grep -q "LIVE_ACTOR_COUNT" "$ENGINE_RS"; then
    ok "LIVE_ACTOR_COUNT instrumentation"
else
    fail "LIVE_ACTOR_COUNT missing"
fi

# Circle Drawer undo
CD_BN="$EXAMPLES_DIR/circle_drawer/circle_drawer.bn"
if grep -q "List/remove_last" "$CD_BN"; then
    ok "Circle Drawer has undo (List/remove_last)"
else
    fail "Circle Drawer missing undo logic"
fi

# CRUD filter
CRUD_BN="$EXAMPLES_DIR/crud/crud.bn"
if grep -q "Text/starts_with" "$CRUD_BN"; then
    ok "CRUD filter uses Text/starts_with"
else
    fail "CRUD filter missing Text/starts_with"
fi

# Milestone support examples
COUNTER_EXPECTED="$EXAMPLES_DIR/counter/counter.expected"
if [ -f "$COUNTER_EXPECTED" ]; then
    ok "Counter expected file exists"
else
    fail "Counter expected file missing"
fi

TODO_EXPECTED="$EXAMPLES_DIR/todo_mvc/todo_mvc.expected"
if [ -f "$TODO_EXPECTED" ]; then
    ok "TodoMVC expected file exists"
else
    fail "TodoMVC expected file missing"
fi

TODO_PHYSICAL_RUN="$EXAMPLES_DIR/todo_mvc_physical/RUN.bn"
if grep -q "Scene/new(" "$TODO_PHYSICAL_RUN"; then
    ok "TodoMVC Physical uses Scene/new"
else
    fail "TodoMVC Physical missing Scene/new"
fi

if grep -q "Scene/Element/" "$TODO_PHYSICAL_RUN"; then
    ok "TodoMVC Physical uses Scene elements"
else
    fail "TodoMVC Physical missing Scene elements"
fi

if grep -q "lights:" "$TODO_PHYSICAL_RUN"; then
    ok "TodoMVC Physical passes lights to Scene/new"
else
    fail "TodoMVC Physical missing Scene lights"
fi

if grep -q "geometry:" "$TODO_PHYSICAL_RUN"; then
    ok "TodoMVC Physical passes geometry to Scene/new"
else
    fail "TodoMVC Physical missing Scene geometry"
fi

TODO_PHYSICAL_THEME_DIR="$EXAMPLES_DIR/todo_mvc_physical/Theme"
for theme_file in \
    "$TODO_PHYSICAL_THEME_DIR/Professional.bn" \
    "$TODO_PHYSICAL_THEME_DIR/Glassmorphism.bn" \
    "$TODO_PHYSICAL_THEME_DIR/Neobrutalism.bn" \
    "$TODO_PHYSICAL_THEME_DIR/Neumorphism.bn"
do
    theme_name="$(basename "$theme_file" .bn)"

    if grep -q "Light/directional(" "$theme_file"; then
        ok "TodoMVC Physical $theme_name defines directional light"
    else
        fail "TodoMVC Physical $theme_name missing directional light"
    fi

    if grep -q "Light/ambient(" "$theme_file"; then
        ok "TodoMVC Physical $theme_name defines ambient light"
    else
        fail "TodoMVC Physical $theme_name missing ambient light"
    fi

    if grep -q "Light/spot(" "$theme_file"; then
        ok "TodoMVC Physical $theme_name defines spot light"
    else
        fail "TodoMVC Physical $theme_name missing spot light"
    fi

    if grep -q "bevel_angle:" "$theme_file"; then
        ok "TodoMVC Physical $theme_name defines bevel angle"
    else
        fail "TodoMVC Physical $theme_name missing bevel angle"
    fi
done

if grep -q '^skip_engines' "$EXAMPLES_DIR/cells/cells.expected"; then
    fail "Cells expected file still skips one or more engines"
else
    ok "Cells expected file no longer skips any engines"
fi

if grep -Fq 'skip_engines = ["DD", "Wasm"]' "$EXAMPLES_DIR/flight_booker/flight_booker.expected"; then
    ok "Flight Booker expected skip list matches current DD/Wasm status"
else
    fail "Flight Booker expected skip list drifted"
fi

if grep -Fq 'skip_engines = ["DD", "Wasm"]' "$EXAMPLES_DIR/crud/crud.expected"; then
    ok "CRUD expected skip list matches current DD/Wasm status"
else
    fail "CRUD expected skip list drifted"
fi

if grep -Fq 'skip_engines = ["Wasm"]' "$EXAMPLES_DIR/timer/timer.expected"; then
    ok "Timer expected skip list matches current Wasm status"
else
    fail "Timer expected skip list drifted"
fi

# Cells grid size (official 7GUIs target: 26 columns x 100 rows)
CELLS_BN="$EXAMPLES_DIR/cells/cells.bn"
if grep -q "List/range(from: 1, to: 100)" "$CELLS_BN"; then
    ok "Cells uses 100 rows"
elif grep -q "List/range(from: 1, to: 30)" "$CELLS_BN"; then
    fail "Cells still uses 30 rows (official target is 100)"
elif grep -q "List/range(from: 1, to: 10)" "$CELLS_BN"; then
    fail "Cells still uses 10 rows (official target is 100)"
else
    skip "Cells row count unclear"
fi

if grep -q "List/range(from: 1, to: 26)" "$CELLS_BN"; then
    ok "Cells uses 26 columns"
elif grep -q "List/range(from: 1, to: 10)" "$CELLS_BN"; then
    fail "Cells still uses 10 columns (milestone target is 26)"
else
    skip "Cells column count unclear"
fi

# ── Section 4: Live browser tests ──
if [ "$STATIC_ONLY" = true ]; then
    echo ""
    echo "4. Live browser tests (SKIPPED — --static-only)"
    for engine in $ENGINES; do
        for ex in $EXAMPLES; do
            skip "$engine/$ex (static-only mode)"
        done
    done
else
    echo ""
    echo "4. Live browser tests"

    # Refresh page to clear any poisoned state from previous testing
    "$BT" exec refresh >/dev/null 2>&1 || true
    sleep 2

    # Verify boon-tools can connect
    if ! "$BT" exec status >/dev/null 2>&1; then
        echo "  WARNING: Cannot connect to browser. Skipping live tests."
        echo "  Ensure playground + WS server + extension are running."
        for engine in $ENGINES; do
            for ex in $EXAMPLES; do
                skip "$engine/$ex (no connection)"
            done
        done
    else
        ok "boon-tools connected to browser"

        for engine in $ENGINES; do
            echo ""
            echo "  --- $engine engine ---"
            for ex in $EXAMPLES; do
                OUTPUT=$("$BT" exec test-examples --engine "$engine" --filter "$ex" --no-launch 2>&1) || true

                if echo "$OUTPUT" | grep -q "\[SKIP\]"; then
                    skip "$engine/$ex"
                elif echo "$OUTPUT" | grep -q "\[FAIL\]"; then
                    fail "$engine/$ex"
                    echo "$OUTPUT" | grep -E "\[FAIL\]|Expected:|Actual:" | head -5 | sed 's/^/         /'
                elif echo "$OUTPUT" | grep -q "\[PASS\]"; then
                    ok "$engine/$ex"
                else
                    fail "$engine/$ex"
                    echo "$OUTPUT" | tail -3 | sed 's/^/         /'
                fi
            done
        done
    fi
fi

# ── Summary ──
echo ""
echo "=== Results ==="
TOTAL=$((PASS + FAIL + SKIP))
echo "  Passed:  $PASS"
echo "  Failed:  $FAIL"
echo "  Skipped: $SKIP"
echo "  Total:   $TOTAL"
echo ""
if [ $FAIL -eq 0 ]; then
    echo "ALL CHECKS PASSED"
    exit 0
else
    echo "FAILURES DETECTED ($FAIL)"
    exit 1
fi
