#!/bin/bash
# Analyze documentation files for TEXT syntax migration
# Shows what needs to be updated in each markdown file

set -e

echo "========================================"
echo "DOCUMENTATION MIGRATION ANALYSIS"
echo "========================================"
echo ""

docs_dir="${1:-docs}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Find all markdown files with Boon code
echo "=== Finding documentation files with Boon code ==="
echo ""

md_files=$(find "$docs_dir" -name "*.md" -type f | grep -v TEXT_SYNTAX | grep -v MIGRATION | grep -v README_MIGRATION)

for file in $md_files; do
    # Check if file contains boon code blocks
    if grep -q 'boon' "$file" 2>/dev/null; then
        echo -e "${BLUE}📄 $file${NC}"

        # Count code blocks
        boon_blocks=$(grep -c '```boon' "$file" 2>/dev/null || echo "0")
        echo "   Boon code blocks: $boon_blocks"

        # Find quoted strings (potential migrations)
        quoted_strings=$(grep -n "'.*'" "$file" 2>/dev/null | wc -l)
        if [ "$quoted_strings" -gt 0 ]; then
            echo -e "   ${YELLOW}Quoted strings found: $quoted_strings${NC}"
            echo "   Sample lines:"
            grep -n "'.*'" "$file" 2>/dev/null | head -5 | sed 's/^/     /'
        else
            echo -e "   ${GREEN}No quoted strings found (may already be migrated)${NC}"
        fi

        # Check for TEXT { syntax
        text_blocks=$(grep -c 'TEXT {' "$file" 2>/dev/null || echo "0")
        if [ "$text_blocks" -gt 0 ]; then
            echo -e "   ${GREEN}TEXT blocks already present: $text_blocks${NC}"
        fi

        echo ""
    fi
done

echo "========================================"
echo "SUMMARY"
echo "========================================"
echo ""

total_md=$(echo "$md_files" | wc -l)
md_with_boon=$(echo "$md_files" | xargs -I {} grep -l 'boon' {} 2>/dev/null | wc -l)

echo "Total markdown files: $total_md"
echo "Files with Boon code: $md_with_boon"
echo ""

echo "Files needing migration:"
for file in $md_files; do
    if grep -q 'boon' "$file" 2>/dev/null; then
        quoted=$(grep -c "'.*'" "$file" 2>/dev/null || echo "0")
        if [ "$quoted" -gt 0 ]; then
            echo -e "  ${YELLOW}⚠${NC}  $file ($quoted quoted strings)"
        fi
    fi
done

echo ""
echo "========================================"
echo "NEXT STEPS"
echo "========================================"
echo ""
echo "1. Review MIGRATION_DOCS.md for migration plan"
echo "2. Start with BUILD_SYSTEM.md (most examples)"
echo "3. Update code examples to use TEXT syntax"
echo "4. Verify consistency with migrated codebase"
echo ""
