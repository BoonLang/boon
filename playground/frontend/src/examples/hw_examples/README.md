# Hardware Examples Guide

Boon hardware examples demonstrating FPGA/ASIC design patterns. Each example shows the Boon implementation (`.bn`) alongside equivalent SystemVerilog (`.sv`) for comparison.

---

## Transpiler Model: PASSED Context & Register Patterns

Boon hardware uses **ambient context** (`PASSED`) for clock signals, with two complementary patterns for registers:

### Core Principles

1. **Clock via PASSED context**
   - Hardware modules establish `PASSED: [clk: clk_input, ...]`
   - Functions access `PASSED.clk` (not in parameters)
   - Keeps function signatures clean
   - See [CLOCK_SEMANTICS.md](CLOCK_SEMANTICS.md) for detailed clock documentation

2. **Two register patterns**
   - **Bits/sum pattern** - For counters/accumulators (delta accumulation)
   - **HOLD pattern** - For FSMs/transformations (needs current value)

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
        default: BITS[count_width] { 10s0  }
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
                    BITS[count_width] { 10s1  }
                [reset: False, load: False, up: False, enabled: True] =>
                    BITS[count_width] { 10s-1  }
                __ => SKIP
            })

        [count: count]
    }
}
```

**Key:** `Bits/sum` is stateful. Patterns show exact conditions (truth table rows).

### Pattern 2: LATEST (Value Transformation)

**Use for:** FSMs, LFSRs (when next value depends on current value)

```boon
FUNCTION fsm(rst, a) {
    BLOCK {
        state: B |> HOLD state {
            PASSED.clk |> THEN {
                rst |> WHILE {
                    True => B
                    False => state |> WHEN {
                        A => C
                        B => D
                        C => a |> WHILE { True => D, False => B }
                        D => A
                    }
                }
            }
        }
        -- Output logic...
    }
}
```

**Key:** `LATEST` allows self-reference, `PASSED.clk |> THEN` triggers on clock impulse.

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
| `PASSED: [clk: clk_input]` | Hardware module establishes clock domain |
| `PASSED.clk \|> THEN { ... }` | `always_ff @(posedge clk)` |
| `LATEST state { ... }` | Register with self-reference |
| `Bits/sum(delta: ...)` | Accumulator logic `... <= ... + delta` |
| `[reset: True, ...]` | Truth table row → if/else condition |
| `control_signals \|> WHILE` | Pattern matching → case/if statements |

### Why This Model?

- **Clean signatures**: Clock via `PASSED` context, not parameters
- **Explicit usage**: See `PASSED.clk |> THEN` where clock is used
- **Declarative**: Patterns read like truth tables
- **Type-safe**: Width tracking, pattern exhaustiveness
- **Two tools for two jobs**: Bits/sum for deltas, LATEST for transforms

See [CLOCK_SEMANTICS.md](CLOCK_SEMANTICS.md) for clock details, and individual `.bn` files for examples.

---

## Elaboration-Time Transpilation

**Key concept:** Boon distinguishes between **runtime operations** (software) and **elaboration-time operations** (hardware generation).

### Fixed-Size LIST Operations

**Rule:** Operations on `LIST[size, element_type]` where `size` is compile-time constant are **elaboration-time** (unrolled by transpiler).

```boon
-- ✅ Elaboration-time (size known)
a: BITS[8] { 10u42  }
a_bits: a |> Bits/to_bool_list()     -- LIST[8, Bool]
inverted: a_bits |> List/map(bit: bit |> Bool/not())
-- Transpiler unrolls to 8 NOT gates

-- ❌ Error in hardware (size unknown)
dynamic: get_items()                  -- LIST { TodoItem }
result: dynamic |> List/map(...)      -- ERROR: "Cannot use dynamic LIST in hardware"
```

### How Transpiler Unrolls LIST Operations

**List/map → Parallel instances:**
```boon
-- Boon
bits: LIST[3] {  a, b, c  }
inverted: bits |> List/map(bit: bit |> Bool/not())

-- Transpiles to SystemVerilog
inverted[0] = ~a;
inverted[1] = ~b;
inverted[2] = ~c;
```

**List/fold → Sequential chain:**
```boon
-- Boon
pairs: LIST[WIDTH] {  a_bits, b_bits  }
    |> List/zip(with: b_bits)
    |> List/fold(
        init: [sums: LIST[WIDTH] {  }, carry: False]
        pair, acc: fulladder(a: pair.first, b: pair.second, d: acc.carry)
    )

-- Transpiles to (Verilog generate loop)
genvar i;
generate
    for (i=0; i<WIDTH; i=i+1) begin
        fulladder fa(a[i], b[i], c[i], s[i], c[i+1]);
    end
endgenerate
```

**List/scan → Chain with outputs:**
```boon
-- Boon: Carry chain with intermediate sums
carry_chain: bits |> List/scan(
    init: False
    bit, carry: [sum: bit ^ carry, carry_out: bit & carry]
)

-- Transpiles to: N half-adders in chain
ha0_sum = a[0] ^ 1'b0;
ha0_carry = a[0] & 1'b0;
ha1_sum = a[1] ^ ha0_carry;
ha1_carry = a[1] & ha0_carry;
...
```

### Transpiler Rules

1. **Size must be compile-time constant:**
   - From BITS width: `BITS[8] { ...  } |> Bits/to_bool_list()` → `LIST[8, Bool]`
   - From literal: `LIST[4] {  a, b, c, d  }`
   - From generic parameter: `LIST[WIDTH, Bool]` where WIDTH is constant

2. **Operations are unrolled:**
   - `List/map` → Parallel instances (combinational)
   - `List/fold` → Sequential chain (pipelined)
   - `List/scan` → Chain with intermediate outputs
   - `List/zip` → Structural pairing

3. **Size mismatch errors:**
   ```boon
   a: LIST[8, Bool]
   b: LIST[4, Bool]
   a |> List/zip(with: b)  -- ERROR: "Size mismatch: 8 vs 4"
   ```

4. **Dynamic operations forbidden:**
   ```boon
   fixed: LIST[8, Bool]
   fixed |> List/append(...)  -- ERROR: "Cannot append to fixed-size LIST in hardware"
   ```

**See:** [LIST.md](../../../docs/language/LIST.md) for complete LIST documentation

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
state: B |> HOLD state {
    PASSED.clk |> THEN {              -- Clock trigger
        rst |> WHILE {                -- ✅ WHILE: Bool signal
            True => B
            False => state |> WHEN {  -- ✅ WHEN: State matching
                A => C
                B => D
                C => input |> WHILE { -- ✅ WHILE: Bool signal
                    True => D
                    False => B
                }
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
- **Operations**: Fixed-size LIST with `List/zip`, `List/scan`
- **Why LIST**: Demonstrates elaboration-time unrolling (BITS → LIST → fold → BITS)
- **Maps to**: Verilog `generate for` loop creating WIDTH full adders

### Pattern Matching (use LIST { Bool })

**prio_encoder.bn** - Priority encoder (4→2)
- **Operations**: Wildcard pattern matching with fixed-size LIST
- **Why LIST**: `LIST[__] {  True, __, __  }` wildcard patterns elegant
- **Compare**: BITS version would need nested patterns
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
    BITS[4] {
        BITS[1] { 2u1  }
        BITS[1] { __  }
        BITS[1] { __  }
        BITS[1] { __  }
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

## HDL Gap Analysis & Research

**[hdl_analysis/](./hdl_analysis/)** - Comprehensive research on Boon's HDL capabilities

This folder contains a detailed analysis comparing Boon to modern HDLs (SpinalHDL, Spade, Chisel, Amaranth, VHDL-2019) and discovering how missing features naturally emerge from Boon's reactive core.

**Key findings:**
- 🎯 Boon is already ~85% complete HDL
- 🔄 Pipelines emerge from LATEST + PASSED (~70% exists)
- 🌊 Streaming emerges from LINK + FLUSH (~80% exists)
- ✅ 7 out of 11 critical features are 75%+ naturally emergent

**Documents:**
- ⭐ [Motivational Review](./hdl_analysis/ACCIDENTALLY_MOTIVATING_REVIEW.md) - **Start here!** Why Boon is "accidentally" complete
- [Gap Analysis Report](./hdl_analysis/QUICK_REPORT.md) - Comparison to modern HDLs
- [Pipelines & Streaming](./hdl_analysis/NATURAL_EMERGENCE_ANALYSIS.md) - How they emerge naturally
- [Remaining Features](./hdl_analysis/REMAINING_FEATURES_EMERGENCE.md) - CDC, interfaces, formal verification, etc.
- [Overview README](./hdl_analysis/README.md) - Complete analysis summary

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
