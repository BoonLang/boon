# Clock Semantics and Wiring in Boon Hardware Examples

## Overview

In Boon, clock signals are provided through an **ambient context** (`PASSED`) that hardware modules establish. This keeps function signatures clean while maintaining explicit visibility of clock usage at the point where sequential logic needs it. This document describes the semantics of clock signals and how to use them in sequential logic.

## Clock Signal Type and Values

### Clock as Impulse Signal

A clock signal in Boon is an **impulse/event signal** with two possible values:

- **`[]`** (unit type): Clock impulse/tick occurred
- **`SKIP`**: No clock impulse (no update)

The unit type `[]` represents a pure event with no data - perfect for modeling clock edges.

### Example Timeline

```
Time:     t0      t1      t2      t3      t4
clk:      SKIP    SKIP    []      SKIP    []
Action:   hold    hold    update  hold    update
```

## The `PASSED` Context

### Ambient Context Pattern

Hardware modules establish a `PASSED` context that contains common control signals:

```boon
HARDWARE my_module(clk_input, rst_input) {
    -- Establish ambient context available to all internal logic
    PASSED: [clk: clk_input, global_rst: rst_input]

    -- Functions can access PASSED.clk without it in parameters
    counter: counter(rst, en, up)
    fsm: fsm(input_signal)
}
```

**Benefits:**
- ✅ Clean function signatures (no `clk` in every parameter list)
- ✅ Explicit usage (you see `PASSED.clk` where it's used)
- ✅ Flexible (can include other ambient signals)
- ✅ Compatible with existing PASS/PASSED patterns in Boon

## The `THEN` Operator

### Semantics

`signal |> THEN { expression }` means:
- **When `signal = []`**: Trigger and evaluate the expression
- **When `signal = SKIP`**: Don't trigger, skip evaluation

For clock signals from PASSED context:
```boon
PASSED.clk |> THEN { next_state_logic }
```

This triggers the next state logic **only when a clock impulse occurs**.

## The `LATEST` Operator

### Purpose

`LATEST` creates a storage element (register) that:
1. Holds its previous value between updates
2. Can reference its own previous value in the next-value expression
3. Updates only when triggered (via `THEN`)

### Syntax

```boon
state: initial_value |> LATEST state {
    PASSED.clk |> THEN {
        -- Expression that computes next value
        -- Can reference `state` (previous value)
    }
}
```

## Standard Pattern for Sequential Logic

### Basic Register

```boon
FUNCTION register(d) {
    q: False |> LATEST q {
        PASSED.clk |> THEN { d }
    }
    [q: q]
}
```

### Register with Synchronous Reset

```boon
FUNCTION register_with_reset(rst, d) {
    q: False |> LATEST q {
        PASSED.clk |> THEN {
            rst |> WHILE {
                True => False    -- Reset to initial value
                False => d       -- Normal operation
            }
        }
    }
    [q: q]
}
```

### State Machine

```boon
FUNCTION fsm(rst, input) {
    state: IDLE |> LATEST state {
        PASSED.clk |> THEN {
            rst |> WHILE {
                True => IDLE
                False => state |> WHEN {
                    IDLE => START
                    START => input |> WHILE {
                        True => RUNNING
                        False => IDLE
                    }
                    RUNNING => DONE
                    DONE => IDLE
                }
            }
        }
    }
    [state: state]
}
```

## Hardware vs Software Semantics

### In Hardware Synthesis

**Translation to HDL:**
```boon
-- Boon code (PASSED.clk provided by hardware module context)
state: init |> LATEST state {
    PASSED.clk |> THEN { next_value }
}
```

**Becomes (Verilog):**
```verilog
reg state = init;
always @(posedge clk) begin
    state <= next_value;
end
```

**Key points:**
- `PASSED.clk` accesses the hardware clock signal from module context
- `[]` impulses map to rising edges
- `THEN` maps to `@(posedge clk)`
- `LATEST` creates a register

### In Software Simulation

**Execution model:**
```python
class HardwareContext:
    def __init__(self):
        self.PASSED = {"clk": SKIP}
        self.state = init

    def tick(self):
        # Set clock impulse
        self.PASSED["clk"] = []

        # Run all sequential logic (evaluates PASSED.clk |> THEN)
        self.state = compute_next_value(self.state, ...)

        # Clear clock
        self.PASSED["clk"] = SKIP

    def run_without_tick(self):
        # PASSED.clk = SKIP, so THEN doesn't trigger
        # Combinational logic still runs
        pass
```

**Key points:**
- Set up `PASSED` context with `clk: SKIP` initially
- Call `tick()` to set `clk = []` and trigger updates
- Clock is ambient - no need to pass through function parameters
- State persists between calls
- Useful for testing and verification

## Multiple Clock Domains

### Single Module, Multiple Clocks

You can have multiple clock signals in the same module via PASSED context:

```boon
HARDWARE multi_clock_module(clk1_input, clk2_input, data1, data2) {
    PASSED: [clk1: clk1_input, clk2: clk2_input]

    reg1: 0 |> LATEST reg1 {
        PASSED.clk1 |> THEN { data1 }
    }

    reg2: 0 |> LATEST reg2 {
        PASSED.clk2 |> THEN { data2 }
    }

    [out1: reg1, out2: reg2]
}
```

### Same Register, Multiple Clocks (Advanced)

**⚠️ Warning:** Multiple clocks updating the same register is problematic in hardware!

```boon
x: 0 |> LATEST x {
    PASSED.clk1 |> THEN { expr1 }
    PASSED.clk2 |> THEN { expr2 }  -- Last one wins if both trigger
}
```

**In software**: This works fine - last clock tick wins.

**In hardware**: This creates clock domain crossing (CDC) issues:
- Metastability risks
- Timing violations
- Race conditions

**Recommendation**:
- ✅ Allowed in software contexts
- ⚠️ Generate warning in hardware contexts
- 📚 Requires explicit CDC synchronizers if intentional

## Common Patterns

### Counter

```boon
FUNCTION counter(rst, en) {
    count: 0 |> LATEST count {
        PASSED.clk |> THEN {
            rst |> WHILE {
                True => 0
                False => en |> WHILE {
                    True => count + 1
                    False => count
                }
            }
        }
    }
    [count: count]
}
```

### Shift Register

```boon
FUNCTION shift_register(rst, shift_in) {
    reg: 0 |> LATEST reg {
        PASSED.clk |> THEN {
            rst |> WHILE {
                True => 0
                False => (reg << 1) | shift_in
            }
        }
    }
    [reg: reg]
}
```

### Accumulator

```boon
FUNCTION accumulator(rst, en, value) {
    acc: 0 |> LATEST acc {
        PASSED.clk |> THEN {
            rst |> WHILE {
                True => 0
                False => en |> WHILE {
                    True => acc + value
                    False => acc
                }
            }
        }
    }
    [acc: acc]
}
```

## Key Takeaways

1. **Clock via PASSED context**: Access clock through `PASSED.clk` - no need in function parameters
2. **Clean signatures**: Functions don't have `clk` pollution in parameter lists
3. **Clock type**: `[]` (impulse) or `SKIP` (no impulse)
4. **`THEN` triggers on impulse**: `PASSED.clk |> THEN { ... }` evaluates when `PASSED.clk = []`
5. **`LATEST` creates registers**: Holds state between clock ticks
6. **Synchronous reset preferred**: Wrap reset logic inside `PASSED.clk |> THEN { ... }`
7. **Software simulation friendly**: Same code runs in hardware and software with different PASSED contexts
8. **Multi-clock support**: PASSED can contain multiple clocks (`clk1`, `clk2`, etc.)
9. **Compatible pattern**: Works with existing PASS/PASSED usage in Boon codebase

## See Also

- `fsm.bn` - State machine example with clock
- `counter.bn` - Up/down counter with clock
- `lfsr.bn` - Linear feedback shift register with clock
- `ram.bn` - Synchronous RAM with clock
