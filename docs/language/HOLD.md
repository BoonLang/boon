# HOLD: Stateful Accumulator

**Date**: 2025-12-01
**Status**: Implementation Document

---

## Overview

HOLD provides stateful accumulation with self-reference. The piped input sets/resets the state, and the body expression produces updates.

```boon
input |> HOLD state_param { body }
```

**Key behaviors:**
- Any emission from the input stream sets the state (not just initial)
- Body can reference `state_param` to compute from current value
- Non-self-reactive - doesn't trigger on its own state changes

For simple event merging without self-reference, see **LATEST.md**.

---

## Syntax

```boon
initial_value |> HOLD state_name {
    body_expression
}
```

- `initial_value` - Sets the initial state (and can reset it later)
- `state_name` - Parameter name to reference current state in body
- `body_expression` - Expression that produces new state values

---

## Reset Behavior

**The input stream can reset the state at any time:**

```boon
reset_value: reset_button.event.press |> THEN { 0 }

counter: LATEST { 0, reset_value } |> HOLD counter {
    increment_button.event.press |> THEN { counter + 1 }
}
```

**Execution:**
1. Initial `0` from LATEST → `counter` = 0
2. User clicks increment → `counter` = 1
3. User clicks increment → `counter` = 2
4. User clicks reset → `reset_value` emits `0` → `counter` = 0 (reset!)
5. User clicks increment → `counter` = 1

**This works because:**
- LATEST merges multiple sources (`0` and `reset_value`)
- Any emission from the merged stream sets/resets the HOLD state
- The body continues to update from the new state value

---

## Naming Convention

Use the same name for the variable and state parameter:

```boon
-- Recommended: same name
counter: 0 |> HOLD counter { ... }
sum: 0 |> HOLD sum { ... }
state: Idle |> HOLD state { ... }

-- Avoid: different names
counter: 0 |> HOLD count { ... }  -- confusing rebinding
counter: 0 |> HOLD x { ... }      -- unclear what x is
```

---

## Non-Self-Reactive Semantics

HOLD does NOT react to its own state changes:

```boon
-- Safe: evaluates once, result is 1
value: 0 |> HOLD value { value + 1 }

-- Execution:
-- 1. value = 0 (piped)
-- 2. Evaluate: value + 1 = 1
-- 3. Update: value = 1
-- 4. Does NOT re-trigger (change from inside HOLD)
-- 5. Result: value = 1 (stays)
```

**Triggers for re-evaluation:**
- Input stream emits new value (reset)
- Events in body fire

**Does NOT trigger:**
- State changes from body updates

---

## Software Examples

### Counter with Multiple Actions

```boon
counter: 0 |> HOLD counter {
    LATEST {
        increment |> THEN { counter + 1 }
        decrement |> THEN { counter - 1 }
    }
}
```

### Counter with Reset

```boon
counter: LATEST { 0, reset } |> HOLD counter {
    LATEST {
        increment |> THEN { counter + 1 }
        decrement |> THEN { counter - 1 }
    }
}
```

### Accumulator

```boon
total: 0 |> HOLD total {
    new_value |> THEN { total + new_value }
}
```

### State Machine

```boon
app_state: Idle |> HOLD app_state {
    LATEST {
        start_event |> THEN { Loading }
        app_state |> WHEN {
            Loading => data_loaded |> THEN { Ready }
            Ready => edit_event |> THEN { Editing }
            Editing => save_event |> THEN { Saving }
            Saving => save_complete |> THEN { Ready }
            __ => app_state
        }
    }
}
```

### Toggle

```boon
enabled: False |> HOLD enabled {
    toggle_button.click |> THEN { enabled |> Bool/not() }
}
```

---

## Hardware Examples

### Counter with Control

```boon
counter: BITS[8] { 0 } |> HOLD counter {
    control |> WHEN {
        [inc: True, dec: False] => counter + 1
        [inc: False, dec: True] => counter - 1
        __ => counter
    }
}
```

**Maps to SystemVerilog:**
```systemverilog
logic [7:0] counter;

always_ff @(posedge clk) begin
    case ({inc, dec})
        2'b10: counter <= counter + 1;
        2'b01: counter <= counter - 1;
        default: counter <= counter;
    endcase
end
```

### FSM with Self-Reference

```boon
state: B |> HOLD state {
    rst |> WHEN {
        True => B
        False => state |> WHEN {
            A => C
            B => D
            C => a |> WHEN { True => D, False => B }
            D => A
        }
    }
}
```

### Loadable Counter

```boon
FUNCTION counter(rst, load, load_value, en) {
    BLOCK {
        default: BITS[8] { 0 }
        control: [reset: rst, load: load, enabled: en]

        count: default |> HOLD count {
            control |> WHEN {
                [reset: True, load: __, enabled: __] => default
                [reset: False, load: True, enabled: True] => load_value
                [reset: False, load: False, enabled: True] => count + 1
                __ => count
            }
        }

        [count: count]
    }
}
```

### LFSR

```boon
lfsr: BITS[8] { 0 } |> HOLD lfsr {
    rst |> WHEN {
        True => BITS[8] { 0 }
        False => BLOCK {
            feedback: lfsr |> Bits/get(7) |> Bool/xor(lfsr |> Bits/get(3))
            lfsr |> Bits/shift_right(1) |> Bits/set(7, feedback)
        }
    }
}
```

---

## Supported Types

HOLD works with these types:

### Supported

- **Scalars**: `Number`, `Text`
- **Tags/Enums**: `True`, `False`, user-defined tags
- **BITS**: Hardware bit vectors
- **Objects**: Structured data like `[x: 0, y: 0]`

### NOT Supported

**LIST** - Use reactive LIST operations instead:

```boon
-- Don't do this
items: LIST {} |> HOLD items {
    event |> THEN { items |> List/push(item) }
}

-- Do this instead
items: LIST {}
    |> List/push(item: new_event)
    |> List/take_last(count: 100)
```

---

## Common Pitfalls

### No External Trigger

```boon
-- Warning: evaluates once then stays
x: 0 |> HOLD x { x + 1 }
-- Result: x = 1 (no re-trigger)

-- Solution: add explicit trigger
x: 0 |> HOLD x {
    timer_event |> THEN { x + 1 }
}
```

### Unused Return Value

```boon
-- Bug: pure function result discarded
value: 0 |> HOLD value {
    event |> THEN {
        some_function(value)  -- returns new value
        value                  -- returns OLD value!
    }
}

-- Fix: use the return value
value: 0 |> HOLD value {
    event |> THEN {
        value |> some_function()
    }
}
```

---

## Building Blocks

HOLD can implement higher-level stateful functions:

```boon
FUNCTION Math/sum(stream) {
    0 |> HOLD accumulator {
        stream |> THEN { accumulator + stream }
    }
}

FUNCTION Math/product(stream) {
    1 |> HOLD accumulator {
        stream |> THEN { accumulator * stream }
    }
}

FUNCTION Bool/toggle(event) {
    False |> HOLD state {
        event |> THEN { state |> Bool/not() }
    }
}
```

---

## See Also

- **LATEST.md** - Event merging without self-reference
- **SKIP.md** - Stay at current value semantics
- **WHEN_VS_WHILE.md** - Pattern matching in reactive contexts
- **PULSES.md** - Counted iteration with HOLD
