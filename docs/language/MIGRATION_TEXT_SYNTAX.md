# Migration Guide: String Literals → TEXT Syntax

**Status**: Migration Guide
**Target**: Boon codebase migration from `'quoted strings'` to `TEXT { }` syntax
**Date**: 2025-11-15

---

## Quick Reference

| Old Syntax | New Syntax | Notes |
|------------|------------|-------|
| `''` | `Text/empty` | Use constant (recommended) |
| `'text'` | `TEXT { text }` | Add padding |
| `'+'` | `TEXT { + }` | Single char needs padding |
| `'{x}'` | `TEXT { {x} }` | Interpolation works the same |
| `'don\'t'` | `TEXT { don't }` | No escaping needed! |
| `'Line 1\nLine 2'` | `TEXT {\n    Line 1\n    Line 2\n}` | Use multiline |
| `Text/empty()` | `Text/is_empty()` | Function renamed |
| `Text/empty() \|> Bool/not()` | `Text/is_not_empty()` | New predicate |

---

## Migration Strategy

### Phase 1: Update Text Module Functions (Do This First!)

**Critical:** Migrate function calls before migrating string literals to avoid breaking code.

#### 1.1 Replace `Text/empty()` → `Text/is_empty()`

```boon
-- BEFORE:
text |> Text/empty() |> WHEN { True => ..., False => ... }
text |> Text/trim() |> Text/empty()

-- AFTER:
text |> Text/is_empty() |> WHEN { True => ..., False => ... }
text |> Text/trim() |> Text/is_empty()
```

**Search pattern:** `Text/empty()`
**Replace with:** `Text/is_empty()`

#### 1.2 Replace `Text/empty() |> Bool/not()` → `Text/is_not_empty()`

```boon
-- BEFORE:
text
    |> Text/trim()
    |> Text/empty()
    |> Bool/not()
    |> WHEN { True => ..., False => ... }

-- AFTER:
text
    |> Text/trim()
    |> Text/is_not_empty()
    |> WHEN { True => ..., False => ... }
```

**Search pattern:** `Text/empty() |> Bool/not()`
**Replace with:** `Text/is_not_empty()`

---

### Phase 2: Migrate String Literals

#### 2.1 Empty Strings

**Pattern:** All occurrences of `''`

```boon
-- BEFORE:
edited_title: LATEST {
    ''
    element.event.change.text
    title_to_save |> THEN { '' }
}

-- AFTER:
edited_title: LATEST {
    Text/empty
    element.event.change.text
    title_to_save |> THEN { Text/empty }
}
```

**Files affected:** All `.bn` files
**Search:** `''`
**Replace:** `Text/empty`

**Example locations from codebase:**
- `todo_mvc.bn:107` - Initial value
- `todo_mvc.bn:109` - Reset value
- `todo_mvc.bn:237` - Text input initial
- `todo_mvc.bn:239` - Text input reset

#### 2.2 Single Character Strings

**Pattern:** Single chars like `'+'`, `'-'`, `'s'`, `'>'`, `'×'`

```boon
-- BEFORE:
label: '+'
label: '-'
maybe_s: count > 1 |> WHEN { True => 's', False => '' }
child: '>'
label: '×'

-- AFTER:
label: TEXT { + }
label: TEXT { - }
maybe_s: count > 1 |> WHEN {
    True => TEXT { s }
    False => Text/empty
}
child: TEXT { > }
label: TEXT { × }
```

**Note:** Single chars MUST have padding: `TEXT { x }` not `TEXT {x}`

**Example locations:**
- `counter.bn:21` - Button label `'+'`
- `complex_counter.bn:24,26` - Labels `'-'` and `'+'`
- `todo_mvc.bn:320` - Arrow `'>'`
- `todo_mvc.bn:422` - Close button `'×'`
- `todo_mvc.bn:464` - Plural suffix `'s'`

#### 2.3 Simple Text Strings

**Pattern:** Short text without special characters

```boon
-- BEFORE:
root: 'Hello world!'
child: 'todos'
label: 'All'
label: 'Active'
label: 'Completed'
label: 'Clear completed'
label: 'Toggle all'

-- AFTER:
root: TEXT { Hello world! }
child: TEXT { todos }
label: TEXT { All }
label: TEXT { Active }
label: TEXT { Completed }
label: TEXT { Clear completed }
label: TEXT { Toggle all }
```

**Example locations:**
- `hello_world.bn:4` - `'Hello world!'`
- `todo_mvc.bn:180` - `'todos'`
- `todo_mvc.bn:517-519` - Filter labels
- `todo_mvc.bn:534` - `'Clear completed'`
- `todo_mvc.bn:304` - `'Toggle all'`

#### 2.4 Strings with Apostrophes (No More Escaping!)

**Pattern:** Text containing apostrophes that needed `\'` escaping

```boon
-- BEFORE:
text: 'I don\'t like it'
text: 'It\'s working'
text: 'User\'s profile'

-- AFTER:
text: TEXT { I don't like it }
text: TEXT { It's working }
text: TEXT { User's profile }
```

**Benefit:** Apostrophes work naturally without escaping!

#### 2.5 Path and URL Strings

**Pattern:** Paths like `'/'`, `'/active'`, `'/completed'`

```boon
-- BEFORE:
Router/route() |> WHEN {
    '/active' => Active
    '/completed' => Completed
    __ => All
}

filter_buttons.all.event.press |> THEN { '/' }
filter_buttons.active.event.press |> THEN { '/active' }
filter_buttons.completed.event.press |> THEN { '/completed' }

-- AFTER:
Router/route() |> WHEN {
    TEXT { /active } => Active
    TEXT { /completed } => Completed
    __ => All
}

filter_buttons.all.event.press |> THEN { TEXT { / } }
filter_buttons.active.event.press |> THEN { TEXT { /active } }
filter_buttons.completed.event.press |> THEN { TEXT { /completed } }
```

**Example locations:**
- `todo_mvc.bn:13-15` - Router paths
- `todo_mvc.bn:19-21` - Navigation events

#### 2.6 Strings with Interpolation

**Pattern:** Strings with `{variable}` interpolation

```boon
-- BEFORE:
'{count} item{maybe_s} left'
'{position}. Fibonacci number is {result}'
'Found {count} items'

-- AFTER:
TEXT { {count} item{maybe_s} left }
TEXT { {position}. Fibonacci number is {result} }
TEXT { Found {count} items }
```

**Important:** Interpolation syntax `{var}` stays the same!

**Example locations:**
- `todo_mvc.bn:465` - `'{count} item{maybe_s} left'`
- `fibonacci.bn:12` - `'{position}. Fibonacci number is {result}'`

#### 2.7 Long Text Content

**Pattern:** Longer descriptive text

```boon
-- BEFORE:
text: 'What needs to be done?'
label: Hidden[text: 'What needs to be done?']
contents: LIST { 'Double-click to edit a todo' }
contents: LIST { 'Created by ' }
contents: LIST { 'Part of ' }

-- AFTER:
text: TEXT { What needs to be done? }
label: Hidden[text: TEXT { What needs to be done? }]
contents: LIST { TEXT { Double-click to edit a todo } }
contents: LIST { TEXT { Created by } }
contents: LIST { TEXT { Part of } }
```

**Example locations:**
- `todo_mvc.bn:235,243` - Placeholder text
- `todo_mvc.bn:548` - Help text
- `todo_mvc.bn:554,565` - Footer text

#### 2.8 Data URLs (Keep As-Is)

**Pattern:** Long encoded URLs

```boon
-- BEFORE:
icon: 'data:image/svg+xml;utf8,%3Csvg%20xmlns%3D%22http%3A//...'

-- AFTER:
icon: TEXT { data:image/svg+xml;utf8,%3Csvg%20xmlns%3D%22http%3A//... }
```

**Note:** No special escaping needed - just wrap with `TEXT { }`

**Example locations:**
- `todo_mvc.bn:353-354` - SVG data URLs

#### 2.9 Font Family Lists

**Pattern:** Lists of font names

```boon
-- BEFORE:
font: [family: LIST { 'Helvetica Neue', 'Helvetica', 'Arial', SansSerif }]

-- AFTER:
font: [family: LIST {
    TEXT { Helvetica Neue }
    TEXT { Helvetica }
    TEXT { Arial }
    SansSerif
}]
```

**Example locations:**
- `todo_mvc.bn:137` - Font family list

#### 2.10 Hidden/Reference Labels

**Pattern:** Accessibility labels

```boon
-- BEFORE:
label: Hidden[text: 'What needs to be done?']
label: Hidden[text: 'Toggle all']
label: Hidden[text: 'selected todo title']

-- AFTER:
label: Hidden[text: TEXT { What needs to be done? }]
label: Hidden[text: TEXT { Toggle all }]
label: Hidden[text: TEXT { selected todo title }]
```

**Example locations:**
- `todo_mvc.bn:235` - New todo input
- `todo_mvc.bn:304` - Toggle all checkbox
- `todo_mvc.bn:344` - Editing input

---

## Migration Checklist

### Pre-Migration
- [ ] Read TEXT_SYNTAX.md specification
- [ ] Backup all `.bn` files
- [ ] Create migration branch: `git checkout -b migrate-text-syntax`

### Phase 1: Functions (CRITICAL - Do First!)
- [ ] Find all `Text/empty()` → Replace with `Text/is_empty()`
- [ ] Find all `Text/empty() |> Bool/not()` → Replace with `Text/is_not_empty()`
- [ ] Test that code still compiles

### Phase 2: String Literals
- [ ] Migrate empty strings: `''` → `Text/empty`
- [ ] Migrate single chars: `'x'` → `TEXT { x }`
- [ ] Migrate simple text: `'text'` → `TEXT { text }`
- [ ] Migrate paths: `'/'` → `TEXT { / }`
- [ ] Migrate interpolated: `'{x}'` → `TEXT { {x} }`
- [ ] Migrate long text
- [ ] Migrate data URLs
- [ ] Migrate font families
- [ ] Migrate labels

### Post-Migration
- [ ] Run tests (if available)
- [ ] Manual review of examples
- [ ] Check for any missed strings: `grep -r "'" --include="*.bn"`
- [ ] Commit changes: `git commit -m "Migrate to TEXT syntax"`

---

## Common Pitfalls

### ❌ Forgetting Padding

```boon
-- WRONG:
label: TEXT {+}

-- CORRECT:
label: TEXT { + }
```

**Error:** `Missing padding. Content must have exactly one space on each side.`

### ❌ Using TEXT {} Instead of Text/empty

```boon
-- DISCOURAGED (but valid):
initial: TEXT {}

-- RECOMMENDED:
initial: Text/empty
```

**Why:** `Text/empty` is clearer and more searchable.

### ❌ Adding Spaces in Interpolation

```boon
-- WRONG:
TEXT { Hello { name }! }
          ^     ^

-- CORRECT:
TEXT { Hello {name}! }
```

**Rule:** No spaces inside interpolation braces.

### ❌ Using Old Function Names

```boon
-- WRONG:
text |> Text/empty()

-- CORRECT:
text |> Text/is_empty()
```

**Error:** `Text/empty` is a constant, not a function!

---

## File-by-File Migration Plan

### Priority Order

1. **Phase 1: Update function calls**
   - Search entire codebase for `Text/empty()` and `Text/empty() |> Bool/not()`
   - This is critical to avoid conflicts with new `Text/empty` constant

2. **Phase 2a: High-impact files** (most string usage)
   - `todo_mvc.bn` (~50+ strings)
   - `todo_mvc_physical/*.bn` files

3. **Phase 2b: Example files**
   - `hello_world.bn`
   - `counter.bn`
   - `complex_counter.bn`
   - `fibonacci.bn`
   - Other examples

4. **Phase 2c: Generated/BUILD files**
   - Note: `BUILD.bn` already uses TEXT syntax! ✅

---

## Verification Script

```bash
#!/bin/bash
# Check for remaining old syntax

echo "=== Checking for old string literals ==="
grep -rn "'" playground/frontend/src/examples --include="*.bn" | grep -v "TEXT {" | grep -v "^--"

echo ""
echo "=== Checking for old Text/empty() function ==="
grep -rn "Text/empty()" playground/frontend/src/examples --include="*.bn"

echo ""
echo "=== Checking for Text/empty() |> Bool/not() ==="
grep -rn "Text/empty().*Bool/not()" playground/frontend/src/examples --include="*.bn"

echo ""
echo "=== Checking for missing padding (TEXT {x}) ==="
grep -rn "TEXT {[^}]*}" playground/frontend/src/examples --include="*.bn" | grep -v "TEXT { .* }"
```

---

## Benefits After Migration

✅ **No more escape sequences**: `don't` just works
✅ **Consistent syntax**: Same for simple to complex strings
✅ **Better readability**: `Text/empty` vs `''`
✅ **Multiline ready**: Natural newline handling
✅ **Visual indentation**: Code generation templates show structure
✅ **Type safety**: TEXT is a language construct, not just a string
✅ **Better tooling**: IDEs can provide better support for TEXT blocks

---

## Support

**Questions?** See [TEXT_SYNTAX.md](./TEXT_SYNTAX.md) for full specification
**Issues?** Check the "Common Pitfalls" section above
**Examples?** See "Complete Examples" section in TEXT_SYNTAX.md

---

**Last Updated:** 2025-11-15
**Next Steps:** Begin Phase 1 migration of function calls!
