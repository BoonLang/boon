# Spread Operator

**Status:** Language Feature
**Date:** 2025-11-16
**Related:** BOON_SYNTAX.md

---

## Table of Contents

1. [Overview](#overview)
2. [Basic Syntax](#basic-syntax)
3. [Semantics](#semantics)
4. [Common Patterns](#common-patterns)
5. [Examples](#examples)

---

## Overview

The **spread operator** (`...`) allows merging record fields within record literal syntax. It provides a declarative way to compose records, override defaults, and build configuration objects.

```boon
// Basic usage:
[
    color: default_color
    gloss: 0.4
    ...user_overrides
]
```

**Key properties:**
- Syntax: `...expression` where expression evaluates to a record
- Position: Allowed anywhere within `[...]` record literals
- Semantics: Last definition wins (left-to-right evaluation)
- UNPLUGGED handling: `UNPLUGGED` spreads as empty record `[]`

---

## Basic Syntax

### Form

```boon
'...' expression
```

**Components:**
- `...` - Three-character spread token
- `expression` - Any expression evaluating to `Record | UNPLUGGED`

### Valid Positions

Spread can appear **anywhere** within a record literal:

```boon
// At end (most common):
[
    field: value
    ...spread
]

// At start:
[
    ...spread
    field: value
]

// In middle (sandwich):
[
    field1: value1
    ...spread
    field2: value2
]

// Multiple spreads:
[
    ...spread1
    field: value
    ...spread2
    ...spread3
]
```

---

## Semantics

### Last-Wins Rule

**Evaluation order:** Left-to-right
**Override rule:** Later definitions override earlier ones

```boon
[
    color: red        // Position 1
    ...spread1        // Position 2 (if spread1.color exists, overwrites red)
    color: blue       // Position 3 (overwrites any previous color)
    ...spread2        // Position 4 (if spread2.color exists, overwrites blue)
]

// Final color value: spread2.color OR blue (if spread2 lacks color)
```

### UNPLUGGED Handling

When spread expression evaluates to `UNPLUGGED`, it behaves as empty record `[]`:

```boon
maybe_overrides: config.theme  // Type: Theme | UNPLUGGED

[
    color: default_color
    ...maybe_overrides  // If UNPLUGGED: no-op, if Theme: spread fields
]

// Result:
// - If maybe_overrides = UNPLUGGED: [color: default_color]
// - If maybe_overrides = [color: red]: [color: red]
```

**No errors or special handling required** - UNPLUGGED spreads as empty.

### Field Overwriting

```boon
// Example 1: Explicit field overridden by spread
[
    color: expensive_computation()
    ...overrides  // If overrides.color exists, overwrites
]

// Example 2: Spread field overridden by explicit field
[
    ...base
    color: red  // Always wins (last)
]

// Example 3: Spread overridden by later spread
[
    ...defaults
    ...overrides  // overrides.* wins over defaults.*
]
```

### Type Requirements

Spread expression must have type: `Record | UNPLUGGED`

```boon
// ✅ Valid - expression is Record:
[
    ...user_config  // Type: [name: String, age: Number]
]

// ✅ Valid - expression is UNPLUGGED-compatible:
[
    ...config.theme  // Type: Theme | UNPLUGGED
]

// ❌ Type error - expression is not Record:
[
    ...42  // Type: Number
]
// Error: "Cannot spread type Number, expected Record"
```

### Duplicate Explicit Fields

**Compile error** when same field defined multiple times explicitly:

```boon
// ❌ Error:
[
    color: red
    color: blue  // Error: "Field 'color' defined multiple times"
]

// ✅ Valid (spread overwrites):
[
    color: red
    ...overrides  // OK even if overrides.color exists
]

// ✅ Valid (explicit overwrites spread):
[
    ...base  // OK even if base.color exists
    color: red
]
```

---

## Common Patterns

### Pattern 1: Defaults with Overrides (Preferred)

**Use case:** Provide defaults, allow user overrides

```boon
[
    color: default_color
    gloss: 0.4
    metal: 0.02
    ...user_overrides
]
```

**Reads:** "Here are my defaults, override with user values"

**This is the PREFERRED pattern** because:
- ✅ Reads naturally top-to-bottom (defaults → overrides)
- ✅ Clear mental model: "set defaults, then apply overrides"
- ✅ Most common use case in practice
- ✅ Matches common configuration patterns

---

### Pattern 2: Fallback Configuration

**Use case:** User config with fallback defaults

```boon
[
    ...user_config
    timeout: 3000     // Fallback if user_config.timeout missing
    retries: 3        // Fallback if user_config.retries missing
]
```

**Reads:** "Use user config, fall back to these defaults"

**Note:** Pattern 1 (defaults-first) is preferred over this pattern for most cases. Use this pattern only when you specifically need fallback behavior for missing fields.

---

### Pattern 3: Sandwich (Selective Override)

**Use case:** Some fields forced, others can be overridden

```boon
[
    color: default_color
    ...user_overrides  // Can override color, but not border_radius
    border_radius: 5   // Always set (forced)
]
```

**Reads:** "Some defaults, user overrides, forced values"

---

### Pattern 4: Cascade (Multiple Layers)

**Use case:** Multiple priority levels (like CSS specificity)

```boon
[
    ...browser_defaults
    ...component_defaults
    ...theme_overrides
    ...user_overrides
]
```

**Reads:** "Layer bottom to top, each overrides previous"

---

### Pattern 5: Conditional Spreads

**Use case:** Conditionally include fields based on state

```boon
[
    color: base_color
    gloss: 0.25

    ...hovered |> WHEN {
        True => [gloss: 0.35, glow: hover_glow]
        False => []
    }

    metal: 0.02
]
```

**Reads:** "Base properties, modify if hovered, other properties"

---

### Pattern 6: Semantic Grouping

**Use case:** Group related fields visually

```boon
[
    // Visual properties
    color: default_color
    gloss: 0.4

    // Physical properties
    metal: 0.02
    roughness: 0.5

    // User overrides (can override any above)
    ...user_overrides

    // Forced values (always set)
    version: 2
]
```

---

## Examples

### Theme Material Base Extraction

```boon
FUNCTION material(material) {
    BLOCK {
        surface_base: [
            color: PASSED.mode |> WHEN {
                Light => Oklch[lightness: 1]
                Dark => Oklch[lightness: 0.15]
            }
            gloss: 0.25
        ]

        surface_variant_base: [
            color: PASSED.mode |> WHEN {
                Light => Oklch[lightness: 0.985]
                Dark => Oklch[lightness: 0.18]
            }
            gloss: 0.25
        ]

        material |> WHEN {
            Surface => surface_base

            SurfaceVariant => surface_variant_base

            Interactive[hovered] => [
                ...surface_variant_base
                gloss: hovered |> WHEN {
                    True => 0.3
                    False => 0.25
                }
                metal: 0.03
            ]

            InteractiveRecessed[focus] => [
                ...surface_base
                gloss: focus |> WHEN {
                    False => 0.65
                    True => 0.15
                }
                glow: focus |> WHEN {
                    True => [
                        color: PASSED.mode |> WHEN {
                            Light => Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
                            Dark => Oklch[lightness: 0.8, chroma: 0.12, hue: 220]
                        }
                        intensity: 0.15
                    ]
                    False => None
                }
            ]
        }
    }
}
```

### Font Variants with Spread

```boon
FUNCTION font(font) {
    BLOCK {
        colors: PASSED.mode |> WHEN {
            Light => [
                text: Oklch[lightness: 0.42]
                text_secondary: Oklch[lightness: 0.57]
                text_disabled: Oklch[lightness: 0.75]
            ]
            Dark => [
                text: Oklch[lightness: 0.9]
                text_secondary: Oklch[lightness: 0.75]
                text_disabled: Oklch[lightness: 0.5]
            ]
        }

        body_base: [
            size: 24
            color: colors.text
        ]

        small_base: [
            size: 10
            color: colors.text_tertiary
        ]

        font |> WHEN {
            Body => body_base

            BodyDisabled => [
                ...body_base
                color: colors.text_disabled
            ]

            Input => [...body_base]

            Placeholder => [
                ...body_base
                style: Italic
                color: colors.text_secondary
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

### Conditional Material Overrides

```boon
FUNCTION delete_button_material(hovered) {
    [
        ...Theme/material(of: SurfaceElevated)
        glow: hovered |> WHEN {
            True => [
                color: Theme/material(of: Danger).color
                intensity: 0.08
            ]
            False => None
        }
    ]
}

FUNCTION filter_button_material(selected, hovered) {
    [
        ...selected |> WHEN {
            True => Theme/material(of: PrimarySubtle)
            False => Theme/material(of: SurfaceVariant)
        }
        gloss: selected |> WHEN {
            False => 0.35
            True => 0.2
        }
        metal: 0.03
        glow: LIST { selected, hovered } |> WHEN {
            LIST { True, __ } => [
                color: Theme/material(of: Primary).color
                intensity: 0.05
            ]
            LIST { False, True } => [
                color: Theme/material(of: Primary).color
                intensity: 0.025
            ]
            LIST { False, False } => None
        }
    ]
}
```

### Font with Conditional Styling

```boon
FUNCTION clear_button_font(hovered) {
    [
        ...Theme/font(of: BodySecondary)
        line: [underline: hovered]
    ]
}

FUNCTION todo_title_font(completed) {
    [
        ...Theme/font(of: Body)
        line: [strike: completed]
        ...completed |> WHEN {
            True => [color: Theme/font(of: BodyDisabled).color]
            False => []
        }
    ]
}
```

### Text Styling with Base Spread

```boon
FUNCTION text(of) {
    BLOCK {
        small_base: [
            font: font(of: Small)
            depth: 1
            move: [further: 4]
            relief: Carved[wall: 1]
        ]

        of |> WHEN {
            Small => small_base

            SmallLink[hovered] => [
                ...small_base
                font: font(of: SmallLink[hovered])
            ]
        }
    }
}
```

---

## Type System

### Type Inference with Spread

Spread contributes to the result record's inferred type:

```boon
base: [color: Color, gloss: Number]
overrides: [metal: Number, glow: Glow]

result: [
    ...base
    ...overrides
]

// Inferred type of result:
// [color: Color, gloss: Number, metal: Number, glow: Glow]
```

### Field Type Conflicts

When spreads define the same field with different types, last wins:

```boon
a: [x: Number]
b: [x: String]

result: [
    ...a
    ...b
]

// Type of result.x: String (last wins)
```

---

## Optimization

### Compiler Optimization Strategy

Boon's compiler uses **monomorphization** (type-specialized code generation) to optimize spread operations:

1. **Type Inference**: Compiler infers types at all call sites
2. **Specialization**: Generate specialized function version per unique type
3. **Dead Code Elimination**: Remove field computations that get overwritten

**Result: Zero runtime overhead** - overridden fields are eliminated at compile time.

### Example Optimization

```boon
FUNCTION create_material(overrides) {
    [
        color: expensive_color_computation()
        gloss: 0.4
        ...overrides
    ]
}

// Call site: overrides = [color: red, glow: x]
// Compiler generates specialized version:
[
    // expensive_color_computation() ✅ ELIMINATED (overridden)
    color: red
    gloss: 0.4
    glow: x
]
```

**Position doesn't matter** - compiler optimizes all patterns equally:

```boon
// Pattern A: Defaults first
[color: expensive(), ...overrides]

// Pattern B: Overrides first
[...overrides, color: expensive()]

// Both optimize identically based on call site types
```

### Inlining Optimization

When spread expression comes from inlined function:

```boon
FUNCTION get_base() {
    [color: expensive_color(), gloss: 0.4]
}

[
    ...get_base()
    color: red
]

// Compiler inlines get_base(), sees:
[
    color: expensive_color()  // Will be overridden
    gloss: 0.4
    color: red
]

// Optimizes to:
[
    gloss: 0.4
    color: red
]
// ✅ expensive_color() ELIMINATED
```

---

## Edge Cases

### Empty Spread

```boon
[
    x: 5
    ...[]
]

// Result: [x: 5]
// Empty spread is no-op, compiler eliminates it entirely
```

### UNPLUGGED Spread

```boon
maybe_config: get_config()  // Type: Config | UNPLUGGED

[
    x: 5
    ...maybe_config
]

// If maybe_config = UNPLUGGED: [x: 5]
// If maybe_config = [y: 10]: [x: 5, y: 10]

// No errors - UNPLUGGED treated as []
```

### Nested Spreads

```boon
[
    x: 5
    ...[
        y: 10
        ...other
    ]
]

// Inner spread evaluates first, then outer spread
// Works as expected
```

### Spreading Non-Record Types

```boon
// ❌ Type error:
[x: 5, ...42]
// Error: "Cannot spread type Number, expected Record"

[x: 5, ..."hello"]
// Error: "Cannot spread type String, expected Record"

[x: 5, ...True]
// Error: "Cannot spread type Bool, expected Record"
```

### Spread in WHEN Expression

```boon
[
    x: 5
    ...mode |> WHEN {
        Light => light_config
        Dark => dark_config
    }
]

// Each branch must return Record | UNPLUGGED
// Compiler creates specialized version per mode
```

---

## Comparison with Other Languages

### JavaScript/TypeScript

**JavaScript:**
```javascript
const result = {
    ...base,
    color: "red",
    ...overrides
};
```

**Boon:**
```boon
result: [
    ...base
    color: red
    ...overrides
]
```

**Key differences:**
- Boon uses `[...]` instead of `{...}` for record literals
- Boon optimizes away dead fields at compile time (JavaScript creates then overwrites at runtime)
- Boon has UNPLUGGED handling (JavaScript has undefined/null issues)
- Boon has compile-time type checking

### Rust

**Rust (struct update syntax):**
```rust
let result = Point {
    x: 1,
    ..base  // base comes after, but fields from base are used as defaults
};
```

**Boon:**
```boon
result: [
    x: 1
    ...base
]
```

**Key differences:**
- Rust puts `..` at end, but it provides BASE values (confusing semantics)
- Boon's last-wins is more intuitive
- Boon allows spread anywhere and multiple spreads
- Rust allows only one spread at end

---

## Design Rationale

### Why `...` (Three Dots)?

**Considered alternatives:**
- `..` (two dots) - conflicts with range operator
- `*` (asterisk) - conflicts with multiplication
- `@` - non-obvious meaning
- `SPREAD` keyword - too verbose

**Decision: `...`**
- ✅ Familiar from JavaScript/TypeScript/Rust
- ✅ Clear "spread/expand" metaphor
- ✅ Lightweight syntax
- ✅ Established pattern reduces learning curve

### Why Allow Spread Anywhere?

**Alternative considered:** Restrict spread to end of record literal

**Decision: Allow anywhere**
- ✅ Maximum flexibility (supports sandwich pattern, conditional overrides)
- ✅ Semantic grouping possible
- ✅ Compiler optimizes all positions equally
- ✅ No artificial constraints

### Why Last-Wins Semantics?

**Alternative considered:** First-wins (earlier definitions take precedence)

**Decision: Last-wins (left-to-right)**
- ✅ Matches CSS cascade mental model
- ✅ Intuitive (consistent with JavaScript, Python)
- ✅ Natural "painting layers" metaphor
- ✅ Allows forcing values at end

### Why UNPLUGGED Spreads as `[]`?

**Alternative considered:** Error on UNPLUGGED spread

**Decision: UNPLUGGED = `[]` (no-op)**
- ✅ Natural fallback behavior
- ✅ No special handling required
- ✅ Matches intuition "spread if it exists"
- ✅ Composes well with conditional spreads

### Why Monomorphization for Optimization?

**Alternative considered:** Runtime merging (like JavaScript)

**Decision: Compile-time monomorphization**
- ✅ Zero runtime overhead
- ✅ Dead fields eliminated at compile time
- ✅ Perfect optimization guaranteed
- ✅ Predictable performance
- ✅ Fits Boon's reactive dataflow model

### Why Only in Record Literals?

**Alternative considered:** Allow spread in other contexts

**Decision: Only in `[...]` literals**
- ✅ Clear, constrained scope
- ✅ Easy to analyze and optimize
- ✅ No ambiguity with other uses
- ✅ Simpler mental model
- ✅ No rest/spread confusion (like JavaScript)

---

## Summary

The spread operator (`...`) provides a simple, powerful way to compose records in Boon:

- **Syntax:** `...expr` within record literals `[...]`
- **Position:** Anywhere (before, after, or between fields)
- **Semantics:** Last wins, UNPLUGGED = `[]`
- **Type safety:** Compile-time checking

**Preferred pattern:** Defaults-first with user overrides at end

```boon
[
    // Defaults
    color: default_color
    gloss: 0.4

    // User overrides (wins)
    ...user_overrides
]
```

---

**Last updated:** 2025-11-16
