# Boon BUILD Patterns & Learnings

**Date:** 2025-01-15
**Updated:** 2025-11-15
**Context:** Lessons from refactoring BUILD.bn with error handling patterns
**Status:** Reference Guide - Finalized with THROW/CATCH pattern

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

### BLOCK Contains Only Variable Bindings + Final Expression

**BLOCK syntax is for defining variables, NOT executing statements:**

```boon
-- ✅ CORRECT: Variables + final expression
BLOCK {
    x: 1
    y: x + 1
    z: y * 2
    z  -- Final expression (return value)
}

-- ✅ CORRECT: Variables with side effects, final return
BLOCK {
    logged: message |> Log/error()
    count: items |> List/count()
    count  -- Return count
}

-- ❌ WRONG: Sequential statements
BLOCK {
    Log/error(message)
    THROW { Error[msg] }
}

-- ✅ CORRECT: Use variable bindings
BLOCK {
    logged: message |> Log/error()
    THROW { Error[msg] }  -- Final expression
}

-- ✅ CORRECT: Or use THEN for sequencing
message |> Log/error()
    |> THEN { THROW { Error[msg] } }
```

**Key rule:** BLOCK is for **dependency graphs**, not **sequential execution**.

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

### Overview: Two Main Approaches

**Approach 1: THROW/CATCH** (Lightweight, familiar)
- Explicit error throwing with THROW
- Error boundaries with CATCH
- No union types needed
- Familiar to mainstream developers

**Approach 2: State Accumulator** (Type-safe, compositional)
- Track state through pipeline with different variable names
- Transparent error propagation via pattern matching
- Errors carry full context
- More functional style

---

### Pattern 0: THROW/CATCH (Finalized - Used in BUILD.bn)

**Lightweight exception-style error handling with Ok tagging for type safety.**

**Key Semantics:**
- **THROW** exits the pipeline immediately (like exceptions)
- Execution **jumps** to nearest CATCH (skipping intermediate steps)
- **CATCH is mandatory** - compilation error if missing
- Similar to PASS/PASSED (separate channel for errors)
- Functions return Ok-tagged values to distinguish from errors
- THEN and CATCH are mutually exclusive paths

#### Basic Function Pattern

```boon
FUNCTION icon_code(item) {
    item.path
        |> File/read_text()
        |> WHEN {
            Ok[text] => text
            error => THROW { error }
        }
        |> Url/encode()  -- SKIPPED if error was thrown
        |> WHEN {
            Ok[encoded] => encoded
            error => THROW { error }
        }
        |> WHEN { encoded =>
            Ok[text: TEXT { {item.file_stem}: data:image/svg+xml;utf8,{encoded} }]
        }
}
-- Returns: Ok[text: TEXT] | ReadError | EncodeError
-- No CATCH - errors propagate to caller
```

**Key Point:** Return `Ok[text: ...]` to wrap success value, making it distinguishable from error types.

#### Fail-Fast with List/try_map

```boon
generation: svg_files
    |> List/try_map(old, new:
        old |> icon_code() |> WHEN {
            Ok[text] => text        -- Extract success value
            error => THROW { error } -- Stop on first error
        }
    )
    -- Returns: List[TEXT] if all succeed, or first error
    |> Text/join_lines()
    |> WHEN { code => TEXT {
        -- Generated from {icons_directory}
        icon: [
            {code}
        ]
    } }
    |> File/write_text(path: output_file)
    |> WHEN {
        Ok => []
        error => THROW { error }
    }
    |> THEN { BLOCK {
        count: svg_files |> List/count()
        logged: TEXT { Included {count} icons } |> Log/info()
        Build/succeed()
    } }
    |> CATCH {
        ReadError[message] => BLOCK {
            logged: TEXT { Cannot read icon: {message} } |> Log/error()
            Build/fail()
        }
        EncodeError[message] => BLOCK {
            logged: TEXT { Cannot encode icon: {message} } |> Log/error()
            Build/fail()
        }
        WriteError[message] => BLOCK {
            logged: TEXT { Cannot write {output_file}: {message} } |> Log/error()
            Build/fail()
        }
    }
```

**Execution Flow:**
1. **Success Path**: try_map → join_lines → write → THEN → Build/succeed()
2. **Error Path**: THROW at any point → skip to CATCH → Build/fail()
3. **Mutual Exclusion**: Only THEN or CATCH runs, not both

#### Accumulate Errors with List/collect_map

```boon
generation: svg_files
    |> List/collect_map(old, new:
        old |> icon_code() |> WHEN {
            Ok[text] => Ok[item: text]  -- Keep Ok wrapper
            error => error               -- Pass through errors
        }
    )
    -- Returns: Ok[icons: List[TEXT]] | Errors[errors: List[Error]]
    |> WHEN {
        Ok[icons] => icons
        Errors[errors] => THROW { IconErrors[errors: errors] }
    }
    |> Text/join_lines()
    |> File/write_text(path: output_file)
    |> WHEN {
        Ok => []
        error => THROW { error }
    }
    |> THEN { BLOCK {
        logged: TEXT { Build succeeded } |> Log/info()
        Build/succeed()
    } }
    |> CATCH {
        IconErrors[errors] => BLOCK {
            count: errors |> List/count()
            logged_all: errors |> List/each(e => e |> WHEN {
                ReadError[message] => TEXT { Cannot read: {message} } |> Log/error()
                EncodeError[message] => TEXT { Cannot encode: {message} } |> Log/error()
            })
            logged_summary: TEXT { Build failed: {count} icon errors } |> Log/error()
            Build/fail()
        }
        WriteError[message] => BLOCK {
            logged: TEXT { Cannot write {output_file}: {message} } |> Log/error()
            Build/fail()
        }
    }
```

**Difference from try_map:**
- `try_map`: Stops on first error (fail-fast)
- `collect_map`: Processes all items, collects all errors (comprehensive reporting)

#### Ok Tagging for Type Safety

**Problem - Bare pattern matching:**
```boon
|> WHEN {
    text => text        -- ❌ Matches EVERYTHING (including errors!)
    error => ...        -- Never reached
}
```

**Solution - Ok tagging:**
```boon
|> WHEN {
    Ok[text] => text    -- ✅ Only matches Ok
    error => THROW { error }  -- ✅ Matches all errors
}
```

**Why Ok tagging is essential:**
- Prevents accidental matching of error types
- Makes success case explicit in function return type
- Enables safe pattern matching in List/try_map and List/collect_map
- Type signature: `Ok[text: TEXT] | ReadError | EncodeError`

#### THEN/CATCH Mutual Exclusion

```boon
|> THEN { BLOCK {
    logged: TEXT { Success } |> Log/info()
    Build/succeed()
} }
|> CATCH {
    error => BLOCK {
        logged: error |> Log/error()
        Build/fail()
    }
}
```

**Semantics:**
- **THEN** runs only if no THROW occurred (success path)
- **CATCH** runs only if THROW occurred (error path)
- Never both - they're mutually exclusive alternatives
- Both return values that merge back into the pipeline
- Use `Build/succeed()` and `Build/fail()` to communicate build status

#### BLOCK Usage with Side Effects

**Correct - variable bindings:**
```boon
CATCH {
    WriteError[message] => BLOCK {
        logged: TEXT { Cannot write: {message} } |> Log/error()
        Build/fail()  -- Final expression (return value)
    }
}
```

**Incorrect - sequential statements:**
```boon
BLOCK {
    Log/error(message)  -- ❌ Not a variable binding
    Build/fail()
}
```

**Rule:** BLOCK contains only variable bindings plus final expression.

#### Benefits

- ✅ Type-safe with Ok tagging
- ✅ Fail-fast or accumulate errors (try_map vs collect_map)
- ✅ Explicit error paths (THROW makes flow visible)
- ✅ Familiar to mainstream developers (like try/catch)
- ✅ Clean separation (THEN for success, CATCH for errors)
- ✅ Composable (functions can throw, caller catches)
- ✅ No abbreviations (message not msg)
- ✅ Proper logging with side effects in BLOCK

#### When to Use

- ✅ Build scripts (like BUILD.bn)
- ✅ When you want fail-fast behavior
- ✅ When comprehensive error reporting is needed
- ✅ When type safety matters (Ok tagging prevents mistakes)
- ✅ When team familiarity is important

---

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

### ❌ Using BLOCK for Sequential Statements

```boon
-- WRONG: BLOCK is not for statements
BLOCK {
    Log/error(message)
    THROW { Error[msg] }
}

-- CORRECT: Use variable bindings
BLOCK {
    logged: message |> Log/error()
    THROW { Error[msg] }  -- Final expression
}

-- CORRECT: Or use THEN
message |> Log/error()
    |> THEN { THROW { Error[msg] } }
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

1. **THROW/CATCH is finalized** - Use for BUILD.bn error handling with Ok tagging
2. **Ok tagging essential** - Wrap success values in `Ok[item: value]` for type safety
3. **Fail-fast vs accumulate** - `List/try_map` (stop on first) vs `List/collect_map` (all errors)
4. **THEN/CATCH mutual exclusion** - Only one executes, never both
5. **BLOCK forms dependency graph** - Variables wait for dependencies, can't be redefined
6. **Pipeline style** - Start with value, pipe through transformations
7. **WHEN vs THEN** - WHEN binds value, THEN ignores it
8. **Tagged objects** - Use adhoc tags with named properties
9. **Function parameters** - Fixed by definition, can't rename at call site
10. **TEXT syntax** - No nesting, interpolation with `{var}`, named properties in tags
11. **No abbreviations** - Use `message` not `msg` for clarity
12. **Build status** - Use `Build/succeed()` and `Build/fail()` to communicate results

**Finalized Error Handling Pattern (from BUILD.bn):**

```boon
-- Function returns Ok[value] or errors
FUNCTION process(item) {
    item
        |> operation()
        |> WHEN {
            Ok[result] => result
            error => THROW { error }
        }
        |> WHEN { result =>
            Ok[value: process(result)]
        }
}

-- Caller uses try_map for fail-fast
items
    |> List/try_map(old, new:
        old |> process() |> WHEN {
            Ok[value] => value
            error => THROW { error }
        }
    )
    |> THEN { BLOCK {
        logged: TEXT { Success } |> Log/info()
        Build/succeed()
    } }
    |> CATCH {
        error => BLOCK {
            logged: error |> Log/error()
            Build/fail()
        }
    }
```

**Implementation Status:**

✅ **BUILD.bn** - Finalized with THROW/CATCH, List/try_map, Ok tagging
✅ **BUILD_SIMPLE_ERRORS.bn** - Same as BUILD.bn (fail-fast pattern)
✅ **BUILD_WITH_ERRORS.bn** - Error accumulation with List/collect_map
✅ **ERROR_HANDLING_LEVELS.md** - Progressive complexity levels documented
✅ **BUILD_SYSTEM.md** - Comprehensive error handling guide added

---

**Last Updated:** 2025-11-15
**Related Files:** BUILD.bn, BUILD_SIMPLE_ERRORS.bn, BUILD_WITH_ERRORS.bn, ERROR_HANDLING_LEVELS.md, BUILD_SYSTEM.md
