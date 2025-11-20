# Boon Language Syntax Rules

This document describes the core syntax rules and conventions of the Boon programming language. These rules have been verified against the codebase and must be followed when writing or refactoring Boon code.

---

## Source File Encoding and Character Sets

### Source File Encoding: UTF-8 Required

**All Boon source files MUST be UTF-8 encoded without BOM (Byte Order Mark).**

This matches modern language standards (Rust, Go, Python 3) and enables:
- Comments in any language
- String literals with Unicode characters
- International development teams

✅ **Examples:**
```boon
-- Comments work in any language
-- English: This is a comment
-- Czech: Toto je komentář
-- Chinese: 这是注释
-- Arabic: هذا تعليق

-- TEXT literals support Unicode
greeting: TEXT { Hello 世界 🌍 }
message: TEXT { Dobrý den! }
price: TEXT { Price: €50 }
```

❌ **Not supported:**
- Other encodings (Latin-1, UTF-16, etc.)
- BOM (Byte Order Mark) - save as UTF-8 without BOM

**Editor setup:** Configure your editor to save files as UTF-8 without BOM.

### Identifier Encoding: ASCII-Only

**Identifiers (variable names, function names, module names) MUST use ASCII characters only.**

Allowed characters:
- Lowercase letters: `a-z`
- Uppercase letters: `A-Z` (for PascalCase tags)
- Digits: `0-9` (not at start)
- Underscore: `_`

✅ **Correct:**
```boon
user_name: TEXT { 张三 }        -- ASCII identifier, Unicode content
temperature_celsius: -15        -- ASCII identifier (no accents)
message: TEXT { Привет мир }    -- ASCII identifier, Cyrillic content
```

❌ **INCORRECT:**
```boon
用户名: TEXT { ... }            -- ERROR: Non-ASCII identifier
température: -15                -- ERROR: é is not ASCII
сообщение: TEXT { ... }         -- ERROR: Cyrillic identifier
```

**Why ASCII-only identifiers?**
1. **Tool compatibility** - Works with all editors, terminals, compilers
2. **No visual confusion** - Avoids lookalike characters (Cyrillic 'а' vs Latin 'a')
3. **Simplicity** - No Unicode normalization or case-folding complexity
4. **Industry standard** - Most systems languages restrict identifiers to ASCII

---

## 1. Naming Conventions

### Functions and Variables: snake_case ONLY

**All function names and variable names MUST use snake_case.**

**This includes compile-time constants.** Boon has no explicit constant keyword - variables set only at declaration are identified as compile-time constants by the compiler through dataflow analysis. These constant variables still use snake_case, not PascalCase or SCREAMING_SNAKE_CASE.

✅ **Correct:**
```boon
FUNCTION new_todo(title) { ... }
FUNCTION create_scene() { ... }
FUNCTION root_element() { ... }

store: [...]
selected_filter: Active
go_to_result: Router/go_to()
title_to_save: TEXT { Hello }

-- Compile-time constants (still snake_case!)
width: 8
packet_size: 64
buffer_length: 1024
max_retry_count: 10
```

❌ **INCORRECT - These will NOT work:**
```boon
FUNCTION NewTodo(title) { ... }      -- PascalCase not allowed
FUNCTION createScene() { ... }       -- camelCase not allowed
FUNCTION CreateScene() { ... }       -- PascalCase not allowed

Store: [...]                         -- PascalCase not allowed
selectedFilter: Active               -- camelCase not allowed

-- Compile-time constants also CANNOT use other conventions:
WIDTH: 8                             -- PascalCase not allowed
PACKET_SIZE: 64                      -- SCREAMING_SNAKE_CASE not allowed
BufferLength: 1024                   -- PascalCase not allowed
maxRetryCount: 10                    -- camelCase not allowed
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
Utf8
Ascii
Little
Big
```

**Important:** SCREAM_CASE is reserved for Boon keywords only (FUNCTION, BLOCK, LATEST, etc.). User-defined tags MUST use PascalCase.

✅ **Correct - Tags use PascalCase:**
```boon
encoding: Utf8          -- Correct
endian: Little          -- Correct
endian: Big             -- Correct
```

❌ **INCORRECT - SCREAM_CASE not allowed for tags:**
```boon
encoding: UTF8          -- WRONG: Reserved for keywords
endian: LITTLE          -- WRONG: Reserved for keywords
endian: BIG             -- WRONG: Reserved for keywords
```

### Shadowing Is Allowed

**Variable shadowing is permitted in Boon.**

You can reuse the same name in nested scopes, including LATEST parameter names:

✅ **Correct - Shadowing in LATEST:**
```boon
state: initial_state |> LATEST state {
    event |> THEN {
        transform(state)  // 'state' refers to LATEST parameter, shadows outer binding
    }
}
```

✅ **Correct - Shadowing in WHEN:**
```boon
value: config.value? |> WHEN {
    UNPLUGGED => default_value
    value => value  // Inner 'value' shadows outer 'value'
}
```

✅ **Correct - Common pattern with state:**
```boon
FUNCTION fibonacci(position) {
    BLOCK {
        state: [previous: 0, current: 1, iteration: 0] |> LATEST state {
            PULSES { position } |> THEN {
                [
                    previous: state.current,    // Refers to LATEST parameter
                    current: state.previous + state.current,
                    iteration: state.iteration + 1
                ]
            }
        }
        state.current  // Refers to outer binding
    }
}
```

**Pattern:** The inner scope shadows the outer, making code clearer when the same conceptual value is being transformed.

### Comparison Uses Double `==`

**Equality comparison in Boon uses double equals `==`, following universal programming conventions.**

**Inequality uses `=/=` for visual consistency with `==` and `__` (double-character operators).**

However, **pattern matching is often more idiomatic than explicit comparison:**

✅ **BEST - Pattern matching (most idiomatic):**
```boon
state.iteration |> WHEN {
    position => result
    __ => SKIP
}

selected_filter |> WHEN {
    Active => show_active_todos()
    __ => show_all_todos()
}

count |> WHEN {
    0 => empty_message()
    __ => items_list()
}
```

✅ **CORRECT - Explicit comparison with `==`:**
```boon
state.iteration == position |> WHEN {
    True => result
    False => SKIP
}

count == 0 |> WHEN {
    True => empty_message()
    False => items_list()
}

count =/= 0 |> WHEN {
    True => items_list()
    False => empty_message()
}
```

❌ **INCORRECT - Single `=` is for mathematical notation only:**
```boon
state.iteration = position  // SYNTAX ERROR (use ==)
count = 0                   // SYNTAX ERROR (use ==)
count != 0                  // SYNTAX ERROR (use =/=)
```

**Note:**
- Pattern matching is preferred when possible (more functional, cleaner)
- Use `==` for equality, `=/=` for inequality when explicit comparison needed
- Assignment uses `:` in bindings (`name: value`)
- Visual consistency: `==`, `=/=`, and `__` are all double-character operators
- In programming fonts with ligatures, `==` and `=/=` render distinctly from `=>` (arrow)

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

### Function Argument Separation

**Function arguments are separated by NEWLINES, not commas.**

✅ **Correct - Newline separation:**
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
```

❌ **INCORRECT - Commas between arguments:**
```boon
Element/text(
    element: [tag: H1],           -- WRONG: No comma!
    style: [font: ...],           -- WRONG: No comma!
    text: TEXT { todos }
)
```

**Rule:** Arguments are separated by newlines, not commas

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

## 9. BLOCK - Dependency Graphs

**BLOCK creates a dependency graph, not sequential statements.**

BLOCK is Boon's fundamental scoping construct for managing multiple variable bindings with dependencies.

### Core Concept: Reactive Dataflow

**BLOCK variables execute in parallel, respecting dependencies:**

```boon
BLOCK {
    a: 1
    b: a + 1  -- Waits for 'a' to resolve
    c: b + 1  -- Waits for 'b' to resolve
    c         -- Return final value
}
-- Result: 3
```

**Think of BLOCK as a reactive dataflow graph, not sequential statements.**

### Syntax: Variables + Final Expression

```boon
BLOCK {
    variable1: expression1
    variable2: expression2  -- Can reference variable1
    variable3: expression3  -- Can reference variable1, variable2
    variable3              -- Final expression (returned)
}
```

✅ **Correct - Variables with dependencies:**
```boon
BLOCK {
    validated: input |> validate()
    processed: validated |> process()
    saved: processed |> save()
    saved  -- Return saved result
}
```

✅ **Correct - Side effects bound to variables:**
```boon
BLOCK {
    logged: message |> Log/error()
    count: items |> List/count()
    count  -- Return value
}
```

❌ **WRONG - Sequential statements:**
```boon
BLOCK {
    Log/error(message)  -- ERROR: Not a variable binding
    FLUSH { error }
}
```

✅ **CORRECT - Bind side effects:**
```boon
BLOCK {
    logged: message |> Log/error()
    FLUSH { error }  -- Final expression (OK)
}
```

### Cannot Redefine Variables

**Each variable name can only be defined once:**

❌ **WRONG - Redefining 'state':**
```boon
BLOCK {
    state: Ready[...]
    state: state |> WHEN { ... }  -- ERROR: Cannot redefine
}
```

✅ **CORRECT - Different names:**
```boon
BLOCK {
    state1: Ready[...]
    state2: state1 |> WHEN { ... }
    state3: state2 |> WHEN { ... }
    state3  -- Return final
}
```

### Variables Can Reference Each Other

```boon
BLOCK {
    base: [color: red, gloss: 0.4]
    enhanced: [...base, metal: 0.03]
    final: enhanced.color
    final  -- Returns: red
}
```

Runtime builds dependency graph and evaluates in correct order.

### BLOCK for Shared Records

**Extract common records to avoid duplication:**

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

            Small => small_base

            SmallLink[hovered] => [
                ...small_base
                line: [underline: hovered]
            ]
        }
    }
}
```

**Why BLOCK?** Direct recursion is not allowed. You cannot call `font(of: Body)` from within `font()`. Instead, extract shared records as BLOCK-scoped variables.

### Key Rules

1. **Dependency graph, not sequential** - Variables can execute in parallel
2. **Cannot redefine variables** - Each name defined once
3. **Must bind expressions to variables** - No bare expressions except final
4. **Final expression is returned** - Last line without variable binding
5. **Used for scoping and sharing** - Extract common values

### See Also

For comprehensive BLOCK documentation including error handling patterns, see `/docs/language/ERROR_HANDLING.md` Section 3: BLOCK Fundamentals.

---

## 10. Reactive Patterns: WHILE, THEN, and SKIP

**Boon provides reactive patterns for conditional evaluation, event handling, and filtering.**

### WHILE - Conditional Evaluation

**WHILE evaluates a condition and returns different values based on pattern matching.**

Similar to WHEN, but semantically used for reactive conditional branching.

✅ **Correct - Conditional rendering:**
```boon
PASSED.store.todos
    |> List/not_empty()
    |> WHILE {
        True => Element/stripe(
            element: []
            items: todos_list()
        )
        False => NoElement
    }
```

✅ **Correct - Conditional values:**
```boon
PASSED.store.selected_filter |> WHILE {
    All => True
    Active => item.completed |> Bool/not()
    Completed => item.completed
}
```

✅ **Correct - Conditional element visibility:**
```boon
element.hovered |> WHILE {
    True => remove_button()
        |> LINK { todo.elements.remove_button }
    False => NoElement
}
```

**Pattern:**
```boon
condition |> WHILE {
    pattern1 => value1
    pattern2 => value2
    __ => default_value
}
```

**Common use cases:**
- Conditional UI rendering (with `NoElement`)
- Filter conditions in List operations
- Reactive value selection

### THEN - Ignore Input Value

**THEN evaluates an expression while ignoring the piped input value.**

Use THEN when you need to trigger an action from an event but don't need the event's value.

✅ **Correct - Event to constant value:**
```boon
LATEST {
    filter_buttons.all.event.press |> THEN { filter_routes.all }
    filter_buttons.active.event.press |> THEN { filter_routes.active }
    filter_buttons.completed.event.press |> THEN { filter_routes.completed }
}
```

✅ **Correct - Reset on event:**
```boon
text: LATEST {
    Text/empty
    input_element.event.change.text
    save_button.event.press |> THEN { Text/empty }  -- Reset to empty
}
```

✅ **Correct - Side effect without using value:**
```boon
logged: items
    |> process()
    |> THEN {
        count: items |> List/count()
        TEXT { Processed {count} items } |> Log/info()
    }
```

**Comparison with WHEN:**

```boon
-- ❌ WRONG: Value not used
event.press |> WHEN {
    __ => perform_action()  -- Ignoring the value with __
}

-- ✅ CORRECT: Use THEN
event.press |> THEN {
    perform_action()
}
```

**Rule:** If you're not using the piped value, use THEN instead of WHEN.

### SKIP - Filter and Early Return

**SKIP signals that a value should be filtered out or skipped in reactive pipelines.**

Used in pattern matching to indicate "no value" or "filter this out".

✅ **Correct - Conditional value with filter:**
```boon
title_to_save: elements.text_input.event.key_down.key |> WHEN {
    Enter => BLOCK {
        new_title: elements.text_input.text |> Text/trim()
        new_title
            |> Text/is_not_empty()
            |> WHEN { True => new_title, False => SKIP }
    }
    __ => SKIP
}
```

✅ **Correct - Event filtering:**
```boon
selected_id: LATEST {
    None
    todos
        |> List/map(old, new: LATEST {
            old.event.key_down.key
                |> WHEN { Escape => None, __ => SKIP }
            old.title_updated
                |> THEN { None }
            old.event.double_click
                |> THEN { old.id }
        })
        |> List/latest()
}
```

**How SKIP works:**
- In WHEN: Signals "no match, skip this value"
- With LATEST: Filtered out, doesn't become the latest value
- With List operations: Item is filtered from the result
- In reactive pipelines: Value doesn't propagate

**Common patterns:**
- Event filtering (only process specific keys/events)
- Conditional value generation (skip if validation fails)
- Filtering in reactive LATEST chains

### NoElement - Absence of UI Element

**NoElement represents the absence of a UI element in conditional rendering.**

Used with WHILE for conditional element visibility.

✅ **Correct - Conditional button:**
```boon
element.hovered |> WHILE {
    True => remove_button()
    False => NoElement
}
```

✅ **Correct - Conditional section:**
```boon
todos |> List/any(item, if: item.completed) |> WHILE {
    True => clear_completed_button()
    False => NoElement
}
```

**Pattern:**
```boon
condition |> WHILE {
    True => some_element()
    False => NoElement
}
```

**NoElement vs empty LIST:**
- `NoElement`: No element rendered at all
- `LIST {}`: Empty list of elements (still takes up space in layout)

### LINK - Reactive Architecture (See Dedicated Doc)

**LINK creates bidirectional reactive channels in Boon's dataflow graph.**

LINK is a core reactive pattern used extensively (38+ uses in todo_mvc) for:
- Multiple consumers of event streams
- Cross-element coordination
- Dynamic element collections with reactive channels
- Compile-time verification of reactive topology

✅ **Common usage - Wiring reactive elements:**
```boon
new_todo_input()
    |> LINK { PASSED.store.elements.new_todo_input }

remove_button()
    |> LINK { todo.elements.remove_button }
```

**LINK Pattern (Three Steps):**
1. **Declare Architecture** - Reserve reactive slots in store
2. **Declare Capabilities** - Advertise reactive streams in elements
3. **Wire Connections** - Connect streams to architectural slots

**For comprehensive LINK documentation**, see `/docs/language/LINK_PATTERN.md`. The LINK pattern is explained in detail with architecture, examples, and best practices (30KB comprehensive guide).

---

## 11. PULSES - Counted Iteration

**PULSES generates counted events to drive iteration with LATEST.**

PULSES is Boon's primitive for iterating N times, replacing traditional loops with reactive event-driven iteration.

### Core Concept

**PULSES fires N times, each producing a unit value `[]`:**

```boon
PULSES { 10 }  // Generates 10 pulses: [], [], [], ...
```

**Used with LATEST for stateful iteration:**

```boon
result: initial_value |> LATEST state {
    PULSES { 10 } |> THEN {
        transform(state)
    }
}
// Transforms state 10 times
```

### Basic Examples

✅ **Correct - Fibonacci:**
```boon
FUNCTION fibonacci(position) {
    BLOCK {
        state: [previous: 0, current: 1] |> LATEST state {
            PULSES { position } |> THEN {
                [previous: state.current, current: state.previous + state.current]
            }
        }
        state.current
    }
}
```

✅ **Correct - Factorial:**
```boon
FUNCTION factorial(count) {
    BLOCK {
        state: [index: 1, product: 1] |> LATEST state {
            PULSES { count } |> THEN {
                [index: state.index + 1, product: state.product * state.index]
            }
        }
        state.product
    }
}
```

✅ **Correct - Count to N:**
```boon
counter: 0 |> LATEST count {
    PULSES { 10 } |> THEN { count + 1 }
}
// Result: 10
```

### Hardware Context (Requires clk)

In hardware, LATEST must be driven by clock:

```boon
counter: BITS[8] { 0 } |> LATEST count {
    clk |> THEN {
        PULSES { 10 } |> THEN { count + 1 }
    }
}
```

**Execution:**
- Every clock edge: `clk` fires
- PULSES checks internal counter
- If under limit: pulse fires, count increments
- After N pulses: stops updating

### Early Termination with SKIP

Use SKIP to stop iterating early:

```boon
result: initial |> LATEST state {
    PULSES { max_iterations } |> THEN {
        converged(state) |> WHEN {
            True => SKIP              // Stop processing
            False => iterate(state)   // Continue
        }
    }
}
```

### Mixing PULSES with Other Events

PULSES composes naturally with other event sources:

```boon
state: initial |> LATEST state {
    // Automatic iteration
    PULSES { 100 } |> THEN {
        auto_update(state)
    }

    // Manual control
    reset_button.click |> THEN {
        initial
    }
}
```

### Key Rules

1. **Syntax:** `PULSES { N }` where N is iteration count
2. **Type:** Generates unit values `[]` (not indices)
3. **Use with LATEST:** PULSES is an event source
4. **Hardware:** Requires `clk |> THEN` wrapper
5. **Software:** Compiles to synchronous loop
6. **Bounded:** Always finite (no infinite loops)

### When to Use PULSES

✅ **Use PULSES for:**
- Counted iteration (N times)
- Sequence generation (Fibonacci, factorial)
- Iterative algorithms
- Hardware counter circuits

❌ **Don't use PULSES for:**
- Single-time computation (just use expressions)
- Event-driven updates (use LATEST with events)
- Infinite sequences (not supported)

### Complete Documentation

📖 **`/docs/language/PULSES.md`** - Comprehensive guide with 10+ examples, hardware synthesis, and design rationale

---

## 12. Lists and Pattern Matching Wildcards

**Boon provides list literals and wildcard patterns for flexible pattern matching.**

### LIST Literals

**Create lists using `LIST { }` syntax:**

✅ **Correct - Empty list:**
```boon
todos: LIST {}
```

✅ **Correct - List with items:**
```boon
items: LIST {
    header()
    main_content()
    footer()
}
```

✅ **Correct - Inline list:**
```boon
items: LIST { item1, item2, item3 }
```

**Lists in UI:**
```boon
Element/stripe(
    element: []
    direction: Column
    items: LIST {
        todo_checkbox(todo: todo)
        todo_title_element(todo: todo)
        remove_button()
    }
)
```

### Wildcard Pattern `__`

**The wildcard `__` matches any value in pattern matching.**

Use `__` as a catch-all pattern in WHEN/WHILE for values you don't care about.

✅ **Correct - Default case:**
```boon
Router/route() |> WHEN {
    filter_routes.active => Active
    filter_routes.completed => Completed
    __ => All  -- Matches everything else
}
```

✅ **Correct - Ignoring specific values:**
```boon
event.key |> WHEN {
    Enter => save()
    Escape => cancel()
    __ => SKIP  -- Ignore all other keys
}
```

✅ **Correct - Event filtering:**
```boon
old.event.key_down.key |> WHEN {
    Escape => None
    __ => SKIP  -- Skip all other keys
}
```

**Pattern:**
```boon
value |> WHEN {
    specific_pattern1 => result1
    specific_pattern2 => result2
    __ => default_result  -- Catch-all
}
```

### LIST Pattern Matching

**Match on list structures with patterns:**

✅ **Correct - Pattern matching on list values:**
```boon
LIST[selected, hovered] |> WHEN {
    LIST[True, __] => [
        color: primary_color
        intensity: 0.05
    ]
    LIST[False, True] => [
        color: primary_color
        intensity: 0.025
    ]
    LIST[False, False] => None
}
```

**In this pattern:**
- `LIST[True, __]` - First element is True, second can be anything
- `LIST[False, True]` - First is False, second is True
- `LIST[False, False]` - Both are False

**Use cases:**
- Matching multiple boolean states
- Coordinating multiple conditions
- Conditional styling based on element states

---

## 12. Module System

**Modules are files.** Each `.bn` file is a module, and functions are called using the file name:

```
Themes.bn          → Theme/material()
Professional.bn    → Professional/material()
Element.bn         → Element/block(), Element/text()
Router.bn          → Router/route(), Router/go_to()
Text.bn            → Text/trim(), Text/empty()
```

---

## 13. Record Composition with Spread Operator

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

## 14. FLUSH and Error Handling - Quick Reference

**Boon uses FLUSH for fail-fast error handling. Errors are values, not exceptions.**

### Essential Pattern

```boon
FUNCTION process(item) {
    item
        |> operation()
        |> WHEN {
            Ok[result] => result
            error => FLUSH { error }  -- Exit expression
        }
        |> next_operation()  -- SKIPPED if error was FLUSHed
        |> WHEN { result =>
            Ok[value: transform(result)]
        }
}
-- Returns: Ok[value: T] | ErrorType
```

### Key Concepts

**FLUSH Semantics:**
- FLUSH exits expression, creates hidden `FLUSHED[value]` wrapper
- FLUSHED propagates transparently (bypasses functions)
- Unwraps at boundaries (variable bindings, function returns)

**Ok Tagging (Required):**
```boon
-- ✅ CORRECT: Ok tagging
|> WHEN {
    Ok[value] => value      -- Only matches Ok
    error => FLUSH { error }  -- Matches all error types
}

-- ❌ WRONG: Bare pattern matches everything!
|> WHEN {
    value => value        -- Matches BOTH success AND errors
    error => FLUSH { ... } -- Never reached
}
```

**Two-Binding Pattern:**
```boon
-- First binding: pipeline with FLUSH
generation_result: svg_files
    |> List/map(item => process(item) |> WHEN {
        Ok[value] => value
        error => FLUSH { error }
    })

-- Second binding: error handling
generation_error_handling: generation_result |> WHEN {
    Ok[values] => handle_success(values)
    error => handle_error(error)
}
```

**List/map Fail-Fast:**
- Regular `List/map` stops on first FLUSH (no `try_map` needed)
- Returns FLUSHED[error] which unwraps at variable boundary

### Comprehensive Documentation

📖 **`/docs/language/ERROR_HANDLING.md`** - Practical patterns and best practices (15KB)
- FLUSH vs State Accumulator vs Helper Functions patterns
- BLOCK fundamentals for error handling
- WHEN vs THEN in error contexts
- Common pitfalls and best practices

📖 **`/docs/language/FLUSH.md`** - Technical specification (27KB)
- Hardware implementation details
- Parallel processing semantics
- Streaming behavior
- Grammar specification
- Complete type system integration

---

## 15. Correct Examples Based on These Rules

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
| **Shadowing** | Allowed | `state: init \|> LATEST state { ... }` ✅ |
| **Comparison operator** | Double `==` for equality, `=/=` for inequality | `count == 0 \|> WHEN { True => ..., False => ... }` ✅ |
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
| **BLOCK** | Dependency graph, not sequential | Variables execute in parallel, cannot redefine |
| **WHILE** | Conditional evaluation (reactive) | `condition \|> WHILE { True => value, False => alternative }` |
| **THEN** | Ignore piped value | Use when you don't need the input value |
| **SKIP** | Filter/early return in pipelines | Signals "skip this value" in WHEN/LATEST |
| **LIST literals** | `LIST { }` syntax | `LIST { item1, item2, item3 }` |
| **Wildcard `__`** | Matches anything in patterns | `\|> WHEN { specific => x, __ => default }` |
| **LIST pattern matching** | Match list structures | `LIST[True, __] => ...` |
| **NoElement** | Absence of UI element | Use with WHILE for conditional rendering |
| **LINK** | Reactive architecture pattern | See `/docs/language/LINK_PATTERN.md` |

---

## When Designing New APIs

When proposing new Boon APIs, remember:

1. ✅ **Use snake_case for all functions and variables**
   - `theme_value()` not `ThemeValue()` or `themeValue()`

2. ✅ **Use PascalCase for tags only**
   - `InputInterior`, `Round`, `Header`

3. ✅ **Shadowing is allowed - use clear names**
   - `state: init |> LATEST state { ... }` is preferred over abbreviated parameter names
   - Makes code more readable when transforming the same conceptual value

4. ✅ **Use `==` for equality comparison, `=/=` for inequality**
   - `count == 0 |> WHEN { True => ..., False => ... }`
   - Assignment uses `:` in bindings (`name: value`)
   - Visual consistency with `__` wildcard (double-character operators)

5. ✅ **Functions must be called with Module/ prefix**
   - `Theme/material()` not `material()`

6. ❌ **Cannot store functions in records or variables**
   - Functions are not first-class - they cannot be assigned, passed, or stored
   - Records can only contain data (values, nested records, lists, tags)

7. ❌ **Cannot create callable values from records**
   - `material: record.function(arg)` doesn't work unless `record.function` is data, not a function
   - For stateful values, you MUST use function calls: `Theme/material(InputInterior[focus: True])`

8. ❌ **Cannot define types or modules inline**
   - No `TYPE`, `MODULE`, `CLASS`, `CONST` declarations

9. ✅ **All function parameters are required**
   - No optional parameters or default values
   - Use polymorphic tags (`Default`, `None`) or records (`overrides: []`) for optional-like behavior

10. ✅ **Use spread operator for structural defaults, LATEST for temporal values**
   - No `??` or `||` operators exist
   - Spread operator: `[defaults... ...overrides]` for function parameters with overrides
   - LATEST: `LATEST { default_value, event_value }` for temporal reactive values only
   - Do NOT use LATEST for structural defaults (causes blink/performance issues)

11. ✅ **Use `obj.field?` for optional fields**
   - Postfix `?` operator accesses potentially missing fields
   - Returns `T | UNPLUGGED` which must be handled with WHEN
   - Pattern: `obj.field? |> WHEN { UNPLUGGED => default, x => x }`

12. ✅ **Use FLUSH for error handling**
   - Always use Ok tagging: `Ok[value: result]`
   - FLUSH exits expression: `error => FLUSH { error }`
   - Two-binding pattern: separate pipeline from error handling
   - No CATCH blocks needed

13. ✅ **Use BLOCK for dependency graphs, not sequential code**
   - Variables execute in parallel, respecting dependencies
   - Cannot redefine variables (use different names)
   - Must bind expressions to variables (except final expression)
   - Use for scoping and extracting shared records

14. ✅ **Use reactive patterns appropriately**
   - **WHILE**: Conditional evaluation, especially for UI rendering
   - **THEN**: When ignoring piped value (event handling without value)
   - **SKIP**: Filtering in WHEN/LATEST chains
   - **NoElement**: Conditional UI rendering with WHILE

15. ✅ **Use LIST and wildcard patterns**
   - `LIST { }` for list literals
   - `__` for catch-all in pattern matching
   - LIST pattern matching for coordinated state: `LIST[True, __] => ...`

16. ✅ **Use LINK for reactive architecture**
   - Declare → Advertise → Wire pattern
   - See `/docs/language/LINK_PATTERN.md` for comprehensive guide
   - Essential for element event coordination

---

## Verified Against Codebase

These rules have been verified against:
- `/playground/frontend/src/examples/todo_mvc_physical/todo_mvc_physical.bn` (699 lines)
- `/playground/frontend/src/examples/todo_mvc_physical/Theme/*.bn` (5 theme files)
- `/playground/frontend/src/examples/todo_mvc/*.bn` (multiple example files)
- All example projects in `/playground/frontend/src/examples/`

**Last updated:** 2025-11-16

**Recent additions:**
- **Section 9: BLOCK** - Comprehensive dependency graph semantics, cannot redefine variables, parallel execution
- **Section 10: Reactive Patterns** - WHILE (conditional evaluation), THEN (ignore value), SKIP (filtering), NoElement (UI absence), LINK (reactive architecture reference)
- **Section 11: Lists and Wildcards** - LIST literals, `__` wildcard pattern, LIST pattern matching
- FLUSH and error handling (Section 14)
- Ok tagging requirement for FLUSH pattern
- Two-binding pattern for error handling
- Spread operator for structural defaults (replaced LATEST misuse)
- Updated Summary table with all reactive patterns and core features
- Clear distinction between spread operator (structural), LATEST (temporal), and WHEN+UNPLUGGED (optional fields)
- Removed partial pattern matching section (prefer explicit field extraction and spread operator)
- All previously undocumented but heavily-used features now documented
