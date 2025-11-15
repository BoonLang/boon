# Boon Error Handling Guide

**Date:** 2025-11-15
**Status:** Reference Guide
**Audience:** All Boon developers

---

## Table of Contents

1. [Introduction](#introduction)
2. [FLUSH Pattern](#flush-pattern)
3. [BLOCK Fundamentals](#block-fundamentals)
4. [Pipeline Patterns](#pipeline-patterns)
5. [WHEN vs THEN](#when-vs-then)
6. [Ok Tagging for Type Safety](#ok-tagging-for-type-safety)
7. [Alternative Error Patterns](#alternative-error-patterns)
8. [Tagged Objects](#tagged-objects)
9. [Common Pitfalls](#common-pitfalls)
10. [Best Practices](#best-practices)

---

## Introduction

Boon provides multiple approaches to error handling, from FLUSH pattern for fail-fast to transparent error propagation through tagged unions. This guide covers the patterns, semantics, and best practices.

**Key Principle:** Errors are values, not exceptions. FLUSH provides fail-fast ergonomics while maintaining transparent value propagation.

> **See Also:** [`FLUSH.md`](FLUSH.md) for comprehensive FLUSH specification including hardware implementation, parallel processing, and streaming semantics.

---

## FLUSH Pattern

### Core Semantics

**FLUSH** provides fail-fast error handling with transparent propagation:

- **FLUSH** exits the current expression and creates hidden `FLUSHED[value]` wrapper
- **FLUSHED[value]** propagates transparently (bypasses functions automatically)
- **Unwraps at boundaries** - variable bindings, function returns
- **No CATCH needed** - errors handled where variable is used

### Basic Example

```boon
FUNCTION process(item) {
    item
        |> operation()
        |> WHEN {
            Ok[result] => result
            error => FLUSH { error }  -- Exits expression
        }
        |> next_operation()  -- SKIPPED (bypassed) if error was FLUSHed
        |> WHEN {
            Ok[value] => value
            error => FLUSH { error }
        }
        |> WHEN { value =>
            Ok[processed: transform(value)]
        }
}
-- Returns: Ok[processed: T] | Error
-- FLUSHED[error] unwraps at function boundary
```

**Key Point:** FLUSHed errors unwrap at boundaries, becoming part of the return type.

### FLUSH for Early Exit

```boon
FUNCTION risky_operation(x) {
    x |> validate() |> WHEN {
        Invalid[reason] => FLUSH { ValidationError[reason: reason] }
        Valid[value] => process(value)
    }
}
-- Returns: T | ValidationError (after unwrapping)
-- Caller handles the error
```

### Error Handling at Variable Level

Instead of CATCH blocks, handle errors where the variable is used:

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
}
```

### Two-Binding Pattern

**Separate pipeline from error handling:**

```boon
generation_result: svg_files
    |> List/map(item =>
        item |> process() |> WHEN {
            Ok[value] => value
            error => FLUSH { error }  -- Stops List/map
        }
    )
    |> transform()  -- Bypassed if FLUSHed

-- Error handling in second binding
generation_error_handling: generation_result |> WHEN {
    Ok => handle_success()
    error => handle_error(error)
}
```

**Benefits:**
- Clear separation: pipeline vs error handling
- No CATCH blocks needed
- Error handling happens at natural boundary

### Ok Tagging Requirement

To distinguish success from errors in pattern matching, wrap success values in `Ok`:

```boon
-- ❌ WRONG: Bare pattern matches everything
|> WHEN {
    value => value        -- Matches BOTH success AND errors!
    error => FLUSH { ... } -- Never reached
}

-- ✅ CORRECT: Ok tagging
|> WHEN {
    Ok[value] => value      -- Only matches Ok
    error => FLUSH { error }  -- Matches all error types
}
```

**Function signature with Ok tagging:**
```boon
FUNCTION process(x) -> Ok[result: T] | ErrorType1 | ErrorType2
```

---

## BLOCK Fundamentals

### BLOCK Forms a Dependency Graph

**BLOCK variables execute in parallel, respecting dependencies:**

```boon
BLOCK {
    a: 1
    b: a + 1  -- Waits for 'a' to resolve
    c: b + 1  -- Waits for 'b' to resolve
    c        -- Return final value
}
-- Result: 3
```

**Think of BLOCK as a reactive dataflow graph, not sequential statements.**

### Cannot Redefine Variables

```boon
-- ❌ WRONG: Redefining 'state'
BLOCK {
    state: Ready[...]
    state: state |> WHEN { ... }  -- ERROR
}

-- ✅ CORRECT: Different names
BLOCK {
    state1: Ready[...]
    state2: state1 |> WHEN { ... }
    state3: state2 |> WHEN { ... }
    state3  -- Return final
}
```

### BLOCK Syntax: Variables + Final Expression

**BLOCK is for dependency graphs, NOT sequential execution:**

```boon
-- ✅ CORRECT: Variables + final expression
BLOCK {
    logged: message |> Log/error()
    count: items |> List/count()
    count  -- Return value
}

-- ❌ WRONG: Sequential statements
BLOCK {
    Log/error(message)  -- ERROR: Not a variable binding
    FLUSH { error }
}

-- ✅ CORRECT: Bind side effects to variables
BLOCK {
    logged: message |> Log/error()
    FLUSH { error }  -- Final expression
}
```

### Variables Can Reference Each Other

```boon
BLOCK {
    base: [color: red, gloss: 0.4]
    enhanced: [...base, metal: 0.03]
    final: enhanced.color
    final
}
```

Runtime builds dependency graph and evaluates in correct order.

---

## Pipeline Patterns

### Pure Pipeline Style

**Start with a value, pipe through transformations:**

```boon
-- ✅ CORRECT: Pure pipeline
item.path
    |> read_file()
    |> transform()
    |> WHEN { result => process(result) }
```

### Chaining Functions

**Happy path (no errors):**

```boon
item
    |> operation1()  -- T -> U
    |> operation2()  -- U -> V
    |> operation3()  -- V -> W
```

**With FLUSH:**

```boon
result: item
    |> operation1()
    |> WHEN {
        Ok[result] => result
        error => FLUSH { error }
    }
    |> operation2()  -- SKIPPED (bypassed) if FLUSHed

-- Handle at variable level
result |> WHEN {
    Ok[value] => use_value(value)
    Error1[msg] => handle_error1(msg)
    Error2[msg] => handle_error2(msg)
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

-- Pattern matching
state |> WHEN {
    Ready[data] => process(data)
    Failed[error] => log(error)
}
```

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

**Rule:** If you're not using the parameter, use THEN instead of WHEN.

---

## Ok Tagging for Type Safety

### The Problem

Without explicit tagging, bare patterns match everything:

```boon
FUNCTION process(item) {
    item
        |> operation()  -- Returns: TEXT | ReadError | EncodeError
        |> WHEN {
            text => text  -- ❌ Matches EVERYTHING (including errors!)
            error => ...  -- Never reached
        }
}
```

### The Solution

Wrap success values in `Ok` tag:

```boon
FUNCTION process(item) {
    item
        |> operation()
        |> WHEN {
            Ok[text] => text  -- ✅ Only matches Ok
            error => FLUSH { error }  -- ✅ Matches all errors
        }
        |> WHEN { text =>
            Ok[result: transform(text)]  -- ✅ Wrap success
        }
}
-- Type: Ok[result: T] | ReadError | EncodeError
```

### Why Ok Tagging

- ✅ Prevents accidental matching of error types
- ✅ Makes success case explicit in type signatures
- ✅ Enables safe pattern matching
- ✅ Works with all error handling patterns
- ✅ Self-documenting code

---

## Alternative Error Patterns

Beyond FLUSH, Boon supports several error handling approaches.

### Pattern 1: State Accumulator

**Use different variable names to track state progression:**

```boon
FUNCTION process_with_states(item) {
    BLOCK {
        state1: Ready[data: item]

        state2: state1 |> WHEN {
            Failed[error] => state1  -- Transparent propagation
            Ready[data] => operation1(data) |> WHEN {
                Error[msg] => Failed[stage: TEXT { op1 }, error: msg]
                Ok[result] => Loaded[data: result]
            }
        }

        state3: state2 |> WHEN {
            Failed[error] => state2  -- Transparent propagation
            Loaded[data] => operation2(data) |> WHEN {
                Error[msg] => Failed[stage: TEXT { op2 }, error: msg]
                Ok[result] => Done[value: result]
            }
        }

        state3 |> WHEN {
            Done[value] => Ok[result: value]
            Failed[stage, error] => Error[msg: TEXT { {stage}: {error} }]
        }
    }
}
```

**Benefits:**
- ✅ Flat structure (no deep nesting)
- ✅ Transparent error propagation
- ✅ Rich error context
- ✅ Each stage waits for previous

### Pattern 2: Helper Functions

```boon
FUNCTION process(item) {
    Ready[data: item]
        |> step1()
        |> step2()
        |> step3()
        |> finalize()
}

FUNCTION step1(state) {
    state |> WHEN {
        Failed[error] => state  -- Pass through errors
        Ready[data] => operation1(data) |> WHEN {
            Error[msg] => Failed[stage: TEXT { step1 }, error: msg]
            Ok[result] => Loaded[data: result]
        }
    }
}

FUNCTION step2(state) {
    state |> WHEN {
        Failed[error] => state
        Loaded[data] => operation2(data) |> WHEN {
            Error[msg] => Failed[stage: TEXT { step2 }, error: msg]
            Ok[result] => Done[value: result]
        }
    }
}
```

**Benefits:**
- ✅ Single responsibility per function
- ✅ Easy to test individually
- ✅ Transparent error propagation
- ✅ Clean pipeline

### Pattern 3: Nested WHEN (Compact)

```boon
FUNCTION process(item) {
    operation1(item) |> WHEN {
        Error[msg] => Error[msg: TEXT { Op1: {msg} }]
        Ok[result1] => operation2(result1) |> WHEN {
            Error[msg] => Error[msg: TEXT { Op2: {msg} }]
            Ok[result2] => Ok[final: result2]
        }
    }
}
```

**Benefits:**
- ✅ Compact (single function)
- ✅ Clear error vs success paths

**Drawbacks:**
- ⚠️ Nesting increases with operations
- ⚠️ Less reusable

---

## Tagged Objects

### Adhoc Tags for State Progression

**Create tags on-the-fly to represent states:**

```boon
Ready[data: T]
Loaded[content: T]
Processed[result: T]
Done[value: T]
Failed[stage: TEXT, error: TEXT]
```

**Use descriptive names showing progression:**
- `Ready` → `Loaded` → `Processed` → `Done`
- `Pending` → `Processing` → `Complete`
- `Input` → `Validated` → `Transformed` → `Output`

### Named Properties Required

```boon
-- ✅ CORRECT: Named properties
Ok[value: TEXT]
Error[message: TEXT]
Failed[stage: TEXT, reason: TEXT]

-- ❌ WRONG: Positional properties
Ok[TEXT]
Error[msg]
```

**All properties in tagged objects MUST be named.**

### Rich Error Context

```boon
-- Minimal (avoid):
Error[msg: TEXT { failed }]

-- Better:
Error[msg: TEXT { Read failed: {reason} }]

-- Best:
Failed[
    stage: TEXT { read }
    path: file_path
    reason: msg
    timestamp: Time/now()
]
```

---

## Common Pitfalls

### ❌ Redefining Variables in BLOCK

```boon
-- WRONG:
BLOCK {
    x: 1
    x: x + 1  -- ERROR
}

-- CORRECT:
BLOCK {
    x: 1
    y: x + 1
}
```

### ❌ Using BLOCK for Sequential Statements

```boon
-- WRONG:
BLOCK {
    Log/error(message)
    FLUSH { error }
}

-- CORRECT:
BLOCK {
    logged: message |> Log/error()
    FLUSH { error }
}
```

### ❌ Bare Pattern Matching Without Ok

```boon
-- WRONG:
WHEN {
    value => value  -- Matches everything!
    error => ...
}

-- CORRECT:
WHEN {
    Ok[value] => value
    error => FLUSH { error }
}
```

### ❌ Using WHEN When THEN Is Better

```boon
-- WRONG:
|> WHEN { result => log(TEXT { done }) }

-- CORRECT:
|> THEN { log(TEXT { done }) }
```

### ❌ Not Handling Errors at Boundary

```boon
-- INCOMPLETE: Error not handled
result: item
    |> operation()
    |> WHEN {
        Ok[x] => x
        error => FLUSH { error }
    }
-- result = T | Error, but never handled!

-- CORRECT: Handle at variable level
result: item
    |> operation()
    |> WHEN {
        Ok[x] => x
        error => FLUSH { error }
    }

result |> WHEN {
    Ok[value] => use_value(value)
    error => handle_error(error)
}
```

---

## Best Practices

### 1. Use Ok Tagging for Type Safety

```boon
-- ✅ DO:
FUNCTION process(x) {
    x |> operation()
      |> WHEN { result => Ok[value: result] }
}

-- ❌ DON'T:
FUNCTION process(x) {
    x |> operation()
      |> WHEN { result => result }  -- Unsafe
}
```

### 2. Choose the Right Pattern

**Use FLUSH when:**
- Fail-fast behavior is desired
- Working with collections (List/map stops on first error)
- Want to skip remaining pipeline steps on error
- Hardware/FPGA implementation matters

**Use State Accumulator when:**
- You need transparent error propagation
- Rich error context is important
- Multiple error types per stage
- Want to accumulate all errors (no FLUSH)

**Use Helper Functions when:**
- Pipeline has many stages
- Testability is critical
- Single responsibility is important

### 3. BLOCK for Dependencies, Not Statements

```boon
-- ✅ DO:
BLOCK {
    validated: input |> validate()
    processed: validated |> process()
    saved: processed |> save()
    saved
}

-- ❌ DON'T:
BLOCK {
    validate(input)
    process()
    save()
}
```

### 4. Use THEN When Ignoring Values

```boon
-- ✅ DO:
|> THEN { count: items |> List/count() }

-- ❌ DON'T:
|> WHEN { __ => count: items |> List/count() }
```

### 5. Provide Rich Error Context

```boon
-- ✅ DO:
Failed[
    operation: TEXT { file_read }
    path: file_path
    reason: error_message
]

-- ❌ DON'T:
Error[msg: TEXT { failed }]
```

### 6. No Abbreviations

```boon
-- ✅ DO:
Error[message: TEXT { ... }]

-- ❌ DON'T:
Error[msg: TEXT { ... }]
```

---

## Summary

**Key Principles:**

1. **FLUSH** - Fail-fast error handling with transparent propagation
2. **Ok Tagging** - Essential for type-safe pattern matching
3. **BLOCK** - Dependency graph, not sequential statements
4. **THEN vs WHEN** - THEN ignores value, WHEN binds it
5. **Tagged Objects** - Use adhoc tags with named properties
6. **Multiple Patterns** - Choose based on needs (FLUSH, State Accumulator, Helper Functions)

**Common Pattern:**

```boon
FUNCTION process(item) {
    item
        |> operation()
        |> WHEN {
            Ok[result] => result
            error => FLUSH { error }
        }
        |> WHEN { result =>
            Ok[value: transform(result)]
        }
}

-- Caller with two-binding pattern:
result: items |> List/map(item => process(item))

result |> WHEN {
    Ok[values] => handle_success(values)
    error => handle_error(error)
}
```

---

**Related Documents:**
- `../build/BUILD_SYSTEM.md` - Error handling in BUILD.bn context
- `BOON_SYNTAX.md` - Core language syntax
- `TAGGED_UNIONS.md` - Tagged union types

**Last Updated:** 2025-11-15
