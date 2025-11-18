# BYTES: Byte-Level Data in Boon

**Date**: 2025-11-17
**Status**: Design Specification

---

## Executive Summary

BYTES provides byte-level data abstraction for buffers, files, network protocols, and serialization in Boon:

- **Byte-oriented** - Always operates on byte boundaries
- **Explicit endianness** - Required for multi-byte reads/writes
- **Simple literals** - `16#FF` notation for hex bytes
- **Typed views** - Read/write u8, u16, u32, i8, i16, i32, f32, f64
- **Universal** - Works across Web/Wasm, Server, Embedded, 3D

**Key principles:**
- Explicit endianness for multi-byte operations
- No surprises in byte order
- Clean text encoding/decoding

**Note:** For bit-level work (hardware registers, flags, bit manipulation), use **BITS** instead (see [BITS.md](./BITS.md)).

---

## Literal Syntax

**Core format: `BYTES { byte, byte, ... }`**

Bytes can be decimal or hex (using `16#` for consistency with BITS).

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

**Note on syntax:**
- **BYTES** uses `16#FF` (base + `#` + value) - no signedness concept
- **BITS** uses `16uFF` / `16sFF` (base + signedness + value)
- Both avoid `0x` prefix for consistency
- The `#` separator indicates "literal byte value"

---

## Core Operations

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
Bytes/map(bytes, byte => byte |> Bits/xor(16#FF))  -- Transform each
Bytes/fill(bytes, start: 0, end: 10, value: 16#00)  -- Fill range
```

---

## Endianness

Endianness is always explicit for multi-byte reads/writes:

```boon
-- Explicit for all multi-byte operations
value: Bytes/read_u32(data, offset: 0, endian: Little)  -- x86 style
value: Bytes/read_u32(data, offset: 0, endian: Big)     -- Network byte order

-- Swap endianness
swapped: Bytes/swap_endian_16(value)
swapped: Bytes/swap_endian_32(value)
swapped: Bytes/swap_endian_64(value)
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
    BYTES { 16#89, 16#50, 16#4E, 16#47 } => PngFile
    BYTES { 16#FF, 16#D8, 16#FF, 16#E0 } => JpegFile
    BYTES { 16#47, 16#49, 16#46, 16#38 } => GifFile
    BYTES { 16#25, 16#50, 16#44, 16#46 } => PdfFile
    __ => UnknownFile
}
```

### HTTP Method Parsing

```boon
request_line |> Bytes/take(count: 7) |> WHEN {
    BYTES { text: TEXT { GET }, encoding: ASCII } => GetMethod
    BYTES { text: TEXT { POST }, encoding: ASCII } => PostMethod
    BYTES { text: TEXT { PUT }, encoding: ASCII } => PutMethod
    __ => parse_other_method(request_line)
}
```

---

## Conversions

### BYTES ↔ Number

```boon
-- Single byte to number
num: BYTES { 16#FF } |> Bytes/get(index: 0)  -- 255

-- Multi-byte to number (via typed read)
num: Bytes/read_u32(bytes, offset: 0, endian: Little)
```

### BYTES ↔ BITS

```boon
-- BYTES to BITS (unsigned by default)
bytes: BYTES { 16#FF, 16#00 }
bits: bytes |> Bytes/to_u_bits()           -- BITS { 16, 16uFF00 }
bits: bytes |> Bytes/to_s_bits()           -- BITS { 16, 16sFF00 }

-- BITS to BYTES (pads to byte boundary if needed)
bits: BITS { 12, 16uABC }                  -- 12 bits
bytes: bits |> Bits/to_bytes()             -- BYTES { 16#0A, 16#BC }
```

### BYTES ↔ Text

```boon
-- BYTES to text
BYTES { 16#FF, 16#00 } |> Bytes/to_hex()             -- "FF00"
BYTES { 16#FF, 16#00 } |> Bytes/to_base64()          -- "/wA="
BYTES { text: TEXT { Hi }, encoding: UTF8 } |> Bytes/to_text(encoding: UTF8)  -- "Hi"

-- Text to BYTES
BYTES { hex: TEXT { FF00ABCD } }
BYTES { base64: TEXT { SGVsbG8= } }
BYTES { text: TEXT { Hello }, encoding: UTF8 }
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
        BYTES { 16#44, 16#44, 16#53, 16#20 } => DDS
        BYTES { 16#AB, 16#4B, 16#54, 16#58 } => KTX
        BYTES { 16#89, 16#50, 16#4E, 16#47 } => PNG
        __ => Unknown
    }
}
```

---

## API Reference

### Constructors

- `BYTES { byte, byte, ... }` - From byte literals
- `BYTES { length: N }` - Zero-filled buffer
- `BYTES { text: T, encoding: E }` - From text
- `BYTES { base64: T }` - From Base64
- `BYTES { hex: T }` - From hex string
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

### Typed Views (Write)

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
- `Bytes/from_hex(text)` - Parse hex
- `Bytes/from_base64(text)` - Parse Base64

---

## Design Rationale

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
-- ❌ WRONG: Platform-dependent behavior
value: Bytes/read_u32(data, offset: 0)  -- Which endianness?

-- ✅ CORRECT: Explicit endianness
value: Bytes/read_u32(data, offset: 0, endian: Big)
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
