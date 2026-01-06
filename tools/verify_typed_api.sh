#!/bin/bash
# tools/verify_typed_api.sh
# Comprehensive verification for Boon-Zoon typed API migration
#
# Checks:
# 1. Style API migration (raw CSS → Zoon typed styles)
# 2. HTML tag implementation (element.tag → with_tag)
# 3. update_raw_el usage reduction
#
# Usage: ./tools/verify_typed_api.sh [--verbose]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BRIDGE_FILE="$REPO_ROOT/crates/boon/src/platform/browser/bridge.rs"

VERBOSE=false
if [ "$1" = "--verbose" ]; then
    VERBOSE=true
fi

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo "═══════════════════════════════════════════════════════════════════"
echo "  Boon-Zoon Typed API Migration Verification"
echo "═══════════════════════════════════════════════════════════════════"
echo ""

if [ ! -f "$BRIDGE_FILE" ]; then
    echo -e "${RED}ERROR: bridge.rs not found at $BRIDGE_FILE${NC}"
    exit 1
fi

PASS=true

# ═══════════════════════════════════════════════════════════════════════
# CHECK 1: Style API Migration
# ═══════════════════════════════════════════════════════════════════════

echo -e "${BLUE}[CHECK 1] Style API Migration${NC}"
echo "─────────────────────────────────────────────────────────────────────"

# Count .style_signal( calls (should be 0 after full migration)
STYLE_SIGNAL_COUNT=$(grep -c '\.style_signal(' "$BRIDGE_FILE" 2>/dev/null | head -1 || echo "0")

# Count .style( calls excluding .style_signal (static, should be 0 after migration)
STYLE_COUNT=$(grep -E '\.style\([^_]' "$BRIDGE_FILE" 2>/dev/null | wc -l | tr -d ' ')

# Count .s( calls (Zoon typed style API)
TYPED_STYLE_COUNT=$(grep -c '\.s(' "$BRIDGE_FILE" 2>/dev/null | head -1 || echo "0")

# ─────────────────────────────────────────────────────────────────────
# Expected after full migration:
# - ALL .style_signal() calls migrated to .s() with typed styles
# - Container content alignment uses AlignContent API (not raw CSS):
#   * align.row → AlignContent::center_x() / left() / right()
#   * align.column → AlignContent::center_y() / top() / bottom()
# - All .style() static calls REMOVED because:
#   1. box-sizing: border-box      → GLOBAL (modern-normalize.css)
#   2. background-repeat: no-repeat → GLOBAL (basic.css)
#   3. background-size: contain    → GLOBAL (basic.css)
#   4. white-space: pre-wrap       → GLOBAL (basic.css)
#   5. display: flex               → Use Stripe::new() instead of El::new()
#   6. flex-direction: column      → Use Stripe.direction(Direction::Column)
# ─────────────────────────────────────────────────────────────────────
EXPECTED_STYLE_SIGNAL=0
EXPECTED_STYLE_STATIC=0

# Current baseline (before migration)
BASELINE_STYLE_SIGNAL=52
BASELINE_STYLE_STATIC=7

echo "  Current:"
echo "    .style_signal() calls: $STYLE_SIGNAL_COUNT (baseline: $BASELINE_STYLE_SIGNAL)"
echo "    .style() static calls: $STYLE_COUNT (baseline: $BASELINE_STYLE_STATIC)"
echo "    .s() typed calls:      $TYPED_STYLE_COUNT"
echo ""

if [ "$STYLE_SIGNAL_COUNT" -eq "$EXPECTED_STYLE_SIGNAL" ]; then
    echo -e "  ${GREEN}✓${NC} style_signal: All migrated"
else
    REMAINING=$((STYLE_SIGNAL_COUNT - EXPECTED_STYLE_SIGNAL))
    echo -e "  ${RED}✗${NC} style_signal: $REMAINING calls need migration"
    PASS=false
fi

if [ "$STYLE_COUNT" -le "$EXPECTED_STYLE_STATIC" ]; then
    echo -e "  ${GREEN}✓${NC} style (static): All removed (global or typed)"
else
    EXTRA=$((STYLE_COUNT - EXPECTED_STYLE_STATIC))
    echo -e "  ${YELLOW}!${NC} style (static): $EXTRA calls to remove"
    echo -e "      ${YELLOW}→ These should be global CSS or replaced with Stripe${NC}"
fi

echo ""

# ═══════════════════════════════════════════════════════════════════════
# CHECK 2: HTML Tag Implementation
# ═══════════════════════════════════════════════════════════════════════

echo -e "${BLUE}[CHECK 2] HTML Tag Implementation${NC}"
echo "─────────────────────────────────────────────────────────────────────"

# Check for with_tag usage (should exist after implementation)
WITH_TAG_COUNT=$(grep -c 'with_tag' "$BRIDGE_FILE" 2>/dev/null | head -1 || echo "0")
if [ -z "$WITH_TAG_COUNT" ]; then WITH_TAG_COUNT=0; fi

# Check for Tag:: enum usage
TAG_ENUM_COUNT=$(grep -c 'Tag::' "$BRIDGE_FILE" 2>/dev/null | head -1 || echo "0")
if [ -z "$TAG_ENUM_COUNT" ]; then TAG_ENUM_COUNT=0; fi

# Boon todo_mvc.bn uses these tags:
# - Header (1x), H1 (1x), Section (1x), Ul (1x), Footer (2x) = 6 usages
EXPECTED_TAG_IMPL=1  # At least some with_tag usage expected

echo "  Current:"
echo "    with_tag() calls:     $WITH_TAG_COUNT"
echo "    Tag:: enum usage:     $TAG_ENUM_COUNT"
echo ""

if [ "$WITH_TAG_COUNT" -gt 0 ]; then
    echo -e "  ${GREEN}✓${NC} HTML tag: Implemented"
else
    echo -e "  ${RED}✗${NC} HTML tag: NOT IMPLEMENTED"
    echo -e "      ${YELLOW}→ Boon element.tag property is being ignored!${NC}"
    echo -e "      ${YELLOW}→ Semantic HTML (header, section, footer) not rendered${NC}"
    PASS=false
fi

echo ""

# ═══════════════════════════════════════════════════════════════════════
# CHECK 3: update_raw_el Usage
# ═══════════════════════════════════════════════════════════════════════

echo -e "${BLUE}[CHECK 3] update_raw_el Usage${NC}"
echo "─────────────────────────────────────────────────────────────────────"

# Count update_raw_el calls (should decrease after migration)
UPDATE_RAW_EL_COUNT=$(grep -c '\.update_raw_el(' "$BRIDGE_FILE" 2>/dev/null | head -1 || echo "0")

# Baseline and target
BASELINE_UPDATE_RAW_EL=8
# All update_raw_el calls eliminated - using AlignContent for container alignment
EXPECTED_UPDATE_RAW_EL=0

echo "  Current:"
echo "    .update_raw_el() calls: $UPDATE_RAW_EL_COUNT (baseline: $BASELINE_UPDATE_RAW_EL)"
echo ""

if [ "$UPDATE_RAW_EL_COUNT" -eq "$EXPECTED_UPDATE_RAW_EL" ]; then
    echo -e "  ${GREEN}✓${NC} update_raw_el: Not used (fully typed)"
elif [ "$UPDATE_RAW_EL_COUNT" -lt "$BASELINE_UPDATE_RAW_EL" ]; then
    REDUCED=$((BASELINE_UPDATE_RAW_EL - UPDATE_RAW_EL_COUNT))
    echo -e "  ${YELLOW}!${NC} update_raw_el: Reduced by $REDUCED (progress made)"
else
    echo -e "  ${YELLOW}!${NC} update_raw_el: Still at baseline ($UPDATE_RAW_EL_COUNT calls)"
fi

echo ""

# ═══════════════════════════════════════════════════════════════════════
# CHECK 4: Typed APIs in Use (Sanity Check)
# ═══════════════════════════════════════════════════════════════════════

echo -e "${BLUE}[CHECK 4] Typed APIs in Use (Sanity)${NC}"
echo "─────────────────────────────────────────────────────────────────────"

# These should exist and stay
TYPED_CONSTRUCTORS=$(grep -cE 'El::new\(\)|Button::new\(\)|TextInput::new\(\)|Checkbox::new\(\)|Label::new\(\)|Paragraph::new\(\)|Link::new\(\)|Stripe::new\(\)|Stack::new\(\)' "$BRIDGE_FILE" 2>/dev/null || echo "0")
TYPED_EVENTS=$(grep -cE '\.on_press\(|\.on_click\(|\.on_change\(|\.on_hovered_change\(|\.on_key_down_event\(|\.on_blur\(|\.on_double_click\(' "$BRIDGE_FILE" 2>/dev/null || echo "0")
TYPED_CONTENT=$(grep -cE '\.child_signal\(|\.items_signal_vec\(|\.layers_signal_vec\(|\.contents_signal_vec\(|\.label_signal\(' "$BRIDGE_FILE" 2>/dev/null || echo "0")

echo "  Element constructors:  $TYPED_CONSTRUCTORS ✓"
echo "  Event handlers:        $TYPED_EVENTS ✓"
echo "  Content methods:       $TYPED_CONTENT ✓"
echo ""

# ═══════════════════════════════════════════════════════════════════════
# VERBOSE: Show detailed findings
# ═══════════════════════════════════════════════════════════════════════

if [ "$VERBOSE" = true ]; then
    echo -e "${BLUE}[VERBOSE] Detailed Findings${NC}"
    echo "─────────────────────────────────────────────────────────────────────"
    echo ""

    if [ "$STYLE_SIGNAL_COUNT" -gt 0 ]; then
        echo "  .style_signal() calls to migrate:"
        grep -n '\.style_signal(' "$BRIDGE_FILE" | head -20 | while read line; do
            echo "    $line"
        done
        if [ "$STYLE_SIGNAL_COUNT" -gt 20 ]; then
            echo "    ... and $((STYLE_SIGNAL_COUNT - 20)) more"
        fi
        echo ""
    fi

    if [ "$STYLE_COUNT" -gt 0 ]; then
        echo "  .style() static calls to remove (should be global or use Stripe):"
        grep -nE '\.style\([^_]' "$BRIDGE_FILE" | while read line; do
            echo "    $line"
        done
        echo ""
    fi
fi

# ═══════════════════════════════════════════════════════════════════════
# SUMMARY
# ═══════════════════════════════════════════════════════════════════════

# Calculate style migration progress
if [ "$BASELINE_STYLE_SIGNAL" -gt 0 ]; then
    STYLE_PROGRESS=$(( (BASELINE_STYLE_SIGNAL - STYLE_SIGNAL_COUNT) * 100 / BASELINE_STYLE_SIGNAL ))
else
    STYLE_PROGRESS=100
fi

echo "═══════════════════════════════════════════════════════════════════"
echo "SUMMARY"
echo "─────────────────────────────────────────────────────────────────────"
echo "  Style migration:  $STYLE_PROGRESS% complete ($STYLE_SIGNAL_COUNT remaining)"
if [ "$WITH_TAG_COUNT" -gt 0 ]; then
    echo "  HTML tags:        Implemented"
else
    echo "  HTML tags:        NOT implemented"
fi
echo "═══════════════════════════════════════════════════════════════════"
echo ""

if [ "$PASS" = true ]; then
    echo -e "${GREEN}✅ PASS: All typed API checks passed${NC}"
    exit 0
else
    echo -e "${RED}❌ FAIL: Some typed API features missing${NC}"
    exit 1
fi
