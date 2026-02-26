#!/usr/bin/env bash

set -euo pipefail

TOOLS_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BT="$TOOLS_DIR/../target/release/boon-tools"
PORT=9224

PASS=0
FAIL=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --port) PORT="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

if [[ ! -f "$BT" ]]; then
    echo "FATAL: boon-tools binary not found at $BT"
    echo "Build it: cd tools && cargo build --release --target-dir ../target"
    exit 1
fi

bt() {
    "$BT" exec --port "$PORT" "$@"
}

reset_project_cache() {
    bt eval-js "(() => {
        localStorage.removeItem('boon-playground-project-files');
        localStorage.removeItem('boon-playground-old-source-code');
        localStorage.removeItem('boon-playground-current-file');
        localStorage.removeItem('boon-playground-span-id-pairs');
        return 'ok';
    })()" >/dev/null
    bt refresh >/dev/null
}

ok() {
    PASS=$((PASS + 1))
    echo "  PASS: $1"
}

fail() {
    FAIL=$((FAIL + 1))
    echo "  FAIL: $1"
}

assert_contains() {
    local text="$1"
    local needle="$2"
    local label="$3"
    if echo "$text" | grep -qF "$needle"; then
        ok "$label"
    else
        fail "$label (missing '$needle')"
    fi
}

assert_focused_input() {
    local index="$1"
    local label="$2"
    local focused
    focused="$(bt get-focused-element 2>/dev/null || true)"
    if echo "$focused" | grep -qF "input_index=$index"; then
        ok "$label"
    else
        fail "$label (got: $focused)"
    fi
}

test_engine() {
    local engine="$1"
    local edit_suffix="$2"

    echo ""
    echo "=== $engine: Edit title + persist ==="

    bt clear-states >/dev/null
    bt select todo_mvc >/dev/null
    bt set-engine "$engine" >/dev/null
    bt run >/dev/null
    sleep 2

    bt dblclick-text "Clean room" >/dev/null
    sleep 1
    assert_focused_input 1 "$engine enters edit mode"

    bt type-text "$edit_suffix" >/dev/null
    sleep 1
    assert_focused_input 1 "$engine keeps edit mode while typing"

    bt press-key Enter >/dev/null
    sleep 1

    local preview
    preview="$(bt preview 2>/dev/null || true)"
    assert_contains "$preview" "Clean room EDITED" "$engine saves edited title on Enter"

    bt run >/dev/null
    sleep 2
    preview="$(bt preview 2>/dev/null || true)"
    assert_contains "$preview" "Clean room EDITED" "$engine persists edited title after rerun"
}

echo "=== TodoMVC Cross-Engine Edit Title Test ==="

reset_project_cache

test_engine "DD" " EDITED"
test_engine "Actors" " EDITED"

echo ""
echo "Results: PASS=$PASS FAIL=$FAIL"

if [[ "$FAIL" -ne 0 ]]; then
    exit 1
fi
