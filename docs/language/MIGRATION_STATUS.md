# TEXT Syntax Migration Status

**Last Updated:** 2025-11-15
**Status:** In Progress

---

## Migration Tools Created

✅ **Scripts Created:**
- `scripts/check_text_migration.sh` - Verification script to check for remaining old syntax
- `scripts/generate_migration_report.sh` - Generates detailed migration report

✅ **Documentation Created:**
- `docs/language/TEXT_SYNTAX.md` - Complete specification (923 lines)
- `docs/language/MIGRATION_TEXT_SYNTAX.md` - Migration guide
- `docs/language/MIGRATION_STATUS.md` - This file

---

## Files Migrated

### ✅ Completed (4 files)

1. **`playground/frontend/src/examples/hello_world/hello_world.bn`**
   - Changed: `'Hello world!'` → `TEXT { Hello world! }`
   - Lines: 1 string migrated
   - Status: ✅ Complete

2. **`playground/frontend/src/examples/fibonacci/fibonacci.bn`**
   - Changed: `'{position}. Fibonacci number is {result}'` → `TEXT { {position}. Fibonacci number is {result} }`
   - Lines: 1 interpolated string migrated
   - Status: ✅ Complete

3. **`playground/frontend/src/examples/counter/counter.bn`**
   - Changed: `'+'` → `TEXT { + }`
   - Lines: 1 single char migrated
   - Status: ✅ Complete

4. **`playground/frontend/src/examples/complex_counter/complex_counter.bn`**
   - Changed: `'-'` → `TEXT { - }`, `'+'` → `TEXT { + }`
   - Lines: 2 single chars migrated
   - Status: ✅ Complete

5. **`playground/frontend/src/examples/todo_mvc_physical/BUILD.bn`**
   - Status: ✅ Already using TEXT syntax (no migration needed)

### ⏳ Pending Migration

#### High Priority (Phase 1: Function Calls)

1. **`playground/frontend/src/examples/todo_mvc/todo_mvc.bn`**
   - `Text/empty()` → `Text/is_empty()` (2 occurrences at lines 28, 115)
   - Status: ⏳ Needs Phase 1 migration

2. **`playground/frontend/src/examples/todo_mvc_physical/RUN.bn`**
   - `Text/empty()` → `Text/is_empty()` (2 occurrences at lines 44, 119)
   - Status: ⏳ Needs Phase 1 migration

#### Medium Priority (Phase 2: String Literals - Main Files)

3. **`playground/frontend/src/examples/todo_mvc/todo_mvc.bn`**
   - Empty strings: `''` → `Text/empty` (~8 occurrences)
   - Single chars: `'>'`, `'×'`, `'s'` → `TEXT { x }`
   - Paths: `'/'`, `'/active'`, `'/completed'`
   - Text: `'todos'`, `'All'`, `'Active'`, `'Completed'`, etc. (~40+ strings)
   - Interpolation: `'{count} item{maybe_s} left'`
   - Data URLs: SVG data URLs (2 long strings)
   - Font family: List of font names
   - Status: ⏳ Large file, ~50+ strings to migrate

4. **`playground/frontend/src/examples/todo_mvc_physical/RUN.bn`**
   - Similar to todo_mvc.bn
   - Empty strings: `''` → `Text/empty` (~8 occurrences)
   - Status: ⏳ Large file

#### Lower Priority (Other Example Files)

5. **`playground/frontend/src/examples/minimal/minimal.bn`** - ⏳ Not checked yet
6. **`playground/frontend/src/examples/interval/interval.bn`** - ⏳ Not checked yet
7. **`playground/frontend/src/examples/latest/latest.bn`** - ⏳ Not checked yet
8. **`playground/frontend/src/examples/then/then.bn`** - ⏳ Not checked yet
9. **`playground/frontend/src/examples/while/while.bn`** - ⏳ Not checked yet
10. **`playground/frontend/src/examples/when/when.bn`** - ⏳ Not checked yet
11. **Theme files** - ⏳ Multiple theme files not checked yet

---

## Migration Statistics

### Completed
- Files fully migrated: **4**
- Files already using TEXT: **1** (BUILD.bn)
- Total strings migrated: **5**

### Pending
- Files needing Phase 1 (function calls): **2**
- Files needing Phase 2 (string literals): **~15+**
- Estimated remaining strings: **~150+**

### Progress
- Simple examples: **80%** complete (4/5 done)
- Complex examples: **0%** complete (0/2 done)
- Theme files: **0%** complete

---

## Next Steps

### Immediate (Phase 1)
1. ⚠️ **CRITICAL**: Migrate `Text/empty()` function calls in:
   - `todo_mvc.bn` (lines 28, 115)
   - `todo_mvc_physical/RUN.bn` (lines 44, 119)

### Then (Phase 2)
2. Migrate `todo_mvc.bn` string literals (~50+ strings)
3. Migrate `todo_mvc_physical/RUN.bn` string literals
4. Check and migrate remaining example files
5. Migrate theme files

### Verification
6. Run `scripts/check_text_migration.sh` after each phase
7. Generate updated report with `scripts/generate_migration_report.sh`

---

## Verification Commands

```bash
# Check migration status
./scripts/check_text_migration.sh

# Generate detailed report
./scripts/generate_migration_report.sh

# Count remaining quoted strings
grep -r "'" playground/frontend/src/examples --include="*.bn" | wc -l

# Find specific patterns
grep -rn "Text/empty()" playground/frontend/src/examples --include="*.bn"
grep -rn "''" playground/frontend/src/examples --include="*.bn"
```

---

## Migration Examples (Reference)

### Example 1: Simple String
```boon
-- Before:
root: 'Hello world!'

-- After:
root: TEXT { Hello world! }
```

### Example 2: Interpolation
```boon
-- Before:
'{position}. Fibonacci number is {result}'

-- After:
TEXT { {position}. Fibonacci number is {result} }
```

### Example 3: Single Character
```boon
-- Before:
label: '+'

-- After:
label: TEXT { + }
```

### Example 4: Empty String
```boon
-- Before:
LATEST {
    ''
    element.event.change.text
}

-- After:
LATEST {
    Text/empty
    element.event.change.text
}
```

### Example 5: Function Call (Phase 1 - Critical!)
```boon
-- Before:
text |> Text/empty() |> WHEN { True => ..., False => ... }

-- After:
text |> Text/is_empty() |> WHEN { True => ..., False => ... }
```

---

**Progress:** 5/20+ files (25%)
**Next:** Phase 1 function calls in todo_mvc files
