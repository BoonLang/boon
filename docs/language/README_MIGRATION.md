# TEXT Syntax Migration - Getting Started

**Welcome to the TEXT Syntax Migration!**

This directory contains everything you need to migrate your Boon codebase from single-quoted strings to the new TEXT syntax.

---

## Quick Start

### 1. Read the Specification
📖 **[TEXT_SYNTAX.md](./TEXT_SYNTAX.md)** - Complete specification with examples

### 2. Read the Migration Guide
📋 **[MIGRATION_TEXT_SYNTAX.md](./MIGRATION_TEXT_SYNTAX.md)** - Step-by-step migration instructions

### 3. Check Current Status
📊 **[MIGRATION_STATUS.md](./MIGRATION_STATUS.md)** - Current progress and next steps

---

## Tools Available

### Verification Script
Check what still needs migration:
```bash
./scripts/check_text_migration.sh
```

### Migration Report Generator
Generate detailed report of what needs migrating:
```bash
./scripts/generate_migration_report.sh
```

---

## Current Status (2025-11-15)

### ✅ Completed
- **Specification**: Complete (923 lines)
- **Migration Guide**: Complete
- **Migration Tools**: 2 scripts created
- **Files Migrated**: 4 simple examples
  - `hello_world.bn` ✅
  - `fibonacci.bn` ✅
  - `counter.bn` ✅
  - `complex_counter.bn` ✅

### ⚠️ Critical - Do This FIRST!
**Phase 1: Function Calls**
- Replace `Text/empty()` → `Text/is_empty()` in:
  - `todo_mvc.bn` (2 occurrences)
  - `todo_mvc_physical/RUN.bn` (2 occurrences)

### ⏳ Pending
**Phase 2: String Literals**
- ~76 quoted strings remaining across ~12 files
- Major files: `todo_mvc.bn`, `todo_mvc_physical/RUN.bn`

---

## Migration Workflow

```
1. Read TEXT_SYNTAX.md
   └─> Understand the new syntax

2. Run verification script
   └─> See what needs migration

3. Phase 1: Fix function calls FIRST
   └─> Text/empty() → Text/is_empty()

4. Phase 2: Migrate string literals
   ├─> Empty strings: '' → Text/empty
   ├─> Single chars: '+' → TEXT { + }
   ├─> Simple text: 'hello' → TEXT { hello }
   ├─> Interpolation: '{x}' → TEXT { {x} }
   └─> Multiline: Use TEXT { \n ... \n }

5. Verify migration
   └─> Run verification script again

6. Test & commit
   └─> Ensure everything works
```

---

## Key Syntax Changes

| Old | New | Notes |
|-----|-----|-------|
| `''` | `Text/empty` | Recommended over `TEXT {}` |
| `'text'` | `TEXT { text }` | Must have padding |
| `'+'` | `TEXT { + }` | Single char needs padding |
| `'{x}'` | `TEXT { {x} }` | Interpolation unchanged |
| `'don\'t'` | `TEXT { don't }` | No escaping! |
| `Text/empty()` | `Text/is_empty()` | **Critical - do first!** |

---

## Quick Examples

### Before → After

```boon
-- Empty string
'' → Text/empty

-- Simple text
'Hello' → TEXT { Hello }

-- Single character
'+' → TEXT { + }

-- Interpolation
'{count} items' → TEXT { {count} items }

-- No escaping needed
'don\'t' → TEXT { don't }

-- Multiline
'Line 1\nLine 2' → TEXT {
    Line 1
    Line 2
}

-- Function call (CRITICAL - do first!)
text |> Text/empty() → text |> Text/is_empty()
```

---

## Migration by File

### Simple Examples (✅ Done)
- ✅ `hello_world.bn` - 1 string
- ✅ `fibonacci.bn` - 1 interpolated string
- ✅ `counter.bn` - 1 single char
- ✅ `complex_counter.bn` - 2 single chars

### Complex Examples (⏳ Pending)
- ⏳ `todo_mvc.bn` - ~40 strings (Phase 1 critical!)
- ⏳ `todo_mvc_physical/RUN.bn` - ~30 strings (Phase 1 critical!)
- ⏳ `when/when.bn` - ~5 strings
- ⏳ `then/then.bn` - ~4 strings
- ⏳ `while/while.bn` - ~5 strings
- ⏳ `latest/latest.bn` - ~3 strings

### Generated Files
- ✅ `BUILD.bn` - Already using TEXT syntax!
- ⏳ `Generated/Assets.bn` - 2 data URLs

---

## Common Mistakes to Avoid

### ❌ Missing Padding
```boon
TEXT {+}          -- WRONG
TEXT { + }        -- CORRECT
```

### ❌ Spaces in Interpolation
```boon
TEXT { { x } }    -- WRONG
TEXT { {x} }      -- CORRECT
```

### ❌ Using Old Function Name
```boon
Text/empty()      -- WRONG (conflicts with constant)
Text/is_empty()   -- CORRECT
```

---

## Next Steps

1. **Read** [TEXT_SYNTAX.md](./TEXT_SYNTAX.md) (10-15 minutes)
2. **Run** `./scripts/check_text_migration.sh` to see current state
3. **Start** with Phase 1 (critical function calls)
4. **Continue** with Phase 2 (string literals)
5. **Verify** with scripts after each phase

---

## Help & Support

- **Specification**: [TEXT_SYNTAX.md](./TEXT_SYNTAX.md)
- **Migration Guide**: [MIGRATION_TEXT_SYNTAX.md](./MIGRATION_TEXT_SYNTAX.md)
- **Current Status**: [MIGRATION_STATUS.md](./MIGRATION_STATUS.md)
- **Verification**: Run `./scripts/check_text_migration.sh`

---

**Good luck with the migration! 🚀**

The TEXT syntax will make your code cleaner, more readable, and easier to maintain.
