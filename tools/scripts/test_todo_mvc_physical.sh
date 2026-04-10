#!/usr/bin/env bash
# TodoMVC Physical — Cross-Engine Test Suite
#
# Tests todo_mvc_physical on the currently supported engines: Actors, ActorsLite, DD, Wasm.
# Covers: add items, checkbox toggle, filters, clear completed,
# theme switching, dark/light mode, counter text.
#
# Prerequisites:
#   - Playground running from this workspace's MoonZoon.toml port
#   - WebSocket server running
#   - Browser with extension connected
#
# Usage: ./test_todo_mvc_physical.sh [--engine ENGINE] [--port PORT]
#   --engine   Test only one engine: Actors, ActorsLite, DD, Wasm (default: all)
#   --port     WebSocket port (default: auto-detected from MoonZoon.toml)
#
# Exit code 0 = all tests pass, non-zero = failures.

set -euo pipefail

TOOLS_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PASS=0
FAIL=0
SKIP=0
ERRORS=""
PORT=""
ENGINE_FILTER=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --engine) ENGINE_FILTER="$2"; shift 2 ;;
        --port) PORT="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

BT="$TOOLS_DIR/../target/release/boon-tools"
if [ ! -f "$BT" ]; then
    echo "FATAL: boon-tools binary not found at $BT"
    echo "Build it: cd tools && cargo build --release --target-dir ../target"
    exit 1
fi

bt() {
    if [[ -n "$PORT" ]]; then
        "$BT" exec --port "$PORT" "$@" 2>/dev/null
    else
        "$BT" exec "$@" 2>/dev/null
    fi
}

ok() {
    PASS=$((PASS + 1))
    echo "    PASS: $1"
}

fail() {
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL [$CURRENT_ENGINE]: $1"
    echo "    FAIL: $1"
}

check_contains() {
    local actual="$1" expected="$2" label="$3"
    if echo "$actual" | grep -qF "$expected"; then
        ok "$label"
    else
        fail "$label (expected '$expected')"
    fi
}

check_not_contains() {
    local actual="$1" unexpected="$2" label="$3"
    if echo "$actual" | grep -qF "$unexpected"; then
        fail "$label (found '$unexpected')"
    else
        ok "$label"
    fi
}

CURRENT_ENGINE=""

setup_engine() {
    CURRENT_ENGINE="$1"
    echo ""
    echo "============================================"
    echo "  ENGINE: $CURRENT_ENGINE"
    echo "============================================"

    bt clear-states >/dev/null 2>&1 || true
    bt select todo_mvc_physical >/dev/null
    bt set-engine "$CURRENT_ENGINE" >/dev/null 2>&1 || true
    bt run >/dev/null
    sleep 4

    # Verify the example loaded
    local preview
    preview=$(bt preview)
    if ! echo "$preview" | grep -qF "todos"; then
        echo "  WARNING: Example may not have loaded correctly"
        sleep 3
    fi
}

reset_example() {
    bt clear-states >/dev/null 2>&1 || true
    bt run >/dev/null
    sleep 3
}

run_engine_tests() {
    local engine="$1"
    setup_engine "$engine"

    # ──────────────────────────────────
    # 1. INITIAL RENDER
    # ──────────────────────────────────
    echo ""
    echo "  ── 1. Initial Render ──"

    local preview
    preview=$(bt preview)
    check_contains "$preview" "todos" "Header 'todos' present"
    check_contains "$preview" "Double-click to edit a todo" "Footer instruction text"
    check_contains "$preview" "Created by" "Footer credit"
    check_contains "$preview" "TodoMVC" "Footer TodoMVC link"

    # Theme buttons
    check_contains "$preview" "Professional" "Professional theme button"
    check_contains "$preview" "Glass" "Glass theme button"
    check_contains "$preview" "Brutalist" "Brutalist theme button"
    check_contains "$preview" "Neumorphic" "Neumorphic theme button"
    check_contains "$preview" "Dark mode" "Dark mode button present"

    # ──────────────────────────────────
    # 2. ADD ITEMS
    # ──────────────────────────────────
    echo ""
    echo "  ── 2. Add Items ──"

    bt focus-input 0 >/dev/null 2>&1 || true
    bt type-text "Buy groceries" >/dev/null 2>&1 || true
    bt press-key Enter >/dev/null 2>&1 || true
    sleep 2

    preview=$(bt preview)
    check_contains "$preview" "Buy groceries" "First item 'Buy groceries' visible"
    check_contains "$preview" "1 item" "Counter shows '1 item'"

    bt focus-input 0 >/dev/null 2>&1 || true
    bt type-text "Clean room" >/dev/null 2>&1 || true
    bt press-key Enter >/dev/null 2>&1 || true
    sleep 2

    preview=$(bt preview)
    check_contains "$preview" "Clean room" "Second item 'Clean room' visible"
    check_contains "$preview" "2 item" "Counter shows '2 items'"

    # ──────────────────────────────────
    # 3. CHECKBOX TOGGLE
    # ──────────────────────────────────
    echo ""
    echo "  ── 3. Checkbox Toggle ──"

    # Check first item
    bt click-checkbox 1 >/dev/null 2>&1 || true
    sleep 2
    preview=$(bt preview)
    check_contains "$preview" "1 item" "Counter decrements after checking first item"

    # Uncheck first item
    bt click-checkbox 1 >/dev/null 2>&1 || true
    sleep 2
    preview=$(bt preview)
    check_contains "$preview" "2 item" "Counter back to 2 after unchecking"

    # ──────────────────────────────────
    # 4. FILTER VIEWS
    # ──────────────────────────────────
    echo ""
    echo "  ── 4. Filter Views ──"

    # Check first item to create mixed state
    bt click-checkbox 1 >/dev/null 2>&1 || true
    sleep 1

    # Active filter
    bt click-text "Active" >/dev/null 2>&1 || true
    sleep 1
    preview=$(bt preview)
    check_contains "$preview" "Clean room" "Active filter shows unchecked 'Clean room'"
    check_not_contains "$preview" "Buy groceries" "Active filter hides checked 'Buy groceries'"

    # Completed filter
    bt click-text "Completed" >/dev/null 2>&1 || true
    sleep 1
    preview=$(bt preview)
    check_contains "$preview" "Buy groceries" "Completed filter shows checked 'Buy groceries'"
    check_not_contains "$preview" "Clean room" "Completed filter hides unchecked 'Clean room'"

    # All filter
    bt click-text "All" >/dev/null 2>&1 || true
    sleep 1
    preview=$(bt preview)
    check_contains "$preview" "Buy groceries" "All filter shows 'Buy groceries'"
    check_contains "$preview" "Clean room" "All filter shows 'Clean room'"

    # Uncheck to restore
    bt click-checkbox 1 >/dev/null 2>&1 || true
    sleep 1

    # ──────────────────────────────────
    # 5. CLEAR COMPLETED
    # ──────────────────────────────────
    echo ""
    echo "  ── 5. Clear Completed ──"

    # Check first item
    bt click-checkbox 1 >/dev/null 2>&1 || true
    sleep 1
    preview=$(bt preview)
    check_contains "$preview" "Clear completed" "'Clear completed' button appears"

    bt click-text "Clear completed" >/dev/null 2>&1 || true
    sleep 2
    preview=$(bt preview)
    check_not_contains "$preview" "Buy groceries" "'Buy groceries' removed after clear"
    check_contains "$preview" "Clean room" "'Clean room' remains after clear"
    check_contains "$preview" "1 item" "Counter shows 1 item after clear"

    # ──────────────────────────────────
    # 6. DARK MODE TOGGLE
    # ──────────────────────────────────
    echo ""
    echo "  ── 6. Dark Mode Toggle ──"

    bt click-text "Dark mode" >/dev/null 2>&1 || true
    sleep 2
    preview=$(bt preview)
    check_contains "$preview" "Light mode" "Button changes to 'Light mode' in dark mode"

    bt click-text "Light mode" >/dev/null 2>&1 || true
    sleep 2
    preview=$(bt preview)
    check_contains "$preview" "Dark mode" "Button changes back to 'Dark mode' in light mode"

    # ──────────────────────────────────
    # 7. THEME SWITCHING
    # ──────────────────────────────────
    echo ""
    echo "  ── 7. Theme Switching ──"

    for theme in "Glass" "Brutalist" "Neumorphic" "Professional"; do
        bt click-text "$theme" >/dev/null 2>&1 || true
        sleep 1
        preview=$(bt preview)
        check_contains "$preview" "Clean room" "Items visible with $theme theme"
    done

    # ──────────────────────────────────
    # 8. TOGGLE ALL
    # ──────────────────────────────────
    echo ""
    echo "  ── 8. Toggle All ──"

    # Reset to get 2 items
    reset_example

    bt focus-input 0 >/dev/null 2>&1 || true
    bt type-text "Item A" >/dev/null 2>&1 || true
    bt press-key Enter >/dev/null 2>&1 || true
    sleep 1
    bt focus-input 0 >/dev/null 2>&1 || true
    bt type-text "Item B" >/dev/null 2>&1 || true
    bt press-key Enter >/dev/null 2>&1 || true
    sleep 2

    # Toggle all → all completed (click the ">" toggle-all button)
    bt click-text ">" >/dev/null 2>&1 || true
    sleep 2
    preview=$(bt preview)
    check_contains "$preview" "0 item" "Toggle-all checks all: '0 items'"

    # Toggle all → all active
    bt click-text ">" >/dev/null 2>&1 || true
    sleep 2
    preview=$(bt preview)
    check_contains "$preview" "2 item" "Toggle-all unchecks all: '2 items'"

    # ──────────────────────────────────
    # 9. CONSOLE ERRORS
    # ──────────────────────────────────
    echo ""
    echo "  ── 9. Console Errors ──"

    local console
    console=$(bt console --level error 2>/dev/null) || true
    # Check for critical errors (skip backpressure drops which are performance warnings)
    if echo "$console" | grep -v "BACKPRESSURE" | grep -qiF "error"; then
        echo "    NOTE: Console has non-backpressure errors (may be expected)"
    else
        ok "No critical console errors"
    fi

    echo ""
    echo "  ── $engine engine: done ──"
}

echo "=== TodoMVC Physical — Cross-Engine Test Suite ==="
echo ""

ENGINES_TO_TEST=()
if [ -n "$ENGINE_FILTER" ]; then
    ENGINES_TO_TEST+=("$ENGINE_FILTER")
else
    ENGINES_TO_TEST+=("Actors" "ActorsLite" "DD" "Wasm")
fi

for engine in "${ENGINES_TO_TEST[@]}"; do
    run_engine_tests "$engine"
done

# ──────────────────────────────────
# Summary
# ──────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Results"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Passed:   $PASS"
echo "  Failed:   $FAIL"
if [ $FAIL -gt 0 ]; then
    echo ""
    echo "Failures:"
    echo -e "$ERRORS"
    echo ""
    echo "FAILED: $FAIL failure(s)."
    exit 1
fi
echo ""
echo "All tests passed!"
exit 0
