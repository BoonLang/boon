# LATEST: Event Merging

**Date**: 2025-01-18
**Status**: Design Document

---

## Overview

LATEST merges multiple event sources into a single reactive value. It does NOT have self-reference - for stateful accumulation, see **HOLD.md**.

```boon
result: LATEST {
    event1 |> THEN { value1 }
    event2 |> THEN { value2 }
    default_value
}
```

**Key principle:** Non-self-reactive - LATEST doesn't react to its own state changes.

---

## Syntax

```boon
LATEST {
    source1 |> THEN { value1 }
    source2 |> THEN { value2 }
    ...
    default_value  -- optional
}
```

**Characteristics:**
- Merges multiple event sources
- Sets to constant values or expressions (no self-reference)
- Starts UNDEFINED (or with default value if provided)
- Last event wins

---

## Software Examples

### Mode Selection

```boon
mode: LATEST {
    start_button.click |> THEN { Running }
    stop_button.click |> THEN { Stopped }
    pause_button.click |> THEN { Paused }
}
```

**Execution:**
- Initially: UNDEFINED
- When `start_button.click` fires → `mode` = Running
- When `stop_button.click` fires → `mode` = Stopped
- Reactive - only re-evaluates when events fire

### Latest Click

```boon
latest_click: LATEST {
    button1.click |> THEN { 1 }
    button2.click |> THEN { 2 }
    button3.click |> THEN { 3 }
    0  -- default
}
```

### Form Validation Status

```boon
validation: LATEST {
    email_invalid |> THEN { Error[field: Email] }
    password_invalid |> THEN { Error[field: Password] }
    all_valid |> THEN { Valid }
}
```

---

## Hardware Examples

### Flag Register (Set/Clear)

```boon
flag: LATEST {
    set_signal |> WHEN { True => True, False => SKIP }
    clear_signal |> WHEN { True => False, False => SKIP }
}
```

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

### State Machine (No Self-Reference)

```boon
mode: LATEST {
    rst |> WHEN { True => Idle, False => SKIP }
    start |> WHEN { True => Running, False => SKIP }
    stop |> WHEN { True => Stopped, False => SKIP }
}
```

---

## Combining with HOLD

Use LATEST inside HOLD to merge multiple update sources:

```boon
counter: 0 |> HOLD counter {
    LATEST {
        increment |> THEN { counter + 1 }
        decrement |> THEN { counter - 1 }
        reset |> THEN { 0 }
    }
}
```

See **HOLD.md** for stateful accumulation patterns.

---

## Open Questions

1. **Initial value for simple LATEST in software?**
   - Starts UNDEFINED?
   - Require default value?

2. **Should simple LATEST allow default value?**
   ```boon
   value: LATEST {
       event1 |> THEN { 1 }
       event2 |> THEN { 2 }
       0  -- Initial/default?
   }
   ```

---

## See Also

- **HOLD.md** - Stateful accumulation with self-reference
- **SKIP.md** - Stay at current value semantics
- **WHEN_VS_WHILE.md** - Pattern matching in reactive contexts
- **LATEST_COMPILER_RULES.md** - Compiler analysis rules
