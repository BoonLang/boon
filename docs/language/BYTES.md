# BYTES: Byte-Level Data in Boon

**Date**: 2025-11-17
**Status**: Design Specification

---

## Executive Summary

BYTES provides byte-level data abstraction for buffers, files, network protocols, and serialization in Boon:

- **Byte-oriented** - Always operates on byte boundaries
- **Explicit endianness** - Required for multi-byte reads/writes
- **Simple literals** - `16uFF` notation for hex bytes (consistent with BITS)
- **Typed views** - Read/write u8, u16, u32, i8, i16, i32, f32, f64
- **Universal** - Works across Web/Wasm, Server, Embedded, 3D

**Key principles:**
- Explicit endianness for multi-byte operations
- No surprises in byte order
- Clean text encoding/decoding

**Note:** For bit-level work (hardware registers, flags, bit manipulation), use **BITS** instead (see [BITS.md](./BITS.md)).

---

## Literal Syntax

**Core format: `BYTES { size, content }`** (consistent with BITS and LIST)

- **size**: Explicit number, compile-time constant, or `__` (infer from content)
- **content**: Byte literals `{ ... }`, BITS values (byte-aligned), or other BYTES values

### Byte Literals (Explicit Base Required)

All number literals require explicit base prefix for clarity:

```boon
-- Hex bytes (most common for binary data)
data: BYTES { __, { 16uFF, 16u00, 16uAB, 16uCD } }  -- 4 bytes (size inferred)

-- Decimal bytes (explicit base required)
data: BYTES { __, { 10u255, 10u0, 10u171, 10u205 } }  -- Same as above

-- Binary bytes (for bit patterns)
flags: BYTES { __, { 2u10110000, 2u11111111 } }  -- 2 bytes

-- Octal bytes (rare, but supported)
perms: BYTES { __, { 8u644 } }  -- 1 byte

-- Single byte
single: BYTES { __, { 16uFF } }  -- 1 byte

-- Mixed bases (use the base that matches semantic meaning)
header: BYTES { __, { 16u89, 16u50, 10u78, 10u71 } }  -- PNG magic bytes
--                    ^^^^   ^^^^   ^^^^   ^^^^
--                    hex    hex    dec    dec (all explicit!)

-- Explicit size (must match content)
exact: BYTES { 4, { 16uFF, 16u00, 16uAB, 16uCD } }  -- Exactly 4 bytes
```

**Note on syntax:**
- **First parameter**: size (number, constant, or `__` to infer)
- **Second parameter**: content in `{ ... }`
- Byte literals use `16uFF` (base + `u` + value) - consistent with BITS
- Bytes are ALWAYS unsigned (0-255 range)
- All numbers require explicit base: `10u`, `16u`, `2u`, `8u`
- No plain `255` allowed - must be `10u255` to avoid ambiguity with variables

### Nested BYTES (Auto-Flattened)

BYTES values can be nested for composition (useful for construction and pattern matching):

```boon
-- Define reusable byte sequences
CRLF: BYTES { __, { 10u13, 10u10 } }  -- CR LF (2 bytes)
STX: BYTES { __, { 10u2 } }           -- Start of Text (1 byte)
ETX: BYTES { __, { 10u3 } }           -- End of Text (1 byte)

-- Compose messages (nested BYTES are flattened)
message: BYTES { __, {
    STX,                                      -- 1 byte (flattened)
    10u72, 10u101, 10u108, 10u108, 10u111,   -- "Hello"
    ETX,                                      -- 1 byte (flattened)
    CRLF                                      -- 2 bytes (flattened)
} }
-- Result: 9 bytes total (1 + 5 + 1 + 2)
```

### BITS Values (Byte-Aligned Only)

Byte-aligned BITS (width must be multiple of 8) can be included and are auto-converted:

```boon
-- 8-bit BITS → 1 byte
flags: BITS { 8, 2u10110000 }
packet: BYTES { __, { 16uFF, flags, 16u00 } }
-- Result: 3 bytes (16uFF, 16uB0, 16u00)

-- 16-bit BITS → 2 bytes (MSB-first)
port: BITS { 16, 16u1F90 }  -- Port 8080
header: BYTES { __, { 16uFF, 16u00, port } }
-- Result: 4 bytes (16uFF, 16u00, 16u1F, 16u90)

-- ❌ Non-byte-aligned BITS are rejected
partial: BITS { 12, 16uABC }
bad: BYTES { __, { partial } }  -- ERROR: BITS width must be multiple of 8 (got 12)
```

**Multi-byte BITS conversion:**
- BITS are always MSB-first (bit 15 is leftmost in a 16-bit value)
- When converted to BYTES, MSB goes in first byte (big-endian style)
- For non-byte-aligned BITS, use `Bits/to_bytes(endian: ...)` function

### Construction Functions (Not Literals)

For conversions and dynamic construction, use functions:

```boon
-- From text (encoding conversion)
utf8_bytes: Bytes/from_text(TEXT { Hello }, encoding: Utf8)
ascii_bytes: Bytes/from_text(TEXT { GET }, encoding: Ascii)

-- From Base64 (decoding)
decoded: Bytes/from_base64(TEXT { SGVsbG8gV29ybGQ= })

-- From hex string (parsing)
parsed: Bytes/from_hex(TEXT { FF00ABCD })

-- Zero-filled buffer (allocation)
buffer: Bytes/zeros(length: 1024)  -- 1KB of zeros
```

**Why functions instead of literal forms?**
- Literals are for direct values, functions are for conversions
- Clearer distinction between compile-time constants and operations
- Works with both constants and variables
- More consistent with Boon's explicit philosophy

---

## Semantics and Rules

### Size and Content Matching

**Rule: Explicit size MUST match content byte count (compile error if mismatch)**

```boon
-- ✅ Size matches content
data: BYTES { 3, { 16uFF, 16u00, 16uAB } }  -- OK: 3 bytes in content

-- ❌ Size mismatch - COMPILE ERROR
bad: BYTES { 4, { 16uFF, 16u00 } }  -- ERROR: Size 4 but content is 2 bytes
bad: BYTES { 2, { 16uFF, 16u00, 16uAB } }  -- ERROR: Size 2 but content is 3 bytes

-- ✅ Use __ to infer size from content
data: BYTES { __, { 16uFF, 16u00 } }  -- OK: Infers size = 2

-- ✅ Use {} for zero-fill (any size)
buffer: BYTES { 1024, {} }  -- OK: 1024 zero bytes
empty: BYTES { 0, {} }      -- OK: 0 bytes
```

### Concatenation Rules

**Rule: Fixed-size containers can only contain fixed-size BYTES. Dynamic containers can contain any BYTES.**

```boon
-- ✅ Fixed-size BYTES can concatenate other FIXED-SIZE BYTES
header: BYTES { __, { 16uFF, 16u00 } }      -- Fixed (2 bytes)
footer: BYTES { __, { 16u00, 16uFF } }      -- Fixed (2 bytes)
frame: BYTES { __, { header, 16uAB, footer } }  -- Fixed (5 bytes = 2+1+2)

-- ✅ Dynamic BYTES can concatenate any BYTES (fixed or dynamic)
dynamic1: BYTES {}                          -- Dynamic
dynamic2: BYTES {}                          -- Dynamic
fixed: BYTES { __, { 16uFF, 16u00 } }       -- Fixed (2 bytes)

combined_dynamic: BYTES { dynamic1, dynamic2 }     -- Dynamic result
mixed: BYTES { fixed, dynamic1, 16uAB }            -- Dynamic result

-- ❌ Fixed-size BYTES CANNOT contain dynamic BYTES
bad: BYTES { __, { dynamic1, dynamic2 } }
-- ERROR: Cannot infer compile-time size from runtime-sized content
```

**See "Size Semantics: Dynamic vs Fixed-Size" section below for complete details.**

### Content Composition and Flattening

**Rule: Content `{ }` can contain byte literals, BITS values, and nested BYTES - all flattened in order**

**Important:** Fixed-size containers can only contain fixed-size BYTES (compile-time known size). Dynamic containers can contain both fixed and dynamic BYTES.

```boon
-- Byte literals (1 byte each)
data: BYTES { __, { 16uFF, 10u255, 2u11111111 } }  -- 3 bytes

-- BITS values (must be byte-aligned: width % 8 == 0)
flags: BITS { 8, 2u10110000 }   -- 8 bits = 1 byte
status: BITS { 16, 16uABCD }    -- 16 bits = 2 bytes
packet: BYTES { __, { 16uFF, flags, status, 16u00 } }
-- Flattened: 1 + 1 + 2 + 1 = 5 bytes
-- Result: { 16uFF, 16uB0, 16uAB, 16uCD, 16u00 }

-- Nested BYTES (flattened to their full content)
header: BYTES { __, { 16uFF, 16u00 } }     -- 2 bytes
footer: BYTES { __, { 16u00, 16uFF } }     -- 2 bytes
frame: BYTES { __, { header, 16uAB, footer } }
-- Flattened: 2 + 1 + 2 = 5 bytes
-- Result: { 16uFF, 16u00, 16uAB, 16u00, 16uFF }

-- ❌ BITS must be byte-aligned
partial: BITS { 12, 16uABC }  -- 12 bits (not byte-aligned)
bad: BYTES { __, { partial } }  -- ERROR: BITS width must be multiple of 8
```

### Empty Content Semantics

**Rule: `{}` means zero-filled (all bytes are 0)**

```boon
-- Zero-filled buffer
zeros: BYTES { 1024, {} }  -- 1024 bytes, all 0x00

-- Empty BYTES (size 0)
empty: BYTES { 0, {} }     -- 0 bytes
empty: BYTES { __, {} }    -- Also 0 bytes (inferred from empty content)

-- These are equivalent
buffer1: BYTES { 4, {} }
buffer2: BYTES { __, { 10u0, 10u0, 10u0, 10u0 } }  -- Same: 4 zero bytes
```

### Common Errors

```boon
-- ❌ Size mismatch
bad: BYTES { 10, { 16uFF, 16u00 } }
-- ERROR: Size 10 but content has 2 bytes

-- ❌ Non-byte-aligned BITS
bad_bits: BITS { 5, 2u10110 }
bad: BYTES { __, { bad_bits } }
-- ERROR: BITS width 5 is not multiple of 8

-- ❌ Appending to fixed-size
header: BYTES { __, { 16uFF, 16u00 } }
header: header |> Bytes/append(byte: 16uAB)
-- ERROR: Cannot append to fixed-size BYTES

-- ❌ Cannot use BYTES<N> syntax (IDE display only)
bad: BYTES<14>
-- ERROR: Not valid Boon syntax (use BYTES { 14, {} })

-- ❌ Cannot put dynamic BYTES in fixed-size container
dynamic_data: BYTES {}
bad: BYTES { __, { dynamic_data, 16uFF } }
-- ERROR: Cannot infer compile-time size from runtime-sized content

-- ✅ CORRECT: Use dynamic container for dynamic BYTES
good: BYTES { dynamic_data, 16uFF }  -- Dynamic result
```

---

## Size Semantics: Dynamic vs Fixed-Size

**BYTES can be dynamic (software) or fixed-size (hardware-compatible).**

This matches the design philosophy of LIST - size can be specified or omitted.

### Dynamic BYTES (Software Only)

**No size specified - can grow/shrink at runtime:**

```boon
-- Dynamic byte buffer
buffer: BYTES {}  -- Type: BYTES (size unknown at compile-time)

-- Can grow
buffer: buffer |> Bytes/append(byte: 16uFF)

-- Can shrink
buffer: buffer |> Bytes/take(count: 10)
```

**Use dynamic BYTES for:**
- Network buffers (variable-length packets)
- File I/O (unknown file sizes)
- String processing (text encoding/decoding)
- Any software context with variable-length data

### Fixed-Size BYTES (Hardware-Compatible)

**Size specified - compile-time known, cannot grow:**

```boon
-- Fixed-size BYTES with explicit size (consistent with BITS/LIST)
buffer: BYTES { 1024, {} }  -- 1024 bytes, zero-filled
packet: BYTES { 64, {} }    -- 64 bytes, zero-filled

-- Size from compile-time constant
packet_size: 64
frame: BYTES { packet_size, {} }  -- 64 bytes

-- Size inference placeholder
config: BYTES { __, {} }  -- Size inferred from context

-- Literals have inferred size (IDE shows BYTES<4>)
magic: BYTES { __, { 16u89, 16u50, 16u4E, 16u47 } }  -- 4 bytes

-- Nested BYTES - size is sum (IDE shows BYTES<2>, BYTES<3>, BYTES<5>)
header: BYTES { __, { 16uFF, 16u00 } }  -- 2 bytes
payload: BYTES { __, { 16u01, 16u02, 16u03 } }  -- 3 bytes
packet: BYTES { __, { header, payload } }  -- 5 bytes (2 + 3)
```

**Use fixed-size BYTES for:**
- Hardware signals (fixed-width wires)
- Protocol headers (fixed-length fields)
- Embedded systems (stack-allocated buffers)
- Any hardware context (FPGA, HDL synthesis)

### Size Must Be Compile-Time Known

**If BYTES size is specified, it MUST be compile-time constant:**

```boon
-- ✅ Literal size (inferred from construction)
data: BYTES { __, { 16uFF, 16u00, 16uAB } }  -- Size: 3 (compile-time known)

-- ✅ Compile-time constant
packet_size: 64  -- Compile-time constant (snake_case!)
buffer: BYTES { packet_size, {} }  -- Size: 64 (compile-time known)

-- ✅ Compile-time expression
header_size: 14
payload_size: 1500
frame: BYTES { header_size + payload_size, {} }  -- Size: 1514 (compile-time known)

-- ✅ Using function to create zero-filled buffer
buffer: Bytes/zeros(length: packet_size)

-- ❌ Runtime size
user_size: get_size_from_input()
buffer: BYTES { user_size, {} }  -- ERROR: Size must be compile-time constant

-- ✅ Use dynamic BYTES instead
dynamic_buffer: BYTES {}  -- OK: no size specified
```

### Hardware Requires Fixed Size

**In hardware (FPGA/HDL), all signal widths must be compile-time known:**

```boon
-- ❌ Bad: Hardware can't handle unknown width
FUNCTION process_packet(data) {
    -- If data is dynamic BYTES, compiler doesn't know width!
    -- Cannot synthesize to HDL!
}

-- ✅ Good: Hardware knows exact width from call site
ethernet_header: BYTES { 14, {} }  -- 14 bytes = 112 bits
payload: BYTES { 1500, {} }  -- 1500 bytes = 12000 bits

result: parse_ethernet(header: ethernet_header, payload: payload)
-- Compiler infers exact sizes from arguments
-- Synthesizer can create exact-width wires
```

### Size Inference from Literals

**The compiler always infers size from BYTES literals:**

```boon
-- Direct literal - size counted (4 bytes)
magic: BYTES { __, { 16u89, 16u50, 16u4E, 16u47 } }

-- Nested BYTES - sizes summed
header: BYTES { __, { 16uFF, 16u00 } }  -- 2 bytes
footer: BYTES { __, { 16u00, 16uFF } }  -- 2 bytes
frame: BYTES { __, { header, 16uAB, footer } }  -- 5 bytes (2 + 1 + 2)

-- BITS in BYTES - size from BITS width
flags: BITS { 16, 16uABCD }  -- 16 bits = 2 bytes
packet: BYTES { __, { 16uFF, flags, 16u00 } }  -- 4 bytes (1 + 2 + 1)

-- Mixed composition
start: BYTES { __, { 16uAA, 16u55 } }  -- 2 bytes
data_bits: BITS { 24, 16uABCDEF }  -- 24 bits = 3 bytes
end: BYTES { __, { 16uFF } }  -- 1 byte
message: BYTES { __, { start, data_bits, end } }  -- 6 bytes (2 + 3 + 1)
```

### Type Inference and Size Checking

**The compiler infers BYTES sizes and checks compatibility:**

```boon
-- These are DIFFERENT sizes (inferred by compiler)
ethernet_header: BYTES { 14, {} }  -- 14 bytes
ip_header: BYTES { 20, {} }  -- 20 bytes

-- ❌ Can't mix different sizes in operations that expect same size
headers: LIST { ethernet_header, ip_header }  -- ERROR: Inconsistent sizes

-- ✅ Compiler infers size requirements from usage
FUNCTION parse_ethernet_header(header) {
    -- If function accesses byte 13, compiler knows header needs ≥14 bytes
    last_byte: header |> Bytes/get(index: 13)
}

-- ✅ Call with correct size
ethernet_header: BYTES { 14, {} }
result: parse_ethernet_header(header: ethernet_header)  -- OK: 14 bytes

-- ❌ Call with wrong size causes error
small_header: BYTES { 10, {} }
result: parse_ethernet_header(header: small_header)  -- ERROR: Only 10 bytes, needs ≥14
```

### Dynamic vs Fixed: Clear Distinction

```boon
-- ✅ Dynamic BYTES (software) - can grow/shrink
buffer: BYTES {}
buffer: buffer |> Bytes/append(byte: 16uFF)  -- OK: dynamic

-- ✅ Fixed-size BYTES (hardware) - cannot grow
header: BYTES { 14, {} }
header: header |> Bytes/append(byte: 16uFF)  -- ERROR: Fixed size, cannot grow

-- ✅ Pattern matching on first bytes (size check)
parse_packet: FUNCTION(data) {
    data |> Bytes/take(count: 4) |> WHEN {
        BYTES { __, { 16u89, 16u50, 16u4E, 16u47 } } => parse_png(data)
        BYTES { __, { 16uFF, 16uD8, 16uFF, 16uE0 } } => parse_jpeg(data)
        __ => UnknownFormat
    }
}
```

### Comparison with Other Types

| Type | Size/Width Parameter | Must Be Compile-Time? | Can Be Dynamic? |
|------|---------------------|----------------------|-----------------|
| **BITS** | Width (bits) | ✅ ALWAYS | ❌ Never |
| **BYTES** | Size (bytes) | ✅ IF SPECIFIED | ✅ Yes (omit size) |
| **LIST** | Size (elements) | ✅ IF SPECIFIED | ✅ Yes (omit size) |
| **MEMORY** | Size (locations) | ✅ ALWAYS | ❌ Never |

**Key insight:** BYTES and LIST are similar - both can be dynamic (software) or fixed (hardware). BITS and MEMORY are always fixed-size.

---

## Core Operations

```boon
-- Size
Bytes/length(bytes)                      -- Number of bytes

-- Byte access
Bytes/get(bytes, index: 0)              -- Single byte (0-255)
Bytes/set(bytes, index: 0, value: 16uFF)-- Set byte
Bytes/slice(bytes, start: 0, end: 4)    -- Sub-range [start, end)
Bytes/drop(bytes, count: 2)             -- Remove first N bytes
Bytes/take(bytes, count: 4)             -- Keep first N bytes

-- Concatenation
Bytes/concat(LIST { a, b, c })          -- Join buffers
Bytes/append(bytes, byte: 16uFF)        -- Add byte at end
Bytes/prepend(bytes, byte: 16u00)       -- Add byte at start

-- Typed views (read multi-byte numbers)
Bytes/read_unsigned(bytes, offset: 0, byte_count: 1)  -- Single byte (no endian needed)
Bytes/read_unsigned(bytes, offset: 0, byte_count: 2, endian: Little)
Bytes/read_unsigned(bytes, offset: 0, byte_count: 4, endian: Big)
Bytes/read_unsigned(bytes, offset: 0, byte_count: 8, endian: Little)
Bytes/read_signed(bytes, offset: 0, byte_count: 1)    -- Single byte (no endian needed)
Bytes/read_signed(bytes, offset: 0, byte_count: 2, endian: Little)
Bytes/read_signed(bytes, offset: 0, byte_count: 4, endian: Big)
Bytes/read_signed(bytes, offset: 0, byte_count: 8, endian: Little)
Bytes/read_float(bytes, offset: 0, byte_count: 4, endian: Little)  -- 32-bit float
Bytes/read_float(bytes, offset: 0, byte_count: 8, endian: Little)  -- 64-bit float

-- Typed writes
Bytes/write_unsigned(bytes, offset: 0, value: 1000, byte_count: 2, endian: Little)
Bytes/write_unsigned(bytes, offset: 4, value: 12345, byte_count: 4, endian: Big)
Bytes/write_float(bytes, offset: 8, value: 3.14, byte_count: 4, endian: Little)

-- Text conversions
Bytes/to_text(bytes, encoding: Utf8)    -- Decode to text
Bytes/to_hex(bytes)                     -- "FF00ABCD"
Bytes/to_base64(bytes)                  -- Base64 encode
Bytes/from_hex(hex_text)
Bytes/from_base64(base64_text)

-- Search and comparison
Bytes/find(bytes, pattern: BYTES { __, { 16uFF, 16u00 } })  -- Find pattern
Bytes/starts_with(bytes, prefix: BYTES { __, { 16u89, 16u50 } })  -- PNG header?
Bytes/ends_with(bytes, suffix: ...)
Bytes/equal(a, b)                       -- Byte-wise equality

-- Transformation
Bytes/reverse(bytes)                    -- Reverse byte order
Bytes/fill(bytes, start: 0, end: 10, value: 16u00)  -- Fill range with value
```

---

## Endianness

Endianness is always explicit for multi-byte reads/writes:

```boon
-- Explicit for all multi-byte operations
value: Bytes/read_unsigned(data, offset: 0, byte_count: 4, endian: Little)  -- x86 style
value: Bytes/read_unsigned(data, offset: 0, byte_count: 4, endian: Big)     -- Network byte order

-- Swap endianness by re-reading with different endian
little_endian: Bytes/read_unsigned(data, offset: 0, byte_count: 4, endian: Little)
big_endian: Bytes/read_unsigned(data, offset: 0, byte_count: 4, endian: Big)
```

**Why explicit?**
- x86 is little-endian
- Network protocols are big-endian
- Endianness bugs are hard to find
- Never assume byte order

---

## Pattern Matching

### File Type Detection

```boon
-- File type by magic bytes
file_bytes |> Bytes/take(count: 4) |> WHEN {
    BYTES { __, { 16u89, 16u50, 16u4E, 16u47 } } => PngFile
    BYTES { __, { 16uFF, 16uD8, 16uFF, 16uE0 } } => JpegFile
    BYTES { __, { 16u47, 16u49, 16u46, 16u38 } } => GifFile
    BYTES { __, { 16u25, 16u50, 16u44, 16u46 } } => PdfFile
    __ => UnknownFile
}
```

### HTTP Method Parsing

```boon
-- Option 1: Convert to TEXT first (recommended for text protocols)
request_line |> Bytes/to_text(encoding: Ascii) |> Text/take(count: 4) |> WHEN {
    TEXT { GET } => GetMethod
    TEXT { POST } => PostMethod
    TEXT { PUT } => PutMethod
    __ => parse_other_method(request_line)
}

-- Option 2: Use direct hex bytes (for binary protocols)
request_line |> Bytes/take(count: 3) |> WHEN {
    BYTES { __, { 16u47, 16u45, 16u54 } } => GetMethod   -- "GET"
    __ => request_line |> Bytes/take(count: 4) |> WHEN {
        BYTES { __, { 16u50, 16u4F, 16u53, 16u54 } } => PostMethod  -- "POST"
        __ => parse_other_method(request_line)
    }
}
```

---

## Conversions

### BYTES ↔ Number

```boon
-- Single byte to number
num: BYTES { __, { 16uFF } } |> Bytes/get(index: 0)  -- 255

-- Multi-byte to number (via typed read)
num: Bytes/read_unsigned(bytes, offset: 0, byte_count: 4, endian: Little)
```

### BYTES ↔ BITS

```boon
-- BYTES to BITS (unsigned by default)
bytes: BYTES { __, { 16uFF, 16u00 } }
bits: bytes |> Bytes/to_u_bits()           -- BITS { 16, 16uFF00 }
bits: bytes |> Bytes/to_s_bits()           -- BITS { 16, 16sFF00 }

-- BITS to BYTES (pads to byte boundary if needed)
bits: BITS { 12, 16uABC }                  -- 12 bits
bytes: bits |> Bits/to_bytes()             -- 2 bytes (padded)
```

### BYTES ↔ Text

```boon
-- BYTES to text
BYTES { __, { 16uFF, 16u00 } } |> Bytes/to_hex()             -- "FF00"
BYTES { __, { 16uFF, 16u00 } } |> Bytes/to_base64()          -- "/wA="
utf8_bytes |> Bytes/to_text(encoding: Utf8)          -- "Hi"

-- Text to BYTES (use functions, not literals)
Bytes/from_hex(TEXT { FF00ABCD })
Bytes/from_base64(TEXT { SGVsbG8= })
Bytes/from_text(TEXT { Hello }, encoding: Utf8)
```

---

## Domain Examples

### Web/Wasm: WebSocket Frame Parsing

```boon
FUNCTION parse_ws_frame(bytes) {
    BLOCK {
        first_byte: bytes |> Bytes/get(index: 0)
        second_byte: bytes |> Bytes/get(index: 1)

        first_bits: Bits/u_from_number(value: first_byte, width: 8)
        second_bits: Bits/u_from_number(value: second_byte, width: 8)

        [
            fin: first_bits |> Bits/get(index: 7)
            opcode: first_bits |> Bits/slice(high: 3, low: 0)
            masked: second_bits |> Bits/get(index: 7)
            payload_len: second_bits |> Bits/slice(high: 6, low: 0) |> Bits/to_number()
        ]
    }
}
```

### Server: TCP Header Parsing

```boon
FUNCTION parse_tcp_header(bytes) {
    BLOCK {
        src_port: Bytes/read_unsigned(bytes, offset: 0, byte_count: 2, endian: Big)
        dst_port: Bytes/read_unsigned(bytes, offset: 2, byte_count: 2, endian: Big)
        seq_num: Bytes/read_unsigned(bytes, offset: 4, byte_count: 4, endian: Big)
        ack_num: Bytes/read_unsigned(bytes, offset: 8, byte_count: 4, endian: Big)

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
            ]
        ]
    }
}
```

### Server: Build Protocol Message

```boon
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
```

### 3D Graphics: Texture Format Detection

```boon
FUNCTION detect_texture_format(header_bytes) {
    header_bytes |> Bytes/take(count: 4) |> WHEN {
        BYTES { __, { 16u44, 16u44, 16u53, 16u20 } } => DDS
        BYTES { __, { 16uAB, 16u4B, 16u54, 16u58 } } => KTX
        BYTES { __, { 16u89, 16u50, 16u4E, 16u47 } } => PNG
        __ => Unknown
    }
}
```

---

## API Reference

### Literal Syntax

- `BYTES { size, { byte, byte, ... } }` - From byte literals with explicit size
- `BYTES { __, { byte, byte, ... } }` - From byte literals with inferred size
- `BYTES { size, {} }` - Zero-filled buffer with explicit size
- `BYTES {}` - Dynamic empty BYTES

### Construction Functions

- `Bytes/zeros(length: N)` - Zero-filled buffer
- `Bytes/from_text(text, encoding: E)` - From text
- `Bytes/from_base64(text)` - From Base64
- `Bytes/from_hex(text)` - From hex string
- `Bytes/from_bits(bits)` - From BITS

### Properties

- `Bytes/length(bytes)` - Byte count
- `Bytes/is_empty(bytes)` - Check if empty

### Byte Access

- `Bytes/get(bytes, index: I)` - Get byte (0-255)
- `Bytes/set(bytes, index: I, value: V)` - Set byte
- `Bytes/slice(bytes, start: S, end: E)` - Sub-range
- `Bytes/take(bytes, count: N)` - First N bytes
- `Bytes/drop(bytes, count: N)` - Skip first N bytes

### Typed Views (Read)

- `Bytes/read_unsigned(bytes, offset: O, byte_count: N)` - Single byte (no endian)
- `Bytes/read_unsigned(bytes, offset: O, byte_count: N, endian: E)` - Multi-byte unsigned
- `Bytes/read_signed(bytes, offset: O, byte_count: N)` - Single byte (no endian)
- `Bytes/read_signed(bytes, offset: O, byte_count: N, endian: E)` - Multi-byte signed
- `Bytes/read_float(bytes, offset: O, byte_count: N, endian: E)` - Float (4 or 8 bytes)

### Typed Views (Write)

- `Bytes/write_unsigned(bytes, offset: O, value: V, byte_count: N)` - Single byte (no endian)
- `Bytes/write_unsigned(bytes, offset: O, value: V, byte_count: N, endian: E)` - Multi-byte unsigned
- `Bytes/write_signed(bytes, offset: O, value: V, byte_count: N)` - Single byte (no endian)
- `Bytes/write_signed(bytes, offset: O, value: V, byte_count: N, endian: E)` - Multi-byte signed
- `Bytes/write_float(bytes, offset: O, value: V, byte_count: N, endian: E)` - Float (4 or 8 bytes)

### Manipulation

- `Bytes/concat(LIST { a, b, c })` - Concatenate
- `Bytes/append(bytes, byte: B)` - Add byte at end
- `Bytes/prepend(bytes, byte: B)` - Add byte at start
- `Bytes/reverse(bytes)` - Reverse order
- `Bytes/fill(bytes, start: S, end: E, value: V)` - Fill range

### Search

- `Bytes/find(bytes, pattern: P)` - Find pattern (index or None)
- `Bytes/starts_with(bytes, prefix: P)` - Check prefix
- `Bytes/ends_with(bytes, suffix: S)` - Check suffix
- `Bytes/contains(bytes, pattern: P)` - Contains pattern?
- `Bytes/equal(a, b)` - Byte-wise equality

### Conversion

- `Bytes/to_u_bits(bytes)` - Convert to unsigned BITS
- `Bytes/to_s_bits(bytes)` - Convert to signed BITS
- `Bytes/to_text(bytes, encoding: E)` - Decode to text
- `Bytes/to_hex(bytes)` - To hex string
- `Bytes/to_base64(bytes)` - To Base64
- `Bytes/from_text(text, encoding: E)` - Encode from text
- `Bytes/from_hex(text)` - Parse hex
- `Bytes/from_base64(text)` - Parse Base64
- `Bytes/zeros(length: N)` - Create zero-filled buffer

---

## Design Rationale

### Why Width Parameters Instead of Type-Specific Functions?

**Boon uses `byte_count` parameters instead of type-specific functions (like Rust's `read_u8`, `read_u16`, etc.):**

```boon
-- ✅ Boon style: Width as parameter
Bytes/read_unsigned(bytes, offset: 0, byte_count: 2, endian: Big)
Bytes/read_unsigned(bytes, offset: 0, byte_count: 4, endian: Big)

-- ❌ Rust style: Type in function name
Bytes/read_u16(bytes, offset: 0, endian: Big)
Bytes/read_u32(bytes, offset: 0, endian: Big)
```

**Benefits:**
1. **Consistent with BITS** - `BITS { width, value }` uses width parameter
2. **Less API surface** - 3 functions instead of 10+ type-specific ones
3. **More flexible** - Can read any byte width (not limited to 1,2,4,8)
4. **No type system assumptions** - Boon doesn't have explicit u8/u16/u32/u64 types
5. **Explicit is better** - `byte_count: 2` is clearer than `u16`

### Why Byte-Level Abstraction?

BYTES serves different purposes from BITS:

| Aspect | BITS | BYTES |
|--------|------|-------|
| **Granularity** | Individual bits | Byte boundaries |
| **Primary use** | Hardware, flags, bit fields | Buffers, I/O, serialization |
| **Width** | Explicit, any bit count | Always multiple of 8 |
| **Endianness** | N/A (bit order MSB-to-LSB) | Explicit for multi-byte ops |
| **Typical size** | 1-64 bits | Bytes to megabytes |

### Why Explicit Endianness?

1. **Cross-platform** - x86 is little-endian, network is big-endian
2. **Protocol correctness** - Binary protocols specify byte order
3. **No surprises** - Endianness bugs are hard to find
4. **Explicit is better** - Never assume byte order

---

## Common Pitfalls

### 1. Ignoring Endianness

```boon
-- ❌ WRONG: Endianness required for multi-byte reads
value: Bytes/read_unsigned(data, offset: 0, byte_count: 4)  -- ERROR: Missing endian parameter

-- ✅ CORRECT: Explicit endianness
value: Bytes/read_unsigned(data, offset: 0, byte_count: 4, endian: Big)
```

### 2. Byte vs Bit Confusion

```boon
-- BYTES operates on byte boundaries
-- BITS operates on individual bits

-- ❌ WRONG: Using BYTES for bit manipulation
byte |> Bytes/get(index: 0)  -- Gets whole byte (0-255)

-- ✅ CORRECT: Convert to BITS for bit access
bits: byte |> Bytes/to_u_bits()
bit: bits |> Bits/get(index: 7)  -- Get individual bit
```

---
