# Documentation Migration - Action Plan

**Date:** 2025-11-15
**Status:** Ready to Execute
**Estimated Time:** 60-90 minutes

---

## Analysis Results

### Files Found

| File | Boon Blocks | Quoted Strings | Priority |
|------|-------------|----------------|----------|
| **BUILD_SYSTEM.md** | 39 | 87 | High |
| **LINK_PATTERN.md** | 27 | 17 | Medium |
| **BOON_SYNTAX.md** | 39 | 9 | Low/Review |

**Total:** 3 files, 105 Boon code blocks, ~113 quoted strings to review

**Note:** Not all quoted strings are in code - many are in prose. Only code examples need migration.

---

## Migration Priority

### 1. BUILD_SYSTEM.md (HIGH PRIORITY)

**Why first:**
- Most Boon code examples
- Shows actual BUILD.bn usage
- Should match the migrated BUILD.bn file
- Examples are used as reference

**What to migrate:**
```boon
-- Old examples in docs:
input_dir: './assets/icons'
data_url: 'data:image/svg+xml;utf8,' ++ ...
Result: '{icon_name}: \'{data_url}\''

-- New examples should be:
input_dir: TEXT { ./assets/icons }
data_url: TEXT { data:image/svg+xml;utf8, } ++ ...
Result: TEXT { {icon_name}: '{data_url}' }
```

**Action items:**
- [ ] Read full file
- [ ] Identify code blocks (not prose) with old syntax
- [ ] Update to match actual BUILD.bn implementation
- [ ] Verify generated code examples match TEXT syntax
- [ ] Update Text module function calls if needed

---

### 2. LINK_PATTERN.md (MEDIUM PRIORITY)

**Why second:**
- Demonstrates LINK pattern
- Has code examples that should match migrated TodoMVC
- 17 quoted strings (mix of code and prose)

**What to expect:**
- Empty strings: `''` → `Text/empty`
- Text literals in element examples
- May reference TodoMVC code (should match migrated version)

**Action items:**
- [ ] Read full file
- [ ] Update code examples to match TodoMVC migration
- [ ] Ensure LINK pattern examples are consistent
- [ ] Check for any `Text/empty()` → `Text/is_empty()` updates

---

### 3. BOON_SYNTAX.md (LOW PRIORITY - NEEDS REVIEW)

**Why last:**
- Core language syntax document
- May intentionally show various syntax forms
- Only 9 quoted strings
- Need to determine if we add TEXT section or replace examples

**Considerations:**
- This might be documenting the language syntax itself
- May need to ADD TEXT syntax section rather than replace
- Could show both old (deprecated) and new (recommended)
- Needs careful editorial review

**Action items:**
- [ ] Read full file to understand purpose
- [ ] Determine if showing old syntax intentionally
- [ ] Decide: Replace, Add Section, or Both
- [ ] Update any code examples using strings
- [ ] Consider adding "String Literals" section referencing TEXT_SYNTAX.md

---

## Migration Approach

### For Each File:

#### Step 1: Read & Analyze (5-10 min)
1. Read the entire file
2. Identify all Boon code blocks
3. Distinguish code from prose
4. Note any "intentional old syntax" examples

#### Step 2: Update Code Examples (15-30 min)
1. Update strings to TEXT syntax
2. Update Text module functions (`Text/empty()` → `Text/is_empty()`)
3. Ensure examples match migrated codebase
4. Keep consistent with TEXT_SYNTAX.md

#### Step 3: Verify (5-10 min)
1. Check all code blocks are syntactically valid
2. Compare with actual migrated code
3. Ensure consistency across doc
4. Check for any missed quotes

---

## Specific Examples to Migrate

### BUILD_SYSTEM.md (Sample Lines)

**Line 95-96:**
```boon
-- Before:
input_dir: './assets/icons'
output_file: './Generated/Assets.bn'

-- After:
input_dir: TEXT { ./assets/icons }
output_file: TEXT { ./Generated/Assets.bn }
```

**Line 105-106:**
```boon
-- Before:
data_url: 'data:image/svg+xml;utf8,' ++ URL/encode(svg_content)
Result: '{icon_name}: \'{data_url}\''

-- After:
data_url: TEXT { data:image/svg+xml;utf8, } ++ URL/encode(svg_content)
Result: TEXT { {icon_name}: '{data_url}' }
```

**Line 119-127 (Code generation):**
```boon
-- Before:
LIST {
    '-- GENERATED CODE'
    ''
    'icon: ['
    -- ...
    ']'
} |> Text/join('\n')

-- After:
LIST {
    TEXT { -- GENERATED CODE }
    Text/empty
    TEXT { icon: [ }
    -- ...
    TEXT { ] }
} |> Text/join(Text/newline)
```

### LINK_PATTERN.md (Sample Lines)

**Line 94, 173, 175:**
```boon
-- Before:
text: LATEST {
    ''
    element.event.change.text
    PASSED.store.title_to_save |> THEN { '' }
}

-- After:
text: LATEST {
    Text/empty
    element.event.change.text
    PASSED.store.title_to_save |> THEN { Text/empty }
}
```

### BOON_SYNTAX.md (Sample Lines)

**Line 62, 96:**
```boon
-- Before:
text: 'todos'
Element/text(text: 'Hello')

-- After:
text: TEXT { todos }
Element/text(text: TEXT { Hello })
```

---

## Quality Checklist

### Before Migration
- [ ] All code files already migrated
- [ ] TEXT_SYNTAX.md specification complete
- [ ] Migration tools created and tested

### During Migration
- [ ] Read each file completely before editing
- [ ] Update only code examples (not prose)
- [ ] Preserve intentional syntax demonstrations
- [ ] Keep consistent with TEXT_SYNTAX.md

### After Migration
- [ ] All Boon code blocks use TEXT syntax
- [ ] Examples match migrated codebase
- [ ] No quoted strings in code blocks (except in TEXT blocks)
- [ ] Documentation is consistent
- [ ] Markdown renders correctly

---

## Tools Available

```bash
# Analyze documentation
./scripts/analyze_docs_migration.sh

# Find quoted strings in specific doc
grep -n "'.*'" docs/build/BUILD_SYSTEM.md | grep -v "^#" | head -20

# Verify no quotes outside TEXT blocks after migration
grep -E "'[^']*'" docs/build/BUILD_SYSTEM.md | grep -v TEXT | grep -v "^#"

# Count code blocks
grep -c '```boon' docs/build/BUILD_SYSTEM.md
```

---

## Success Criteria

✅ **Documentation Migration Complete When:**
1. All Boon code examples use TEXT syntax
2. No quoted strings in code blocks (except inside TEXT blocks)
3. Examples match the migrated codebase
4. Documentation is consistent with TEXT_SYNTAX.md
5. All code examples are syntactically valid

---

## Estimated Timeline

| Task | Time | Status |
|------|------|--------|
| Analyze files | 15 min | ✅ Done |
| Migrate BUILD_SYSTEM.md | 30 min | ⏳ Pending |
| Migrate LINK_PATTERN.md | 15 min | ⏳ Pending |
| Review BOON_SYNTAX.md | 10 min | ⏳ Pending |
| Migrate BOON_SYNTAX.md | 20 min | ⏳ Pending |
| Verification | 10 min | ⏳ Pending |
| **Total** | **~100 min** | **0% Complete** |

---

## Next Action

**Start with:** `docs/build/BUILD_SYSTEM.md`

This file has the most examples and should be straightforward - update code examples to match the already-migrated BUILD.bn file.

---

**Ready to begin documentation migration!**
