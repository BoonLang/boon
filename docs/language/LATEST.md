# LATEST: Reactive State in Boon

**Date**: 2025-01-18
**Status**: Design Document
**Scope**: LATEST semantics for Software and Hardware

---

## Overview

LATEST provides reactive state management in Boon, working seamlessly across software (event-driven) and hardware (clock-driven) contexts.

**Two forms:**
1. **Simple LATEST** - Event merging without self-reference
2. **Piped LATEST** - Stateful with self-reference parameter

**Key principle:** Non-self-reactive - LATEST doesn't react to its own state changes, preventing infinite loops naturally.

---

## Quick Examples

### Software

```boon
// Simple event merging
mode: LATEST {
    start_button.click |> THEN { Running }
    stop_button.click |> THEN { Stopped }
}

// Stateful counter (with self-reference)
counter: 0 |> LATEST count {
    increment |> THEN { count + 1 }
    decrement |> THEN { count - 1 }
}
```

### Hardware

```boon
// Simple register (set to constants)
flag: LATEST {
    set_signal |> WHEN { True => True, False => SKIP }
    clear_signal |> WHEN { True => False, False => SKIP }
}

// Stateful register (compute from current)
counter: 0 |> LATEST count {
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

### Form 2: Piped LATEST (Stateful with Self-Reference)

**Syntax:**
```boon
initial_value |> LATEST parameter_name {
    next_value_expression
}
```

**Characteristics:**
- ✅ Has self-reference via parameter
- ✅ Can compute from current value
- ✅ Initial value explicit (piped in)
- ✅ User-chosen parameter name

**Software example:**
```boon
counter: 0 |> LATEST count {
    increment |> THEN { count + 1 }
    decrement |> THEN { count - 1 }
}
```

**Hardware example:**
```boon
state: B |> LATEST current {
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
value: 0 |> LATEST v { v + 1 }

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
value: 0 |> LATEST v { v + 1 }
// Result: v = 1 (evaluates once, no re-trigger)
```

**Event input (evaluates when event fires):**
```boon
counter: 0 |> LATEST count {
    increment |> THEN { count + 1 }
}
// When increment fires → count + 1 → stays until next increment
```

**Reactive input (evaluates when input changes):**
```boon
counter: reset_value |> LATEST count { count + 1 }
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
counter: 0 |> LATEST count {
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
counter: 0 |> LATEST count {
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
total: 0 |> LATEST sum {
    new_value |> THEN { sum + new_value }
}
```

**State machine:**
```boon
mode: Idle |> LATEST state {
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
counter: 0 |> LATEST count {
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
state: B |> LATEST current {
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
lfsr: BITS { 8, 10u0 } |> LATEST current {
    rst |> WHEN {
        True => BITS { 8, 10u0 }
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
    0 |> LATEST accumulator {
        stream |> THEN { value =>
            accumulator + value
        }
    }
}

// Usage
sum: value |> Math/sum()

// Expands to
sum: 0 |> LATEST accumulator {
    value |> THEN { v => accumulator + v }
}
```

**Other stateful operations:**

```boon
// Math/product
FUNCTION Math/product(stream) {
    1 |> LATEST accumulator {
        stream |> THEN { v => accumulator * v }
    }
}

// Bool/toggle
FUNCTION Bool/toggle(stream, event) {
    False |> LATEST state {
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
   - Makes `v |> LATEST v { v + 1 }` safe!

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
counter: 0 |> LATEST count {
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
counter: 0 |> LATEST count {
    control |> WHEN { [inc: True] => count + 1, __ => count }
}
```

**Rule:** Both forms create registers
- Simple LATEST: For setting to constants
- Piped LATEST: For computing from current
- Both evaluated every clock cycle
- SKIP/wildcard provides stay behavior

---

## LATEST vs Queue/iterate

**Two complementary abstractions:**

| Feature | LATEST | Queue/iterate |
|---------|--------|---------------|
| **Purpose** | Reactive state | Lazy sequences |
| **Evaluation** | Push (events drive) | Pull (consumer drives) |
| **Finiteness** | N/A (reactive) | Can be infinite |
| **Self-reference** | Via parameter | Via parameter |
| **Use cases** | Counters, FSMs, accumulators | Fibonacci, generators |

**When to use LATEST:**
- ✅ Reactive state (software events, hardware registers)
- ✅ Event-driven updates
- ✅ Known inputs (events, signals)

**When to use Queue/iterate:**
- ✅ Lazy evaluation needed
- ✅ Infinite sequences
- ✅ Pull-based iteration

**Example comparison:**

```boon
// LATEST - reactive counter
counter: 0 |> LATEST count {
    increment |> THEN { count + 1 }
}
// Updates when increment event fires

// Queue/iterate - lazy fibonacci
LIST { 0, 1 } |> Queue/iterate(prev, next:
    prev |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    }
)
// Generates values on demand when pulled
```

---

## Complete Examples

### Software: Todo Counter

```boon
todo_count: 0 |> LATEST count {
    add_todo_event |> THEN { count + 1 }
    remove_todo_event |> THEN { count - 1 }
    clear_all_event |> THEN { 0 }
}
```

### Software: State Machine

```boon
app_state: Idle |> LATEST state {
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
        default: BITS { 8, 10u0 }
        control: [reset: rst, load: load, enabled: en]

        count: default |> LATEST current {
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
        state: B |> LATEST current {
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
counter: 0 |> LATEST count { count + 1 }
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
   - Current: `initial |> LATEST param { ... }`
   - Alternative: `initial |> LATEST (param) { ... }` (parens?)
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

1. Finalize syntax details
2. Update hw_examples to use LATEST
3. Document transpilation to SystemVerilog
4. Add LATEST to standard library documentation
5. Write tutorial showing progression from simple to piped LATEST

---

**LATEST: Simple, powerful, universal reactive state! 🚀**
