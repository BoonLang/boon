# BITS: Bit-Level Data in Boon

**Date**: 2025-11-17
**Status**: Design Specification

---

## Executive Summary

BITS provides bit-level data abstraction for hardware registers, flags, and bit manipulation in Boon:

- **Explicit width** - Always required, no guessing
- **Explicit signedness** - `u` for unsigned, `s` for signed
- **Unified literal syntax** - `base[s|u]value` format (e.g., `10u100`, `2s1010`, `16uFF`)
- **Hardware-first** - Maps directly to HDL bit vectors
- **Universal** - Works across FPGA, Web/Wasm, Server, Embedded, 3D

**Key principles:**
- Width and signedness always explicit
- Errors on overflow (debug mode)
- Clean conversions to/from Bool, Numbers, BYTES

---

## When to Use: BITS vs LIST { Bool } vs Bool

### Quick Decision Tree

```
Representing hardware signal?
│
├─ Single bit signal?
│  └─ Use Bool
│     Examples: enable, valid, ready, clock
│     Operations: Bool/and(), Bool/or(), Bool/not()
│
├─ Multiple bits for arithmetic or bit manipulation?
│  └─ Use BITS
│     Examples: counter, accumulator, ALU, shifter, CRC
│     Operations: Bits/add(), Bits/increment(), Bits/shift_left()
│     Benefit: Concise, type-safe, width-tracked
│
└─ Multiple bits for pattern matching on bit patterns?
   └─ Use LIST { Bool }
      Examples: priority encoder, instruction decoder
      Operations: WHEN { LIST { True, __, __ } => ... }
      Benefit: Elegant wildcard patterns
```

### Comparison Table

| Task | Best Choice | Why | Example |
|------|-------------|-----|---------|
| **Counter (up/down)** | BITS | `Bits/increment()` | `count |> Bits/increment()` |
| **Accumulator** | BITS | `Bits/add()` concise | `sum |> Bits/add(value)` |
| **Shift register (LFSR)** | BITS | `Bits/shift_right()` 1 line vs 8 | `state |> Bits/shift_right(by: 1)` |
| **ALU operations** | BITS | All arithmetic operators | `a |> Bits/add(b)` |
| **Priority encoder** | LIST { Bool } | Wildcard patterns elegant | `WHEN { LIST { True, __, __ } => 3 }` |
| **Bit pattern decoder** | LIST { Bool } | Pattern matching clear | `WHEN { LIST { False, True, __, __ } => ... }` |
| **Enable signal** | Bool | Single bit | `enable |> WHEN { True => ..., False => ... }` |

### Code Comparison: LFSR Example

**BITS (Recommended for shift operations):**
```boon
-- 3 lines: clear and concise
out
    |> Bits/shift_right(by: 1)
    |> Bits/set(index: 7, value: feedback)
```

**LIST { Bool } (Verbose for shift operations):**
```boon
-- 11 lines: manual reconstruction
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

**Verdict:** BITS is 73% shorter for bit manipulation.

---

## Literal Syntax

**Core format: `BITS[width] { base[s|u]value  }`**

Width is always required. Value uses unified format: `base[s|u]digits` where base is 2, 8, 10, or 16, and `s`/`u` indicates signed/unsigned.

```boon
-- Decimal values (base 10)
counter: BITS[8] { 10u42  }              -- 8-bit, unsigned, value 42
threshold: BITS[16] { 10u1000  }         -- 16-bit, unsigned, value 1000
zero_reg: BITS[32] { 10u0  }             -- 32-bit, unsigned, all zeros

-- Signed decimal values
temperature: BITS[12] { 10s0  }          -- 12-bit, signed, value 0
offset: BITS[16] { 10s-500  }            -- 16-bit, signed, negative 500
positive_signed: BITS[8] { 10s100  }     -- 8-bit, signed, positive 100

-- Binary patterns (base 2)
flags: BITS[8] { 2u10110010  }           -- 8-bit, unsigned, binary pattern
mask: BITS[4] { 2u1111  }                -- 4-bit, unsigned, all ones
signed_pattern: BITS[8] { 2s11111111  }  -- 8-bit, signed, pattern = -1

-- Hexadecimal patterns (base 16)
color: BITS[32] { 16uFF8040FF  }         -- 32-bit, unsigned, RGBA
address: BITS[16] { 16uABCD  }           -- 16-bit, unsigned, hex
signed_hex: BITS[8] { 16sFF  }           -- 8-bit, signed, pattern = -1

-- Octal patterns (base 8)
octal_val: BITS[12] { 8u7777  }          -- 12-bit, unsigned, octal

-- Compile-time parametric width (hardware generics/parameters)
reg: BITS[width] { 10u0  }               -- width must be compile-time constant
reg: BITS[width * 2] { 16uFF  }          -- Expressions of compile-time constants

-- Underscore separators for readability (planned feature)
long_mask: BITS[32] { 2u1111_0000_1111_0000_1111_0000_1111_0000  }
hex_color: BITS[32] { 16uFF_80_40_FF  }  -- RGBA color
ipv4_mask: BITS[32] { 16uFF_FF_FF_00  }  -- 255.255.255.0
```

### Nested BITS (Concatenation)

BITS values can be nested for concatenation (useful for construction and pattern matching):

```boon
-- Define reusable bit fields
opcode: BITS[4] { 16uA  }        -- 4-bit opcode
register: BITS[3] { 10u5  }      -- 3-bit register number
immediate: BITS[8] { 10u42  }    -- 8-bit immediate value

-- Concatenate fields (nested BITS are concatenated)
instruction: BITS[__] {  opcode, register, immediate  }
-- Width inferred: 4 + 3 + 8 = 15 bits
-- Result: BITS[15] { ...  } with fields concatenated MSB-first

-- Explicit width (must match sum of nested widths)
instruction: BITS[15] {  opcode, register, immediate  }  -- OK: 15 = 4+3+8

-- ❌ Width mismatch - COMPILE ERROR
bad: BITS[16] {  opcode, register, immediate  }  -- ERROR: Width 16 but content is 15 bits

-- Mix literals and nested BITS (literals must specify width)
packet: BITS { __, { BITS[2] { 2u11  }, opcode, register, BITS[2] { 2u00  } } }
-- Width: 2 + 4 + 3 + 2 = 11 bits

-- Pattern matching with nested BITS (destructuring)
-- ALL fields must specify width (literals AND variables)
instruction |> WHEN {
    BITS[15] {
        BITS[4] { 16uA  }    -- Match opcode = 0xA
        BITS[3] { reg  }     -- Extract 3-bit register
        BITS[8] { imm  }     -- Extract 8-bit immediate
    }} => execute_load(register: reg, value: imm)

    BITS[15] {
        BITS[4] { 16uB  }    -- Match opcode = 0xB
        BITS[3] { __  }      -- Ignore register
        BITS[8] { __  }      -- Ignore immediate
    }} => execute_store()

    __ => invalid_instruction()
}
```

**Concatenation order:** MSB (most significant) first, LSB (least significant) last

```boon
high: BITS[4] { 16uA  }   -- 1010
low: BITS[4] { 16u5  }    -- 0101
combined: BITS[__] {  high, low  }
-- Result: BITS[8] { 2u10100101  }
--         high ^^^^    low ^^^^
```

### Width and Value Rules

**Pattern has more digits than width → ERROR**
```boon
BITS[4] { 2u10110010  }  -- ERROR: 8-bit pattern doesn't fit in 4-bit width
```

**Pattern has fewer digits than width → Zero-extend from left**
```boon
BITS[16] { 2u1010  }     -- OK: becomes 0000_0000_0000_1010
BITS[8] { 16uF  }        -- OK: becomes 0000_1111
```

**Decimal value exceeds width → ERROR**
```boon
BITS[8] { 10u256  }      -- ERROR: 256 requires 9 bits
BITS[8] { 10s128  }      -- ERROR: 128 exceeds 8-bit signed max (127)
```

**Negative values only with signed (s)**
```boon
BITS[8] { 10s-100  }     -- OK: signed negative decimal
BITS[8] { 10u-100  }     -- ERROR: unsigned cannot be negative
```

---

## Semantics and Rules

### Width Matching (Nested BITS)

**Rule: Explicit width MUST match sum of nested widths (compile error if mismatch)**

```boon
-- ✅ Width matches nested content
field1: BITS[4] { 16uA  }
field2: BITS[4] { 16uB  }
combined: BITS[8] {  field1, field2  }  -- OK: 8 = 4 + 4

-- ❌ Width mismatch - COMPILE ERROR
bad: BITS[10] {  field1, field2  }  -- ERROR: Width 10 but content is 8 bits
bad: BITS[6] {  field1, field2  }   -- ERROR: Width 6 but content is 8 bits

-- ✅ Use __ to infer width from nested content
combined: BITS[__] {  field1, field2  }  -- OK: Infers width = 8

-- ✅ Mix literals and nested BITS (literals must have width)
packet: BITS { __, { BITS[2] { 2u11  }, field1, BITS[2] { 2u00  } } }  -- Infers: 2 + 4 + 2 = 8 bits
```

### Concatenation Order

**Rule: MSB (most significant) first, LSB (least significant) last**

```boon
high: BITS[4] { 16uA  }  -- 1010
mid: BITS[4] { 16u5  }   -- 0101
low: BITS[4] { 16u3  }   -- 0011

result: BITS[__] {  high, mid, low  }
-- Width: 12 bits
-- Value: 1010_0101_0011 (MSB first)
--        high mid  low
```

### Pattern Matching with Nested BITS

**Rule: ALL fields must specify width explicitly (both literals and variables)**

```boon
-- Construction
opcode: BITS[4] { 16uA  }
register: BITS[3] { 10u5  }
immediate: BITS[8] { 10u42  }
instruction: BITS[__] {  opcode, register, immediate  }

-- Pattern matching (destructuring)
-- Total width must be explicit, ALL fields need width
instruction |> WHEN {
    -- Match specific opcode (literal), extract variables
    BITS[15] {
        BITS[4] { 16uA  }    -- Match: opcode must be 0xA
        BITS[3] { reg  }     -- Extract: 3-bit register
        BITS[8] { imm  }     -- Extract: 8-bit immediate
    }} => load_instruction(reg, imm)

    -- Match specific opcode, ignore rest
    BITS[15] {
        BITS[4] { 16uB  }    -- Match: opcode must be 0xB
        BITS[3] { __  }      -- Ignore: don't extract register
        BITS[8] { __  }      -- Ignore: don't extract immediate
    }} => store_instruction()

    -- Extract all fields (no literal matching)
    BITS[15] {
        BITS[4] { op  }      -- Extract: 4-bit opcode
        BITS[3] { reg  }     -- Extract: 3-bit register
        BITS[8] { imm  }     -- Extract: 8-bit immediate
    }} => generic_handler(op, reg, imm)

    -- Default
    __ => invalid_instruction()
}
```

### Mixing Literals and Variables

**Rule: In pattern matching, ALL fields need explicit width (literals and variables)**

```boon
opcode: BITS[4] { 16uA  }
register: BITS[3] { 10u5  }

-- Construction: Mix literal BITS and variables
instruction: BITS { __, { opcode, register, BITS[8] { 10u42  }, BITS[2] { 2u11  } } }
-- Width: 4 + 3 + 8 + 2 = 17 bits

-- Pattern matching: ALL fields need width
data |> WHEN {
    BITS[17] {
        BITS[4] { 16uA  }    -- Match literal
        BITS[3] { reg  }     -- Extract variable
        BITS[8] { imm  }     -- Extract variable
        BITS[2] { 2u11  }    -- Match literal
    }} => process(reg, imm)
    __ => error()
}
```

### Common Errors

```boon
-- ❌ Standalone literal without width
opcode: BITS[4] { 16uA  }
bad: BITS[__] {  16uA, opcode  }
-- ERROR: Literal 16uA must specify width (use BITS[width] { 16uA  })

-- ✅ CORRECT: Wrap literals with width
good: BITS { __, { BITS[4] { 16uA  }, opcode } }

-- ❌ Variables without width in pattern matching
instruction |> WHEN {
    BITS { 15, { BITS[4] { 16uA  }, reg, imm } } => ...
    -- ERROR: Variables 'reg' and 'imm' need width specification
}

-- ✅ CORRECT: All fields have explicit width
instruction |> WHEN {
    BITS[15] {
        BITS[4] { 16uA  }    -- Literal with width
        BITS[3] { reg  }     -- Variable with width
        BITS[8] { imm  }     -- Variable with width
    }} => ...
}

-- ❌ Width mismatch in nested BITS
field1: BITS[4] { 16uA  }
field2: BITS[5] { 16uB  }
bad: BITS[8] {  field1, field2  }
-- ERROR: Width 8 but content is 9 bits (4 + 5)

-- ❌ Signedness mismatch in concatenation
signed_field: BITS[4] { 10s-1  }
unsigned_field: BITS[4] { 10u5  }
bad: BITS[__] {  signed_field, unsigned_field  }
-- ERROR: Cannot mix signed and unsigned BITS in concatenation

-- ❌ Pattern exceeds width
BITS[4] { 2u10110010  }
-- ERROR: 8-bit pattern doesn't fit in 4-bit width

-- ❌ Using BITS<N> syntax (IDE display only)
bad: BITS<16>
-- ERROR: Not valid Boon syntax (use BITS[16] { ...  })
```

---

## Width Must Be Compile-Time Known

**Critical design principle:** BITS width is ALWAYS known at compile-time, never at runtime.

### Why Compile-Time Width?

1. **Hardware Reality** - Hardware registers, signals, and buses have fixed sizes known at synthesis/compile time
2. **Type Safety** - Width is part of the type, enabling compile-time verification
3. **Performance** - Zero runtime overhead for width checking or dynamic allocation
4. **Clarity** - Function signatures explicitly declare bit widths

### Width as Part of Type

Width is part of the BITS type, similar to array sizes in systems languages:

```boon
-- These are DIFFERENT types
flags8: BITS(8) = BITS[8] { 16uFF  }      -- Type: BITS(8)
flags16: BITS(16) = BITS[16] { 16uFFFF  } -- Type: BITS(16)

-- ❌ Type mismatch
flags8: BITS(8) = BITS[16] { 16uFFFF  }   -- ERROR: Expected BITS(8), got BITS(16)

-- ✅ Functions specify width in type signature
process_byte: FUNCTION(data: BITS(8)) -> Result {
    -- Compiler knows data is exactly 8 bits
}

-- ❌ Can't pass wrong width
word: BITS(16) = BITS[16] { 16uABCD  }
process_byte(word)  -- ERROR: Expected BITS(8), got BITS(16)
```

### What's Allowed: Compile-Time Constants

```boon
-- ✅ Literal width (most common)
BITS[8] { 16uFF  }                        -- Width: 8 (compile-time known)

-- ✅ Compile-time constant (parameter/generic)
width: 8  -- Compile-time constant
BITS[width] { 16uFF  }                    -- Width: 8 (compile-time known)

-- ✅ Compile-time expression
BITS[width * 2] { 16uABCD  }              -- Width: 16 (compile-time known)

-- ✅ Type parameter in generic functions
FUNCTION create_register<width>() -> BITS(width) {
    Bits/u_zeros(width: width)           -- width is compile-time parameter
}
```

### What's NOT Allowed: Runtime Width

```boon
-- ❌ Runtime variable width
user_input: get_width_from_user()
BITS[user_input] { 16uFF  }               -- ERROR: Width must be compile-time constant

-- ❌ Conditional width
width: if condition { 8 } else { 16 }
BITS[width] { 16uFF  }                    -- ERROR: Width unknown at compile-time

-- ❌ Function returning dynamic width
get_dynamic_bits: FUNCTION() -> BITS {   -- ERROR: Width required in type
    BITS[8] { 16uFF  }
}

-- ✅ Function with explicit width
get_bits: FUNCTION() -> BITS(8) {        -- Width in return type
    BITS[8] { 16uFF  }
}

-- ✅ Generic function with width parameter
create_register: FUNCTION<width>() -> BITS(width) {
    Bits/u_zeros(width: width)
}
```

### Compile-Time Width Across Domains

Width is compile-time known in ALL domains, not just hardware:

**Embedded/Hardware:**
```boon
-- GPIO register (32-bit, hardware-defined)
gpio_reg: BITS(32)
```

**Server/Network:**
```boon
-- TCP flags (9 bits, RFC-defined)
tcp_flags: BITS(9)
```

**Web/Wasm:**
```boon
-- WebSocket opcode (4 bits, spec-defined)
opcode: BITS(4)
```

**3D Graphics:**
```boon
-- Color channel (8 bits, format-defined)
red: BITS(8)
```

In all cases, the width is defined by specifications, standards, or design decisions made at compile-time.

### Benefits of Compile-Time Width

1. **Early error detection** - Width mismatches caught at compile-time
2. **Optimized code generation** - Compiler can generate exact-width operations
3. **Self-documenting** - Function signatures show exact bit counts
4. **No runtime overhead** - No dynamic width tracking needed
5. **Pattern matching safety** - All patterns must have matching widths

```boon
-- Compile-time width checking in pattern matching
parse_opcode: FUNCTION(code: BITS(4)) {
    code |> WHEN {
        BITS[4] { 2u0000  } => Continuation  -- ✅ 4 bits
        BITS[4] { 2u0001  } => TextFrame     -- ✅ 4 bits
        BITS[8] { 16u00  } => Invalid        -- ❌ ERROR: 8 bits doesn't match BITS(4)
    }
}
```

### Helper Functions

```boon
-- Unsigned helpers (u_ prefix)
Bits/u_zeros(width: 32)                 -- 32-bit unsigned all zeros
Bits/u_ones(width: 16)                  -- 16-bit unsigned all ones (65535)
Bits/u_from_number(value: 42, width: 8) -- Unsigned from number

-- Signed helpers (s_ prefix)
Bits/s_zeros(width: 32)                 -- 32-bit signed all zeros
Bits/s_ones(width: 16)                  -- 16-bit signed all ones (-1)
Bits/s_from_number(value: -42, width: 8) -- Signed from number
```

---

## Core Operations

```boon
-- Width and value
Bits/width(bits)                        -- Number of bits
Bits/to_number(bits)                    -- Convert to number (respects signedness)
Bits/u_from_number(value: 42, width: 8) -- Number to unsigned bits
Bits/s_from_number(value: -42, width: 8) -- Number to signed bits

-- Signedness conversion (reinterpret bit pattern, zero-cost)
Bits/as_unsigned(bits)                  -- Force unsigned interpretation
Bits/as_signed(bits)                    -- Force signed interpretation
Bits/is_signed(bits)                    -- Check if signed (returns Bool)

-- Bit access
Bits/get(bits, index: 3)                -- Get single bit (Bool)
Bits/set(bits, index: 3, value: True)   -- Set single bit
Bits/slice(bits, high: 7, low: 4)       -- Extract bit range
Bits/set_slice(bits, high: 7, low: 4, value: nibble)

-- Bitwise operations
Bits/and(a, b)                          -- Bitwise AND
Bits/or(a, b)                           -- Bitwise OR
Bits/xor(a, b)                          -- Bitwise XOR
Bits/not(a)                             -- Bitwise NOT (invert all)
Bits/nand(a, b)                         -- NOT AND
Bits/nor(a, b)                          -- NOT OR

-- Shifts and rotations
Bits/shift_left(bits, by: 2)            -- Logical shift left
Bits/shift_right(bits, by: 2)           -- Logical shift right
Bits/shift_right_arithmetic(bits, by: 2) -- Arithmetic (sign-extend)
Bits/rotate_left(bits, by: 3)           -- Rotate left
Bits/rotate_right(bits, by: 3)          -- Rotate right

-- Concatenation
Bits/concat(LIST { a, b, c })           -- Join bit vectors (matching signedness)
Bits/replicate(bits, times: 4)          -- Repeat pattern
Bits/reverse(bits)                      -- Reverse bit order

-- Arithmetic (width-preserving, wraps on overflow)
Bits/add(a, b)                          -- Addition
Bits/subtract(a, b)                     -- Subtraction
Bits/multiply(a, b)                     -- Multiplication (lower bits)
Bits/increment(bits)                    -- Add 1
Bits/decrement(bits)                    -- Subtract 1

-- Comparison
Bits/equal(a, b)                        -- Equality
Bits/less_than(a, b)                    -- Unsigned comparison
Bits/greater_than(a, b)
Bits/less_than_signed(a, b)             -- Signed comparison

-- Reduction (single-bit result)
Bits/reduce_and(bits)                   -- AND all bits
Bits/reduce_or(bits)                    -- OR all bits
Bits/reduce_xor(bits)                   -- XOR all bits (parity)

-- Queries
Bits/count_ones(bits)                   -- Population count
Bits/leading_zeros(bits)                -- Count leading zeros
Bits/is_zero(bits)                      -- All zeros?
```

### Width Handling

```boon
-- Operations preserve width of first operand
a: BITS[8] { 10u200  }
b: BITS[8] { 10u100  }
sum: a |> Bits/add(b)  -- Width preserved: BITS[8] { ...  }

-- ❌ Mismatched widths: COMPILE ERROR
a: BITS[8] { 16uFF  }
b: BITS[4] { 16u3  }
result: a |> Bits/and(b)  -- ERROR: "Width mismatch: 8 bits vs 4 bits"

-- ✅ CORRECT: Explicit width conversion
result: a |> Bits/and(b |> Bits/zero_extend(to: 8))  -- OK

-- Explicit width change
extended: BITS[4] { 16uF  } |> Bits/zero_extend(to: 8)  -- 16u0F
extended: BITS[4] { 16sF  } |> Bits/sign_extend(to: 8)  -- 16sFF
truncated: BITS[8] { 16uFF  } |> Bits/truncate(to: 4)   -- 16uF
```

### Overflow Behavior

**Debug mode:** Panic on overflow (catches bugs early)
**Release/FPGA mode:** Wrap silently (hardware behavior)

```boon
a: BITS[8] { 10u200  }
b: BITS[8] { 10u100  }
sum: a |> Bits/add(b)  -- 300 overflows 8 bits!
-- Debug: PANIC
-- Release: Returns BITS[8] { 10u44  } (300 mod 256)
```

**Explicit variants:**
```boon
sum: a |> Bits/add_wrapping(b)       -- Always wraps
sum: a |> Bits/add_checked(b)        -- Returns Result
sum: a |> Bits/add_saturating(b)     -- Clamps to max/min
sum: a |> Bits/add_widening(b)       -- BITS[9] { ...  }
```

---

## Concatenation

Build composite BITS values from multiple fields:

```boon
-- Construction syntax: BITS[__] {  field_values  }
-- Width is inferred from sum of field widths

-- ✅ VALID: All unsigned fields
header: BITS[8] { 10u5  }
payload: BITS[24] { 16uDEADBEEF  }

packet: BITS[__] { 
    header
    payload
 }
-- Result: BITS[32] { ...  } unsigned (8 + 24 = 32)

-- ✅ VALID: All signed fields
temp1: BITS[16] { 10s-5  }
temp2: BITS[16] { 10s-3  }

temps: BITS[__] { 
    temp1
    temp2
 }
-- Result: BITS[32] { ...  } signed

-- ❌ COMPILE ERROR: Mixed signedness not allowed
s_field: BITS[8] { 10s-5  }      -- signed
u_field: BITS[8] { 10u10  }      -- unsigned

bad: BITS[__] { 
    s_field
    u_field
 }
-- Error: "Cannot concatenate BITS with different signedness"

-- ✅ Convert signedness explicitly
packet: BITS[__] { 
    s_field |> Bits/as_unsigned()    -- Reinterpret
    u_field
 }
```

### Real-World Example: RISC-V Instruction Encoding

Many instruction sets mix signed and unsigned fields. Here's how to handle it:

```boon
-- I-type instruction: opcode + rd + funct3 + rs1 + imm
-- Most fields unsigned, but immediate is signed (two's complement)

opcode: BITS[7] { 10u19  }       -- ADDI opcode (unsigned)
rd: BITS[5] { 10u10  }           -- Destination register x10 (unsigned)
funct3: BITS[3] { 10u0  }        -- Function code 0 (unsigned)
rs1: BITS[5] { 10u5  }           -- Source register x5 (unsigned)
imm: BITS[12] { 10s-100  }       -- Signed immediate -100

-- ❌ Cannot concatenate directly due to mixed signedness
bad: BITS[__] { 
    imm      -- signed
    rs1      -- unsigned (ERROR: mixed signedness)
    funct3
    rd
    opcode
 }

-- ✅ CORRECT: Treat immediate as bit pattern (reinterpret as unsigned)
instruction: BITS[__] { 
    imm |> Bits/as_unsigned()   -- Reinterpret two's complement as bit pattern
    rs1
    funct3
    rd
    opcode
 }
-- Result: BITS[32] { ...  } unsigned - ready to emit to assembler

-- The immediate's two's complement encoding (-100) is preserved
-- When decoded by CPU, it will interpret those bits as signed
```

**Why reinterpret to unsigned?**
- Instruction encoding is a **bit pattern** (unsigned concept)
- Individual fields have **semantic signedness** (immediate is signed value)
- Concatenation builds bit pattern → use unsigned
- CPU hardware decodes and interprets signedness later

---

## Pattern Matching

### Exact Value Matching

```boon
opcode |> WHEN {
    BITS[8] { 16u00  } => Nop
    BITS[8] { 16u01  } => Load
    BITS[8] { 16u02  } => Store
    __ => Unknown
}
```

### Field Decomposition

**Syntax:** `BITS[total_width] {  field_patterns  }`

```boon
-- 8-bit value split into two 4-bit nibbles
byte |> WHEN {
    BITS[8] {
        BITS[4] { high  }
        BITS[4] { low  }
    }} => process(high, low)
}
```

**Key properties:**
- Fields extracted **MSB-first** (left to right = high to low bits)
- Field widths must sum to total width
- Each extracted field is a BITS value

### Hardware Instruction Decoding

```boon
-- RISC-V R-type instruction (32-bit)
instruction |> WHEN {
    BITS[32] {
        BITS[7] { funct7  }
        BITS[5] { rs2  }
        BITS[5] { rs1  }
        BITS[3] { funct3  }
        BITS[5] { rd  }
        BITS[7] { 2u0110011  }    -- Match exact opcode
    }} => R_Type { funct7, rs2, rs1, funct3, rd }
}
```

### Mixing Literal Matches and Extraction

```boon
-- IPv4 header: [4-bit version][4-bit IHL][8-bit ToS][16-bit length]
first_word |> WHEN {
    BITS[32] {
        BITS[4] { 10u4  }     -- Match IPv4 version (literal 4)
        BITS[4] { ihl  }      -- Extract IHL
        BITS[8] { tos  }      -- Extract ToS
        BITS[16] { length  }  -- Extract length
    }} => IPv4Header { ihl, tos, length }
}
```

### Wildcards

```boon
instruction |> WHEN {
    BITS[32] {
        BITS[4] { 16uF  }     -- Match high nibble = F
        BITS[28] { __  }      -- Ignore remaining 28 bits
    }} => SpecialInstruction
}
```

---

## Conversions

### BITS ↔ Number

```boon
-- To number (respects internal signedness)
num: BITS[8] { 16uFF  } |> Bits/to_number()       -- 255 (unsigned)
num: BITS[8] { 16sFF  } |> Bits/to_number()       -- -1 (signed)

-- From number (u_ or s_ prefix)
bits: Bits/u_from_number(value: 42, width: 8)    -- BITS[8] { 10u42  }
bits: Bits/s_from_number(value: -1, width: 8)    -- BITS[8] { 10s-1  }
```

### BITS ↔ Bool

```boon
-- Single bit to Bool
bit: BITS[1] { 2u1  }
bool: bit |> Bits/to_bool()  -- True

-- Bool to single bit
bool: True
bit: bool |> Bool/to_u_bit()   -- BITS[1] { 2u1  }
bit: bool |> Bool/to_s_bit()   -- BITS[1] { 2s1  }

-- Get bit as Bool
flag: register |> Bits/get(index: 7)  -- Returns Bool
```

### BITS ↔ LIST { Bool }

```boon
-- BITS to LIST { Bool } (MSB-first order)
bits: BITS[8] { 2u10110010  }
bool_list: bits |> Bits/to_bool_list()
-- Result: LIST { True, False, True, True, False, False, True, False }
--         [bit 7, bit 6, bit 5, bit 4, bit 3, bit 2, bit 1, bit 0]

-- LIST { Bool } to unsigned BITS
bool_list: LIST { True, False, True, False }  -- 4 bits
bits: bool_list |> List/to_u_bits()
-- Result: BITS[4] { 2u1010  } (unsigned)

-- LIST { Bool } to signed BITS
bits: bool_list |> List/to_s_bits()
-- Result: BITS[4] { 2s1010  } (signed)
```

### BITS ← TEXT

TEXT can be converted to BITS (must be byte-aligned, auto-converts to UTF-8):

```boon
-- Simple TEXT to BITS (UTF-8 encoding)
signature: BITS { TEXT { BN } }
// 'B' = 16u42, 'N' = 16u4E
// Result: BITS[16] { 16u424E  }  (2 chars × 8 bits = 16 bits)

-- Protocol header with TEXT
header: BITS[__] {
    BITS[4] { 16uA  },              -- Version nibble
    TEXT { OK },                   -- Status (16 bits, UTF-8)
    BITS[4] { 16u0  }               -- Reserved
} }
// Result: BITS[24] { ...  }  (4 + 16 + 4 = 24 bits)

-- Non-ASCII TEXT (UTF-8 multi-byte)
status: BITS { TEXT { ✓ } }
// ✓ = 3 bytes UTF-8 = 24 bits
// Result: BITS[24] { ...  }
```

**Requirements:**
- TEXT must result in byte-aligned bits (width multiple of 8)
- UTF-8 encoding is automatic

**Hardware examples:**

```boon
-- Protocol signature
magic: BITS { TEXT { BOON } }
// Result: BITS[32] { 16u424F4F4E  }

-- Instruction encoding with mnemonic
instruction: BITS[__] {
    TEXT { LD },                   -- Mnemonic (16 bits)
    BITS[8] { reg_addr  },          -- Register
    BITS[8] { immediate  }          -- Immediate value
} }
// Total: 32 bits (16 + 8 + 8)
```

### BITS ↔ BYTES

```boon
-- BITS to BYTES (pads to byte boundary if needed)
bits: BITS[12] { 16uABC }                  -- 12 bits
bytes: bits |> Bits/to_bytes()             -- BYTES[__] { 16u0A, 16uBC }

-- BYTES to BITS (unsigned by default)
bytes: BYTES[__] { 16uFF, 16u00 }
bits: bytes |> Bytes/to_u_bits()           -- BITS[16] { 16uFF00 }
bits: bytes |> Bytes/to_s_bits()           -- BITS[16] { 16sFF00 }
```

---

## Hardware Examples

### Loadable Counter

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

### Priority Encoder

```boon
FUNCTION priority_encoder(input) {
    input |> WHEN {
        BITS[4] {
            BITS[1] { 2u1  }    -- Bit 3 set (highest priority)
            BITS[1] { __  }
            BITS[1] { __  }
            BITS[1] { __  }
        }} => [y: BITS[2] { 2u11  }, valid: True]

        BITS[4] {
            BITS[1] { 2u0  }    -- Bit 3 clear
            BITS[1] { 2u1  }    -- Bit 2 set
            BITS[1] { __  }
            BITS[1] { __  }
        }} => [y: BITS[2] { 2u10  }, valid: True]

        __ => [y: BITS[2] { 2u00  }, valid: False]
    }
}
```

### LFSR

```boon
FUNCTION lfsr_step(state) {
    feedback: state |> Bits/get(index: 7)
        |> Bool/xor(state |> Bits/get(index: 3))
        |> Bool/not()

    state
        |> Bits/shift_right(by: 1)
        |> Bits/set(index: 7, value: feedback)
}
```

---

## Flags Pattern

Convert between record of Bools ↔ BITS:

```boon
-- Define flags as record of Bools
render_state: [
    wireframe: False
    depth_test: True
    backface_cull: True
    alpha_blend: False
]

-- Pack into bits
flag_schema: [
    wireframe: 0
    depth_test: 1
    backface_cull: 2
    alpha_blend: 3
]

packed: render_state |> Flags/u_pack(schema: flag_schema)
-- Result: BITS[4] { 2u0110  }

-- Individual flag operations
Flags/set(packed, flag: depth_test, schema: flag_schema)
Flags/test(packed, flag: backface_cull, schema: flag_schema)
```

---

## Design Rationale

### Why Explicit Width?

1. **Hardware synthesis** - FPGA needs exact widths
2. **Overflow semantics** - Wrapping behavior depends on width
3. **Memory layout** - Pack into known bit positions
4. **Type safety** - Prevent accidental width mismatches

### Why base[s|u]value Syntax?

1. **Unified format** - base AND signedness in one notation
2. **Always explicit** - `10u42` is unambiguous
3. **Self-documenting** - `16sFF` shows hex, signed, pattern FF
4. **Function naming matches** - `Bits/u_zeros()` mirrors `10u0`

---
