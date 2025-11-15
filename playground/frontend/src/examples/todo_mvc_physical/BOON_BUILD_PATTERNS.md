# Boon BUILD Patterns & Learnings

**Date:** 2025-01-15
**Context:** Lessons from refactoring BUILD.bn with error handling patterns
**Status:** Reference Guide

---

## Table of Contents

1. [Core Principles](#core-principles)
2. [BLOCK Behavior](#block-behavior)
3. [Pipeline Patterns](#pipeline-patterns)
4. [WHEN vs THEN](#when-vs-then)
5. [Error Handling Patterns](#error-handling-patterns)
6. [Tagged Objects](#tagged-objects)
7. [Happy Path Example](#happy-path-example)
8. [Common Pitfalls](#common-pitfalls)

---

## Core Principles

### Function Parameters Are Fixed

**You cannot rename function parameters at call site:**

```boon
-- ❌ WRONG: Cannot rename 'item' to 'file'
svg_files |> List/retain(file, if: file.extension = TEXT { svg })

-- ✅ CORRECT: Must use parameter name from function definition
svg_files |> List/retain(item, if: item.extension = TEXT { svg })
svg_files |> List/map(old, new: icon_code(old))
```

**Parameter names are defined by the function, not the caller.**

---

### Tagged Objects Need Named Properties

**Tags can be bare or have named properties:**

```boon
-- ✅ CORRECT: Bare tags
Ok
Error
None

-- ✅ CORRECT: Tagged objects with named properties
Ok[value: TEXT { success }]
Error[msg: TEXT { failed }]
Result[ok: content]
Failed[stage: TEXT { read }, reason: msg]

-- ❌ WRONG: Positional properties
Ok[TEXT { success }]
Error[msg]
Result[content]
```

**All properties in tagged objects MUST be named.**

---

### TEXT Syntax Rules

**Cannot nest TEXT inside TEXT:**

```boon
-- ❌ WRONG: Nested TEXT
TEXT { {name}: TEXT { data:image/svg+xml } }

-- ✅ CORRECT: Single TEXT with interpolations
TEXT { {name}: data:image/svg+xml;utf8,{encoded} }
```

**Interpolation uses `{variable}` - no spaces inside braces:**

```boon
-- ✅ CORRECT:
TEXT { Hello {name}! }
TEXT { {stem}: {encoded} }

-- ❌ WRONG:
TEXT { Hello { name }! }
```

---

## BLOCK Behavior

### BLOCK Variables Form a Dependency Graph

**All variables in a BLOCK are declared simultaneously, but execution respects dependencies:**

```boon
BLOCK {
    a: 1
    b: a + 1  -- ✅ Waits for 'a' to resolve
    c: b + 1  -- ✅ Waits for 'b' to resolve
}
-- Result: c = 3
```

**Think of it as a reactive dataflow graph, not sequential statements.**

---

### Cannot Redefine Variables in Same BLOCK

```boon
-- ❌ WRONG: Redefining 'state'
BLOCK {
    state: Ready[...]
    state: state |> WHEN { ... }  -- ERROR: 'state' already defined
    state: state |> WHEN { ... }  -- ERROR: 'state' already defined
}

-- ✅ CORRECT: Different variable names
BLOCK {
    state1: Ready[...]
    state2: state1 |> WHEN { ... }  -- Waits for state1
    state3: state2 |> WHEN { ... }  -- Waits for state2
    state3  -- Return final state
}
```

---

### BLOCK Variables Can Reference Each Other

**Later variables can reference earlier ones:**

```boon
BLOCK {
    base: [color: red, gloss: 0.4]

    enhanced: [
        ...base
        metal: 0.03
    ]

    final: enhanced.color  -- References 'enhanced'
}
```

**The runtime builds a dependency graph and evaluates in correct order.**

---

## Pipeline Patterns

### Pure Pipeline Style

**Start with a value, pipe through transformations:**

```boon
-- ✅ CORRECT: Pure pipeline
item.path
    |> File/read_text()
    |> Url/encode()
    |> WHEN { encoded => TEXT { {encoded} } }

-- Also valid but less pure:
File/read_text(item.path)
    |> Url/encode()
    |> WHEN { encoded => TEXT { {encoded} } }
```

**Starting with a value makes the flow clearer.**

---

### Chaining Functions

**In happy path, functions can chain directly:**

```boon
-- Happy path (no errors):
item.path
    |> File/read_text()    -- TEXT -> TEXT
    |> Url/encode()        -- TEXT -> TEXT
    |> process()           -- TEXT -> TEXT
```

**With error handling:**

```boon
-- With Results:
item.path
    |> File/read_text()  -- TEXT -> Result[ok: TEXT] | Error[msg: TEXT]
    |> WHEN {
        Result[ok: content] => Url/encode(content) |> WHEN {
            Result[ok: encoded] => TEXT { {encoded} }
            Error[msg] => Error[msg]
        }
        Error[msg] => Error[msg]
    }
```

---

## WHEN vs THEN

### Use WHEN When You Need the Value

```boon
-- WHEN binds the previous value
value |> WHEN {
    result => TEXT { Got: {result} }
}

-- Pattern matching on tagged objects
state |> WHEN {
    Ready[data] => process(data)
    Failed[error] => log(error)
}
```

---

### Use THEN When You Ignore the Value

```boon
-- ❌ WRONG: Value not used
|> WHEN {
    result => BLOCK {
        count: items |> List/count()
        log(count)
    }
}

-- ✅ CORRECT: Use THEN
|> THEN {
    count: items |> List/count()
    log(count)
}
```

**If you're not using the parameter, use THEN instead of WHEN.**

---

## Error Handling Patterns

### Pattern 1: State Accumulator (Flat, Transparent)

**Use different variable names for each stage:**

```boon
FUNCTION process_with_errors(item) {
    BLOCK {
        state1: Ready[stem: item.file_stem, path: item.path]

        state2: state1 |> WHEN {
            Failed[error] => state1  -- Transparent propagation
            Ready[stem, path] => File/read_text(path) |> WHEN {
                Error[msg] => Failed[stage: TEXT { read }, error: msg]
                Result[ok: text] => Loaded[stem: stem, content: text]
            }
        }

        state3: state2 |> WHEN {
            Failed[error] => state2  -- Transparent propagation
            Loaded[stem, content] => Url/encode(content) |> WHEN {
                Error[msg] => Failed[stage: TEXT { encode }, error: msg]
                Result[ok: enc] => Done[stem: stem, encoded: enc]
            }
        }

        state3 |> WHEN {
            Done[stem, encoded] => Result[ok: TEXT { {stem}: {encoded} }]
            Failed[stage, error] => Error[msg: TEXT { {stage}: {error} }]
        }
    }
}
```

**Benefits:**
- ✅ Flat structure (no deep nesting)
- ✅ Transparent error propagation (`Failed[__] => state`)
- ✅ Each stage waits for previous stage
- ✅ Rich error context

---

### Pattern 2: Helper Functions (Sequential Steps)

```boon
FUNCTION icon_code(item) {
    Ready[stem: item.file_stem, path: item.path]
        |> read_file_step()
        |> encode_step()
        |> format_result()
}

FUNCTION read_file_step(state) {
    state |> WHEN {
        Failed[error] => state
        Ready[stem, path] => File/read_text(path) |> WHEN {
            Error[msg] => Failed[stage: TEXT { read }, error: msg]
            Result[ok: text] => Loaded[stem: stem, content: text]
        }
    }
}

FUNCTION encode_step(state) {
    state |> WHEN {
        Failed[error] => state
        Loaded[stem, content] => Url/encode(content) |> WHEN {
            Error[msg] => Failed[stage: TEXT { encode }, error: msg]
            Result[ok: enc] => Done[stem: stem, encoded: enc]
        }
    }
}

FUNCTION format_result(state) {
    state |> WHEN {
        Done[stem, encoded] => Result[ok: TEXT { {stem}: {encoded} }]
        Failed[stage, error] => Error[msg: TEXT { {stage}: {error} }]
    }
}
```

**Benefits:**
- ✅ Clean pipeline
- ✅ Each function has single responsibility
- ✅ Easy to test individually
- ✅ Transparent error propagation

---

### Pattern 3: Nested WHEN (Compact)

```boon
FUNCTION icon_code(item) {
    File/read_text(item.path) |> WHEN {
        Error[msg] => Error[msg: TEXT { Read failed: {msg} }]
        Result[ok: content] => Url/encode(content) |> WHEN {
            Error[msg] => Error[msg: TEXT { Encode failed: {msg} }]
            Result[ok: encoded] => Result[ok: TEXT { {item.file_stem}: {encoded} }]
        }
    }
}
```

**Benefits:**
- ✅ Compact (single function)
- ✅ Clear error vs success paths

**Drawbacks:**
- ⚠️ Nesting (but only 2 levels)
- ⚠️ Less reusable

---

### Pattern 4: Error Accumulation

**Collect all errors instead of failing on first:**

```boon
FUNCTION gather_results(results) {
    results |> List/fold(
        init: [successes: LIST {}, errors: LIST {}]
        step: gather_one
    ) |> WHEN {
        [successes, errors] => errors |> List/is_empty() |> WHEN {
            True => Result[ok: successes]
            False => Error[msg: errors |> Text/join_lines()]
        }
    }
}

FUNCTION gather_one(acc, result) {
    result |> WHEN {
        Result[ok: value] => [
            successes: acc.successes |> List/append(value)
            errors: acc.errors
        ]
        Error[msg] => [
            successes: acc.successes
            errors: acc.errors |> List/append(msg)
        ]
    }
}
```

**Use when you want to see ALL errors, not just the first.**

---

## Tagged Objects

### Adhoc Tags for State Progression

**Create tags on-the-fly to represent states:**

```boon
Ready[stem: TEXT, path: TEXT]
Loaded[stem: TEXT, content: TEXT]
Encoded[stem: TEXT, encoded: TEXT]
Done[value: TEXT]
Failed[stage: TEXT, error: TEXT]
```

**Use descriptive names that show progression:**
- `Ready` → `Loaded` → `Encoded` → `Done`
- `Pending` → `Processing` → `Complete`
- `Input` → `Validated` → `Processed` → `Output`

---

### Rich Error Context

**Include context in error tags:**

```boon
-- Minimal:
Error[msg: TEXT { failed }]

-- Better:
Error[msg: TEXT { Read failed: {reason} }]

-- Best:
Failed[
    stage: TEXT { read }
    path: item.path
    reason: msg
    timestamp: now()
]
```

---

## Happy Path Example

**From BUILD.bn:**

```boon
FUNCTION icon_code(item) {
    item.path
        |> File/read_text()
        |> Url/encode()
        |> WHEN { encoded =>
            TEXT { {item.file_stem}: data:image/svg+xml;utf8,{encoded} }
        }
}

generation: svg_files
    |> List/map(old, new: icon_code(old))
    |> Text/join_lines()
    |> WHEN { code => TEXT {
        -- Generated from {icons_directory}

        icon: [
            {code}
        ]

    } }
    |> File/write_text(path: output_file)
    |> THEN {
        count: svg_files |> List/count()
        TEXT { Included {count} icons } |> Log/info()
    }
```

**Key points:**
- Pure pipeline style
- WHEN binds values
- THEN ignores previous value
- TEXT interpolation
- Multiline TEXT

---

## Common Pitfalls

### ❌ Redefining Variables in BLOCK

```boon
-- WRONG:
BLOCK {
    x: 1
    x: x + 1  -- ERROR: x already defined
}

-- CORRECT:
BLOCK {
    x: 1
    y: x + 1
}
```

---

### ❌ Using Positional Tag Properties

```boon
-- WRONG:
Ok[value]
Error[msg]

-- CORRECT:
Ok[value: TEXT { success }]
Error[msg: TEXT { failed }]
```

---

### ❌ Nesting TEXT

```boon
-- WRONG:
TEXT { {name}: TEXT { value } }

-- CORRECT:
TEXT { {name}: value }
```

---

### ❌ Renaming Function Parameters

```boon
-- WRONG:
List/map(file, new: process(file))

-- CORRECT:
List/map(old, new: process(old))
```

---

### ❌ Using WHEN When THEN Is Better

```boon
-- WRONG:
|> WHEN { result => log(TEXT { done }) }

-- CORRECT:
|> THEN { log(TEXT { done }) }
```

---

## Summary

**Key Learnings:**

1. **BLOCK forms dependency graph** - variables wait for dependencies, but can't be redefined
2. **Pipeline style** - start with value, pipe through transformations
3. **WHEN vs THEN** - WHEN binds value, THEN ignores it
4. **Tagged objects** - use adhoc tags with named properties for state progression
5. **Error handling** - state accumulator or helper functions for flat structure
6. **Function parameters** - fixed by definition, can't rename at call site
7. **TEXT syntax** - no nesting, interpolation with `{var}`, named properties in tags

**Next Steps:**

When adding error handling to BUILD.bn, consider:
- State accumulator pattern (flat, transparent)
- Helper functions pattern (testable, clear)
- Error accumulation pattern (collect all errors)

---

**Last Updated:** 2025-01-15
**Related Files:** BUILD.bn, BOON_SYNTAX.md, TEXT_SYNTAX.md
