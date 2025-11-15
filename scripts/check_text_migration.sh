#!/bin/bash
# Text Syntax Migration Verification Script
# Checks for remaining old syntax in .bn files

set -e

SEARCH_DIR="${1:-playground/frontend/src/examples}"

echo "================================================"
echo "TEXT SYNTAX MIGRATION VERIFICATION"
echo "================================================"
echo "Searching in: $SEARCH_DIR"
echo ""

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

total_issues=0

echo "=== 1. Checking for old Text/empty() function calls ==="
if grep -rn "Text/empty()" "$SEARCH_DIR" --include="*.bn" 2>/dev/null; then
    echo -e "${RED}❌ Found old Text/empty() function calls${NC}"
    count=$(grep -r "Text/empty()" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | wc -l)
    echo -e "${YELLOW}   Found $count occurrences${NC}"
    total_issues=$((total_issues + count))
else
    echo -e "${GREEN}✅ No old Text/empty() function calls found${NC}"
fi
echo ""

echo "=== 2. Checking for Text/empty() |> Bool/not() pattern ==="
if grep -rn "Text/empty().*Bool/not()" "$SEARCH_DIR" --include="*.bn" 2>/dev/null; then
    echo -e "${RED}❌ Found Text/empty() |> Bool/not() pattern${NC}"
    count=$(grep -r "Text/empty().*Bool/not()" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | wc -l)
    echo -e "${YELLOW}   Found $count occurrences${NC}"
    total_issues=$((total_issues + count))
else
    echo -e "${GREEN}✅ No Text/empty() |> Bool/not() pattern found${NC}"
fi
echo ""

echo "=== 3. Checking for single-quoted strings ==="
# Exclude comments and already-migrated TEXT { } blocks
if grep -rn "'" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | grep -v "^[[:space:]]*--" | grep -v "TEXT {.*'.*}" ; then
    echo -e "${YELLOW}⚠️  Found single-quoted strings (may need migration)${NC}"
    count=$(grep -r "'" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | grep -v "^[[:space:]]*--" | grep -v "TEXT {.*'.*}" | wc -l)
    echo -e "${YELLOW}   Found $count occurrences (some may be inside TEXT blocks)${NC}"
    total_issues=$((total_issues + count))
else
    echo -e "${GREEN}✅ No single-quoted strings found outside TEXT blocks${NC}"
fi
echo ""

echo "=== 4. Checking for potential missing padding: TEXT {x} ==="
if grep -rn "TEXT {[^ ]" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | grep -v "TEXT {}"; then
    echo -e "${RED}❌ Found potential missing padding (TEXT {x} should be TEXT { x })${NC}"
    count=$(grep -r "TEXT {[^ ]" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | grep -v "TEXT {}" | wc -l)
    echo -e "${YELLOW}   Found $count occurrences${NC}"
    total_issues=$((total_issues + count))
else
    echo -e "${GREEN}✅ No missing padding detected${NC}"
fi
echo ""

echo "=== 5. Checking for TEXT {} vs Text/empty ==="
if grep -rn "TEXT {}" "$SEARCH_DIR" --include="*.bn" 2>/dev/null; then
    echo -e "${YELLOW}⚠️  Found TEXT {} (recommend using Text/empty instead)${NC}"
    count=$(grep -r "TEXT {}" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | wc -l)
    echo -e "${YELLOW}   Found $count occurrences (style preference, not an error)${NC}"
else
    echo -e "${GREEN}✅ No TEXT {} found (good - using Text/empty)${NC}"
fi
echo ""

echo "================================================"
if [ $total_issues -eq 0 ]; then
    echo -e "${GREEN}✅ MIGRATION COMPLETE! No issues found.${NC}"
else
    echo -e "${YELLOW}⚠️  Found $total_issues potential issues to review${NC}"
    echo "Review the output above and migrate remaining items"
fi
echo "================================================"

exit 0
