#!/bin/bash
# visual_debug.sh - Interactive visual convergence helper
#
# This script guides you through fixing visual differences between
# the current render and the reference image by:
# 1. Taking screenshots
# 2. Analyzing differences with spatial metrics
# 3. Identifying the worst regions
# 4. Providing CSS coordinates for debugging
# 5. Looping until convergence
#
# USAGE:
#   ./visual_debug.sh [--reference PATH] [--threshold 0.90]
#
# PREREQUISITES:
#   - Boon playground running (cd playground && makers mzoon start)
#   - WebSocket server running (boon-tools server start)
#   - Browser with extension connected to playground
#   - Example loaded in playground

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BOON_ROOT="$SCRIPT_DIR/../.."
BOON_TOOLS="$BOON_ROOT/target/release/boon-tools"
OUTPUT_DIR="/tmp/boon-visual-debug"
CURRENT="$OUTPUT_DIR/current.png"
DIFF="$OUTPUT_DIR/diff.png"
THRESHOLD="0.90"
REFERENCE=""
WS_PORT="9224"

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --reference|-r)
            REFERENCE="$2"
            shift 2
            ;;
        --threshold|-t)
            THRESHOLD="$2"
            shift 2
            ;;
        --port|-p)
            WS_PORT="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -r, --reference PATH   Path to reference image (required)"
            echo "  -t, --threshold VALUE  SSIM threshold (default: 0.90)"
            echo "  -p, --port PORT        WebSocket server port (default: 9224)"
            echo "  -h, --help             Show this help"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Validate reference
if [[ -z "$REFERENCE" ]]; then
    echo -e "${RED}ERROR: Reference image required${NC}"
    echo "Usage: $0 --reference PATH [--threshold 0.90]"
    exit 1
fi

if [[ ! -f "$REFERENCE" ]]; then
    echo -e "${RED}ERROR: Reference image not found: $REFERENCE${NC}"
    exit 1
fi

# Check boon-tools
if [[ ! -f "$BOON_TOOLS" ]]; then
    echo -e "${RED}ERROR: boon-tools not found at $BOON_TOOLS${NC}"
    echo "       Run: cd tools && cargo build --release"
    exit 1
fi

# Create output directory
mkdir -p "$OUTPUT_DIR"

echo -e "${BLUE}=== Visual Convergence Debug Tool ===${NC}"
echo ""
echo "Reference: $REFERENCE"
echo "Threshold: $THRESHOLD"
echo "Output:    $OUTPUT_DIR"
echo ""

iteration=0

while true; do
    iteration=$((iteration + 1))
    echo -e "${YELLOW}--- Iteration $iteration ---${NC}"
    echo ""

    # Take screenshot
    echo "Taking screenshot..."
    if ! "$BOON_TOOLS" exec --port "$WS_PORT" screenshot-preview --output "$CURRENT" --width 700 --height 700 --hidpi 2>/dev/null; then
        echo -e "${RED}ERROR: Failed to take screenshot${NC}"
        echo "Make sure the browser is connected and playground is running."
        echo ""
        echo "Press Enter to retry, or 'q' to quit..."
        read -r input
        [[ "$input" == "q" ]] && exit 1
        continue
    fi

    echo "Screenshot saved: $CURRENT"
    echo ""

    # Run analysis
    echo "Analyzing differences..."
    echo ""

    # Capture output but also display it
    ANALYSIS=$("$BOON_TOOLS" pixel-diff \
        --reference "$REFERENCE" \
        --current "$CURRENT" \
        --output "$DIFF" \
        --threshold "$THRESHOLD" \
        --grid 2>&1) || true

    echo "$ANALYSIS"
    echo ""

    # Check if passed
    if echo "$ANALYSIS" | grep -q "^PASS:"; then
        echo -e "${GREEN}=== CONVERGED! ===${NC}"
        echo ""
        echo "SSIM threshold met after $iteration iteration(s)."
        echo ""
        exit 0
    fi

    # Extract worst region info for guidance
    WORST_REGION=$(echo "$ANALYSIS" | grep "<< WORST" || echo "")
    if [[ -n "$WORST_REGION" ]]; then
        echo -e "${YELLOW}Focus Area:${NC}"
        echo "  $WORST_REGION"
        echo ""
    fi

    # Extract bounding box for guidance
    BBOX_TOP=$(echo "$ANALYSIS" | grep "Top-left:" | sed 's/.*(\([0-9]*\), \([0-9]*\)).*/\1,\2/')
    if [[ -n "$BBOX_TOP" ]]; then
        CSS_INFO=$(echo "$ANALYSIS" | grep -A2 "CSS coordinates" || echo "")
        if [[ -n "$CSS_INFO" ]]; then
            echo -e "${YELLOW}CSS Debug Info:${NC}"
            echo "$CSS_INFO" | tail -2
            echo ""
        fi
    fi

    echo "Diff image saved: $DIFF"
    echo ""
    echo -e "${BLUE}Actions:${NC}"
    echo "  [Enter] Take new screenshot after making changes"
    echo "  [v]     View diff image"
    echo "  [j]     Show JSON analysis"
    echo "  [q]     Quit"
    echo ""
    echo -n "Choice: "
    read -r input

    case "$input" in
        q|Q)
            echo "Exiting..."
            exit 0
            ;;
        v|V)
            if command -v xdg-open &> /dev/null; then
                xdg-open "$DIFF" &
            elif command -v open &> /dev/null; then
                open "$DIFF"
            else
                echo "Cannot open image. View manually: $DIFF"
            fi
            echo ""
            echo "Press Enter to continue..."
            read -r
            ;;
        j|J)
            echo ""
            "$BOON_TOOLS" pixel-diff \
                --reference "$REFERENCE" \
                --current "$CURRENT" \
                --threshold "$THRESHOLD" \
                --json 2>&1 || true
            echo ""
            echo "Press Enter to continue..."
            read -r
            ;;
        *)
            # Default: continue to next iteration
            ;;
    esac

    echo ""
done
