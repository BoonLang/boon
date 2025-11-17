# BITS and BYTES: Binary Data in Boon

**Date**: 2025-11-17
**Status**: Design Specification
**Scope**: Binary data literals and operations across all Boon domains

---

## Executive Summary

Boon provides two complementary abstractions for binary data:

- **BITS** - Bit-level data with explicit width, for hardware registers, flags, bit manipulation
- **BYTES** - Byte-level data, for buffers, files, network protocols, serialization

Both use consistent literal syntax (like TEXT {} and LIST {}) and provide domain-appropriate operations that work universally across FPGA, Web/Wasm, Server, Embedded, and 3D contexts.

**Key design principles:**
- **Explicit over implicit** - Widths and sizes are always known
- **Semantic clarity** - BITS for bit-level work, BYTES for byte-level work
- **Universal applicability** - Same abstractions work everywhere
- **Clean conversions** - Bridge between Bool, Tags, Numbers, Text

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

```boon
-- Binary literal (width inferred from digit count)
flags: BITS { 1011 }                    -- 4-bit: 0b1011

-- Explicit width with decimal value
counter: BITS { width: 8, value: 42 }  -- 8-bit: 0b00101010

-- Hexadecimal (width = 4 * hex digits)
color: BITS { 0xFF80 }                  -- 16-bit: 0xFF80
mask: BITS { 0x0F }                     -- 8-bit: 0x0F

-- Verilog-style literals (familiar for hardware)
register: BITS { 32'd12345 }            -- 32-bit decimal
opcode: BITS { 8'hFF }                  -- 8-bit hex
state: BITS { 4'b1010 }                 -- 4-bit binary

-- Zero/one filled
zero_reg: BITS { width: 16, zero }      -- All zeros
one_reg: BITS { width: 16, ones }       -- All ones

-- From list of bools (LSB first)
manual: BITS { bits: LIST { True, True, False, True } }  -- 4'b1011
```

### Core Operations

```boon
-- Width and value
Bits/width(bits)                        -- Number of bits
Bits/to_number(bits)                    -- Convert to number
Bits/from_number(value: 42, width: 8)   -- Number to bits

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
Bits/concat(LIST { a, b, c })           -- Join bit vectors
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
a: BITS { width: 8, value: 200 }
b: BITS { width: 8, value: 100 }
sum: Bits/add(a, b)  -- Returns BITS { width: 8, value: 44 } (wraps)

-- Mismatched widths: shorter operand is zero-extended
a: BITS { width: 8, value: 0xFF }
b: BITS { width: 4, value: 0x3 }
result: Bits/and(a, b)  -- b becomes 0x03, result is 0x03

-- Explicit width change
extended: BITS { width: 4, value: 0xF } |> Bits/zero_extend(to: 8)  -- 0x0F
extended: BITS { width: 4, value: 0xF } |> Bits/sign_extend(to: 8)  -- 0xFF
truncated: BITS { width: 8, value: 0xFF } |> Bits/truncate(to: 4)   -- 0xF
```

---

## BYTES - Byte-Level Data

### Literal Syntax

```boon
-- Hex bytes (most common)
data: BYTES { 0xFF, 0x00, 0xAB, 0xCD }  -- 4 bytes

-- Decimal bytes
data: BYTES { 255, 0, 171, 205 }        -- Same as above

-- From text with encoding
utf8_data: BYTES { text: TEXT { Hello }, encoding: UTF8 }
ascii_data: BYTES { text: TEXT { OK }, encoding: ASCII }

-- From Base64
decoded: BYTES { base64: TEXT { SGVsbG8gV29ybGQ= } }

-- Zero-filled buffer
buffer: BYTES { length: 1024 }          -- 1KB of zeros

-- From hex string
parsed: BYTES { hex: TEXT { FF00ABCD } }

-- Single byte
single: BYTES { 0xFF }                   -- 1 byte
```

### Core Operations

```boon
-- Size
Bytes/length(bytes)                      -- Number of bytes

-- Byte access
Bytes/get(bytes, index: 0)              -- Single byte (0-255)
Bytes/set(bytes, index: 0, value: 0xFF) -- Set byte
Bytes/slice(bytes, start: 0, end: 4)    -- Sub-range [start, end)
Bytes/drop(bytes, count: 2)             -- Remove first N bytes
Bytes/take(bytes, count: 4)             -- Keep first N bytes

-- Concatenation
Bytes/concat(LIST { a, b, c })          -- Join buffers
Bytes/append(bytes, byte: 0xFF)         -- Add byte at end
Bytes/prepend(bytes, byte: 0x00)        -- Add byte at start

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
Bytes/find(bytes, pattern: BYTES { 0xFF, 0x00 })  -- Find pattern
Bytes/starts_with(bytes, prefix: BYTES { 0x89, 0x50 })  -- PNG header?
Bytes/ends_with(bytes, suffix: ...)
Bytes/equal(a, b)                       -- Byte-wise equality

-- Transformation
Bytes/reverse(bytes)                    -- Reverse byte order
Bytes/map(bytes, byte => byte |> Bits/xor(0xFF))  -- Transform each byte
Bytes/fill(bytes, start: 0, end: 10, value: 0x00)  -- Fill range
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
bits: BITS { 12'hABC }                  -- 12 bits
bytes: bits |> Bits/to_bytes()          -- BYTES { 0x0A, 0xBC } (2 bytes, padded)

-- BYTES to BITS
bytes: BYTES { 0xFF, 0x00 }
bits: bytes |> Bytes/to_bits()          -- BITS { width: 16, 0xFF00 }

-- Explicit padding control
bits: BITS { 5'b11011 }                 -- 5 bits
bytes: bits |> Bits/to_bytes(pad: Left)  -- BYTES { 0b00011011 }
bytes: bits |> Bits/to_bytes(pad: Right) -- BYTES { 0b11011000 }
```

### BITS ↔ Number

```boon
-- To number (unsigned)
num: BITS { 8'hFF } |> Bits/to_number()  -- 255

-- To number (signed, two's complement)
num: BITS { 8'hFF } |> Bits/to_number_signed()  -- -1

-- From number
bits: Bits/from_number(value: 42, width: 8)  -- BITS { 8'd42 }
bits: Bits/from_number(value: -1, width: 8)  -- BITS { 8'hFF }
```

### BYTES ↔ Number

```boon
-- Single byte to number
num: BYTES { 0xFF } |> Bytes/get(index: 0)  -- 255

-- Multi-byte to number (via typed read)
num: Bytes/read_u32(bytes, offset: 0, endian: Little)
```

### BITS ↔ Bool

```boon
-- Single bit to Bool
bit: BITS { 1'b1 }
bool: bit |> Bits/to_bool()  -- True

-- Bool to single bit
bool: True
bit: bool |> Bool/to_bit()  -- BITS { 1'b1 }

-- Get bit as Bool
flag: register |> Bits/get(index: 7)  -- Returns Bool
```

### BITS/BYTES ↔ Text

```boon
-- BITS to text representations
BITS { 8'hFF } |> Bits/to_text(format: Binary)  -- "11111111"
BITS { 8'hFF } |> Bits/to_text(format: Hex)     -- "FF"
BITS { 8'd255 } |> Bits/to_text(format: Decimal) -- "255"

-- Parse text to BITS
BITS { binary: TEXT { 11111111 } }              -- 8-bit from binary string
BITS { hex: TEXT { FF }, width: 8 }             -- 8-bit from hex string

-- BYTES to text
BYTES { 0xFF, 0x00 } |> Bytes/to_hex()          -- "FF00"
BYTES { 0xFF, 0x00 } |> Bytes/to_base64()       -- "/wA="
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

packed: render_state |> Flags/pack(schema: flag_schema)
-- Result: BITS { 4'b0110 }

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

-- Encode tag to bits
type_bits: Data |> Tag/to_bits(encoding: message_encoding, width: 4)
-- Result: BITS { 4'd1 }

-- Decode bits to tag
type_tag: BITS { 4'd2 } |> Tag/from_bits(encoding: message_encoding)
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

### Matching on BITS Values

```boon
-- Exact match
opcode |> WHEN {
    BITS { 8'h00 } => Nop
    BITS { 8'h01 } => Load
    BITS { 8'h02 } => Store
    BITS { 8'hFF } => Halt
    __ => Unknown
}

-- Match with don't-care bits (using __ placeholder)
instruction |> WHEN {
    BITS { 0, 0, 0, 0, __, __, __, __ } => TypeA  -- High nibble = 0
    BITS { 0, 0, 0, 1, __, __, __, __ } => TypeB  -- High nibble = 1
    BITS { 1, __, __, __, __, __, __, __ } => TypeC  -- MSB = 1
    __ => Unknown
}

-- Extract fields during match
instruction |> WHEN {
    BITS { 0, 0, op1, op0, rd3, rd2, rd1, rd0 } => BLOCK {
        opcode: BITS { bits: LIST { op0, op1 } }
        rd: BITS { bits: LIST { rd0, rd1, rd2, rd3 } }
        Execute[op: opcode, dest: rd]
    }
    __ => Invalid
}
```

### Matching on BYTES Values

```boon
-- File type detection by magic bytes
file_bytes |> Bytes/take(count: 4) |> WHEN {
    BYTES { 0x89, 0x50, 0x4E, 0x47 } => PngFile
    BYTES { 0xFF, 0xD8, 0xFF, 0xE0 } => JpegFile
    BYTES { 0x47, 0x49, 0x46, 0x38 } => GifFile
    BYTES { 0x25, 0x50, 0x44, 0x46 } => PdfFile
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
        BITS { width: 4, zero }

        rst_event |> WHEN {
            Rising => BITS { width: 4, zero }
            __ => SKIP
        }

        clk_event |> WHEN {
            Rising => count |> Bits/increment()
            __ => SKIP
        }
    }

    [count: count]
}

-- Priority encoder
FUNCTION priority_encoder(input) {
    input |> WHEN {
        BITS { 1, __, __, __ } => [y: BITS { 2'b11 }, valid: True]
        BITS { 0, 1, __, __ } => [y: BITS { 2'b10 }, valid: True]
        BITS { 0, 0, 1, __ } => [y: BITS { 2'b01 }, valid: True]
        BITS { 0, 0, 0, 1 } => [y: BITS { 2'b00 }, valid: True]
        __ => [y: BITS { 2'b00 }, valid: False]
    }
}

-- Linear Feedback Shift Register
FUNCTION lfsr_step(state) {
    -- state is BITS { width: 8 }
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

        first_bits: BITS { width: 8, value: first_byte }
        second_bits: BITS { width: 8, value: second_byte }

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
    pixel_bits: BITS { width: 32, value: pixel }

    r: pixel_bits |> Bits/slice(high: 7, low: 0) |> Bits/to_number()
    g: pixel_bits |> Bits/slice(high: 15, low: 8) |> Bits/to_number()
    b: pixel_bits |> Bits/slice(high: 23, low: 16) |> Bits/to_number()
    a: pixel_bits |> Bits/slice(high: 31, low: 24) |> Bits/to_number()

    new_r: (r * factor) |> Number/min(255) |> Number/floor()
    new_g: (g * factor) |> Number/min(255) |> Number/floor()
    new_b: (b * factor) |> Number/min(255) |> Number/floor()

    BITS { width: 32, zero }
        |> Bits/set_slice(high: 7, low: 0, value: Bits/from_number(value: new_r, width: 8))
        |> Bits/set_slice(high: 15, low: 8, value: Bits/from_number(value: new_g, width: 8))
        |> Bits/set_slice(high: 23, low: 16, value: Bits/from_number(value: new_b, width: 8))
        |> Bits/set_slice(high: 31, low: 24, value: Bits/from_number(value: new_a, width: 8))
}

-- Feature flags for WebGL
render_flags: [
    enable_shadows: True
    enable_reflections: False
    enable_antialiasing: True
    enable_bloom: False
] |> Flags/pack(schema: [
    enable_shadows: 0
    enable_reflections: 1
    enable_antialiasing: 2
    enable_bloom: 3
])
-- Result: BITS { 4'b0101 }
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
        flags: BITS { width: 8, value: flags_byte }

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
            type_tag |> Tag/to_bits(encoding: message_encoding, width: 4)
            Bits/from_number(value: sequence, width: 12)
            Bits/from_number(value: Bytes/length(payload), width: 16)
        })

        header_bytes: header |> Bits/to_bytes()
        Bytes/concat(LIST { header_bytes, payload })
    }
}

-- Hash computation (XOR-based simple hash)
FUNCTION simple_hash(data) {
    data
        |> Bytes/to_bits()
        |> Bits/reduce_xor()
}
```

### Embedded / IoT

```boon
-- GPIO register manipulation
FUNCTION configure_gpio(pin, mode) {
    BLOCK {
        -- Read current config
        config_reg: Memory/read(address: 0x4000_0000)
        config_bits: BITS { width: 32, value: config_reg }

        -- Set mode bits for pin (2 bits per pin)
        pin_offset: pin * 2
        mode_bits: mode |> WHEN {
            Input => BITS { 2'b00 }
            Output => BITS { 2'b01 }
            Alternate => BITS { 2'b10 }
            Analog => BITS { 2'b11 }
        }

        new_config: config_bits
            |> Bits/set_slice(
                high: pin_offset + 1
                low: pin_offset
                value: mode_bits
            )

        -- Write back
        Memory/write(address: 0x4000_0000, value: new_config |> Bits/to_number())
    }
}

-- SPI data transfer
FUNCTION spi_transfer(tx_byte) {
    tx_bits: BITS { width: 8, value: tx_byte }

    -- Shift out MSB first, shift in response
    List/range(start: 7, end: -1) |> List/fold(
        init: [tx: tx_bits, rx: BITS { width: 8, zero }]
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
i2c_address: BITS { width: 8, zero }
    |> Bits/set_slice(high: 7, low: 1, value: Bits/from_number(value: device_addr, width: 7))
    |> Bits/set(index: 0, value: read_mode)
```

### 3D Graphics

```boon
-- Vertex attribute packing (10-10-10-2 normal format)
FUNCTION pack_normal(nx, ny, nz) {
    -- Convert -1..1 floats to 10-bit signed integers
    scale: 511  -- (2^9 - 1)

    nx_bits: Bits/from_number(value: (nx * scale) |> Number/round(), width: 10)
    ny_bits: Bits/from_number(value: (ny * scale) |> Number/round(), width: 10)
    nz_bits: Bits/from_number(value: (nz * scale) |> Number/round(), width: 10)
    w_bits: BITS { 2'b00 }  -- Padding

    Bits/concat(LIST { nx_bits, ny_bits, nz_bits, w_bits })
}

-- Color conversion (float RGBA to packed u32)
FUNCTION pack_color(r, g, b, a) {
    BITS { width: 32, zero }
        |> Bits/set_slice(high: 7, low: 0, value: Bits/from_number(value: (r * 255) |> Number/floor(), width: 8))
        |> Bits/set_slice(high: 15, low: 8, value: Bits/from_number(value: (g * 255) |> Number/floor(), width: 8))
        |> Bits/set_slice(high: 23, low: 16, value: Bits/from_number(value: (b * 255) |> Number/floor(), width: 8))
        |> Bits/set_slice(high: 31, low: 24, value: Bits/from_number(value: (a * 255) |> Number/floor(), width: 8))
}

-- Texture format detection
FUNCTION detect_texture_format(header_bytes) {
    header_bytes |> Bytes/take(count: 4) |> WHEN {
        BYTES { 0x44, 0x44, 0x53, 0x20 } => DDS
        BYTES { 0xAB, 0x4B, 0x54, 0x58 } => KTX
        BYTES { 0x89, 0x50, 0x4E, 0x47 } => PNG
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
] |> Flags/pack(schema: [
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
- `BITS { binary_digits }` - From binary literal
- `BITS { width: N, value: V }` - Explicit width and value
- `BITS { hex_value }` - From hex (0x prefix)
- `BITS { N'bXXX }` - Verilog-style binary
- `BITS { N'dXXX }` - Verilog-style decimal
- `BITS { N'hXXX }` - Verilog-style hex
- `BITS { bits: LIST { bools } }` - From list of bools
- `BITS { width: N, zero }` - All zeros
- `BITS { width: N, ones }` - All ones
- `Bits/from_number(value: V, width: N)` - From number

#### Properties
- `Bits/width(bits)` - Get width
- `Bits/to_number(bits)` - To unsigned number
- `Bits/to_number_signed(bits)` - To signed number (two's complement)
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

### Why Verilog-Style Literals?

1. **Familiar to hardware engineers** - `8'hFF` is standard
2. **Explicit width** - No ambiguity
3. **Multiple bases** - Binary, decimal, hex in one syntax
4. **Transpiler friendly** - Direct mapping to SystemVerilog

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
a: BITS { width: 8, value: 200 }
b: BITS { width: 8, value: 100 }
sum: Bits/add(a, b)  -- Result: 44 (300 mod 256)

-- ✅ CORRECT: Use wider result
a: BITS { width: 8, value: 200 }
b: BITS { width: 8, value: 100 }
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
bits: BITS { 8'b10110010 }
--          ^      ^
--        bit 7   bit 0

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
result: Bits/add(bits, Bits/from_number(value: 1, width: Bits/width(bits)))
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
GPIO_CONTROL: BITS/REGISTER { address: 0x4000_0000, width: 32 }
GPIO_CONTROL |> Bits/set(index: 0, value: True)  -- Writes to hardware
```

---

**Last Updated:** 2025-11-17
**Status:** Design Specification
**Related:** TEXT_SYNTAX.md, BOON_SYNTAX.md, ERROR_HANDLING.md
