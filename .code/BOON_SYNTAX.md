# Boon Language Syntax Rules

This document describes the core syntax rules and conventions of the Boon programming language. These rules have been verified against the codebase and must be followed when writing or refactoring Boon code.

---

## 1. Naming Conventions

### Functions and Variables: snake_case ONLY

**All function names and variable names MUST use snake_case.**

✅ **Correct:**
```boon
FUNCTION new_todo(title) { ... }
FUNCTION create_scene() { ... }
FUNCTION root_element() { ... }

store: [...]
selected_filter: Active
go_to_result: Router/go_to()
title_to_save: "Hello"
```

❌ **INCORRECT - These will NOT work:**
```boon
FUNCTION NewTodo(title) { ... }      -- PascalCase not allowed
FUNCTION createScene() { ... }       -- camelCase not allowed
FUNCTION CreateScene() { ... }       -- PascalCase not allowed

Store: [...]                         -- PascalCase not allowed
selectedFilter: Active               -- camelCase not allowed
```

### Tags and Tagged Objects: PascalCase

Tags (type variants) use PascalCase:
```boon
Active
Completed
All
Light
Dark
Professional
Neobrutalism
InputInterior
ButtonDelete
TodoId
```

---

## 2. Function Arguments Must Be Named (Except When Piped)

**ALL function arguments MUST be named, with ONE exception: the first argument when piped.**

✅ **Correct - Named arguments:**
```boon
Element/text(
    element: [tag: H1]
    style: [font: Theme/font(of: Header)]
    text: 'todos'
)

Scene/new(
    root: root_element(PASS: [store: store])
    lights: Theme/lights()
    geometry: Theme/geometry()
)

List/append(item: new_todo)
List/retain(item, if: item.completed)
```

✅ **Correct - Piped first argument (unnamed):**
```boon
Theme/font(of: Header)
Theme/material(of: Panel)
user_input |> Text/trim()
is_empty |> Bool/not()
route |> Router/go_to()
```

❌ **INCORRECT - Positional arguments not allowed:**
```boon
Theme/material(InputInterior[focus: True])  -- INVALID: argument must be named
Text/trim(user_input)                        -- INVALID: must pipe or use text: parameter
Element/text('Hello')                        -- INVALID: must use text: parameter
```

**Correct versions:**
```boon
-- Option 1: Use named parameter
Theme/material(of: InputInterior[focus: True])
Text/trim(text: user_input)
Element/text(text: 'Hello')

-- Option 2: Use pipe for first argument
Theme/material(of: InputInterior[focus: True])
user_input |> Text/trim()
'Hello' |> Element/text()  -- Though named is more readable here
```

---

## 3. Functions Are Not First-Class

**Functions CANNOT be:**
- Assigned to variables
- Passed as arguments
- Returned from other functions
- Defined inside other functions

**Functions MUST be:**
- Defined at the module/file root level
- Called using `Module/function()` syntax

✅ **Correct:**
```boon
-- File: Themes.bn
FUNCTION material(material) {
    PASSED.theme_options.name |> WHEN {
        Professional => material |> Professional/material()
        Neobrutalism => material |> Neobrutalism/material()
    }
}

-- Usage:
Theme/material(of: Panel)
InputInterior[focus: True] |> Professional/material()
```

❌ **INCORRECT - These will NOT work:**
```boon
-- Cannot assign function to variable
resolver: Theme/material
material: resolver(Panel)

-- Cannot define function inside function
FUNCTION outer() {
    FUNCTION inner() { ... }  -- NOT ALLOWED
}

-- Cannot use bare function name (must use Module/function syntax)
theme: material(Panel)  -- ERROR: Which module is "material" from?
```

**Correct way to call functions:**
```boon
-- Always use Module/function() syntax
result: MyModule/my_function(arg1, arg2)
text: Text/trim(user_input)
route: Router/route()
id: Ulid/generate()
```

---

## 4. Tags and Tagged Objects Are Inferred, Not Declared

**There is NO type system with explicit type declarations.**

Tags and tagged objects are inferred from usage - you don't (and can't) declare them with `TYPE`, `MODULE`, `CLASS`, or similar keywords.

✅ **Correct - Just use tags directly:**
```boon
-- Simple tags
selected_filter: All
selected_filter: Active
selected_filter: Completed

mode: Light
mode: Dark

theme_name: Professional
theme_name: Neobrutalism

-- Tagged objects with fields
todo_id: TodoId[id: Ulid/generate()]
material: InputInterior[focus: True]
material: ButtonDelete[hover: element.hovered]
material: TodoCheckbox[checked: todo.completed, hover: element.hovered]

-- Pattern matching on tags
selected_filter |> WHEN {
    All => show_all_todos()
    Active => show_active_todos()
    Completed => show_completed_todos()
}

-- Pattern matching on tagged objects
material |> WHEN {
    InputInterior[focus] => [
        gloss: focus |> WHEN { True => 0.15, False => 0.65 }
    ]
    ButtonDelete[hover] => [
        gloss: hover |> WHEN { True => 0.35, False => 0.25 }
    ]
    Panel => [gloss: 0.12]
}
```

❌ **INCORRECT - These declarations don't exist in Boon:**
```boon
-- NO TYPE SYSTEM - These are not valid Boon syntax
TYPE Filter {
    All
    Active
    Completed
}

TYPE Material {
    InputInterior[focus: Bool]
    ButtonDelete[hover: Bool]
    Panel
}

MODULE Material {
    TYPE Variant { ... }
}

CLASS TodoId { ... }

CONST MAX_TODOS = 100
```

**How tags work:**
- Tags are just used - if you write `All`, it's a tag
- Tagged objects have the syntax: `TagName[field1: value1, field2: value2]`
- Pattern matching automatically destructures tagged objects
- The Boon compiler/runtime infers tag types from usage

---

---

## 5. Records Can Only Contain Data, Not Functions

**Records (dictionaries/objects) can ONLY contain:**
- Primitive values (numbers, strings, booleans)
- Other records (nested)
- Lists
- Tags and tagged objects

**Records CANNOT contain:**
- Functions (because functions are not first-class)

✅ **Correct - Static data in records:**
```boon
theme: [
    corners: [
        round: 6,
        sharp: 0,
        standard: 4,
        subtle: 2,
        full: Fully
    ],
    depth: [
        major: 8,
        standard: 6,
        subtle: 2
    ],
    materials: [
        Panel: [color: Oklch[...], gloss: 0.12, metal: 0.02],
        Background: [color: Oklch[...]]
    ]
]

-- Access static values
corners: PASSED.theme.corners.round  -- Returns: 6
material: PASSED.theme.materials.Panel  -- Returns: [color: ..., gloss: 0.12, ...]
```

❌ **INCORRECT - Cannot store functions in records:**
```boon
theme: [
    materials: [
        -- BROKEN: Cannot store function in record!
        InputInterior: FUNCTION(focus) {
            focus |> WHEN { True => [gloss: 0.15], False => [gloss: 0.65] }
        }
    ]
]

-- BROKEN: InputInterior is not a function, it's just data
material: PASSED.theme.materials.InputInterior(element.focused)
```

**Implication for stateful theme values:**

For theme values that depend on state (like `InputInterior[focus: element.focused]`), you CANNOT pre-compute them in a record. You MUST use function calls:

✅ **Correct - Functions for stateful values:**
```boon
-- Must call function to get stateful material
material: Theme/material(InputInterior[focus: element.focused])
material: Theme/material(ButtonDelete[hover: element.hovered])
```

❌ **Incorrect - Cannot access stateful values from record:**
```boon
-- BROKEN: theme.materials.InputInterior is just data, not callable
material: PASSED.theme.materials.InputInterior(element.focused)
```

**Valid hybrid approach:**

You CAN use records for STATIC values and functions for STATEFUL values:

```boon
-- Theme record with static values only
theme: [
    corners: [round: 6, sharp: 0, standard: 4],
    depth: [major: 8, standard: 6],
    elevation: [card: 50, raised: 4, grounded: 0]
]

-- Usage:
corners: PASSED.theme.corners.round          -- Static: OK from record
depth: PASSED.theme.depth.major              -- Static: OK from record
material: Theme/material(InputInterior[focus: element.focused])  -- Stateful: Must use function
font: Theme/font(Header)                    -- Could be static or function depending on design
```

---

## 6. Module System

**Modules are files.** Each `.bn` file is a module, and functions are called using the file name:

```
Themes.bn          → Theme/material()
Professional.bn    → Professional/material()
Element.bn         → Element/block(), Element/text()
Router.bn          → Router/route(), Router/go_to()
Text.bn            → Text/trim(), Text/empty()
```

---

## 7. Correct Examples Based on These Rules

### Example 1: Unified Theme Router

Given the rules above, here's how a unified theme API would work:

✅ **Correct Boon syntax with named parameters:**
```boon
-- Option A: Simple function per category (must use named parameter)
material: Theme/material(of: InputInterior[focus: element.focused])
corners: Theme/corners(variant: Round)
font: Theme/font(font: Header)

-- Option B: Unified function with nested tagged parameter
material: Theme/value(token: Material[variant: InputInterior[focus: element.focused]])
corners: Theme/value(token: Corners[variant: Round])
font: Theme/value(token: Font[variant: Header])

-- Option C: Two-parameter function
material: Theme/value(group: Material, token: InputInterior[focus: element.focused])
corners: Theme/value(group: Corners, token: Round)

-- Option D: Keep current piped pattern (no change needed)
material: Theme/material(of: InputInterior[focus: element.focused])
corners: Round |> Theme/corners()
font: Theme/font(of: Header)
```

### Example 2: Hybrid Approach (Records for Static, Functions for Stateful)

✅ **Correct - using records for static values, functions for stateful:**
```boon
-- Generate theme record with ONLY static values
theme: Professional/static_theme_values(PASSED.theme_options.mode)
-- Returns: [
--     corners: [round: 6, sharp: 0, standard: 4],
--     depth: [major: 8, standard: 6],
--     elevation: [card: 50, raised: 4]
-- ]

-- Pass via context
Scene/new(
    root: root_element(PASS: [theme: theme])
)

-- Access theme properties
Element/text_input(
    style: [
        corners: PASSED.theme.corners.round              -- Static from record
        depth: PASSED.theme.depth.standard               -- Static from record
        material: Theme/material(InputInterior[focus: element.focused])  -- Stateful via function
    ]
)

-- Local alias for brevity (snake_case!)
t: PASSED.theme
style: [
    corners: t.corners.round                            -- Static: 6
    depth: t.depth.standard                             -- Static: 6
    material: Theme/material(Panel)                    -- Stateful: function call
]
```

---

## Summary

| Rule | Requirement | Example |
|------|-------------|---------|
| **Function/variable naming** | snake_case ONLY | `new_todo`, `selected_filter`, `title_to_save` |
| **Tag naming** | PascalCase | `Active`, `Light`, `InputInterior`, `TodoId` |
| **Function arguments** | Must be named (except first when piped) | `f(x: 1)` ✅, `f(1)` ❌, `1 \|> f()` ✅ |
| **Function calls** | Module/function() syntax | `Theme/material()`, `Text/trim()` |
| **Function definitions** | Root level only, not first-class | `FUNCTION material(m) { ... }` |
| **Records** | Data only, no functions | `[corners: [round: 6]]` ✅, `[fn: FUNCTION...]` ❌ |
| **Type declarations** | Don't exist - tags are inferred | Just use `All`, `InputInterior[focus: True]` |
| **Modules** | One file = one module | `Themes.bn` → `Theme/material()` |

---

## When Designing New APIs

When proposing new Boon APIs, remember:

1. ✅ **Use snake_case for all functions and variables**
   - `theme_value()` not `ThemeValue()` or `themeValue()`

2. ✅ **Use PascalCase for tags only**
   - `InputInterior`, `Round`, `Header`

3. ✅ **Functions must be called with Module/ prefix**
   - `Theme/material()` not `material()`

4. ❌ **Cannot store functions in records or variables**
   - Functions are not first-class - they cannot be assigned, passed, or stored
   - Records can only contain data (values, nested records, lists, tags)

5. ❌ **Cannot create callable values from records**
   - `material: record.function(arg)` doesn't work unless `record.function` is data, not a function
   - For stateful values, you MUST use function calls: `Theme/material(InputInterior[focus: True])`

6. ❌ **Cannot define types or modules inline**
   - No `TYPE`, `MODULE`, `CLASS`, `CONST` declarations

---

## Verified Against Codebase

These rules have been verified against:
- `/playground/frontend/src/examples/todo_mvc_physical/todo_mvc_physical.bn` (699 lines)
- `/playground/frontend/src/examples/todo_mvc_physical/Theme/*.bn` (5 theme files)
- `/playground/frontend/src/examples/todo_mvc/*.bn` (multiple example files)
- All example projects in `/playground/frontend/src/examples/`

**Last updated:** 2025-11-10
