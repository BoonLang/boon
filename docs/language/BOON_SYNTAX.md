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
title_to_save: TEXT { Hello }
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
    text: TEXT { todos }
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
Element/text(TEXT { Hello })                 -- INVALID: must use text: parameter
```

**Correct versions:**
```boon
-- Option 1: Use named parameter
Theme/material(of: InputInterior[focus: True])
Text/trim(text: user_input)
Element/text(text: TEXT { Hello })

-- Option 2: Use pipe for first argument
Theme/material(of: InputInterior[focus: True])
user_input |> Text/trim()
TEXT { Hello } |> Element/text()  -- Though named is more readable here
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

## 6. All Function Parameters Are Required

**Function parameters in Boon have NO default values and are ALL required.**

You cannot write optional parameters like `function(required, optional = default)`. Every parameter must be provided when calling a function.

### Workarounds for "Optional" Behavior

Since parameters are required, use these patterns for optional-like behavior:

#### Pattern A: Polymorphic Tag Values (for simple cases)

✅ **Correct - Use a tag like `Default` or `None`:**
```boon
FUNCTION spotlight(target, softness) {
    actual_softness: softness |> WHEN {
        Default => 0.85
        Soft => 0.85
        Medium => 0.6
        Sharp => 0.3
        numeric => numeric  -- Pass through numeric values
    }
    -- Use actual_softness...
}

-- Usage:
Light/spotlight(target: FocusedElement, softness: Default)
Light/spotlight(target: FocusedElement, softness: Soft)
Light/spotlight(target: FocusedElement, softness: 0.95)
```

#### Pattern B: Record with Optional Properties (for multiple optional values)

✅ **Correct - Use an empty record or record with overrides:**
```boon
FUNCTION spotlight(of, overrides) {
    target: LATEST {
        FocusedElement  -- Default for FocusSpotlight
        overrides.target
    }
    color: LATEST {
        Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
        overrides.color
    }
    softness: LATEST {
        0.85
        overrides.softness
    }
    intensity: LATEST {
        0.3
        overrides.intensity
    }
    -- Use target, color, softness, intensity...
}

-- Usage:
Theme/light(of: FocusSpotlight, overrides: [])
Theme/light(of: FocusSpotlight, overrides: [softness: 0.95])
Theme/light(of: FocusSpotlight, overrides: [target: hero_element.position, softness: Sharp])
```

❌ **INCORRECT - Optional parameters don't exist:**
```boon
-- BROKEN: No default parameter syntax exists
FUNCTION spotlight(target, softness = 0.85) { ... }
FUNCTION spotlight(target, softness?) { ... }

-- BROKEN: No ?? operator exists (see next section)
FUNCTION spotlight(target, softness) {
    actual_softness: softness ?? 0.85
}
```

---

## 7. No `??` Operator - Use `LATEST` Combinator

**Boon does NOT have a null-coalescing operator (`??`).**

To provide fallback values, use the `LATEST` combinator.

✅ **Correct - Use LATEST for fallback:**
```boon
FUNCTION get_value(overrides) {
    color: LATEST {
        Oklch[lightness: 0.7, chroma: 0.1, hue: 220]  -- Default
        overrides.color                                  -- Override if provided
    }
    softness: LATEST {
        0.85
        overrides.softness
    }
}

-- LATEST takes the most recent value from its reactive expressions
-- If overrides.color is present, it becomes the "latest" value
-- Otherwise, the default value is used
```

✅ **Correct - LATEST with events:**
```boon
value: LATEST {
    Text/empty                               -- Initial value
    input_element.event.change.text          -- Updates on change event
    save_button.event.press |> THEN { Text/empty }  -- Resets on save
}
```

❌ **INCORRECT - ?? operator doesn't exist:**
```boon
-- BROKEN: No ?? operator
color: overrides.color ?? Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
softness: overrides.softness ?? 0.85

-- BROKEN: No || operator
value: overrides.value || default_value
```

**How LATEST works:**
- Takes multiple expressions as a list
- Returns the most recent value that has been produced
- Reactive - updates when any expression produces a new value
- First value in the list is typically the default/initial value

---

## 8. UNPLUGGED State and Optional Field Access

**Boon has a special UNPLUGGED state representing structural absence - missing object fields.**

### What is UNPLUGGED?

UNPLUGGED represents a field that doesn't exist in a record. It's similar to `null`/`undefined` in other languages, but:
- **Explicit:** Only appears with `?` operator
- **Type-safe:** Compiler tracks and prevents unhandled UNPLUGGED
- **Single source:** Only `obj.field?` can produce UNPLUGGED

### Postfix `?` Operator

The `?` operator (postfix) safely accesses potentially missing fields:

✅ **Correct - Postfix `?` operator:**
```boon
-- Access potentially missing field
theme: config.theme?  -- Type: Theme | UNPLUGGED

-- Must handle before use
color: config.theme? |> WHEN {
    UNPLUGGED => default_theme
    value => value
}

-- Chaining for nested optional fields
primary: config.ui?.theme?.primary_color? |> WHEN {
    UNPLUGGED => default_blue
    color => color
}
```

❌ **INCORRECT - Other syntaxes don't exist:**
```boon
theme: config?.theme    -- WRONG: ? is postfix, not prefix
theme: config.theme     -- WRONG: Errors if theme doesn't exist
theme: config.theme ?? default  -- WRONG: No ?? operator
```

### UNPLUGGED Must Be Handled

**You CANNOT use a potentially UNPLUGGED value directly:**

```boon
x: obj.field?
y: x + 1              -- COMPILE ERROR: x might be UNPLUGGED

-- Must handle with WHEN first:
y: x |> WHEN {
    UNPLUGGED => 0
    value => value + 1
}  -- OK: UNPLUGGED handled
```

### Pattern Matching UNPLUGGED

The ONLY way to handle UNPLUGGED is with WHEN pattern matching:

```boon
value: obj.field? |> WHEN {
    UNPLUGGED => default_value  -- Provide fallback
    actual_value => actual_value  -- Use actual value
}
```

**Note:** You cannot write `UNPLUGGED` directly:
```boon
x: UNPLUGGED  -- ERROR: Cannot assign UNPLUGGED
```

UNPLUGGED only appears as a result of `?` operator and must be matched.

### Type Inference and Optimization

The compiler tracks UNPLUGGED through type inference:

```boon
-- Compiler knows field exists - optimizes away ?
obj: [a: 1, b: 2]
x: obj.a?  -- Warning: "? unnecessary, field 'a' always exists"
           -- Optimized to: x: obj.a

-- Compiler knows field missing - type is UNPLUGGED
y: obj.c?  -- Type: UNPLUGGED (c doesn't exist)

-- Compiler tracks through flow
x: obj.field?           -- Type: T | UNPLUGGED
y: x |> WHEN {
    UNPLUGGED => 0
    value => value
}                       -- Type: T (UNPLUGGED handled)
z: y + 1                -- OK: y is definitely T
```

### UNPLUGGED vs LATEST

**Do NOT use LATEST for structural defaults:**

❌ **WRONG - Causes blink and performance issues:**
```boon
softness: LATEST {
    0.85         -- Shows first
    of.softness?  -- Then shows second
}
-- Problem: Shows 0.85, then switches to actual value = UI blink!
```

✅ **CORRECT - Use WHEN for structural alternatives:**
```boon
softness: of.softness? |> WHEN {
    UNPLUGGED => 0.85
    value => value
}
-- Evaluated once, no blink, semantically correct
```

**When to use each:**
- **LATEST:** Temporal reactive values (events, changing data over time)
- **WHEN + UNPLUGGED:** Structural alternatives (defaults for missing fields)

### Examples

#### Example 1: Optional Configuration
```boon
-- Loading user configuration
config: load_user_config()

-- Safe access with defaults
theme: config.theme? |> WHEN {
    UNPLUGGED => Professional
    t => t
}

font_size: config.ui?.font_size? |> WHEN {
    UNPLUGGED => 14
    size => size
}
```

#### Example 2: Data Migration
```boon
-- Handle renamed fields gracefully
user_name: user.name? |> WHEN {
    UNPLUGGED => user.display_name? |> WHEN {
        UNPLUGGED => "Anonymous"
        name => name
    }
    name => name
}
```

#### Example 3: Theme System Overrides
```boon
FUNCTION light(of, with) {
    of |> WHEN {
        FocusSpotlight => BLOCK {
            -- Access optional override fields
            softness: with.softness? |> WHEN {
                UNPLUGGED => 0.85  -- Theme default
                value => value
            }

            target: with.target? |> WHEN {
                UNPLUGGED => FocusedElement  -- Semantic default
                value => value
            }
        }
    }
}

-- Usage:
Theme/light(of: FocusSpotlight, with: [])  -- Uses all defaults
Theme/light(of: FocusSpotlight, with: [softness: 0.95])  -- Override softness
```

---

## 9. Partial Pattern Matching for Tagged Objects

**A bare tag pattern matches the tag regardless of what fields it has.**

### Basic Behavior

```boon
light |> WHEN {
    FocusSpotlight => handle_focus(light)
    -- Matches: FocusSpotlight, FocusSpotlight[], FocusSpotlight[softness: 0.95], etc.
}
```

Without partial matching, you'd need to match every field combination:
```boon
light |> WHEN {
    FocusSpotlight => ...
    FocusSpotlight[softness] => ...
    FocusSpotlight[target] => ...
    FocusSpotlight[softness, target] => ...
    -- Combinatorial explosion!
}
```

### Accessing Fields with `?`

Inside a partial match, use `?` to access potentially missing fields:

```boon
FUNCTION process(config) {
    config |> WHEN {
        AppConfig => BLOCK {
            -- config matches AppConfig with any fields
            theme: config.theme? |> WHEN {
                UNPLUGGED => Professional
                t => t
            }

            mode: config.mode? |> WHEN {
                UNPLUGGED => Light
                m => m
            }
        }
    }
}

-- Works with any field combination:
process(config: AppConfig)
process(config: AppConfig[theme: Dark])
process(config: AppConfig[theme: Dark, mode: Dark])
```

### Why This Works

Combining partial matching + UNPLUGGED + `?` operator eliminates need for:
- Spread syntax (`Tag[...fields]`)
- Combinatorial pattern matching
- Magic field extraction

Each field is accessed explicitly with clear defaults.

---

## 10. Module System

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
| **Function parameters** | All required, no defaults | Use `Default` tag or `with: []` record |
| **Optional fields** | Use `obj.field?` postfix | Returns `T \| UNPLUGGED`, must handle with WHEN |
| **Fallback values (temporal)** | Use `LATEST` | `LATEST { default, event_value }` ✅ |
| **Fallback values (structural)** | Use `WHEN + UNPLUGGED` | `obj.field? \|> WHEN { UNPLUGGED => default, x => x }` |
| **Partial pattern matching** | Bare tag matches any fields | `FocusSpotlight =>` matches all variants |
| **Function calls** | Module/function() syntax | `Theme/material()`, `Text/trim()` |
| **Function definitions** | Root level only, not first-class | `FUNCTION material(m) { ... }` |
| **Records** | Data only, no functions | `[corners: [round: 6]]` ✅, `[fn: FUNCTION...]` ❌ |
| **Type declarations** | Don't exist - tags are inferred | Just use `All`, `InputInterior[focus: True]` |
| **Type tracking** | Full inference with UNPLUGGED | Compiler tracks all UNPLUGGED through flow |
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

7. ✅ **All function parameters are required**
   - No optional parameters or default values
   - Use polymorphic tags (`Default`, `None`) or records (`overrides: []`) for optional-like behavior

8. ✅ **Use LATEST for fallback values**
   - No `??` or `||` operators exist
   - `LATEST { default_value, override_value }` pattern for defaults

9. ✅ **Use `obj.field?` for optional fields**
   - Postfix `?` operator accesses potentially missing fields
   - Returns `T | UNPLUGGED` which must be handled with WHEN
   - Do NOT use LATEST for structural defaults (causes blink/performance issues)

10. ✅ **Use partial pattern matching + UNPLUGGED for flexible APIs**
   - Bare tag matches any field combination
   - Access fields individually with `?` operator
   - Explicit defaults for each field
   - No need for spread syntax or combinatorial patterns

---

## Verified Against Codebase

These rules have been verified against:
- `/playground/frontend/src/examples/todo_mvc_physical/todo_mvc_physical.bn` (699 lines)
- `/playground/frontend/src/examples/todo_mvc_physical/Theme/*.bn` (5 theme files)
- `/playground/frontend/src/examples/todo_mvc/*.bn` (multiple example files)
- All example projects in `/playground/frontend/src/examples/`

**Last updated:** 2025-11-12

**Recent additions:**
- UNPLUGGED state and postfix `?` operator
- Partial pattern matching for tagged objects
- Type inference and compile-time UNPLUGGED tracking
- Clear distinction between LATEST (temporal) and WHEN+UNPLUGGED (structural)
