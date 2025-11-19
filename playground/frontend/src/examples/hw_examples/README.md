# Hardware Examples Guide

Boon hardware examples demonstrating FPGA/ASIC design patterns. Each example shows the Boon implementation (`.bn`) alongside equivalent SystemVerilog (`.sv`) for comparison.

---

## Transpiler Model: Two Register Patterns

Boon hardware uses **implicit clock domain** (SpinalHDL-style) with two complementary patterns for registers:

### Core Principles

1. **Implicit clock signal**
   - `clk` is NOT in function parameters
   - Transpiler adds `input clk` to generated module
   - All signals are `Bool` or `BITS` types

2. **Two register patterns**
   - **Bits/sum pattern** - For counters/accumulators (delta accumulation)
   - **LATEST pattern** - For FSMs/transformations (needs current value)

3. **Pattern matching = Declarative logic**
   - Control signals bundled into records
   - Patterns read like truth tables
   - Wildcards (`__`) show don't-care signals

### Pattern 1: Bits/sum (Delta Accumulation)

**Use for:** Counters, accumulators, arithmetic registers

```boon
FUNCTION counter(rst, load, load_value, up, en) {
    BLOCK {
        count_width: 8
        default: BITS { count_width, 10s0 }
        control_signals: [reset: rst, load: load, up: up, enabled: en]

        -- Pipeline = next-state logic (this function IS a register)
        count: default
            |> Bits/set(control_signals |> WHEN {
                [reset: True, load: __, up: __, enabled: __] => default
                __ => SKIP
            })
            |> Bits/set(control_signals |> WHEN {
                [reset: False, load: True, up: __, enabled: True] => load_value
                __ => SKIP
            })
            |> Bits/sum(delta: control_signals |> WHEN {
                [reset: False, load: False, up: True, enabled: True] =>
                    BITS { count_width, 10s1 }
                [reset: False, load: False, up: False, enabled: True] =>
                    BITS { count_width, 10s-1 }
                __ => SKIP
            })

        [count: count]
    }
}
```

**Key:** `Bits/sum` is stateful. Patterns show exact conditions (truth table rows).

### Pattern 2: LATEST (Value Transformation)

**Use for:** FSMs, LFSRs, RAMs (when next value depends on current value)

```boon
FUNCTION fsm(rst, a) {
    BLOCK {
        state: LATEST {
            B  -- Reset state
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
        -- Output logic...
    }
}
```

**Key:** LATEST allows self-reference (`state |> WHEN`) for transformations.

### When to Use Which Pattern?

| Pattern | Use Case | Example | Why |
|---------|----------|---------|-----|
| **Bits/sum** | Counter | next = current + delta | Delta depends only on control signals |
| **Bits/sum** | Accumulator | next = current + value | Adding values to accumulator |
| **LATEST** | FSM | next = f(current, input) | Next state depends on current state |
| **LATEST** | LFSR | next = shift(current) + feedback(current) | Transformation needs current bits |
| **LATEST** | RAM | mem[addr] = value | Update specific array element |

### Transpiler Mapping

| Boon Pattern | SystemVerilog Output |
|--------------|---------------------|
| `FUNCTION name(rst, ...)` | `module name(input clk, input rst, ...)` |
| `Bits/sum(delta: ...)` | `always_ff @(posedge clk ...) ... <= ... + delta` |
| `LATEST { ... }` | `always_ff @(posedge clk ...)` |
| `[reset: True, ...]` | Truth table row → if/else condition |
| `control_signals |> WHEN` | Pattern matching → case/if statements |

### Why This Model?

- **Declarative**: Patterns read like truth tables
- **Type-safe**: Width tracking, pattern exhaustiveness
- **Two tools for two jobs**: Bits/sum for deltas, LATEST for transforms
- **Simple transpiler**: Direct mapping to SystemVerilog

See individual `.bn` files for detailed examples.

---

## Quick Reference: WHEN vs WHILE

Boon provides two pattern matching constructs with distinct **evaluation semantics**:

### WHILE - Flowing Dependencies (Reactive Evaluation)
**Use for:** Record patterns, Bool signals, tag matching with dependencies

```boon
-- ✅ Record pattern matching (fields flow reactively)
control_signals: [reset: rst, enable: en]
control_signals |> WHILE {
    [reset: True, enable: __] => reset_state  -- Reacts to rst/en changes
    [reset: False, enable: True] => active
}

-- ✅ Bool signal checking
rst |> WHILE {
    True => reset_state    -- While reset is asserted
    False => normal_state  -- While reset is not asserted
}
```

**Semantics:** Pattern matching **re-evaluated** as dependencies change (flowing)

### WHEN - Frozen Evaluation (Pure Pattern Matching)
**Use for:** State machine states (pure transitions), constant mappings

```boon
state |> WHEN {
    Idle => Running      -- Pure state transition
    Running => Stopped   -- No external dependencies
}
```

**Semantics:** Pattern matching **evaluated once** when input value changes (frozen)

**Critical Rule:** Always use **WHILE for record pattern matching** - fields are dependencies that need to flow!

### Example: FSM with Both
```boon
state: B |> LATEST state {
    rst |> WHILE {                    -- ✅ WHILE: Bool signal
        True => B
        False => state |> WHEN {      -- ✅ WHEN: State matching
            A => C
            B => D
            C => input |> WHILE {     -- ✅ WHILE: Bool signal
                True => D
                False => B
            }
        }
    }
}
```

**See:** [WHEN_VS_WHILE.md](../../../docs/language/WHEN_VS_WHILE.md) for complete guide

---

## Quick Reference: When to Use What

### Use BITS for:
- ✅ **Arithmetic operations** (counters, accumulators, ALUs)
- ✅ **Bit manipulation** (shifts, rotates, masks)
- ✅ **Width-typed data** (registers, data buses)

### Use LIST { Bool } for:
- ✅ **Pattern matching** with wildcards
- ✅ **Bit pattern decoding**
- ✅ **Individual signal grouping**

### Use Bool for:
- ✅ **Single-bit signals** (enable, valid, ready)
- ✅ **Boolean logic** (gates, combinational)

See [BITS_AND_BYTES.md](../../../docs/language/BITS_AND_BYTES.md#when-to-use-bits-vs-list--bool--vs-bool) for detailed decision tree.

---

## Examples by Category

### Arithmetic & Counters (use BITS)

**cycleadder_arst.bn** - Accumulator with async reset
- **Operations**: `Bits/add()`
- **Why BITS**: Arithmetic accumulation
- **Maps to**: `always_ff` with addition

**counter.bn** - Loadable up/down counter
- **Operations**: `Bits/increment()`, `Bits/decrement()`
- **Why BITS**: Arithmetic inc/dec are concise (1 line vs manual)
- **Maps to**: `always_ff` with `+1` / `-1`

**alu.bn** - Arithmetic Logic Unit
- **Operations**: All arithmetic and bitwise ops
- **Why BITS**: Showcases full BITS operator set
- **Maps to**: `always_comb` with `case` statement

### Bit Manipulation (use BITS)

**lfsr.bn** - Linear Feedback Shift Register
- **Operations**: `Bits/shift_right()`, `Bits/set()`
- **Why BITS**: Shift is 1 line (vs 8 lines with LIST)
- **Maps to**: Concatenation `{out[6:0], feedback}`

**serialadder.bn** - Bit-serial adder
- **Operations**: Bit-level full adder
- **Why BITS/LIST**: Either works, example uses LIST
- **Maps to**: Sequential bit processing

### Pattern Matching (use LIST { Bool })

**prio_encoder.bn** - Priority encoder (4→2)
- **Operations**: Wildcard pattern matching
- **Why LIST**: `LIST { True, __, __ }` is elegant
- **Compare**: BITS version would be verbose
- **Maps to**: `casez` with wildcards

**fsm.bn** - Finite State Machine
- **Operations**: State pattern matching
- **Why Tags/LIST**: Readable state encoding
- **Alternative**: Uses Tags for clearest code
- **Maps to**: `case` on state register

### Single-Bit Logic (use Bool)

**sr_gate.bn** - SR latch (NOR gates)
- **Operations**: `Bool/not()`, `Bool/and()`
- **Why Bool**: Individual signal logic
- **Maps to**: Combinational assign statements

**sr_neg_gate.bn** - SR latch (NAND gates)
- **Operations**: `Bool/nand()`
- **Why Bool**: Gate-level modeling
- **Maps to**: NAND gate logic

**dlatch_gate.bn** - D latch
- **Operations**: Boolean operations
- **Why Bool**: Single-bit data/enable
- **Maps to**: Level-sensitive latch

**dff_masterslave.bn** - D flip-flop (master-slave)
- **Operations**: Sequential Bool
- **Why Bool**: Single-bit storage
- **Maps to**: Edge-triggered FF

**fulladder.bn** - Full adder circuit
- **Operations**: `Bool/xor()`, `Bool/and()`, `Bool/or()`
- **Why Bool**: 1-bit arithmetic, Boolean logic
- **Maps to**: Combinational arithmetic gates

### Memory (use MEMORY)

**ram.bn** - Synchronous RAM
- **Operations**: `Memory/initialize()`, `Memory/write()`, `Memory/read()`
- **Why MEMORY**: Fixed-size stateful storage with per-address reactivity
- **Maps to**: Memory array with sync write

**rom.bn** - Asynchronous ROM
- **Operations**: `Memory/initialize()`, `Memory/read()`
- **Why MEMORY**: Consistent with RAM pattern for memory content
- **Maps to**: ROM with initial values

---

## Code Comparison

### LFSR: BITS vs LIST { Bool }

**BITS (Recommended) - 3 lines:**
```boon
out
    |> Bits/shift_right(by: 1)
    |> Bits/set(index: 7, value: feedback)
```

**LIST { Bool } (Verbose) - 11 lines:**
```boon
LIST {
    out |> List/get(index: 6)
    out |> List/get(index: 5)
    out |> List/get(index: 4)
    out |> List/get(index: 3)
    out |> List/get(index: 2)
    out |> List/get(index: 1)
    out |> List/get(index: 0)
    feedback
}
```

**Verdict:** BITS is 73% shorter for shifts.

### Priority Encoder: LIST vs BITS

**LIST { Bool } (Recommended) - Elegant:**
```boon
input |> WHEN {
    LIST { True, __, __, __ } => 3
    LIST { False, True, __, __ } => 2
    LIST { False, False, True, __ } => 1
}
```

**BITS (Verbose) - Nested patterns:**
```boon
input |> WHEN {
    BITS { 4, {
        BITS { 1, 2u1 }
        BITS { 1, __ }
        BITS { 1, __ }
        BITS { 1, __ }
    }} => 3
}
```

**Verdict:** LIST wildcard patterns are clearer.

---

## Learning Path

### Beginner (Start Here)
1. **fulladder.bn** - Boolean logic basics
2. **sr_gate.bn** - Simple sequential logic
3. **counter.bn** - BITS arithmetic intro

### Intermediate
4. **lfsr.bn** - Bit manipulation with BITS
5. **alu.bn** - Complete BITS operator showcase
6. **prio_encoder.bn** - Pattern matching with LIST
7. **fsm.bn** - State machines with Tags

### Advanced
8. **cycleadder_arst.bn** - Parameterized designs
9. **ram.bn** / **rom.bn** - Memory modeling
10. **dff_masterslave.bn** - Master-slave construction

---

## File Naming Convention

- `.bn` - Boon source files
- `.sv` - SystemVerilog reference implementation

---

## Running Examples

Examples are synthesizable Boon code. To use:

1. **Study the Boon code** - see how operations map to hardware intent
2. **Compare with SystemVerilog** - understand the compilation target
3. **Try variations** - modify parameters, add features
4. **Check synthesis** - ensure your transpiler generates correct `.sv`

---

## Additional Resources

- [BITS and BYTES Documentation](../../../docs/language/BITS_AND_BYTES.md)
- [Boon Syntax Guide](../../../docs/language/BOON_SYNTAX.md)
- [Pattern Matching Guide](../../../docs/language/BOON_SYNTAX.md#pattern-matching)

---

## Contributing

When adding new examples:

1. **Choose the right data type** (BITS/LIST/Bool) - see decision tree above
2. **Add rationale comment** at top explaining "Why BITS" or "Why LIST"
3. **Include SystemVerilog** equivalent for comparison
4. **Update this README** with categorization

---

**Happy Hardware Hacking! 🚀**
