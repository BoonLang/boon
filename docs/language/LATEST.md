# LATEST: Reactive State in Boon

**Date**: 2025-01-18
**Status**: Design Document
**Scope**: LATEST semantics for Software and Hardware

---

## Overview

LATEST provides event merging in Boon, working seamlessly across software (event-driven) and hardware (clock-driven) contexts.

**Note:** For stateful constructs with self-reference, see **HOLD**:
```boon
initial |> HOLD state { event |> THEN { state + 1 } }
```

**LATEST** merges multiple event sources without self-reference.

**Key principle:** Non-self-reactive - neither LATEST nor HOLD react to their own state changes.

---

## Supported Types in LATEST

LATEST manages state for specific types. Understanding what LATEST supports is crucial for writing correct reactive code.

### ✅ Supported Types

**1. Scalars** - Simple primitive values
- `Number`, `Text`
- Example: `counter: 0 |> HOLD count { count + 1 }`

**2. Tags/Enums** - Enumeration values
- Built-in tags like `True`, `False`
- User-defined state types for state machines
- Example: `state: Idle |> HOLD state { state |> next_state() }`
- Example: `flag: True |> HOLD flag { toggle |> THEN { flag |> Bool/not() } }`

**3. BITS** - Hardware bit vectors
- Fixed-width bit patterns for hardware
- Example: `reg: BITS[8] { 10u0  } |> HOLD reg { reg + 1 }`

**4. Objects** - Structured data with named fields
- Composite values like `[x: 0, y: 0]`, `[counter: 5, enabled: True]`
- Example: `pos: [x: 0, y: 0] |> HOLD pos { [x: pos.x + 1, y: pos.y] }`

### ❌ NOT Supported - Use Alternatives Instead

**LIST** - Don't use LIST in LATEST! Use reactive LIST operations:

```boon
// ❌ DON'T: LIST in LATEST (anti-pattern)
items: LIST {} |> HOLD list {
    event |> THEN { list |> List/push(item) }
}

// ✅ DO: Reactive LIST operations (no LATEST needed)
items: LIST {}
    |> List/push(item: new_event)
    |> List/take_last(count: 100)
```

**Fixed-size collections** - Use MEMORY primitive instead:

```boon
// ❌ DON'T: Collections for indexed storage
mem: List/range(0, 16) |> HOLD mem {
    mem |> List/set(index: addr, value: data)
}

// ✅ DO: Use MEMORY for fixed-size indexed storage
mem: MEMORY[16] { 0  }
    |> Memory/write(address: addr, data: data)
```

**Why no LIST in LATEST?**
1. **Different reactivity models:**
   - LIST is reactive at collection level (sends VecDiff operations)
   - LATEST is for value-level state tracking
   - Mixing these creates confusion

2. **Performance:**
   - LIST operations with structural sharing have overhead
   - Per-update copies can be expensive

3. **Better alternatives exist:**
   - **Dynamic collections:** Use reactive LIST operations directly
   - **Fixed-size storage:** Use MEMORY primitive
   - **Iteration:** Use PULSES (see PULSES.md)

4. **Simpler mental model:**
   - LATEST for simple stateful values
   - LIST/MEMORY for collections
   - Clear separation of concerns

---

## Quick Examples

### Software

```boon
// Simple event merging
mode: LATEST {
    start_button.click |> THEN { Running }
    stop_button.click |> THEN { Stopped }
}

// Stateful counter (with self-reference) - uses HOLD
counter: 0 |> HOLD count {
    LATEST {
        increment |> THEN { count + 1 }
        decrement |> THEN { count - 1 }
    }
}
```

### Hardware

```boon
// Simple register (set to constants)
flag: LATEST {
    set_signal |> WHEN { True => True, False => SKIP }
    clear_signal |> WHEN { True => False, False => SKIP }
}

// Stateful register (compute from current) - uses HOLD
counter: 0 |> HOLD count {
    control |> WHEN {
        [inc: True] => count + 1
        [dec: True] => count - 1
        __ => count
    }
}
```

---

## Two Forms of LATEST

### Form 1: Simple LATEST (Event Merging)

**Syntax:**
```boon
LATEST {
    event1 |> THEN { value1 }
    event2 |> THEN { value2 }
    default_value
}
```

**Characteristics:**
- ✅ No self-reference
- ✅ Merges multiple event sources
- ✅ Sets to constant values or expressions
- ✅ Starts UNDEFINED (or with default value)

**Software example:**
```boon
latest_click: LATEST {
    button1.click |> THEN { 1 }
    button2.click |> THEN { 2 }
    0  // Default
}
```

**Hardware example:**
```boon
mode: LATEST {
    rst |> WHEN { True => Idle, False => SKIP }
    start |> WHEN { True => Running, False => SKIP }
    stop |> WHEN { True => Stopped, False => SKIP }
}
```

---

### Form 2: HOLD (Stateful with Self-Reference)

**Note:** The stateful form is now a separate construct called **HOLD**.

**Syntax:**
```boon
initial_value |> HOLD parameter_name {
    single_expression
}
```

**Characteristics:**
- ✅ Has self-reference via parameter
- ✅ Can compute from current value
- ✅ Initial value explicit (piped in)
- ✅ User-chosen parameter name
- ✅ Single-arm body (compose with LATEST for multiple events)

**Software example:**
```boon
counter: 0 |> HOLD count {
    LATEST {
        increment |> THEN { count + 1 }
        decrement |> THEN { count - 1 }
    }
}
```

**Hardware example:**
```boon
state: B |> HOLD current {
    PASSED.clk |> THEN {
        rst |> WHEN {
            True => B
            False => current |> WHEN {
                A => C
                B => D
                C => a |> WHEN { True => D, False => B }
                D => A
            }
        }
    }
}
```

**Parameter naming conventions:**
- Counters: `count`, `current`, `value`
- State machines: `state`, `current`
- Accumulators: `sum`, `total`, `acc`, `accumulator`

---

## Non-Self-Reactive Semantics

**Key safety principle:** LATEST doesn't react to its own state changes!

### This Makes Expressions Like `state + 1` Safe

```boon
// ✅ SAFE - evaluates once, no infinite loop
value: 0 |> HOLD v { v + 1 }

// Execution:
// 1. v = 0 (piped value)
// 2. Evaluate: v + 1 = 1
// 3. Update: v = 1
// 4. ❌ Don't re-trigger (change from inside LATEST)
// 5. Result: v = 1 (stays)
```

### LATEST Reacts to INPUTS

**Inputs that trigger re-evaluation:**
- ✅ Piped value changes
- ✅ Events referenced in body fire
- ✅ External signals change

**Does NOT react to:**
- ❌ Its own state changes
- ❌ Updates from inside the body

### Examples

**Constant input (evaluates once):**
```boon
value: 0 |> HOLD v { v + 1 }
// Result: v = 1 (evaluates once, no re-trigger)
```

**Event input (evaluates when event fires):**
```boon
counter: 0 |> HOLD count {
    increment |> THEN { count + 1 }
}
// When increment fires → count + 1 → stays until next increment
```

**Reactive input (evaluates when input changes):**
```boon
counter: reset_value |> HOLD count { count + 1 }
// When reset_value changes → count = reset_value + 1 → stays
```

**Like React's useEffect:**
- Depends on inputs (dependencies)
- Doesn't depend on its own state
- Evaluates once per input change
- Prevents infinite loops naturally!

---

## Exhaustive WHEN and SKIP

**WHEN is exhaustive** - must handle all cases.

### SKIP Semantics

`SKIP` means "don't update, stay at current value"

```boon
// ❌ ERROR: Missing False case
flag: LATEST {
    set_signal |> WHEN { True => True }
}
// Error: WHEN must be exhaustive. Add: False => SKIP

// ✅ OK: All cases handled
flag: LATEST {
    set_signal |> WHEN { True => True, False => SKIP }
    clear_signal |> WHEN { True => False, False => SKIP }
}
```

### SKIP Cascades

```boon
mode: LATEST {
    rst |> WHEN { True => Idle, False => SKIP }
    start |> WHEN { True => Running, False => SKIP }
    stop |> WHEN { True => Stopped, False => SKIP }
}
// If all evaluate to SKIP → mode stays at current value
```

### Wildcard Alternative

```boon
counter: 0 |> HOLD count {
    control |> WHEN {
        [inc: True, dec: False] => count + 1
        [inc: False, dec: True] => count - 1
        __ => count  // Wildcard = explicit stay
    }
}
```

**Difference:**
- `SKIP` in individual branches → stay if that branch taken
- `__ => value` at end → default value if no match

---

## Software Context (Event-Driven)

### Simple LATEST

```boon
mode: LATEST {
    start_event |> THEN { Running }
    stop_event |> THEN { Stopped }
}
```

**Execution:**
- Initially: UNDEFINED (or default value)
- When `start_event` fires → `mode` = Running
- When `stop_event` fires → `mode` = Stopped
- When neither fires → `mode` stays unchanged
- Reactive - only re-evaluates when events fire

### Piped LATEST

```boon
counter: 0 |> HOLD count {
    increment |> THEN { count + 1 }
    decrement |> THEN { count - 1 }
}
```

**Execution:**
- Initially: `count` = 0 (piped value)
- When `increment` event fires → `count` = count + 1
- When `decrement` event fires → `count` = count - 1
- When neither fires → `count` stays at current value
- Non-self-reactive prevents infinite loop!

### More Examples

**Accumulator:**
```boon
total: 0 |> HOLD sum {
    new_value |> THEN { sum + new_value }
}
```

**State machine:**
```boon
mode: Idle |> HOLD state {
    start_event |> THEN { Running }
    state |> WHEN {
        Running => pause_event |> THEN { Paused }
        Paused => resume_event |> THEN { Running }
        __ => state
    }
}
```

---

## Hardware Context (Clock-Driven)

### Simple LATEST (Register Without Self-Reference)

```boon
flag: LATEST {
    set_signal |> WHEN { True => True, False => SKIP }
    clear_signal |> WHEN { True => False, False => SKIP }
}
```

**Execution:**
- Initially: UNDEFINED
- **Every clock cycle**, evaluate conditions:
  - If `set_signal` is True → `flag` <= True
  - Else if `clear_signal` is True → `flag` <= False
  - Else (all SKIP) → `flag` <= flag (stay)

**Maps to SystemVerilog:**
```systemverilog
logic flag;

always_ff @(posedge clk) begin
    if (set_signal)
        flag <= 1'b1;
    else if (clear_signal)
        flag <= 1'b0;
    // else: stay (all SKIP cases)
end
```

**Use when:**
- ✅ Setting to constant values
- ✅ No need for self-reference
- ✅ Simple state machines

### Piped LATEST (Register With Self-Reference)

```boon
counter: 0 |> HOLD count {
    control |> WHEN {
        [inc: True, dec: False] => count + 1
        [inc: False, dec: True] => count - 1
        __ => count
    }
}
```

**Execution:**
- Initially: `count` = 0 (piped value)
- **Every clock cycle**, evaluate WHEN:
  - If pattern matches → update
  - If no pattern matches → stay at count

**Maps to SystemVerilog:**
```systemverilog
logic [7:0] count;

always_ff @(posedge clk) begin
    case ({inc, dec})
        2'b10: count <= count + 1;
        2'b01: count <= count - 1;
        default: count <= count;
    endcase
end
```

**Use when:**
- ✅ Computing from current value
- ✅ Counters, accumulators
- ✅ Complex FSMs with state transformations

### More Examples

**FSM:**
```boon
state: B |> HOLD current {
    rst |> WHEN {
        True => B
        False => current |> WHEN {
            A => C
            B => D
            C => a |> WHEN { True => D, False => B }
            D => A
        }
    }
}
```

**LFSR:**
```boon
lfsr: BITS[8] { 10u0  } |> HOLD current {
    rst |> WHEN {
        True => BITS[8] { 10u0  }
        False => BLOCK {
            feedback: current |> Bits/get(7) |> Bool/xor(current |> Bits/get(3))
            current |> Bits/shift_right(1) |> Bits/set(7, feedback)
        }
    }
}
```

---

## LATEST as Building Block for Stateful Operations

**Math/sum can be implemented as LATEST:**

```boon
FUNCTION Math/sum(stream) {
    0 |> HOLD accumulator {
        stream |> THEN { value =>
            accumulator + value
        }
    }
}

// Usage
sum: value |> Math/sum()

// Expands to
sum: 0 |> HOLD accumulator {
    value |> THEN { v => accumulator + v }
}
```

**Other stateful operations:**

```boon
// Math/product
FUNCTION Math/product(stream) {
    1 |> HOLD accumulator {
        stream |> THEN { v => accumulator * v }
    }
}

// Bool/toggle
FUNCTION Bool/toggle(stream, event) {
    False |> HOLD state {
        event |> THEN { state |> Bool/not() }
    }
}
```

**Benefits:**
- ✅ LATEST is fundamental primitive
- ✅ Users can write custom accumulators
- ✅ Shows power and simplicity of LATEST

---

## Safety Rules Summary

### Core Principles

1. **Non-self-reactive** - Prevents infinite loops
   - Reacts to inputs (piped value, events)
   - Doesn't react to output (its own state)
   - Makes `v |> HOLD v { v + 1 }` safe!

2. **Exhaustive WHEN** - Explicit control flow
   - Use `SKIP` to mean "stay at current"
   - Or use wildcard `__ => value` for default
   - Compiler enforces exhaustiveness

3. **Two forms, clear purposes**
   - Simple LATEST: Event merging, no self-ref
   - Piped LATEST: Stateful, with self-ref

### Software (Event-Driven)

```boon
// ✅ Simple LATEST - event merging
latest: LATEST {
    event1 |> THEN { value1 }
    event2 |> THEN { value2 }
}

// ✅ Piped LATEST - stateful
counter: 0 |> HOLD count {
    increment |> THEN { count + 1 }
}
```

**Rule:** Only evaluates when inputs change
- Expressions only evaluate when events fire
- No event = no evaluation = stay at current
- Safe by design!

### Hardware (Clock-Driven)

```boon
// ✅ Simple LATEST - register without self-ref
flag: LATEST {
    set |> WHEN { True => True, False => SKIP }
}

// ✅ Piped LATEST - register with self-ref
counter: 0 |> HOLD count {
    control |> WHEN { [inc: True] => count + 1, __ => count }
}
```

**Rule:** Both forms create registers
- Simple LATEST: For setting to constants
- Piped LATEST: For computing from current
- Both evaluated every clock cycle
- SKIP/wildcard provides stay behavior

---

## LATEST vs PULSES

**Two complementary abstractions:**

| Feature | LATEST | PULSES |
|---------|--------|--------|
| **Purpose** | Reactive state | Counted iteration |
| **Trigger** | External events | Internal counter |
| **Evaluation** | Event-driven | Iteration-driven |
| **Use cases** | Counters, FSMs, accumulators | Fibonacci, sequences, loops |

**When to use LATEST alone:**
- ✅ Reactive state (software events, hardware registers)
- ✅ Event-driven updates
- ✅ External triggers (button clicks, signals)

**When to use LATEST + PULSES:**
- ✅ Counted iteration (N times)
- ✅ Sequence generation (Fibonacci, factorial)
- ✅ Iterative algorithms

**Example comparison:**

```boon
// LATEST - reactive counter (event-driven)
counter: 0 |> HOLD count {
    increment |> THEN { count + 1 }
}
// Updates when increment event fires

// LATEST + PULSES - counted iteration
counter: 0 |> HOLD count {
    PULSES { 10 } |> THEN { count + 1 }
}
// Counts to 10 automatically
```

---

## Complete Examples

### Software: Todo Counter

```boon
todo_count: 0 |> HOLD count {
    add_todo_event |> THEN { count + 1 }
    remove_todo_event |> THEN { count - 1 }
    clear_all_event |> THEN { 0 }
}
```

### Software: State Machine

```boon
app_state: Idle |> HOLD state {
    start_event |> THEN { Loading }
    state |> WHEN {
        Loading => data_loaded_event |> THEN { Ready }
        Ready => edit_event |> THEN { Editing }
        Editing => save_event |> THEN { Saving }
        Saving => save_complete_event |> THEN { Ready }
        __ => state
    }
}
```

### Hardware: Loadable Counter

```boon
FUNCTION counter(rst, load, load_value, en) {
    BLOCK {
        default: BITS[8] { 10u0  }
        control: [reset: rst, load: load, enabled: en]

        count: default |> HOLD current {
            control |> WHEN {
                [reset: True, load: __, enabled: __] => default
                [reset: False, load: True, enabled: True] => load_value
                [reset: False, load: False, enabled: True] => current + 1
                __ => current
            }
        }

        [count: count]
    }
}
```

### Hardware: FSM with Output Logic

```boon
FUNCTION fsm(rst, a) {
    BLOCK {
        state: B |> HOLD current {
            rst |> WHEN {
                True => B
                False => current |> WHEN {
                    A => C
                    B => D
                    C => a |> WHEN { True => D, False => B }
                    D => A
                }
            }
        }

        // Combinational output logic
        b: state |> WHEN {
            A => False
            B => True
            C => a
            D => False
        }

        [b: b]
    }
}
```

---

## Common Pitfalls and Compiler Warnings

While LATEST is designed with safety in mind (non-self-reactive semantics prevent infinite loops), there are still patterns that can cause confusion or bugs. The Boon compiler includes static analysis rules to catch these at compile-time.

### Pitfall 1: No External Trigger (Evaluates Once)

**Problem:**
```boon
// ⚠️ WARNING: No external trigger - evaluates once then stays
x: 0 |> HOLD x { x + 1 }
// Result: x = 1 (evaluates once on init, no re-trigger)
```

**Why it happens:**
- Piped LATEST evaluates once on initialization
- With no event or reactive input, it never re-evaluates
- Non-self-reactive: doesn't react to its own state change

**Solution:**
```boon
// ✅ Add explicit trigger
x: 0 |> HOLD x {
    timer_event |> THEN { x + 1 }
}
```

**Compiler warning:**
```
Warning: LATEST has no external trigger
  --> example.bn:1:4
   |
 1 | x: 0 |> HOLD x { x + 1 }
   |    ^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = Note: This will evaluate once on initialization and never update
   = Help: Add an event trigger (e.g., `event |> THEN { x + 1 }`)
```

### Pitfall 2: Unused Pure Function Return Value

**Problem:**
```boon
// ❌ ERROR: Pure function call, but result not used
value: 0 |> HOLD v {
    event |> THEN {
        some_pure_function(v)  // Returns new value
        v                       // Returns old value! BUG!
    }
}
```

**Why it happens:**
- Boon has immutable values
- Pure functions return NEW values, don't modify in-place
- Last expression `v` returns the OLD value (unchanged)

**Solution:**
```boon
// ✅ Use the returned value
value: 0 |> HOLD v {
    event |> THEN {
        v |> some_pure_function()  // Pipe to use result
    }
}

// OR bind it
value: 0 |> HOLD v {
    event |> THEN {
        new_value: some_pure_function(v)
        new_value
    }
}
```

**Compiler error:**
```
Error: Pure function return value unused
  --> example.bn:3:9
   |
 3 |         some_pure_function(v)
   |         ^^^^^^^^^^^^^^^^^^^^^
   |
   = Note: Function returns a new value, but the result is discarded
   = Help: Use the return value: `v |> some_pure_function()`
```

**Note:** This pitfall applies to any pure function. A previous version showed LIST in LATEST, but LIST is no longer supported in LATEST - see "Supported Types" section above.

### Pitfall 3: Circular Dependencies

**Problem:**
```boon
// ⚠️ WARNING: Confusing evaluation order
a: 0 |> HOLD a {
    event |> THEN { a + b }
}
b: a * 2  // Depends on 'a'
```

**Why it happens:**
- `b` depends on `a`, `a` depends on `b` (via event)
- Evaluation order unclear
- Can cause confusion about what values are used

**Solution:**
```boon
// ✅ Make dependencies explicit and one-way
counter: 0 |> HOLD count {
    event |> THEN { count + 1 }
}
double: counter * 2  // Clear one-way dependency

// OR use shared state
[
    counter: 0 |> HOLD count {
        event |> THEN { count + 1 }
    }
    double: counter * 2
]
```

**Compiler warning:**
```
Warning: Circular dependency detected
  --> example.bn:1:4
   |
 1 | a: 0 |> HOLD a { event |> THEN { a + b } }
   |    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
 2 | b: a * 2
   |    -----
   |
   = Note: 'a' depends on 'b', 'b' depends on 'a'
   = Help: Consider restructuring to eliminate cycle
```

### Pitfall 4: Same Name Confusion (Not a Real Issue!)

**Common concern:**
```boon
counter: 0 |> HOLD counter { counter + 1 }
//       ^            ^         ^
//       |            |         |
//       Variable   Parameter  Parameter reference
```

**This is actually FINE!**
- Standard shadowing rules apply
- Parameter `counter` shadows variable `counter` inside the block
- Outside the block, `counter` refers to the variable
- Clear and unambiguous

**Recommended pattern:**
```boon
// ✅ Same name is idiomatic - no need for generic names
counter: 0 |> HOLD counter { event |> THEN { counter + 1 } }
sum: 0 |> HOLD sum { value |> THEN { sum + value } }
state: Idle |> HOLD state { event |> THEN { next_state(state) } }

// ❌ Avoid generic/confusing names
counter: 0 |> HOLD x { event |> THEN { x + 1 } }        // What is 'x'?
counter: 0 |> HOLD current { event |> THEN { current + 1 } }  // Verbose
```

### Compiler Rules Summary

The Boon compiler implements static analysis at three strictness levels:

**Level 1: Permissive (default)**
- ❌ Rule 1: LIST in LATEST (ERROR - not supported type)
- ❌ Rule 2: Unused pure return value (ERROR)
- ⚠️ Rule 3: No external trigger (WARNING)

**Level 2: Strict (`--strict` flag)**
- All Permissive rules, plus:
- ⚠️ Rule 4: Circular dependencies (WARNING)

**Level 3: Pedantic (`--pedantic` flag)**
- All Strict rules, plus:
- ℹ️ Rule 5: Parameter name matches variable (INFO)
- ℹ️ Rule 6: Pure functions only (INFO)

**Suppressing warnings:**
```boon
// Suppress specific warning
#[allow(no_external_trigger)]
counter: 0 |> HOLD counter { counter + 1 }

// Suppress at module level
#![allow(circular_dependencies)]
```

See `LATEST_COMPILER_RULES.md` for complete rule specification.

### Best Practices

**DO:**
- ✅ Use same name for parameter and variable (clear shadowing)
- ✅ Add explicit triggers (events, reactive inputs)
- ✅ Use supported types only (scalars, enums, BITS, objects)
- ✅ Use pure functions correctly (pipe results)
- ✅ Keep dependencies one-way (no cycles)

**DON'T:**
- ❌ Use LIST in LATEST (use reactive LIST operations instead)
- ❌ Forget to use return values from pure functions
- ❌ Create circular dependencies
- ❌ Use generic names like `x`, `current`, `value` (unless appropriate)

---

## Design Rationale

### Why Two Forms?

**Simple LATEST:**
- Common pattern: merge events from multiple sources
- Don't always need self-reference
- Cleaner syntax when just setting values

**Piped LATEST:**
- When you need current value
- Explicit initial value (piped in)
- User-chosen parameter name (self-documenting)

### Why Non-Self-Reactive?

**Problem with self-reactive:**
```boon
// Would cause infinite loop in software
counter: 0 |> HOLD count { count + 1 }
// Without non-self-reactive: 0 → 1 → 2 → 3 → ... ∞
```

**Solution: Non-self-reactive**
- Only reacts to inputs (piped value, events)
- Doesn't react to its own changes
- Natural and safe!

### Why Exhaustive WHEN?

**Catches errors at compile time:**
```boon
// ❌ Error caught
flag: LATEST {
    set |> WHEN { True => True }  // Missing False case!
}
```

**Forces explicit handling:**
- Must think about all cases
- Use SKIP to be explicit about "stay"
- No undefined behavior

### Why SKIP?

**Makes "stay" explicit:**
- Not implicit magic
- Clear in the code
- Compiler can verify exhaustiveness

---

## Open Questions

1. **Initial value for simple LATEST in software?**
   - Starts UNDEFINED?
   - Require default value in block?
   - Or always use piped LATEST when initial value needed?

2. **Syntax for parameter?**
   - Current: `initial |> HOLD param { ... }`
   - Alternative: `initial |> HOLD (param) { ... }` (parens?)
   - Keep current (cleaner)?

3. **Should simple LATEST allow default value?**
   ```boon
   value: LATEST {
       event1 |> THEN { 1 }
       event2 |> THEN { 2 }
       0  // Initial/default?
   }
   ```

---

## Next Steps

1. ✅ ~~Finalize syntax details~~ - DONE (piped LATEST syntax finalized)
2. ✅ ~~Update hw_examples to use LATEST~~ - DONE (all examples updated)
3. Document transpilation to SystemVerilog (hardware backend)
4. Add LATEST to standard library documentation
5. Write tutorial showing progression from simple to piped LATEST
6. Implement compiler rules (see LATEST_COMPILER_RULES.md for specification)

---

**LATEST: Simple, powerful, universal reactive state! 🚀**
