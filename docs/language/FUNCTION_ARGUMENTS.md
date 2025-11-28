# Function Arguments: Piping vs PASS/PASSED

This document clarifies two distinct mechanisms for passing values to functions in Boon.

## Piping (`|>`) - Syntactic Sugar for First Argument

Piping is purely syntactic sugar that lets you write the first argument before the function call instead of inside the parentheses. These are equivalent:

```boon
// Using pipe
x |> fn()

// Equivalent explicit form
fn(param: x)

// With additional arguments
x |> fn(other: y)
fn(param: x, other: y)
```

**Key point**: Piping just determines WHERE you write the first argument. The value is bound to the function's first parameter by position.

## PASS/PASSED - Implicit Context Threading

PASS/PASSED is a completely separate mechanism for threading implicit context through nested function calls without explicitly passing it at each level.

```boon
// Calling with PASS: provides implicit context
something |> outer_function(PASS: store)

FUNCTION outer_function(param) {
    // PASSED is available here (contains store)
    // param contains the piped value (something)

    inner_function()  // PASSED automatically available in nested calls
}

FUNCTION inner_function() {
    // PASSED.store is still available without re-passing
    PASSED.store.field
}
```

**Key point**: PASS/PASSED is for context that should flow through the call stack implicitly, similar to React Context or Scala implicit parameters.

## Comparison

| Aspect | Piping (`\|>`) | PASS/PASSED |
|--------|---------------|-------------|
| Purpose | Write first arg before function | Thread context through call stack |
| Binding | Binds to first parameter by position | Accessible via `PASSED` keyword |
| Propagation | Does not propagate | Automatically available in nested calls |
| Syntax | `x \|> fn()` | `fn(PASS: context)` |
| Access | Via parameter name | Via `PASSED` or `PASSED.field` |

## Example: TodoMVC Pattern

```boon
// store is passed as implicit context
app(PASS: store)

FUNCTION app() {
    Element/container(
        items: LIST {
            // PASSED.store is available in nested calls
            header(PASS: PASSED)  // Forward the context
            main_section(PASS: PASSED)
        }
    )
}

FUNCTION header() {
    // PASSED.store available here too
    new_todo_input(PASS: PASSED)
}
```

## Implementation Notes

- **Parser**: PASS: is parsed as a special argument with name "PASS"
- **Evaluator**: When PASS: argument is present, sets `actor_context.passed`
- **Nested calls**: PASSED value persists in actor_context through the call stack
- **Parameter binding**: Regular parameters are bound in `actor_context.parameters`
