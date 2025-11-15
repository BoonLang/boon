# Documentation Migration Guide - TEXT Syntax

**Date:** 2025-11-15
**Status:** Migration Plan
**Scope:** Update all Boon code examples in markdown documentation to use TEXT syntax

---

## Overview

Now that the codebase migration is complete, we need to update all documentation files to show the new TEXT syntax in code examples.

---

## Files to Migrate

### Identified Documentation Files with Boon Code

1. **`docs/build/BUILD_SYSTEM.md`**
   - BUILD.bn examples with string generation
   - File paths, URLs, generated code strings
   - ~15-20 quoted strings in code examples

2. **`docs/patterns/LINK_PATTERN.md`**
   - LINK pattern examples
   - Likely has string literals in examples

3. **`docs/language/BOON_SYNTAX.md`**
   - Core language syntax examples
   - May have many string examples (but might be intentionally showing old syntax)

---

## Migration Strategy

### Phase 1: Analyze Each File

For each documentation file:
1. Read the file completely
2. Identify all Boon code blocks (```boon ... ```)
3. Within code blocks, find quoted strings that should use TEXT syntax
4. Check if any examples are specifically showing "old syntax" (skip those)

### Phase 2: Update Code Examples

#### Rules for Documentation Updates

1. **Update all code examples** to use new TEXT syntax
2. **Preserve intentional old syntax examples** (if documenting migration)
3. **Update inline code** snippets where appropriate
4. **Add notes** about TEXT syntax where helpful
5. **Keep examples consistent** with the actual migrated codebase

#### Example Transformations

**Before (in docs):**
```boon
input_dir: './assets/icons'
output_file: './Generated/Assets.bn'
data_url: 'data:image/svg+xml;utf8,' ++ URL/encode(svg_content)
Result: '{icon_name}: \'{data_url}\''
```

**After (in docs):**
```boon
input_dir: TEXT { ./assets/icons }
output_file: TEXT { ./Generated/Assets.bn }
data_url: TEXT { data:image/svg+xml;utf8, } ++ URL/encode(svg_content)
Result: TEXT { {icon_name}: '{data_url}' }
```

### Phase 3: Verify Consistency

1. Compare doc examples with actual migrated code
2. Ensure BUILD.bn examples match actual BUILD.bn file
3. Run documentation through markdown linter
4. Check that all code blocks are syntactically consistent

---

## Specific File Plans

### 1. BUILD_SYSTEM.md

**Occurrences found:**
- Line 95: `input_dir: './assets/icons'`
- Line 96: `output_file: './Generated/Assets.bn'`
- Line 103: `icon_name: file.name |> Text/trim_suffix('.svg')`
- Line 105: `data_url: 'data:image/svg+xml;utf8,' ++ URL/encode(svg_content)`
- Line 106: `Result: '{icon_name}: \'{data_url}\''`
- Line 115: `.name |> Text/ends_with('.svg')`
- Line 119: `'-- GENERATED CODE'`
- Line 120-126: Multiple strings for code generation
- Line 134: `Build/rerun_if_changed('BUILD.bn')`

**Migration approach:**
- Update all strings to TEXT syntax
- Ensure generated code examples match actual BUILD.bn output
- Update Text module function names if needed

### 2. LINK_PATTERN.md

**To be analyzed:**
- Need to read full file
- Identify LINK pattern examples
- Update any string literals

### 3. BOON_SYNTAX.md

**Special considerations:**
- This is a language syntax document
- May intentionally show old syntax for comparison
- Need to determine if we're replacing or adding TEXT syntax section
- Consider adding a note that TEXT is the new standard

---

## Checklist

### Pre-Migration
- [ ] Read each documentation file completely
- [ ] Identify all Boon code blocks
- [ ] Note any "intentional old syntax" examples
- [ ] Create backup of docs (via git)

### Migration
- [ ] Migrate BUILD_SYSTEM.md code examples
- [ ] Migrate LINK_PATTERN.md code examples
- [ ] Migrate BOON_SYNTAX.md code examples
- [ ] Update any inline code snippets
- [ ] Add TEXT syntax notes where helpful

### Post-Migration
- [ ] Verify all code examples are syntactically correct
- [ ] Compare with actual migrated codebase
- [ ] Check for consistency across all docs
- [ ] Run markdown linter if available
- [ ] Update table of contents if needed

---

## Notes

### Documentation Standards

1. **Code blocks should be runnable** - All Boon code examples should use current syntax
2. **Show TEXT syntax** - New examples should demonstrate TEXT, not quoted strings
3. **Migration notes** - If showing old→new, clearly label which is which
4. **Consistency** - Docs should match the actual codebase

### Special Cases

**If documenting string syntax itself:**
- Show both old and new in comparison sections
- Clearly label "Old (deprecated)" and "New (recommended)"
- Reference TEXT_SYNTAX.md for full specification

**If showing BUILD.bn examples:**
- Ensure examples match actual BUILD.bn files in codebase
- Update generated output examples to match new TEXT syntax
- Note that BUILD.bn already uses TEXT syntax

---

## Estimated Effort

- **BUILD_SYSTEM.md**: 15-20 minutes (straightforward replacements)
- **LINK_PATTERN.md**: 5-10 minutes (likely fewer examples)
- **BOON_SYNTAX.md**: 20-30 minutes (need careful review)

**Total**: 40-60 minutes

---

## Migration Command Reference

```bash
# Find all markdown files with Boon code
grep -r 'boon' docs/ --include='*.md' -l

# Find quoted strings in specific file
grep -n "'.*'" docs/build/BUILD_SYSTEM.md

# Count Boon code blocks in file
grep -c '```boon' docs/build/BUILD_SYSTEM.md

# Verify migration (no quotes outside TEXT blocks)
grep "''" docs/build/BUILD_SYSTEM.md | grep -v TEXT
```

---

## Next Steps

1. **Start with BUILD_SYSTEM.md** - Most examples, clearest migration path
2. **Then LINK_PATTERN.md** - Likely simpler
3. **Finally BOON_SYNTAX.md** - Requires careful consideration

After documentation migration, the entire TEXT syntax migration will be complete!

---

**Status**: Ready to begin
**Priority**: Medium (code migration complete, docs should follow)
