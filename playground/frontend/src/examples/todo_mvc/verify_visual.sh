#!/bin/bash
# verify_visual.sh - Verify todo_mvc matches reference image
#
# EXIT CODES:
#   0 = PASS (SSIM >= threshold)
#   1 = FAIL (SSIM < threshold or error)
#
# USAGE:
#   ./verify_visual.sh [--threshold 0.95] [--output /tmp/diff.png]
#
# PREREQUISITES:
#   - Boon playground running (cd playground && makers mzoon start)
#   - WebSocket server running (boon-tools server start)
#   - Browser with extension connected to playground
#   - todo_mvc example loaded

set -e

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REFERENCE="$SCRIPT_DIR/reference_700x700.png"
OUTPUT_DIR="/tmp/boon-visual-tests"
OUTPUT="$OUTPUT_DIR/todo_mvc_screenshot.png"
DIFF="$OUTPUT_DIR/todo_mvc_diff.png"
SSIM_THRESHOLD="0.95"

# Find boon-tools binary
BOON_ROOT="$SCRIPT_DIR/../../../../.."
BOON_TOOLS="$BOON_ROOT/target/release/boon-tools"

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --threshold)
            SSIM_THRESHOLD="$2"
            shift 2
            ;;
        --output)
            DIFF="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--threshold 0.95] [--output /tmp/diff.png]"
            exit 1
            ;;
    esac
done

# Create output directory
mkdir -p "$OUTPUT_DIR"

# Check prerequisites
if [[ ! -f "$BOON_TOOLS" ]]; then
    echo "ERROR: boon-tools not found at $BOON_TOOLS"
    echo "       Run: cd tools && cargo build --release"
    exit 1
fi

if [[ ! -f "$REFERENCE" ]]; then
    echo "ERROR: Reference image not found: $REFERENCE"
    exit 1
fi

echo "=== TodoMVC Visual Verification ==="
echo "Reference: $REFERENCE"
echo "Threshold: $SSIM_THRESHOLD"
echo ""

# Step 1: Set preview size to 700x700
echo "[1/4] Setting preview size to 700x700..."
"$BOON_TOOLS" exec set-preview-size 700 700 --port 9224 || {
    echo "ERROR: Failed to set preview size"
    echo "       Make sure the browser extension is connected"
    exit 1
}

# Step 2: Select todo_mvc example
echo "[2/4] Selecting todo_mvc example..."
"$BOON_TOOLS" exec select todo_mvc --port 9224 || {
    echo "ERROR: Failed to select todo_mvc example"
    exit 1
}

# Step 3: Wait for render and take screenshot of preview pane
echo "[3/4] Taking screenshot of preview pane..."
sleep 1  # Allow time for render
"$BOON_TOOLS" exec screenshot-preview --output "$OUTPUT" --port 9224 || {
    echo "ERROR: Failed to take screenshot"
    exit 1
}

echo "      Screenshot saved: $OUTPUT"

# Step 4: Compare images
echo "[4/4] Comparing images..."
"$BOON_TOOLS" pixel-diff \
    --reference "$REFERENCE" \
    --current "$OUTPUT" \
    --output "$DIFF" \
    --threshold "$SSIM_THRESHOLD"

RESULT=$?

if [[ $RESULT -eq 0 ]]; then
    echo ""
    echo "=== PASS ==="
    echo "TodoMVC visual verification passed!"
else
    echo ""
    echo "=== FAIL ==="
    echo "TodoMVC visual verification failed!"
    echo "Diff image saved: $DIFF"
fi

exit $RESULT
