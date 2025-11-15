# Complete Documentation Migration - All Files

**Date:** 2025-11-15
**Status:** Execution Plan
**Scope:** Migrate ALL 18 files to TEXT syntax
**Reason:** Language in design phase, no users, no need for historical syntax

---

## Executive Decision

✅ **Migrate everything** - No exceptions
- Language is in design phase
- No users yet
- All docs should show current syntax
- No historical records needed

---

## All Files to Migrate (18 files, 447 blocks)

### Group 1: Main Documentation (3 files, 105 blocks)
```
1. docs/build/BUILD_SYSTEM.md                      39 blocks
2. docs/patterns/LINK_PATTERN.md                   27 blocks
3. docs/language/BOON_SYNTAX.md                    39 blocks
```
**Time:** ~90 minutes

---

### Group 2: Theme Documentation (4 files, 48 blocks)
```
4. playground/.../docs/theme/USAGE.md              19 blocks
5. playground/.../docs/theme/ARCHITECTURE.md       17 blocks
6. playground/.../docs/theme/README.md              3 blocks
7. playground/.../docs/theme/STRUCTURE.md           9 blocks
```
**Time:** ~60 minutes

---

### Group 3: Design Documents (6 files, 119 blocks)
```
8.  playground/.../docs/EMERGENT_GEOMETRY_CONCEPT.md    15 blocks
9.  playground/.../docs/EMERGENT_THEME_TOKENS.md        20 blocks
10. playground/.../docs/EMERGENT_BORDERS.md             17 blocks
11. playground/.../docs/TEXT_DEPTH_HIERARCHY.md         20 blocks
12. playground/.../docs/POINTER_MAGNETISM.md            25 blocks
13. playground/.../docs/PHYSICALLY_BASED_RENDERING.md   22 blocks
```
**Time:** ~2 hours

---

### Group 4: Analysis & Research (4 files, 174 blocks)
```
14. playground/.../docs/CODE_ANALYSIS_AND_IMPROVEMENTS.md  53 blocks
15. playground/.../docs/3D_API_DESIGN.md                   28 blocks
16. playground/.../docs/PATTERNS_STATUS.md                 26 blocks
17. playground/.../docs/LANGUAGE_FEATURES_RESEARCH.md      67 blocks
```
**Time:** ~3 hours

---

### Group 5: READMEs (1 file, 2 blocks)
```
18. playground/.../todo_mvc_physical/README.md              2 blocks
```
**Time:** ~5 minutes

---

## Migration Order & Estimated Timeline

| Group | Files | Blocks | Time | Status |
|-------|-------|--------|------|--------|
| 1. Main Docs | 3 | 105 | 90 min | ⏳ Start here |
| 2. Theme Docs | 4 | 48 | 60 min | ⏳ Pending |
| 3. Design Docs | 6 | 119 | 120 min | ⏳ Pending |
| 4. Analysis/Research | 4 | 174 | 180 min | ⏳ Pending |
| 5. READMEs | 1 | 2 | 5 min | ⏳ Pending |
| **TOTAL** | **18** | **447** | **~7.5 hrs** | **0% Complete** |

---

## Migration Approach

### For Each File:
1. **Read** the file
2. **Find** all Boon code blocks
3. **Update** strings to TEXT syntax:
   - `''` → `Text/empty`
   - `'text'` → `TEXT { text }`
   - `'{var}'` → `TEXT { {var} }`
   - `'path/to/file'` → `TEXT { path/to/file }`
4. **Update** function calls:
   - `Text/empty()` → `Text/is_empty()`
   - `Text/empty() |> Bool/not()` → `Text/is_not_empty()`
5. **Verify** code blocks are consistent

---

## Common Patterns to Migrate

### Pattern 1: Paths
```boon
-- Before:
'./assets/icons'
'./Generated/Assets.bn'

-- After:
TEXT { ./assets/icons }
TEXT { ./Generated/Assets.bn }
```

### Pattern 2: Empty Strings
```boon
-- Before:
initial: ''
reset |> THEN { '' }

-- After:
initial: Text/empty
reset |> THEN { Text/empty }
```

### Pattern 3: Interpolation
```boon
-- Before:
'{name}: \'{value}\''

-- After:
TEXT { {name}: '{value}' }
```

### Pattern 4: Generated Code
```boon
-- Before:
'-- GENERATED CODE'
'icon: ['
']'

-- After:
TEXT { -- GENERATED CODE }
TEXT { icon: [ }
TEXT { ] }
```

### Pattern 5: Function Calls
```boon
-- Before:
Text/join('\n')

-- After:
Text/join(Text/newline)
```

---

## Execution Strategy

### Session 1: Main Docs (Now)
- Migrate Group 1 (3 files)
- Time: 90 minutes
- Establish patterns

### Session 2: Theme & Design
- Migrate Groups 2 & 3 (10 files)
- Time: 3 hours
- Follow established patterns

### Session 3: Analysis & READMEs
- Migrate Groups 4 & 5 (5 files)
- Time: 3 hours
- Complete migration

**Alternative:** Do all in one extended session if preferred

---

## Quality Checks

After each group:
- [ ] No single-quoted strings in code blocks
- [ ] All TEXT blocks have proper padding
- [ ] Function calls use new API
- [ ] Code examples are syntactically valid
- [ ] Consistent with migrated codebase

---

## Tools

```bash
# Check specific file for quoted strings
grep -n "'.*'" docs/build/BUILD_SYSTEM.md | grep -v "^#"

# Verify after migration (should return nothing)
grep "''" docs/build/BUILD_SYSTEM.md | grep -v TEXT | grep -v "^#"

# Count remaining files
grep -r "'" playground/.../docs/ --include="*.md" | grep -v TEXT | wc -l
```

---

## Success Criteria

✅ **Migration Complete When:**
1. All 18 files migrated
2. All 447 Boon code blocks use TEXT syntax
3. No quoted strings in code blocks (except inside TEXT)
4. All examples match migrated codebase
5. Verification scripts pass

---

## Next Action

**Start with Group 1:**
1. docs/build/BUILD_SYSTEM.md
2. docs/patterns/LINK_PATTERN.md
3. docs/language/BOON_SYNTAX.md

Ready to begin!
