# Hardware Examples Guide

Boon hardware examples demonstrating FPGA/ASIC design patterns. Each example shows the Boon implementation (`.bn`) alongside equivalent SystemVerilog (`.sv`) for comparison.

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
- **Compare**: Includes LIST alternative showing verbosity
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

### Memory (use Numbers/Lists)

**ram.bn** - Synchronous RAM
- **Operations**: `List/set()`, `List/get()`
- **Why Numbers**: Memory content is numeric values
- **Maps to**: Memory array with sync write

**rom.bn** - Asynchronous ROM
- **Operations**: `List/get()`, `List/range()`
- **Why Numbers**: Initialized memory content
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
