# BITS and BYTES: Binary Data in Boon

**Date**: 2025-11-17
**Status**: Design Specification (Updated)
**Scope**: Binary data literals and operations across all Boon domains

---

## Executive Summary

Boon provides two complementary abstractions for binary data:

- **BITS** - Bit-level data with explicit width, for hardware registers, flags, bit manipulation
- **BYTES** - Byte-level data, for buffers, files, network protocols, serialization

Both use consistent literal syntax (like TEXT {} and LIST {}) and provide domain-appropriate operations that work universally across FPGA, Web/Wasm, Server, Embedded, and 3D contexts.

**Key design principles:**
- **Explicit over implicit** - Width and signedness always required, no guessing
- **No surprises** - Errors on overflow, pattern mismatch (Rust/Elm philosophy)
- **Semantic clarity** - BITS for bit-level work, BYTES for byte-level work
- **Universal applicability** - Same abstractions work everywhere
- **Clean conversions** - Bridge between Bool, Tags, Numbers, Text
- **Unified literal syntax** - `base[s|u]value` format (e.g., `10u100`, `2s1010`, `16uFF`)

---

## Table of Contents

1. [BITS - Bit-Level Data](#bits---bit-level-data)
2. [BYTES - Byte-Level Data](#bytes---byte-level-data)
3. [Conversions Between Types](#conversions-between-types)
4. [Bool and Tag Interaction](#bool-and-tag-interaction)
5. [Pattern Matching](#pattern-matching)
6. [Domain-Specific Examples](#domain-specific-examples)
7. [API Reference](#api-reference)
8. [Design Rationale](#design-rationale)

---

## BITS - Bit-Level Data

### Literal Syntax

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

-- Long patterns (readability tip: use hex for long bit patterns)
long_mask: BITS { 32, 2u11110000111100001111000011110000 }  -- Binary
long_mask_hex: BITS { 32, 16uF0F0F0F0 }                      -- Same, more readable
hex_color: BITS { 32, 16uFF8040FF }                          -- RGBA color

-- Dynamic width (parameterized modules)
reg: BITS { width, 10u0 }               -- Width from variable
reg: BITS { width * 2, 16uFF }          -- Width from expression
```

### Width and Value Rules

**Pattern has more digits than width → ERROR**
```boon
BITS { 4, 2u10110010 }  -- ERROR: 8-bit pattern doesn't fit in 4-bit width
-- Compile-time error if width is literal
-- Runtime error if width is dynamic
```

**Pattern has fewer digits than width → Zero-extend from left**
```boon
BITS { 16, 2u1010 }     -- OK: becomes 0000_0000_0000_1010
BITS { 8, 16uF }        -- OK: becomes 0000_1111
```

**Decimal value exceeds width → ERROR**
```boon
BITS { 8, 10u256 }      -- ERROR: 256 requires 9 bits, max for 8-bit unsigned is 255
BITS { 4, 10u20 }       -- ERROR: 20 requires 5 bits
BITS { 8, 10s128 }      -- ERROR: 128 exceeds 8-bit signed max (127)
```

**Negative values only with signed (s) → Unsigned cannot be negative**
```boon
BITS { 8, 10s-100 }     -- OK: signed negative decimal
BITS { 8, 10u-100 }     -- ERROR: unsigned cannot be negative
BITS { 8, 2s11111111 }  -- OK: signed pattern (two's complement = -1)
BITS { 8, 2u11111111 }  -- OK: unsigned pattern (= 255)
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

-- These match the literal syntax: u_ for unsigned, s_ for signed
```

### Construction via Concatenation

Build composite BITS values from multiple fields using the same syntax as pattern matching (but in reverse):

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
-- Error: "Cannot concatenate BITS with different signedness. All fields must be signed or all unsigned. Use Bits/as_unsigned() or Bits/as_signed() to convert."
```

**Signedness matching rule**: All fields must have the same signedness (all signed or all unsigned). This prevents accidental mixing and makes intent explicit.

**Converting signedness** (reinterpret bit pattern, zero-cost):

```boon
-- Explicit conversion for mixed-signedness concatenation
s_field: BITS { 8, 10s-5 }      -- signed (-5 = 0xFB)
u_field: BITS { 8, 10u10 }      -- unsigned

-- ✅ Convert to all unsigned
packet: BITS { __, {
    s_field |> Bits/as_unsigned()    -- Reinterpret as BITS { 8, 10u251 }
    u_field
}}
-- Result: BITS { 16, ... } unsigned

-- ✅ Convert to all signed
packet: BITS { __, {
    s_field
    u_field |> Bits/as_signed()      -- Reinterpret as BITS { 8, 10s10 }
}}
-- Result: BITS { 16, ... } signed
```

**Real-world example - RISC-V instruction encoding**:

```boon
-- I-type instruction: opcode + rd + funct3 + rs1 + imm
-- Most fields unsigned, but immediate is signed

opcode: BITS { 7, 10u19 }       -- ADDI opcode
rd: BITS { 5, 10u10 }           -- Destination register
funct3: BITS { 3, 10u0 }        -- Function code
rs1: BITS { 5, 10u5 }           -- Source register
imm: BITS { 12, 10s-100 }       -- Signed immediate

-- Build instruction (all fields as unsigned bit patterns)
instruction: BITS { __, {
    opcode
    rd
    funct3
    rs1
    imm |> Bits/as_unsigned()   -- Treat signed immediate as bit pattern
}}
-- Result: BITS { 32, ... } unsigned - ready to emit to assembler
```

**Alternative using `Bits/concat` function**:

```boon
-- Functional style (equivalent to syntactic form)
header: BITS { 8, 10u5 }
payload: BITS { 24, 16uDEADBEEF }
packet: Bits/concat(LIST { header, payload })  -- OK: both unsigned

-- ❌ Same signedness rules apply strictly
s_field: BITS { 8, 10s-5 }
u_field: BITS { 8, 10u10 }
bad: Bits/concat(LIST { s_field, u_field })
-- ERROR: "Cannot concatenate BITS with different signedness. Field 1 is signed, field 2 is unsigned. Use Bits/as_unsigned() or Bits/as_signed() to convert."

-- ✅ Correct: explicit conversion
ok: Bits/concat(LIST {
    s_field |> Bits/as_unsigned()   -- Reinterpret as unsigned
    u_field
})  -- OK: both unsigned now
```

**Design rationale**:
- **Symmetric syntax**: Construction mirrors pattern matching for consistency
- **Type safety**: Catching mixed signedness prevents bugs (following VHDL/Rust)
- **Explicit conversions**: `Bits/as_unsigned()` / `Bits/as_signed()` make intent clear
- **Hardware reality**: Concatenation joins bit patterns; signedness is interpretation

### Core Operations

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

-- Concatenation and replication
Bits/concat(LIST { a, b, c })           -- Join bit vectors (all must have matching signedness)
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
Bits/count_zeros(bits)
Bits/leading_zeros(bits)                -- Count leading zeros
Bits/trailing_zeros(bits)
Bits/highest_set(bits)                  -- Index of MSB set (or None)
Bits/lowest_set(bits)                   -- Index of LSB set (or None)
Bits/is_zero(bits)                      -- All zeros?
Bits/is_power_of_two(bits)
```

### Width Handling

```boon
-- Operations preserve width of first operand
a: BITS { 8, 10u200 }
b: BITS { 8, 10u100 }
sum: a |> Bits/add(b)  -- Width preserved: BITS { 8, ... }

-- ❌ Mismatched widths: COMPILE ERROR (explicit conversions required)
a: BITS { 8, 16uFF }
b: BITS { 4, 16u3 }
result: a |> Bits/and(b)  -- ERROR: "Width mismatch: 8 bits vs 4 bits"

-- ✅ CORRECT: Explicit width conversion required
result: a |> Bits/and(b |> Bits/zero_extend(to: 8))  -- OK
result: a |> Bits/truncate(to: 4) |> Bits/and(b)     -- OK (truncates a)

-- Explicit width change
extended: BITS { 4, 16uF } |> Bits/zero_extend(to: 8)  -- 16u0F (zero-extended)
extended: BITS { 4, 16sF } |> Bits/sign_extend(to: 8)  -- 16sFF (sign bit extends)
truncated: BITS { 8, 16uFF } |> Bits/truncate(to: 4)   -- 16uF (lower 4 bits)
```

### Overflow Behavior (Arithmetic)

**Debug mode:** Panic on overflow (catches bugs early)
**Release/FPGA mode:** Wrap silently (hardware behavior, performance)

```boon
a: BITS { 8, 10u200 }
b: BITS { 8, 10u100 }
sum: a |> Bits/add(b)  -- 300 overflows 8 bits!
-- Debug: PANIC: overflow, 200 + 100 = 300 exceeds 8-bit unsigned max (255)
-- Release: Returns BITS { 8, 10u44 } (300 mod 256)
```

**Explicit variants (always available):**
```boon
-- Wrapping (always wraps, no panic)
sum: a |> Bits/add_wrapping(b)           -- BITS { 8, 10u44 }

-- Checked (returns Result)
sum: a |> Bits/add_checked(b)            -- Error[Overflow] or Ok[BITS { 8, ... }]

-- Saturating (clamps to max/min)
sum: a |> Bits/add_saturating(b)         -- BITS { 8, 10u255 } (clamped)

-- Widening (result has more bits)
sum: a |> Bits/add_widening(b)           -- BITS { 9, 10u300 } (wider result)
```

### Signedness

BITS tracks signedness internally (like it tracks width). Signedness is determined by the `s` or `u` in the literal.

```boon
-- Unsigned (u in literal)
u: BITS { 8, 16uFF }                     -- Unsigned: 255
u |> Bits/is_signed()                    -- False
u |> Bits/to_number()                    -- 255

-- Signed (s in literal)
s: BITS { 8, 16sFF }                     -- Signed: -1
s |> Bits/is_signed()                    -- True
s |> Bits/to_number()                    -- -1

-- Signed negative decimal
s: BITS { 8, 10s-100 }                   -- Signed negative
s |> Bits/to_number()                    -- -100

-- Operations respect signedness
u |> Bits/shift_right(by: 2)             -- Logical shift (zero-fill)
s |> Bits/shift_right(by: 2)             -- Arithmetic shift (sign-extend)

u |> Bits/less_than(other_u)             -- Unsigned comparison
s |> Bits/less_than(other_s)             -- Signed comparison

-- Mixing signed/unsigned = Error
u |> Bits/add(s)                         -- ERROR: signed/unsigned mismatch
-- Must convert explicitly:
u |> Bits/add(s |> Bits/as_unsigned())   -- OK
```

---

## BYTES - Byte-Level Data

### Literal Syntax

**Core format: `BYTES { byte, byte, ... }`**

Bytes can be decimal or hex (using 16# for consistency with BITS).

```boon
-- Hex bytes (most common)
data: BYTES { 16#FF, 16#00, 16#AB, 16#CD }  -- 4 bytes

-- Decimal bytes
data: BYTES { 255, 0, 171, 205 }            -- Same as above

-- Mixed
header: BYTES { 16#89, 16#50, 78, 71 }      -- PNG magic bytes

-- Single byte
single: BYTES { 16#FF }                      -- 1 byte

-- From text with encoding
utf8_data: BYTES { text: TEXT { Hello }, encoding: UTF8 }

-- From Base64
decoded: BYTES { base64: TEXT { SGVsbG8gV29ybGQ= } }

-- Zero-filled buffer
buffer: BYTES { length: 1024 }              -- 1KB of zeros

-- From hex string
parsed: BYTES { hex: TEXT { FF00ABCD } }
```

**Note on syntax difference:**
- **BITS** uses `16uFF` / `16sFF` (base + signedness + value) - signedness is required for interpretation
- **BYTES** uses `16#FF` (base + `#` + value) - no signedness concept (bytes are just 0-255 values)
- Both use base prefix (2, 8, 10, 16) and avoid `0x` prefix for consistency
- The `#` separator in BYTES indicates "literal byte value" vs `u`/`s` signedness markers in BITS

### Core Operations

```boon
-- Size
Bytes/length(bytes)                      -- Number of bytes

-- Byte access
Bytes/get(bytes, index: 0)              -- Single byte (0-255)
Bytes/set(bytes, index: 0, value: 16#FF)-- Set byte
Bytes/slice(bytes, start: 0, end: 4)    -- Sub-range [start, end)
Bytes/drop(bytes, count: 2)             -- Remove first N bytes
Bytes/take(bytes, count: 4)             -- Keep first N bytes

-- Concatenation
Bytes/concat(LIST { a, b, c })          -- Join buffers
Bytes/append(bytes, byte: 16#FF)        -- Add byte at end
Bytes/prepend(bytes, byte: 16#00)       -- Add byte at start

-- Typed views (read as specific type)
Bytes/read_u8(bytes, offset: 0)
Bytes/read_u16(bytes, offset: 0, endian: Little)
Bytes/read_u32(bytes, offset: 0, endian: Big)
Bytes/read_u64(bytes, offset: 0, endian: Little)
Bytes/read_i8(bytes, offset: 0)         -- Signed integers
Bytes/read_i16(bytes, offset: 0, endian: Little)
Bytes/read_i32(bytes, offset: 0, endian: Big)
Bytes/read_i64(bytes, offset: 0, endian: Little)
Bytes/read_f32(bytes, offset: 0, endian: Little)
Bytes/read_f64(bytes, offset: 0, endian: Little)

-- Typed writes
Bytes/write_u16(bytes, offset: 0, value: 1000, endian: Little)
Bytes/write_u32(bytes, offset: 4, value: 12345, endian: Big)
Bytes/write_f32(bytes, offset: 8, value: 3.14, endian: Little)

-- Text conversions
Bytes/to_text(bytes, encoding: UTF8)    -- Decode to text
Bytes/to_hex(bytes)                     -- "FF00ABCD"
Bytes/to_base64(bytes)                  -- Base64 encode
Bytes/from_hex(hex_text)
Bytes/from_base64(base64_text)

-- Search and comparison
Bytes/find(bytes, pattern: BYTES { 16#FF, 16#00 })  -- Find pattern
Bytes/starts_with(bytes, prefix: BYTES { 16#89, 16#50 })  -- PNG header?
Bytes/ends_with(bytes, suffix: ...)
Bytes/equal(a, b)                       -- Byte-wise equality

-- Transformation
Bytes/reverse(bytes)                    -- Reverse byte order
Bytes/map(bytes, byte => byte |> Bits/xor(16#FF))  -- Transform each byte
Bytes/fill(bytes, start: 0, end: 10, value: 16#00)  -- Fill range
```

### Endianness

```boon
-- Endianness is always explicit for multi-byte reads/writes
value: Bytes/read_u32(data, offset: 0, endian: Little)  -- x86 style
value: Bytes/read_u32(data, offset: 0, endian: Big)     -- Network byte order

-- Swap endianness
swapped: Bytes/swap_endian_16(value)
swapped: Bytes/swap_endian_32(value)
swapped: Bytes/swap_endian_64(value)
```

---

## Conversions Between Types

### BITS ↔ BYTES

```boon
-- BITS to BYTES (pads to byte boundary if needed)
bits: BITS { 12, 16uABC }                  -- 12 bits
bytes: bits |> Bits/to_bytes()             -- BYTES { 16#0A, 16#BC } (2 bytes, padded)

-- BYTES to BITS (unsigned by default)
bytes: BYTES { 16#FF, 16#00 }
bits: bytes |> Bytes/to_u_bits()           -- BITS { 16, 16uFF00 } (unsigned)
bits: bytes |> Bytes/to_s_bits()           -- BITS { 16, 16sFF00 } (signed)

-- Explicit padding control
bits: BITS { 5, 2u11011 }                  -- 5 bits
bytes: bits |> Bits/to_bytes(pad: Left)   -- BYTES { 16#1B } (00011011)
bytes: bits |> Bits/to_bytes(pad: Right)  -- BYTES { 16#D8 } (11011000)
```

### BITS ↔ Number

```boon
-- To number (respects internal signedness)
num: BITS { 8, 16uFF } |> Bits/to_number()       -- 255 (unsigned)
num: BITS { 8, 16sFF } |> Bits/to_number()       -- -1 (signed)

-- From number (u_ or s_ prefix)
bits: Bits/u_from_number(value: 42, width: 8)    -- BITS { 8, 10u42 }
bits: Bits/s_from_number(value: -1, width: 8)    -- BITS { 8, 10s-1 }
```

### BYTES ↔ Number

```boon
-- Single byte to number
num: BYTES { 16#FF } |> Bytes/get(index: 0)  -- 255

-- Multi-byte to number (via typed read)
num: Bytes/read_u32(bytes, offset: 0, endian: Little)
```

### BITS ↔ Bool

```boon
-- Single bit to Bool
bit: BITS { 1, 2u1 }
bool: bit |> Bits/to_bool()  -- True

-- Bool to single bit (unsigned by default)
bool: True
bit: bool |> Bool/to_u_bit()   -- BITS { 1, 2u1 }
bit: bool |> Bool/to_s_bit()   -- BITS { 1, 2s1 }

-- Get bit as Bool
flag: register |> Bits/get(index: 7)  -- Returns Bool
```

### BITS/BYTES ↔ Text

```boon
-- BITS to text representations
BITS { 8, 16uFF } |> Bits/to_text(format: Binary)   -- "11111111"
BITS { 8, 16uFF } |> Bits/to_text(format: Hex)      -- "FF"
BITS { 8, 10u255 } |> Bits/to_text(format: Decimal) -- "255"

-- Parse text to BITS (must specify signedness)
Bits/u_from_binary_text(TEXT { 11111111 })          -- 8-bit unsigned from binary string
Bits/s_from_binary_text(TEXT { 11111111 }, width: 8) -- 8-bit signed from binary string
Bits/u_from_hex_text(TEXT { FF }, width: 8)         -- 8-bit unsigned from hex string
Bits/s_from_hex_text(TEXT { FF }, width: 8)         -- 8-bit signed from hex string

-- BYTES to text
BYTES { 16#FF, 16#00 } |> Bytes/to_hex()             -- "FF00"
BYTES { 16#FF, 16#00 } |> Bytes/to_base64()          -- "/wA="
BYTES { text: TEXT { Hi }, encoding: UTF8 } |> Bytes/to_text(encoding: UTF8)  -- "Hi"
```

---

## Bool and Tag Interaction

### Flags Pattern - Record of Bools ↔ BITS

```boon
-- Define flags as record of Bools
render_state: [
    wireframe: False
    depth_test: True
    backface_cull: True
    alpha_blend: False
]

-- Pack into bits (bit positions defined by schema)
flag_schema: [
    wireframe: 0
    depth_test: 1
    backface_cull: 2
    alpha_blend: 3
]

packed: render_state |> Flags/u_pack(schema: flag_schema)
-- Result: BITS { 4, 2u0110 }

-- Unpack back to record
unpacked: packed |> Flags/unpack(schema: flag_schema)
-- Result: [wireframe: False, depth_test: True, ...]

-- Individual flag operations
Flags/set(packed, flag: depth_test, schema: flag_schema)     -- Set flag
Flags/clear(packed, flag: wireframe, schema: flag_schema)    -- Clear flag
Flags/test(packed, flag: backface_cull, schema: flag_schema) -- Test flag (Bool)
Flags/toggle(packed, flag: alpha_blend, schema: flag_schema) -- Flip flag
```

### Tag Encoding - Tag ↔ BITS

```boon
-- Define encoding for tags
MessageType: [Handshake, Data, Ack, Close, Error]

message_encoding: [
    Handshake: 0
    Data: 1
    Ack: 2
    Close: 3
    Error: 15
]

-- Encode tag to bits (u_ for unsigned)
type_bits: Data |> Tag/to_u_bits(encoding: message_encoding, width: 4)
-- Result: BITS { 4, 10u1 }

-- Decode bits to tag
type_tag: BITS { 4, 10u2 } |> Tag/from_bits(encoding: message_encoding)
-- Result: Ack

-- Pattern matching helper
Tag/from_bits(bits, encoding: message_encoding) |> WHEN {
    Handshake => handle_handshake()
    Data => handle_data()
    Ack => handle_ack()
    Close => handle_close()
    Error => handle_error()
    __ => handle_unknown()
}
```

### Tagged Unions with Bit Payloads

```boon
-- Message with tag + payload
FUNCTION parse_message(raw_bits) {
    BLOCK {
        header: raw_bits |> Bits/slice(high: 31, low: 28)  -- 4-bit tag
        payload: raw_bits |> Bits/slice(high: 27, low: 0)  -- 28-bit payload

        tag: header |> Tag/from_bits(encoding: message_encoding)

        tag |> WHEN {
            Data => DataMessage[
                sequence: payload |> Bits/slice(high: 27, low: 12)
                length: payload |> Bits/slice(high: 11, low: 0)
            ]
            Ack => AckMessage[
                sequence: payload |> Bits/slice(high: 27, low: 12)
            ]
            __ => UnknownMessage[bits: raw_bits]
        }
    }
}
```

---

## Pattern Matching

BITS supports two types of pattern matching: **exact value matching** and **field decomposition**.

### Exact Value Matching

Match a BITS value against specific literals:

```boon
opcode |> WHEN {
    BITS { 8, 16u00 } => Nop
    BITS { 8, 16u01 } => Load
    BITS { 8, 16u02 } => Store
    BITS { 8, 16uFF } => Halt
    __ => Unknown
}
```

### Field Decomposition Pattern

**Syntax:** `BITS { total_width, { field_patterns }}`

Decompose a BITS value into consecutive fields from MSB to LSB:

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
- Fields are extracted **MSB-first** (left to right = high to low bits)
- Field widths must sum to total width (compiler validates)
- Each extracted field is a BITS value with its specified width
- Newlines separate field patterns (no commas needed)

### Hardware Instruction Decoding

**ARM Thumb instruction (16-bit):**
```boon
-- Format: [2-bit op1][3-bit op2][11-bit data]
thumb_inst |> WHEN {
    BITS { 16, {
        BITS { 2, op1 }      -- Extract bits [15:14]
        BITS { 3, op2 }      -- Extract bits [13:11]
        BITS { 11, data }    -- Extract bits [10:0]
    }} => handle_thumb(op1, op2, data)
}
```

**RISC-V R-type instruction (32-bit):**
```boon
-- Format: [7-bit funct7][5-bit rs2][5-bit rs1][3-bit funct3][5-bit rd][7-bit opcode]
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
-- IPv4 header first word: [4-bit version][4-bit IHL][8-bit ToS][16-bit length]
first_word |> WHEN {
    BITS { 32, {
        BITS { 4, 10u4 }     -- Match IPv4 version (literal 4)
        BITS { 4, ihl }      -- Extract IHL
        BITS { 8, tos }      -- Extract ToS
        BITS { 16, length }  -- Extract length
    }} => IPv4Header { ihl, tos, length }

    BITS { 32, {
        BITS { 4, 10u6 }     -- Match IPv6 version
        BITS { 28, __ }      -- Ignore rest
    }} => IPv6Header
}
```

### Nested Field Decomposition

Hierarchically decompose fields:

```boon
-- 16-bit: [8-bit high][8-bit low], where low = [4-bit][4-bit]
value |> WHEN {
    BITS { 16, {
        BITS { 8, high_byte }
        BITS { 8, {
            BITS { 4, low_nibble }
            BITS { 4, high_nibble }
        }}
    }} => nested(high_byte, low_nibble, high_nibble)
}
```

### Two-Stage Matching

Extract fields first, then match on extracted values:

```boon
-- Extract opcode, then match it
instruction |> WHEN {
    BITS { 16, {
        BITS { 4, opcode }
        BITS { 4, dest }
        BITS { 8, imm }
    }} => opcode |> WHEN {
        BITS { 4, 16u0 } => Load { dest, imm }
        BITS { 4, 16u1 } => Store { dest, imm }
        BITS { 4, 16u2 } => Add { dest, imm }
        __ => Unknown
    }
}
```

This approach separates extraction from matching, keeping each WHEN block focused.

### UTF-8 Byte Classification

```boon
first_byte |> WHEN {
    BITS { 8, {
        BITS { 1, 2u0 }      -- 0xxxxxxx
        BITS { 7, payload }
    }} => OneByte(payload)

    BITS { 8, {
        BITS { 3, 2u110 }    -- 110xxxxx
        BITS { 5, payload }
    }} => TwoByteStart(payload)

    BITS { 8, {
        BITS { 4, 2u1110 }   -- 1110xxxx
        BITS { 4, payload }
    }} => ThreeByteStart(payload)

    BITS { 8, {
        BITS { 5, 2u11110 }  -- 11110xxx
        BITS { 3, payload }
    }} => FourByteStart(payload)
}
```

### Wildcards and Don't-Care Fields

Use `__` to ignore fields you don't need:

```boon
instruction |> WHEN {
    BITS { 32, {
        BITS { 4, 16uF }     -- Match high nibble = F
        BITS { 28, __ }      -- Ignore remaining 28 bits
    }} => SpecialInstruction

    BITS { 32, {
        BITS { 8, opcode }
        BITS { 24, __ }      -- Extract opcode, ignore rest
    }} => process_opcode(opcode)
}
```

### Variable Width Fields (Wildcard Width)

For header + variable-length payload patterns, you can use `__` as the **width** of the **last field only**:

```boon
-- ✅ VALID: Wildcard width in last position
packet |> WHEN {
    BITS { __, {
        BITS { 8, msg_type }
        BITS { 16, length }
        BITS { __, payload }     -- Extract remaining bits as payload
    }} => process_packet(msg_type, length, payload)
}

-- ✅ VALID: Parse header, capture rest
message |> WHEN {
    BITS { __, {
        BITS { 4, version }
        BITS { 4, flags }
        BITS { __, data }        -- Everything after 8-bit header
    }} => Message { version, flags, data }
}
```

**Wildcard width is ONLY allowed in the last field position.** This keeps the rules simple and explicit:

```boon
-- ❌ INVALID: Wildcard width in middle position
BITS { 32, {
    BITS { 8, header }
    BITS { __, middle }      -- ERROR: wildcard not last
    BITS { 8, footer }
}}
-- Error: "Wildcard width (__) only allowed in last field position"

-- ❌ INVALID: Multiple wildcard widths
BITS { 32, {
    BITS { __, field1 }      -- ERROR: multiple wildcards
    BITS { __, field2 }
}}
-- Error: "Multiple wildcard widths not allowed"
```

**Design rationale:**
- **Explicit is better**: If you know the total width (32) and field sizes (8, 8), just write `BITS { 16, middle }` explicitly
- **Simpler rules**: "Last position only" is unambiguous, no complex resolution algorithms needed
- **Main use case covered**: Header + variable payload is the primary pattern needing variable width
- **Consistent with Boon philosophy**: No surprises, explicit over implicit

### Width Validation

The compiler validates that field widths sum to the total width:

```boon
-- ✅ VALID: 4 + 4 + 8 = 16
BITS { 16, {
    BITS { 4, a }
    BITS { 4, b }
    BITS { 8, c }
}}

-- ❌ COMPILE ERROR: 4 + 4 + 4 = 12 ≠ 16
BITS { 16, {
    BITS { 4, a }
    BITS { 4, b }
    BITS { 4, c }
}}
-- Error: "Field widths (12) don't match total width (16)"

-- ❌ COMPILE ERROR: 4 + 4 + 10 = 18 > 16
BITS { 16, {
    BITS { 4, a }
    BITS { 4, b }
    BITS { 10, c }
}}
-- Error: "Field widths (18) exceed total width (16)"
```

### Pattern Syntax Summary

**Construction (create BITS value):**
```boon
-- Literal syntax
BITS { width, base[s|u]value }
-- Examples:
BITS { 8, 10u42 }                  -- Unsigned decimal
BITS { 8, 2s11111111 }             -- Signed binary
BITS { 16, 16uABCD }               -- Unsigned hex

-- Concatenation syntax (symmetric with pattern matching)
BITS { __, { field_values }}
-- Examples:
BITS { __, {                       -- Build from two fields
    BITS { 8, 10u5 }               -- Width inferred: 8 + 24 = 32
    BITS { 24, 16uDEADBEEF }
}}
-- All fields must have matching signedness (all signed or all unsigned)
```

**Pattern matching (destructure BITS value):**
```boon
BITS { width, value }              -- Match exact value
BITS { width, { fields } }         -- Decompose into fields
-- Examples:
BITS { 8, 16uFF }                  -- Match 0xFF exactly
BITS { 8, {                        -- Extract two 4-bit fields
    BITS { 4, high }
    BITS { 4, low }
}}
```

**Key differences:**
- **Literal construction**: Uses `base[s|u]value` (e.g., `10u42`, `2s1010`)
- **Concatenation construction**: Uses `{ field_values }` with inferred width (`__`)
- **Pattern matching**: Uses field decomposition or exact value matching
- **Symmetric syntax**: Construction via concatenation mirrors pattern matching

---

### Matching on BYTES Values

```boon
-- File type detection by magic bytes
file_bytes |> Bytes/take(count: 4) |> WHEN {
    BYTES { 16#89, 16#50, 16#4E, 16#47 } => PngFile
    BYTES { 16#FF, 16#D8, 16#FF, 16#E0 } => JpegFile
    BYTES { 16#47, 16#49, 16#46, 16#38 } => GifFile
    BYTES { 16#25, 16#50, 16#44, 16#46 } => PdfFile
    __ => UnknownFile
}

-- HTTP method parsing
request_line |> Bytes/take(count: 7) |> WHEN {
    BYTES { text: TEXT { GET }, encoding: ASCII } => GetMethod
    BYTES { text: TEXT { POST }, encoding: ASCII } => PostMethod
    BYTES { text: TEXT { PUT }, encoding: ASCII } => PutMethod
    __ => parse_other_method(request_line)
}
```

---

## Domain-Specific Examples

### FPGA / Hardware

```boon
-- SR Latch using BITS
FUNCTION sr_latch(s, r) {
    q: s |> Bits/or(q) |> Bits/and(r |> Bits/not())
    nq: q |> Bits/not()
    [q: q, nq: nq]
}

-- 4-bit counter with async reset
FUNCTION counter(clk_event, rst_event) {
    count: LATEST {
        BITS { 4, 10u0 }

        rst_event |> WHEN {
            Rising => BITS { 4, 10u0 }
            __ => SKIP
        }

        clk_event |> WHEN {
            Rising => count |> Bits/increment()
            __ => SKIP
        }
    }

    [count: count]
}

-- Priority encoder (4-bit input)
FUNCTION priority_encoder(input) {
    -- input is BITS { 4, ... }, match highest priority bit
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

        BITS { 4, {
            BITS { 1, 2u0 }
            BITS { 1, 2u0 }
            BITS { 1, 2u1 }    -- Bit 1 set
            BITS { 1, __ }
        }} => [y: BITS { 2, 2u01 }, valid: True]

        BITS { 4, {
            BITS { 1, 2u0 }
            BITS { 1, 2u0 }
            BITS { 1, 2u0 }
            BITS { 1, 2u1 }    -- Bit 0 set (lowest priority)
        }} => [y: BITS { 2, 2u00 }, valid: True]

        __ => [y: BITS { 2, 2u00 }, valid: False]
    }
}

-- Linear Feedback Shift Register
FUNCTION lfsr_step(state) {
    -- state is BITS { 8, ... }
    feedback: state |> Bits/get(index: 7)
        |> Bool/xor(state |> Bits/get(index: 3))
        |> Bool/not()

    state
        |> Bits/shift_right(by: 1)
        |> Bits/set(index: 7, value: feedback)
}
```

### Web / Wasm

```boon
-- WebSocket binary frame parsing
FUNCTION parse_ws_frame(bytes) {
    BLOCK {
        first_byte: bytes |> Bytes/get(index: 0)
        second_byte: bytes |> Bytes/get(index: 1)

        first_bits: Bits/u_from_number(value: first_byte, width: 8)
        second_bits: Bits/u_from_number(value: second_byte, width: 8)

        [
            fin: first_bits |> Bits/get(index: 7)
            rsv1: first_bits |> Bits/get(index: 6)
            rsv2: first_bits |> Bits/get(index: 5)
            rsv3: first_bits |> Bits/get(index: 4)
            opcode: first_bits |> Bits/slice(high: 3, low: 0)
            masked: second_bits |> Bits/get(index: 7)
            payload_len: second_bits |> Bits/slice(high: 6, low: 0) |> Bits/to_number()
        ]
    }
}

-- Canvas pixel manipulation (RGBA packed)
FUNCTION adjust_brightness(pixel_bytes, factor) {
    pixel: Bytes/read_u32(pixel_bytes, offset: 0, endian: Little)
    pixel_bits: Bits/u_from_number(value: pixel, width: 32)

    r: pixel_bits |> Bits/slice(high: 7, low: 0) |> Bits/to_number()
    g: pixel_bits |> Bits/slice(high: 15, low: 8) |> Bits/to_number()
    b: pixel_bits |> Bits/slice(high: 23, low: 16) |> Bits/to_number()
    a: pixel_bits |> Bits/slice(high: 31, low: 24) |> Bits/to_number()

    new_r: (r * factor) |> Number/min(255) |> Number/floor()
    new_g: (g * factor) |> Number/min(255) |> Number/floor()
    new_b: (b * factor) |> Number/min(255) |> Number/floor()

    BITS { 32, 10u0 }
        |> Bits/set_slice(high: 7, low: 0, value: Bits/u_from_number(value: new_r, width: 8))
        |> Bits/set_slice(high: 15, low: 8, value: Bits/u_from_number(value: new_g, width: 8))
        |> Bits/set_slice(high: 23, low: 16, value: Bits/u_from_number(value: new_b, width: 8))
        |> Bits/set_slice(high: 31, low: 24, value: Bits/u_from_number(value: new_a, width: 8))
}

-- Feature flags for WebGL
render_flags: [
    enable_shadows: True
    enable_reflections: False
    enable_antialiasing: True
    enable_bloom: False
] |> Flags/u_pack(schema: [
    enable_shadows: 0
    enable_reflections: 1
    enable_antialiasing: 2
    enable_bloom: 3
])
-- Result: BITS { 4, 2u0101 }
```

### Server / Networking

```boon
-- TCP header parsing
FUNCTION parse_tcp_header(bytes) {
    BLOCK {
        src_port: Bytes/read_u16(bytes, offset: 0, endian: Big)
        dst_port: Bytes/read_u16(bytes, offset: 2, endian: Big)
        seq_num: Bytes/read_u32(bytes, offset: 4, endian: Big)
        ack_num: Bytes/read_u32(bytes, offset: 8, endian: Big)

        flags_byte: bytes |> Bytes/get(index: 13)
        flags: Bits/u_from_number(value: flags_byte, width: 8)

        [
            source_port: src_port
            dest_port: dst_port
            sequence: seq_num
            acknowledgment: ack_num
            flags: [
                fin: flags |> Bits/get(index: 0)
                syn: flags |> Bits/get(index: 1)
                rst: flags |> Bits/get(index: 2)
                psh: flags |> Bits/get(index: 3)
                ack: flags |> Bits/get(index: 4)
                urg: flags |> Bits/get(index: 5)
            ]
        ]
    }
}

-- Build protocol message
FUNCTION build_message(type_tag, sequence, payload) {
    BLOCK {
        header: Bits/concat(LIST {
            type_tag |> Tag/to_u_bits(encoding: message_encoding, width: 4)
            Bits/u_from_number(value: sequence, width: 12)
            Bits/u_from_number(value: Bytes/length(payload), width: 16)
        })

        header_bytes: header |> Bits/to_bytes()
        Bytes/concat(LIST { header_bytes, payload })
    }
}

-- Hash computation (XOR-based simple hash)
FUNCTION simple_hash(data) {
    data
        |> Bytes/to_u_bits()
        |> Bits/reduce_xor()
}
```

### Embedded / IoT

```boon
-- GPIO register manipulation
FUNCTION configure_gpio(pin, mode) {
    BLOCK {
        -- Read current config
        config_reg: Memory/read(address: 16u40000000)
        config_bits: Bits/u_from_number(value: config_reg, width: 32)

        -- Set mode bits for pin (2 bits per pin)
        pin_offset: pin * 2
        mode_bits: mode |> WHEN {
            Input => BITS { 2, 2u00 }
            Output => BITS { 2, 2u01 }
            Alternate => BITS { 2, 2u10 }
            Analog => BITS { 2, 2u11 }
        }

        new_config: config_bits
            |> Bits/set_slice(
                high: pin_offset + 1
                low: pin_offset
                value: mode_bits
            )

        -- Write back
        Memory/write(address: 16u40000000, value: new_config |> Bits/to_number())
    }
}

-- SPI data transfer
FUNCTION spi_transfer(tx_byte) {
    tx_bits: Bits/u_from_number(value: tx_byte, width: 8)

    -- Shift out MSB first, shift in response
    List/range(start: 7, end: -1) |> List/fold(
        init: [tx: tx_bits, rx: BITS { 8, 10u0 }]
        bit_idx, acc: BLOCK {
            -- Set MOSI
            mosi: acc.tx |> Bits/get(index: bit_idx)
            Gpio/write(pin: MOSI_PIN, value: mosi)

            -- Clock pulse
            Gpio/write(pin: SCK_PIN, value: True)
            Delay/microseconds(1)

            -- Read MISO
            miso: Gpio/read(pin: MISO_PIN)

            Gpio/write(pin: SCK_PIN, value: False)

            -- Update RX
            [
                tx: acc.tx
                rx: acc.rx |> Bits/set(index: bit_idx, value: miso)
            ]
        }
    ).rx |> Bits/to_number()
}

-- I2C address with R/W bit
i2c_address: BITS { 8, 10u0 }
    |> Bits/set_slice(high: 7, low: 1, value: Bits/u_from_number(value: device_addr, width: 7))
    |> Bits/set(index: 0, value: read_mode)
```

### 3D Graphics

```boon
-- Vertex attribute packing (10-10-10-2 normal format)
FUNCTION pack_normal(nx, ny, nz) {
    -- Convert -1..1 floats to 10-bit signed integers
    scale: 511  -- (2^9 - 1)

    nx_bits: Bits/s_from_number(value: (nx * scale) |> Number/round(), width: 10)
    ny_bits: Bits/s_from_number(value: (ny * scale) |> Number/round(), width: 10)
    nz_bits: Bits/s_from_number(value: (nz * scale) |> Number/round(), width: 10)
    w_bits: BITS { 2, 2s00 }  -- Padding (signed to match other fields)

    -- All fields signed - signedness matching rule satisfied
    Bits/concat(LIST { nx_bits, ny_bits, nz_bits, w_bits })
}

-- Color conversion (float RGBA to packed u32)
FUNCTION pack_color(r, g, b, a) {
    BITS { 32, 10u0 }
        |> Bits/set_slice(high: 7, low: 0, value: Bits/u_from_number(value: (r * 255) |> Number/floor(), width: 8))
        |> Bits/set_slice(high: 15, low: 8, value: Bits/u_from_number(value: (g * 255) |> Number/floor(), width: 8))
        |> Bits/set_slice(high: 23, low: 16, value: Bits/u_from_number(value: (b * 255) |> Number/floor(), width: 8))
        |> Bits/set_slice(high: 31, low: 24, value: Bits/u_from_number(value: (a * 255) |> Number/floor(), width: 8))
}

-- Texture format detection
FUNCTION detect_texture_format(header_bytes) {
    header_bytes |> Bytes/take(count: 4) |> WHEN {
        BYTES { 16#44, 16#44, 16#53, 16#20 } => DDS
        BYTES { 16#AB, 16#4B, 16#54, 16#58 } => KTX
        BYTES { 16#89, 16#50, 16#4E, 16#47 } => PNG
        __ => Unknown
    }
}

-- GPU buffer flags
buffer_usage: [
    vertex: True
    index: False
    uniform: True
    storage: False
    copy_src: False
    copy_dst: True
] |> Flags/u_pack(schema: [
    vertex: 0
    index: 1
    uniform: 2
    storage: 3
    copy_src: 4
    copy_dst: 5
])
```

---

## API Reference

### BITS Module

#### Constructors
- `BITS { width, base[s|u]value }` - Core format: width and value with explicit base/signedness
  - `BITS { 8, 10u42 }` - 8-bit unsigned decimal 42
  - `BITS { 8, 10s-100 }` - 8-bit signed decimal -100
  - `BITS { 8, 2u10110010 }` - 8-bit unsigned binary pattern
  - `BITS { 16, 16sFFFF }` - 16-bit signed hex pattern
- `BITS { bits: LIST { bools } }` - From list of bools

#### Helper Functions (u_ for unsigned, s_ for signed)
- `Bits/u_zeros(width: N)` - Unsigned all zeros
- `Bits/s_zeros(width: N)` - Signed all zeros
- `Bits/u_ones(width: N)` - Unsigned all ones
- `Bits/s_ones(width: N)` - Signed all ones (-1)
- `Bits/u_from_number(value: V, width: N)` - Unsigned from number
- `Bits/s_from_number(value: V, width: N)` - Signed from number
- `Bits/u_from_binary_text(text: T)` - Unsigned from binary string
- `Bits/s_from_binary_text(text: T, width: N)` - Signed from binary string
- `Bits/u_from_hex_text(text: T, width: N)` - Unsigned from hex string
- `Bits/s_from_hex_text(text: T, width: N)` - Signed from hex string

#### Properties
- `Bits/width(bits)` - Get width
- `Bits/is_signed(bits)` - Check if signed
- `Bits/to_number(bits)` - To number (respects signedness)
- `Bits/to_bool(bits)` - Single bit to Bool
- `Bits/is_zero(bits)` - Check if all zeros

#### Bit Access
- `Bits/get(bits, index: I)` - Get bit at index (Bool)
- `Bits/set(bits, index: I, value: Bool)` - Set bit
- `Bits/slice(bits, high: H, low: L)` - Extract range [H:L]
- `Bits/set_slice(bits, high: H, low: L, value: V)` - Set range

#### Bitwise Operations
- `Bits/and(a, b)` - AND
- `Bits/or(a, b)` - OR
- `Bits/xor(a, b)` - XOR
- `Bits/not(a)` - NOT (invert)
- `Bits/nand(a, b)` - NAND
- `Bits/nor(a, b)` - NOR
- `Bits/xnor(a, b)` - XNOR

#### Shifts and Rotations
- `Bits/shift_left(bits, by: N)` - Logical left shift
- `Bits/shift_right(bits, by: N)` - Logical right shift
- `Bits/shift_right_arithmetic(bits, by: N)` - Arithmetic right shift
- `Bits/rotate_left(bits, by: N)` - Rotate left
- `Bits/rotate_right(bits, by: N)` - Rotate right

#### Arithmetic
- `Bits/add(a, b)` - Addition (wrapping)
- `Bits/subtract(a, b)` - Subtraction (wrapping)
- `Bits/multiply(a, b)` - Multiplication (lower bits)
- `Bits/increment(bits)` - Add 1
- `Bits/decrement(bits)` - Subtract 1
- `Bits/negate(bits)` - Two's complement negation

#### Comparison
- `Bits/equal(a, b)` - Equality
- `Bits/less_than(a, b)` - Unsigned less than
- `Bits/greater_than(a, b)` - Unsigned greater than
- `Bits/less_than_signed(a, b)` - Signed less than
- `Bits/greater_than_signed(a, b)` - Signed greater than

#### Reduction
- `Bits/reduce_and(bits)` - AND all bits (Bool)
- `Bits/reduce_or(bits)` - OR all bits (Bool)
- `Bits/reduce_xor(bits)` - XOR all bits (Bool)

#### Queries
- `Bits/count_ones(bits)` - Population count
- `Bits/count_zeros(bits)` - Zero count
- `Bits/leading_zeros(bits)` - Count leading zeros
- `Bits/trailing_zeros(bits)` - Count trailing zeros
- `Bits/highest_set(bits)` - Index of MSB set (or None)
- `Bits/lowest_set(bits)` - Index of LSB set (or None)
- `Bits/is_power_of_two(bits)` - Check if power of 2

#### Manipulation
- `Bits/concat(LIST { a, b, c })` - Concatenate
- `Bits/replicate(bits, times: N)` - Repeat pattern
- `Bits/reverse(bits)` - Reverse bit order
- `Bits/zero_extend(bits, to: N)` - Zero extend to width N
- `Bits/sign_extend(bits, to: N)` - Sign extend to width N
- `Bits/truncate(bits, to: N)` - Truncate to width N

#### Conversion
- `Bits/to_bytes(bits)` - Convert to BYTES
- `Bits/to_text(bits, format: Binary|Hex|Decimal)` - To string representation

### BYTES Module

#### Constructors
- `BYTES { byte, byte, ... }` - From byte literals
- `BYTES { length: N }` - Zero-filled buffer
- `BYTES { text: T, encoding: E }` - From text
- `BYTES { base64: T }` - From Base64
- `BYTES { hex: T }` - From hex string
- `Bytes/from_bits(bits)` - From BITS

#### Properties
- `Bytes/length(bytes)` - Byte count
- `Bytes/is_empty(bytes)` - Check if empty

#### Byte Access
- `Bytes/get(bytes, index: I)` - Get byte (0-255)
- `Bytes/set(bytes, index: I, value: V)` - Set byte
- `Bytes/slice(bytes, start: S, end: E)` - Sub-range
- `Bytes/take(bytes, count: N)` - First N bytes
- `Bytes/drop(bytes, count: N)` - Skip first N bytes

#### Typed Views (Read)
- `Bytes/read_u8(bytes, offset: O)`
- `Bytes/read_u16(bytes, offset: O, endian: E)`
- `Bytes/read_u32(bytes, offset: O, endian: E)`
- `Bytes/read_u64(bytes, offset: O, endian: E)`
- `Bytes/read_i8(bytes, offset: O)`
- `Bytes/read_i16(bytes, offset: O, endian: E)`
- `Bytes/read_i32(bytes, offset: O, endian: E)`
- `Bytes/read_i64(bytes, offset: O, endian: E)`
- `Bytes/read_f32(bytes, offset: O, endian: E)`
- `Bytes/read_f64(bytes, offset: O, endian: E)`

#### Typed Views (Write)
- `Bytes/write_u8(bytes, offset: O, value: V)`
- `Bytes/write_u16(bytes, offset: O, value: V, endian: E)`
- `Bytes/write_u32(bytes, offset: O, value: V, endian: E)`
- `Bytes/write_u64(bytes, offset: O, value: V, endian: E)`
- `Bytes/write_i8(bytes, offset: O, value: V)`
- `Bytes/write_i16(bytes, offset: O, value: V, endian: E)`
- `Bytes/write_i32(bytes, offset: O, value: V, endian: E)`
- `Bytes/write_i64(bytes, offset: O, value: V, endian: E)`
- `Bytes/write_f32(bytes, offset: O, value: V, endian: E)`
- `Bytes/write_f64(bytes, offset: O, value: V, endian: E)`

#### Manipulation
- `Bytes/concat(LIST { a, b, c })` - Concatenate
- `Bytes/append(bytes, byte: B)` - Add byte at end
- `Bytes/prepend(bytes, byte: B)` - Add byte at start
- `Bytes/reverse(bytes)` - Reverse order
- `Bytes/fill(bytes, start: S, end: E, value: V)` - Fill range

#### Search
- `Bytes/find(bytes, pattern: P)` - Find pattern (index or None)
- `Bytes/starts_with(bytes, prefix: P)` - Check prefix
- `Bytes/ends_with(bytes, suffix: S)` - Check suffix
- `Bytes/contains(bytes, pattern: P)` - Contains pattern?
- `Bytes/equal(a, b)` - Byte-wise equality

#### Conversion
- `Bytes/to_bits(bytes)` - Convert to BITS
- `Bytes/to_text(bytes, encoding: E)` - Decode to text
- `Bytes/to_hex(bytes)` - To hex string
- `Bytes/to_base64(bytes)` - To Base64
- `Bytes/from_hex(text)` - Parse hex
- `Bytes/from_base64(text)` - Parse Base64

### Flags Module

- `Flags/pack(record, schema: S)` - Record of Bools to BITS
- `Flags/unpack(bits, schema: S)` - BITS to record of Bools
- `Flags/set(bits, flag: F, schema: S)` - Set flag
- `Flags/clear(bits, flag: F, schema: S)` - Clear flag
- `Flags/test(bits, flag: F, schema: S)` - Test flag (Bool)
- `Flags/toggle(bits, flag: F, schema: S)` - Toggle flag

### Tag Module (Binary Encoding)

- `Tag/to_bits(tag, encoding: E, width: N)` - Encode tag
- `Tag/from_bits(bits, encoding: E)` - Decode tag

---

## Design Rationale

### Why Two Separate Abstractions?

**BITS** and **BYTES** serve different purposes:

| Aspect | BITS | BYTES |
|--------|------|-------|
| **Granularity** | Individual bits | Byte boundaries |
| **Primary use** | Hardware, flags, bit fields | Buffers, I/O, serialization |
| **Width** | Explicit, any bit count | Always multiple of 8 |
| **Endianness** | N/A (bit order is MSB-to-LSB) | Explicit for multi-byte ops |
| **Typical size** | 1-64 bits | Bytes to megabytes |

### Why Explicit Width for BITS?

1. **Hardware synthesis** - FPGA needs exact widths
2. **Overflow semantics** - Wrapping behavior depends on width
3. **Memory layout** - Pack into known bit positions
4. **Type safety** - Prevent accidental width mismatches

### Why Explicit Endianness for BYTES?

1. **Cross-platform** - x86 is little-endian, network is big-endian
2. **Protocol correctness** - Binary protocols specify byte order
3. **No surprises** - Endianness bugs are hard to find
4. **Explicit is better** - Never assume byte order

### Why base[s|u]value Syntax?

1. **Unified format** - `base[s|u]value` encodes base AND signedness in one notation
2. **Always explicit** - `10u42` is unambiguous: base 10, unsigned, value 42
3. **Consistent everywhere** - Same pattern for decimal, binary, hex: `10u`, `2s`, `16u`
4. **Self-documenting** - `16sFF` immediately shows: hex, signed, pattern FF
5. **Function naming matches** - `Bits/u_zeros()` mirrors `10u0` literal syntax
6. **No abbreviations** - Full `s` for signed, `u` for unsigned, not cryptic symbols

### Why Separate Flags Module?

1. **Semantic clarity** - Flags are named booleans, not raw bits
2. **Type safety** - Schema enforces bit positions
3. **Reusable pattern** - Common across all domains
4. **Ergonomic** - Record syntax is more readable than bit manipulation

---

## Common Pitfalls

### 1. Forgetting Width Causes Truncation

```boon
-- ❌ WRONG: Result truncated to 8 bits
a: BITS { 8, 10u200 }
b: BITS { 8, 10u100 }
sum: Bits/add(a, b)  -- Result: 44 (300 mod 256) in release mode

-- ✅ CORRECT: Use wider result
a: BITS { 8, 10u200 }
b: BITS { 8, 10u100 }
sum: Bits/add(
    a |> Bits/zero_extend(to: 9)
    b |> Bits/zero_extend(to: 9)
)  -- Result: 300
```

### 2. Ignoring Endianness

```boon
-- ❌ WRONG: Platform-dependent behavior
value: Bytes/read_u32(data, offset: 0)  -- Which endianness?

-- ✅ CORRECT: Explicit endianness
value: Bytes/read_u32(data, offset: 0, endian: Big)
```

### 3. Bit Index Confusion

```boon
-- BITS uses MSB-to-LSB indexing (index 0 is LSB)
bits: BITS { 8, 2u10110010 }
--              ^      ^
--            bit 7   bit 0

bit_7: bits |> Bits/get(index: 7)  -- True (MSB)
bit_0: bits |> Bits/get(index: 0)  -- False (LSB)
```

### 4. Mixing BITS and Numbers

```boon
-- ❌ WRONG: Cannot add BITS and number directly
result: bits + 1

-- ✅ CORRECT: Use Bits/increment or convert
result: bits |> Bits/increment()
-- or
result: Bits/add(bits, Bits/u_from_number(value: 1, width: Bits/width(bits)))
```

### 5. Mixing Signed and Unsigned

```boon
-- ❌ WRONG: Signed/unsigned mismatch
u_val: BITS { 8, 10u100 }
s_val: BITS { 8, 10s50 }
sum: u_val |> Bits/add(s_val)  -- ERROR: type mismatch

-- ✅ CORRECT: Convert explicitly
sum: u_val |> Bits/add(s_val |> Bits/as_unsigned())
```

---

## Future Considerations

### 1. Const Expressions for Width

```boon
-- Compile-time width calculation
WIDTH: 8
data: BITS { width: WIDTH * 2, zero }  -- 16-bit
```

### 2. Bit Field Struct Syntax

```boon
-- Declarative bit field layout
BITFIELD TcpHeader {
    source_port: 16
    dest_port: 16
    sequence: 32
    ack: 32
    data_offset: 4
    reserved: 3
    flags: 9
    window: 16
    checksum: 16
    urgent: 16
}

header: TcpHeader/parse(bytes)
header.source_port  -- Automatically extracts correct bits
```

### 3. SIMD Operations

```boon
-- Parallel bit operations
Bits/simd_add(vectors: LIST { a, b, c, d }, width: 32)
```

### 4. Memory-Mapped I/O First-Class Support

```boon
-- Direct hardware register access
GPIO_CONTROL: BITS/REGISTER { address: 16u40000000, width: 32 }
GPIO_CONTROL |> Bits/set(index: 0, value: True)  -- Writes to hardware
```

---

**Last Updated:** 2025-11-17
**Status:** Design Specification
**Related:** TEXT_SYNTAX.md, BOON_SYNTAX.md, ERROR_HANDLING.md
