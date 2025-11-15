#!/bin/bash
# Generate detailed migration report with examples
# Shows specific lines that need migration with suggested replacements

set -e

SEARCH_DIR="${1:-playground/frontend/src/examples}"
OUTPUT_FILE="${2:-migration_report.txt}"

echo "Generating migration report for: $SEARCH_DIR"
echo "Output file: $OUTPUT_FILE"
echo ""

{
    echo "========================================"
    echo "TEXT SYNTAX MIGRATION REPORT"
    echo "========================================"
    echo "Generated: $(date)"
    echo "Search directory: $SEARCH_DIR"
    echo ""

    echo "========================================"
    echo "PHASE 1: FUNCTION CALLS (DO FIRST!)"
    echo "========================================"
    echo ""

    echo "--- 1.1 Text/empty() → Text/is_empty() ---"
    if grep -rn "Text/empty()" "$SEARCH_DIR" --include="*.bn" 2>/dev/null; then
        echo ""
        echo "Action: Replace all occurrences with Text/is_empty()"
    else
        echo "✅ None found"
    fi
    echo ""

    echo "--- 1.2 Text/empty() |> Bool/not() → Text/is_not_empty() ---"
    if grep -rn "Text/empty().*Bool/not()" "$SEARCH_DIR" --include="*.bn" 2>/dev/null; then
        echo ""
        echo "Action: Replace pattern with Text/is_not_empty()"
    else
        echo "✅ None found"
    fi
    echo ""

    echo "========================================"
    echo "PHASE 2: STRING LITERALS"
    echo "========================================"
    echo ""

    echo "--- 2.1 Empty Strings: '' → Text/empty ---"
    grep -rn "LATEST {" "$SEARCH_DIR" --include="*.bn" -A 5 2>/dev/null | grep -B 1 "''" || echo "✅ None found in LATEST blocks"
    echo ""

    echo "--- 2.2 Single Character Strings ---"
    echo "Examples of single chars that need migration:"
    grep -rn ": '[^']\{1\}'" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | head -10 || echo "✅ None found"
    echo ""

    echo "--- 2.3 Simple Text Strings ---"
    echo "Examples of simple strings that need migration:"
    grep -rn "label: '[^']*'" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | head -10 || echo "✅ None found"
    echo ""

    echo "--- 2.4 Path Strings ---"
    echo "URL/Path strings that need migration:"
    grep -rn "'/[^']*'" "$SEARCH_DIR" --include="*.bn" 2>/dev/null || echo "✅ None found"
    echo ""

    echo "--- 2.5 Interpolated Strings ---"
    echo "Strings with interpolation {var}:"
    grep -rn "'{[^}]*}[^']*'" "$SEARCH_DIR" --include="*.bn" 2>/dev/null || echo "✅ None found"
    echo ""

    echo "========================================"
    echo "STATISTICS"
    echo "========================================"
    echo ""

    total_quotes=$(grep -r "'" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | wc -l)
    total_empty=$(grep -r "''" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | wc -l)
    total_text_blocks=$(grep -r "TEXT {" "$SEARCH_DIR" --include="*.bn" 2>/dev/null | wc -l)

    echo "Total lines with quotes: $total_quotes"
    echo "Empty strings (''): $total_empty"
    echo "Already migrated (TEXT {): $total_text_blocks"
    echo ""

    echo "========================================"
    echo "NEXT STEPS"
    echo "========================================"
    echo ""
    echo "1. Fix Phase 1 function calls first"
    echo "2. Then migrate string literals category by category"
    echo "3. Run check_text_migration.sh to verify"
    echo ""

} > "$OUTPUT_FILE"

echo "✅ Report generated: $OUTPUT_FILE"
echo ""
echo "Preview:"
head -50 "$OUTPUT_FILE"
echo ""
echo "... (see full report in $OUTPUT_FILE)"
