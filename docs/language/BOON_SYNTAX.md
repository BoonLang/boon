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

#### Pattern B: Record with Spread Operator (for multiple optional values)

✅ **Correct - Use spread operator with defaults:**
```boon
FUNCTION spotlight(of, overrides) {
    config: [
        target: FocusedElement
        color: Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
        softness: 0.85
        intensity: 0.3
        ...overrides  -- Override any matching fields
    ]
    -- Use config.target, config.color, config.softness, config.intensity...
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

## 7. No `??` Operator - Use Spread Operator or `LATEST`

**Boon does NOT have a null-coalescing operator (`??`).**

To provide fallback values:
- **For structural defaults (missing fields):** Use spread operator or `WHEN + UNPLUGGED`
- **For temporal reactive values (events):** Use `LATEST` combinator

✅ **Correct - Use spread operator for structural defaults:**
```boon
FUNCTION get_value(overrides) {
    [
        color: Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
        softness: 0.85
        ...overrides  -- Override any matching fields
    ]
}

-- Spread operator merges records (defaults first, overrides last)
-- If overrides.color exists, it overrides the default
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

**When to use each:**
- **Spread operator:** For structural defaults (function parameters with overrides)
- **WHEN + UNPLUGGED:** For optional field access with defaults
- **LATEST:** For temporal reactive values (events, changing data over time)

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

## 9. Module System

**Modules are files.** Each `.bn` file is a module, and functions are called using the file name:

```
Themes.bn          → Theme/material()
Professional.bn    → Professional/material()
Element.bn         → Element/block(), Element/text()
Router.bn          → Router/route(), Router/go_to()
Text.bn            → Text/trim(), Text/empty()
```

---

## 10. Record Composition with Spread Operator

**The spread operator (`...`) allows merging record fields within record literals.**

This enables composing records by spreading one record into another, following a "defaults first, overrides last" pattern.

### Basic Usage

✅ **Correct - Spreading a record:**
```boon
base: [
    color: red
    gloss: 0.4
]

result: [
    ...base        -- Spreads all fields from base
    metal: 0.03    -- Add new field
]
-- Result: [color: red, gloss: 0.4, metal: 0.03]
```

### Override Pattern (Defaults First, Overrides Last)

✅ **Correct - Defaults with overrides:**
```boon
FUNCTION create_material(user_overrides) {
    [
        color: default_color
        gloss: 0.25
        metal: 0.02
        ...user_overrides  -- Overrides any matching fields
    ]
}
```

This is the **preferred pattern** - define defaults first, then spread overrides.

### Extracting Base Records in BLOCK

When you need to share common fields between variants, extract them in BLOCK scope:

✅ **Correct - Using BLOCK to avoid recursion:**
```boon
FUNCTION font(of) {
    BLOCK {
        body_base: [
            size: 24
            color: colors.text
        ]

        small_base: [
            size: 10
            color: colors.text_tertiary
        ]

        of |> WHEN {
            Body => body_base

            BodyDisabled => [
                ...body_base
                color: colors.text_disabled
            ]

            Input => [...body_base]

            Small => small_base

            SmallLink[hovered] => [
                ...small_base
                line: [underline: hovered]
            ]
        }
    }
}
```

**Why BLOCK?** Direct recursion is not allowed in Boon. You cannot call `font(of: Body)` from within the `font()` function body. Instead, extract shared records as BLOCK-scoped variables.

### Caller-Side Spread (Recommended)

✅ **Correct - Caller composes records:**
```boon
delete_button_material: [
    ...Theme/material(of: SurfaceElevated)
    glow: hovered |> WHEN {
        True => [color: danger_color, intensity: 0.08]
        False => None
    }
]
```

This pattern gives the caller full control over composition without forcing functions to support override parameters.

### Key Rules

1. **Spread only in record literals** - `...expr` only works inside `[...]`
2. **Last wins** - Later field definitions override earlier ones (left-to-right)
3. **UNPLUGGED handling** - Spreading UNPLUGGED is treated as empty record `[]`
4. **No duplicate explicit fields** - Cannot define the same field twice explicitly
5. **Spread anywhere** - Can place `...` before, after, or between field definitions

❌ **INCORRECT - Duplicate field definition:**
```boon
[
    color: red
    color: blue  -- Error: Field 'color' defined multiple times
]
```

✅ **Correct - Spread can override:**
```boon
[
    color: red
    ...overrides  -- OK even if overrides.color exists
]
```

### Complete Specification

For the complete spread operator specification including type system, optimization guarantees, and advanced patterns, see `/docs/language/SPREAD_OPERATOR.md`.

---

## 11. FLUSH and Error Handling

**Boon uses FLUSH for fail-fast error handling with transparent propagation.**

Unlike exceptions, errors are values. FLUSH provides ergonomic early exit while maintaining transparent value propagation through pipelines.

### Core Semantics

**FLUSH** exits the current expression and creates a hidden `FLUSHED[value]` wrapper:
- **FLUSH** exits immediately from current expression
- Creates hidden `FLUSHED[value]` wrapper (not user-accessible)
- **FLUSHED[value]** propagates transparently (bypasses functions)
- **Unwraps at boundaries** - variable bindings, function returns

### Basic FLUSH Pattern

✅ **Correct - FLUSH for early exit:**
```boon
FUNCTION process(item) {
    item
        |> operation()
        |> WHEN {
            Ok[result] => result
            error => FLUSH { error }  -- Exit expression
        }
        |> next_operation()  -- SKIPPED if error was FLUSHed
        |> WHEN {
            Ok[value] => value
            error => FLUSH { error }
        }
        |> WHEN { value =>
            Ok[result: transform(value)]
        }
}
-- Returns: Ok[result: T] | ErrorType1 | ErrorType2
-- FLUSHED[error] unwraps at function boundary
```

### Ok Tagging Requirement

**To distinguish success from errors, wrap success values in `Ok`:**

❌ **WRONG - Bare pattern matches everything:**
```boon
|> WHEN {
    value => value        -- Matches BOTH success AND errors!
    error => FLUSH { ... } -- Never reached
}
```

✅ **CORRECT - Ok tagging:**
```boon
|> WHEN {
    Ok[value] => value      -- Only matches Ok
    error => FLUSH { error }  -- Matches all error types
}
```

**Function signature with Ok tagging:**
```boon
FUNCTION process(x) -> Ok[result: T] | ErrorType1 | ErrorType2
```

### Two-Binding Pattern

**Separate pipeline from error handling for clarity:**

✅ **Correct - Two-binding pattern:**
```boon
-- First binding: pipeline with FLUSH
generation_result: svg_files
    |> List/map(item =>
        item |> process() |> WHEN {
            Ok[value] => value
            error => FLUSH { error }  -- Stops List/map
        }
    )
    |> transform()  -- Bypassed if FLUSHed

-- Second binding: error handling
generation_error_handling: generation_result |> WHEN {
    Ok[values] => handle_success(values)
    ReadError[message] => handle_read_error(message)
    EncodeError[message] => handle_encode_error(message)
}
```

**Benefits:**
- Clear separation: pipeline vs error handling
- No CATCH blocks needed
- Error handling at natural boundary

### FLUSH with List/map (Fail-Fast)

**Regular `List/map` (not `try_map`) handles FLUSH automatically:**

```boon
items: [1, 2, 3, 4, 5]

result: items
    |> List/map(item =>
        item |> risky_operation() |> WHEN {
            Ok[value] => value
            error => FLUSH { error }  -- Stop on first error
        }
    )
-- List/map sees FLUSHED[error], stops processing, returns FLUSHED[error]

-- Handle error at variable level
result |> WHEN {
    Ok[values] => use_values(values)
    error => handle_error(error)
}
```

### Error Handling at Variable Level

**Instead of CATCH blocks, handle errors where the variable is used:**

✅ **Correct - Handle at variable level:**
```boon
result: items
    |> process()  -- May return FLUSHED[error] internally

-- Handle at variable level
result |> WHEN {
    Ok[value] => use_value(value)
    ValidationError[reason] => BLOCK {
        logged: TEXT { Validation failed: {reason} } |> Log/error()
        default_value()
    }
    NetworkError[message] => BLOCK {
        logged: TEXT { Network error: {message} } |> Log/error()
        retry()
    }
}
```

### Tagged Error Types

**Use descriptive tagged objects for errors:**

✅ **Correct - Rich error context:**
```boon
-- Good error types
ReadError[message: TEXT]
EncodeError[message: TEXT]
ValidationError[field: TEXT, reason: TEXT]
NetworkError[url: TEXT, status: 404]

-- Pattern matching
error |> WHEN {
    ReadError[message] => TEXT { Cannot read: {message} }
    EncodeError[message] => TEXT { Cannot encode: {message} }
    ValidationError[field, reason] => TEXT { {field} validation failed: {reason} }
}
```

### Complete BUILD.bn Example

```boon
-- Generates Assets.bn from SVG icon files
icons_directory: TEXT { ./assets/icons }
output_file: TEXT { ./Generated/Assets.bn }

svg_files: Directory/entries(icons_directory)
    |> List/retain(item, if: item.extension = TEXT { svg })
    |> List/sort_by(item, key: item.path)

generation_result: svg_files
    |> List/map(item =>
        item |> icon_code() |> WHEN {
            Ok[text] => text
            error => FLUSH { error }
        }
    )
    |> Text/join_lines()
    |> WHEN { code => TEXT {
        -- Generated from {icons_directory}
        icon: [
            {code}
        ]
    } }
    |> File/write_text(path: output_file)

generation_error_handling: generation_result |> WHEN {
    Ok => BLOCK {
        count: svg_files |> List/count()
        logged: TEXT { Included {count} icons } |> Log/info()
        Build/succeed()
    }
    error => BLOCK {
        error_message: error |> WHEN {
            ReadError[message] => TEXT { Cannot read icon: {message} }
            EncodeError[message] => TEXT { Cannot encode icon: {message} }
            WriteError[message] => TEXT { Cannot write {output_file}: {message} }
        }
        logged: error_message |> Log/error()
        Build/fail()
    }
}

FUNCTION icon_code(item) {
    item.path
        |> File/read_text()
        |> WHEN {
            Ok[text] => text
            error => FLUSH { error }
        }
        |> Url/encode()
        |> WHEN {
            Ok[encoded] => encoded
            error => FLUSH { error }
        }
        |> WHEN { encoded =>
            Ok[text: TEXT { {item.file_stem}: data:image/svg+xml;utf8,{encoded} }]
        }
}
```

### Key Rules

1. **Always use Ok tagging** - Wrap success values in `Ok[...]` to distinguish from errors
2. **FLUSH exits expression** - Creates hidden FLUSHED[value] wrapper
3. **No CATCH needed** - Handle errors at variable level with WHEN
4. **Two-binding pattern** - Separate pipeline from error handling
5. **List/map fail-fast** - Regular List/map stops on first FLUSH
6. **Rich error context** - Use tagged objects with descriptive fields

### See Also

- `/docs/language/FLUSH.md` - Comprehensive FLUSH specification
- `/docs/language/ERROR_HANDLING.md` - Error handling patterns and best practices

---

## 12. Correct Examples Based on These Rules

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
| **Fallback values (structural)** | Use spread operator or `WHEN + UNPLUGGED` | `[defaults... ...overrides]` or `obj.field? \|> WHEN { UNPLUGGED => default, x => x }` |
| **Error handling** | Use `FLUSH` for fail-fast | `error => FLUSH { error }` |
| **Ok tagging** | Wrap success values in `Ok` | `Ok[value: result]` to distinguish from errors |
| **Error handling pattern** | Two-binding pattern | Separate pipeline from error handling |
| **Function calls** | Module/function() syntax | `Theme/material()`, `Text/trim()` |
| **Function definitions** | Root level only, not first-class | `FUNCTION material(m) { ... }` |
| **Records** | Data only, no functions | `[corners: [round: 6]]` ✅, `[fn: FUNCTION...]` ❌ |
| **Type declarations** | Don't exist - tags are inferred | Just use `All`, `InputInterior[focus: True]` |
| **Type tracking** | Full inference with UNPLUGGED | Compiler tracks all UNPLUGGED through flow |
| **Modules** | One file = one module | `Themes.bn` → `Theme/material()` |
| **Record composition** | Use spread operator | `[...base, ...overrides]` |

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

8. ✅ **Use spread operator for structural defaults, LATEST for temporal values**
   - No `??` or `||` operators exist
   - Spread operator: `[defaults... ...overrides]` for function parameters with overrides
   - LATEST: `LATEST { default_value, event_value }` for temporal reactive values only
   - Do NOT use LATEST for structural defaults (causes blink/performance issues)

9. ✅ **Use `obj.field?` for optional fields**
   - Postfix `?` operator accesses potentially missing fields
   - Returns `T | UNPLUGGED` which must be handled with WHEN
   - Pattern: `obj.field? |> WHEN { UNPLUGGED => default, x => x }`

10. ✅ **Use FLUSH for error handling**
   - Always use Ok tagging: `Ok[value: result]`
   - FLUSH exits expression: `error => FLUSH { error }`
   - Two-binding pattern: separate pipeline from error handling
   - No CATCH blocks needed

---

## Verified Against Codebase

These rules have been verified against:
- `/playground/frontend/src/examples/todo_mvc_physical/todo_mvc_physical.bn` (699 lines)
- `/playground/frontend/src/examples/todo_mvc_physical/Theme/*.bn` (5 theme files)
- `/playground/frontend/src/examples/todo_mvc/*.bn` (multiple example files)
- All example projects in `/playground/frontend/src/examples/`

**Last updated:** 2025-11-16

**Recent additions:**
- FLUSH and error handling (Section 11)
- Ok tagging requirement for FLUSH pattern
- Two-binding pattern for error handling
- Spread operator for structural defaults (replaced LATEST misuse)
- Updated Summary table with FLUSH/error handling rows
- Clear distinction between spread operator (structural), LATEST (temporal), and WHEN+UNPLUGGED (optional fields)
- Removed partial pattern matching section (prefer explicit field extraction and spread operator)
