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

**Core format: `BITS { width, base[s|u]value }`**

Width is always required. Value uses unified format: `base[s|u]digits` where base is 2, 8, 10, or 16, and `s`/`u` indicates signed/unsigned.

```boon
-- Decimal values (base 10)
counter: BITS { 8, 10u42 }              -- 8-bit, unsigned, value 42
threshold: BITS { 16, 10u1000 }         -- 16-bit, unsigned, value 1000
zero_reg: BITS { 32, 10u0 }             -- 32-bit, unsigned, all zeros

-- Signed decimal values
temperature: BITS { 12, 10s0 }          -- 12-bit, signed, value 0
offset: BITS { 16, 10s-500 }            -- 16-bit, signed, negative 500
positive_signed: BITS { 8, 10s100 }     -- 8-bit, signed, positive 100

-- Binary patterns (base 2)
flags: BITS { 8, 2u10110010 }           -- 8-bit, unsigned, binary pattern
mask: BITS { 4, 2u1111 }                -- 4-bit, unsigned, all ones
signed_pattern: BITS { 8, 2s11111111 }  -- 8-bit, signed, pattern = -1

-- Hexadecimal patterns (base 16)
color: BITS { 32, 16uFF8040FF }         -- 32-bit, unsigned, RGBA
address: BITS { 16, 16uABCD }           -- 16-bit, unsigned, hex
signed_hex: BITS { 8, 16sFF }           -- 8-bit, signed, pattern = -1

-- Octal patterns (base 8)
octal_val: BITS { 12, 8u7777 }          -- 12-bit, unsigned, octal

-- Dynamic width (parameterized modules)
reg: BITS { width, 10u0 }               -- Width from variable
reg: BITS { width * 2, 16uFF }          -- Width from expression
```

### Width and Value Rules

**Pattern has more digits than width → ERROR**
```boon
BITS { 4, 2u10110010 }  -- ERROR: 8-bit pattern doesn't fit in 4-bit width
```

**Pattern has fewer digits than width → Zero-extend from left**
```boon
BITS { 16, 2u1010 }     -- OK: becomes 0000_0000_0000_1010
BITS { 8, 16uF }        -- OK: becomes 0000_1111
```

**Decimal value exceeds width → ERROR**
```boon
BITS { 8, 10u256 }      -- ERROR: 256 requires 9 bits
BITS { 8, 10s128 }      -- ERROR: 128 exceeds 8-bit signed max (127)
```

**Negative values only with signed (s)**
```boon
BITS { 8, 10s-100 }     -- OK: signed negative decimal
BITS { 8, 10u-100 }     -- ERROR: unsigned cannot be negative
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
a: BITS { 8, 10u200 }
b: BITS { 8, 10u100 }
sum: a |> Bits/add(b)  -- Width preserved: BITS { 8, ... }

-- ❌ Mismatched widths: COMPILE ERROR
a: BITS { 8, 16uFF }
b: BITS { 4, 16u3 }
result: a |> Bits/and(b)  -- ERROR: "Width mismatch: 8 bits vs 4 bits"

-- ✅ CORRECT: Explicit width conversion
result: a |> Bits/and(b |> Bits/zero_extend(to: 8))  -- OK

-- Explicit width change
extended: BITS { 4, 16uF } |> Bits/zero_extend(to: 8)  -- 16u0F
extended: BITS { 4, 16sF } |> Bits/sign_extend(to: 8)  -- 16sFF
truncated: BITS { 8, 16uFF } |> Bits/truncate(to: 4)   -- 16uF
```

### Overflow Behavior

**Debug mode:** Panic on overflow (catches bugs early)
**Release/FPGA mode:** Wrap silently (hardware behavior)

```boon
a: BITS { 8, 10u200 }
b: BITS { 8, 10u100 }
sum: a |> Bits/add(b)  -- 300 overflows 8 bits!
-- Debug: PANIC
-- Release: Returns BITS { 8, 10u44 } (300 mod 256)
```

**Explicit variants:**
```boon
sum: a |> Bits/add_wrapping(b)       -- Always wraps
sum: a |> Bits/add_checked(b)        -- Returns Result
sum: a |> Bits/add_saturating(b)     -- Clamps to max/min
sum: a |> Bits/add_widening(b)       -- BITS { 9, ... }
```

---

## Concatenation

Build composite BITS values from multiple fields:

```boon
-- Construction syntax: BITS { __, { field_values }}
-- Width is inferred from sum of field widths

-- ✅ VALID: All unsigned fields
header: BITS { 8, 10u5 }
payload: BITS { 24, 16uDEADBEEF }

packet: BITS { __, {
    header
    payload
}}
-- Result: BITS { 32, ... } unsigned (8 + 24 = 32)

-- ✅ VALID: All signed fields
temp1: BITS { 16, 10s-5 }
temp2: BITS { 16, 10s-3 }

temps: BITS { __, {
    temp1
    temp2
}}
-- Result: BITS { 32, ... } signed

-- ❌ COMPILE ERROR: Mixed signedness not allowed
s_field: BITS { 8, 10s-5 }      -- signed
u_field: BITS { 8, 10u10 }      -- unsigned

bad: BITS { __, {
    s_field
    u_field
}}
-- Error: "Cannot concatenate BITS with different signedness"

-- ✅ Convert signedness explicitly
packet: BITS { __, {
    s_field |> Bits/as_unsigned()    -- Reinterpret
    u_field
}}
```

---

## Pattern Matching

### Exact Value Matching

```boon
opcode |> WHEN {
    BITS { 8, 16u00 } => Nop
    BITS { 8, 16u01 } => Load
    BITS { 8, 16u02 } => Store
    __ => Unknown
}
```

### Field Decomposition

**Syntax:** `BITS { total_width, { field_patterns }}`

```boon
-- 8-bit value split into two 4-bit nibbles
byte |> WHEN {
    BITS { 8, {
        BITS { 4, high }
        BITS { 4, low }
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
    BITS { 32, {
        BITS { 7, funct7 }
        BITS { 5, rs2 }
        BITS { 5, rs1 }
        BITS { 3, funct3 }
        BITS { 5, rd }
        BITS { 7, 2u0110011 }    -- Match exact opcode
    }} => R_Type { funct7, rs2, rs1, funct3, rd }
}
```

### Mixing Literal Matches and Extraction

```boon
-- IPv4 header: [4-bit version][4-bit IHL][8-bit ToS][16-bit length]
first_word |> WHEN {
    BITS { 32, {
        BITS { 4, 10u4 }     -- Match IPv4 version (literal 4)
        BITS { 4, ihl }      -- Extract IHL
        BITS { 8, tos }      -- Extract ToS
        BITS { 16, length }  -- Extract length
    }} => IPv4Header { ihl, tos, length }
}
```

### Wildcards

```boon
instruction |> WHEN {
    BITS { 32, {
        BITS { 4, 16uF }     -- Match high nibble = F
        BITS { 28, __ }      -- Ignore remaining 28 bits
    }} => SpecialInstruction
}
```

---

## Conversions

### BITS ↔ Number

```boon
-- To number (respects internal signedness)
num: BITS { 8, 16uFF } |> Bits/to_number()       -- 255 (unsigned)
num: BITS { 8, 16sFF } |> Bits/to_number()       -- -1 (signed)

-- From number (u_ or s_ prefix)
bits: Bits/u_from_number(value: 42, width: 8)    -- BITS { 8, 10u42 }
bits: Bits/s_from_number(value: -1, width: 8)    -- BITS { 8, 10s-1 }
```

### BITS ↔ Bool

```boon
-- Single bit to Bool
bit: BITS { 1, 2u1 }
bool: bit |> Bits/to_bool()  -- True

-- Bool to single bit
bool: True
bit: bool |> Bool/to_u_bit()   -- BITS { 1, 2u1 }
bit: bool |> Bool/to_s_bit()   -- BITS { 1, 2s1 }

-- Get bit as Bool
flag: register |> Bits/get(index: 7)  -- Returns Bool
```

### BITS ↔ LIST { Bool }

```boon
-- BITS to LIST { Bool } (MSB-first order)
bits: BITS { 8, 2u10110010 }
bool_list: bits |> Bits/to_bool_list()
-- Result: LIST { True, False, True, True, False, False, True, False }
--         [bit 7, bit 6, bit 5, bit 4, bit 3, bit 2, bit 1, bit 0]

-- LIST { Bool } to unsigned BITS
bool_list: LIST { True, False, True, False }  -- 4 bits
bits: bool_list |> List/to_u_bits()
-- Result: BITS { 4, 2u1010 } (unsigned)

-- LIST { Bool } to signed BITS
bits: bool_list |> List/to_s_bits()
-- Result: BITS { 4, 2s1010 } (signed)
```

### BITS ↔ BYTES

```boon
-- BITS to BYTES (pads to byte boundary if needed)
bits: BITS { 12, 16uABC }                  -- 12 bits
bytes: bits |> Bits/to_bytes()             -- BYTES { 16#0A, 16#BC }

-- BYTES to BITS (unsigned by default)
bytes: BYTES { 16#FF, 16#00 }
bits: bytes |> Bytes/to_u_bits()           -- BITS { 16, 16uFF00 }
bits: bytes |> Bytes/to_s_bits()           -- BITS { 16, 16sFF00 }
```

---

## Hardware Examples

### Loadable Counter

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

### Priority Encoder

```boon
FUNCTION priority_encoder(input) {
    input |> WHEN {
        BITS { 4, {
            BITS { 1, 2u1 }    -- Bit 3 set (highest priority)
            BITS { 1, __ }
            BITS { 1, __ }
            BITS { 1, __ }
        }} => [y: BITS { 2, 2u11 }, valid: True]

        BITS { 4, {
            BITS { 1, 2u0 }    -- Bit 3 clear
            BITS { 1, 2u1 }    -- Bit 2 set
            BITS { 1, __ }
            BITS { 1, __ }
        }} => [y: BITS { 2, 2u10 }, valid: True]

        __ => [y: BITS { 2, 2u00 }, valid: False]
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
-- Result: BITS { 4, 2u0110 }

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
