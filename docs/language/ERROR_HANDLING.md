# Boon Error Handling Guide

**Date:** 2025-11-15
**Status:** Reference Guide
**Audience:** All Boon developers

---

## Table of Contents

1. [Introduction](#introduction)
2. [THROW/CATCH Pattern](#throwcatch-pattern)
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

Boon provides multiple approaches to error handling, from explicit THROW/CATCH to transparent error propagation through tagged unions. This guide covers the patterns, semantics, and best practices.

**Key Principle:** Errors are values, not exceptions (though THROW/CATCH provides exception-like ergonomics).

---

## THROW/CATCH Pattern

### Core Semantics

**THROW/CATCH** provides exception-style error handling similar to PASS/PASSED:

- **THROW** exits the pipeline immediately, jumping to the nearest CATCH
- Execution **skips intermediate steps** until CATCH is reached
- **CATCH is mandatory** - compilation error if THROW is not caught
- Errors flow through a separate channel (like PASS/PASSED)

### Basic Example

```boon
FUNCTION process(item) {
    item
        |> operation()
        |> WHEN {
            Ok[result] => result
            error => THROW { error }
        }
        |> next_operation()  -- SKIPPED if error was thrown
        |> WHEN {
            Ok[value] => value
            error => THROW { error }
        }
        |> WHEN { value =>
            Ok[processed: transform(value)]
        }
}
-- Returns: Ok[processed: T] | Error
-- No CATCH - errors propagate to caller
```

**Key Point:** Functions without CATCH return union types where thrown errors become part of the return type.

### THROW Without CATCH

```boon
FUNCTION risky_operation(x) {
    x |> validate() |> WHEN {
        Invalid[reason] => THROW { ValidationError[reason: reason] }
        Valid[value] => process(value)
    }
}
-- Returns: T | ValidationError
-- Caller must handle the error
```

### CATCH Handling

```boon
value
    |> risky_operation()
    |> CATCH {
        ValidationError[reason] => BLOCK {
            logged: TEXT { Validation failed: {reason} } |> Log/error()
            default_value()
        }
    }
-- Returns: T (error handled, returns default)
```

### THEN/CATCH Mutual Exclusion

**THEN and CATCH are mutually exclusive** - only one executes:

```boon
pipeline
    |> operation()
    |> THEN {
        -- Only runs if no THROW occurred
        success_handling()
    }
    |> CATCH {
        -- Only runs if THROW occurred
        error => error_handling(error)
    }
```

**Execution paths:**
- **Success**: operation → THEN → return value from THEN
- **Error**: operation → THROW → CATCH → return value from CATCH

### Ok Tagging Requirement

To distinguish success from errors in pattern matching, wrap success values in `Ok`:

```boon
-- ❌ WRONG: Bare pattern matches everything
|> WHEN {
    value => value        -- Matches BOTH success AND errors!
    error => THROW { ... } -- Never reached
}

-- ✅ CORRECT: Ok tagging
|> WHEN {
    Ok[value] => value    -- Only matches Ok
    error => THROW { error }  -- Matches all error types
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
    THROW { error }
}

-- ✅ CORRECT: Bind side effects to variables
BLOCK {
    logged: message |> Log/error()
    THROW { error }  -- Final expression
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

**With THROW/CATCH:**

```boon
item
    |> operation1()
    |> WHEN {
        Ok[result] => result
        error => THROW { error }
    }
    |> operation2()  -- SKIPPED if thrown
    |> WHEN {
        Ok[result] => result
        error => THROW { error }
    }
    |> CATCH {
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
            error => THROW { error }  -- ✅ Matches all errors
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

Beyond THROW/CATCH, Boon supports several error handling approaches.

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
    THROW { error }
}

-- CORRECT:
BLOCK {
    logged: message |> Log/error()
    THROW { error }
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
    error => THROW { error }
}
```

### ❌ Using WHEN When THEN Is Better

```boon
-- WRONG:
|> WHEN { result => log(TEXT { done }) }

-- CORRECT:
|> THEN { log(TEXT { done }) }
```

### ❌ Forgetting CATCH

```boon
-- WRONG (compilation error):
item
    |> operation()
    |> WHEN {
        Ok[x] => x
        error => THROW { error }
    }
-- ERROR: Uncaught THROW

-- CORRECT:
item
    |> operation()
    |> WHEN {
        Ok[x] => x
        error => THROW { error }
    }
    |> CATCH {
        error => handle(error)
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

**Use THROW/CATCH when:**
- You want familiar exception-like semantics
- Fail-fast behavior is desired
- Team familiarity matters

**Use State Accumulator when:**
- You need transparent error propagation
- Rich error context is important
- Multiple error types per stage

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

1. **THROW/CATCH** - Exception-like error handling, CATCH mandatory
2. **Ok Tagging** - Essential for type-safe pattern matching
3. **BLOCK** - Dependency graph, not sequential statements
4. **THEN vs WHEN** - THEN ignores value, WHEN binds it
5. **Tagged Objects** - Use adhoc tags with named properties
6. **Multiple Patterns** - Choose based on needs (THROW/CATCH, State Accumulator, Helper Functions)

**Common Pattern:**

```boon
FUNCTION process(item) {
    item
        |> operation()
        |> WHEN {
            Ok[result] => result
            error => THROW { error }
        }
        |> WHEN { result =>
            Ok[value: transform(result)]
        }
}

-- Caller:
items
    |> List/map(item => process(item))
    |> THEN { results => handle_success(results) }
    |> CATCH { error => handle_error(error) }
```

---

**Related Documents:**
- `../build/BUILD_SYSTEM.md` - Error handling in BUILD.bn context
- `BOON_SYNTAX.md` - Core language syntax
- `TAGGED_UNIONS.md` - Tagged union types

**Last Updated:** 2025-11-15
