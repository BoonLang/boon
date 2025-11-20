# Text Encoding in Boon

**Date**: 2025-11-20
**Status**: Design Specification

---

## Executive Summary

**Encoding design:**
- **Source files**: UTF-8 required (modern standard)
- **Identifiers**: ASCII-only (a-z, A-Z, 0-9, _)
- **TEXT type**: Unicode text (internally UTF-8)
- **Default encoding**: UTF-8 everywhere (software and hardware)
- **ASCII encoding**: Explicit when needed via `Text/to_bytes(encoding: Ascii)`

**Key benefits:**
- ✅ Consistent across all domains
- ✅ International text works out of the box
- ✅ Zero overhead for ASCII text
- ✅ Negligible performance cost (<0.01%)

---

## Source Files: UTF-8 Required

All Boon source files MUST be UTF-8 encoded without BOM (Byte Order Mark).

**Why UTF-8:**
- Modern standard (Rust, Go, Python 3)
- Enables comments in any language
- Supports Unicode in string literals
- International development teams

**Editor setup:**
- Save files as UTF-8 without BOM
- Most modern editors default to this

---

## Identifiers: ASCII-Only

Identifiers (variable names, function names, module names) MUST use ASCII characters only.

**Allowed characters:**
- Lowercase: `a-z`
- Uppercase: `A-Z` (for PascalCase tags)
- Digits: `0-9` (not at start)
- Underscore: `_`

**Why ASCII-only:**
1. **Tool compatibility** - Works with all editors, terminals, compilers
2. **No confusion** - Avoids lookalike characters (Cyrillic 'а' vs Latin 'a')
3. **Simplicity** - No Unicode normalization issues
4. **Industry standard** - Most systems languages restrict identifiers to ASCII

**Examples:**

```boon
-- ✅ Correct
user_name: TEXT { 张三 }        -- ASCII identifier, Unicode content
temperature_celsius: -15        -- ASCII identifier
message: TEXT { Привет мир }    -- ASCII identifier, Cyrillic content

-- ❌ Incorrect
用户名: TEXT { ... }            -- ERROR: Non-ASCII identifier
température: -15                -- ERROR: é is not ASCII
сообщение: TEXT { ... }         -- ERROR: Cyrillic identifier
```

---

## TEXT Type: Unicode

TEXT represents Unicode text - a sequence of Unicode code points.

**Internal representation:**
- UTF-8 bytes (implementation detail)
- Opaque to users (you work with characters, not bytes)

**TEXT can contain any Unicode:**

```boon
-- Any language works
english: TEXT { Hello World }
czech: TEXT { Dobrý den }
chinese: TEXT { 你好世界 }
arabic: TEXT { مرحبا }
emoji: TEXT { Status: ✓ }
mixed: TEXT { Price: €50, Temp: -5°C }
```

---

## Encoding: UTF-8 Default

**UTF-8 is the default encoding everywhere** (software and hardware).

### Why UTF-8 Default?

1. **Consistent** - Same encoding in all contexts
2. **Universal** - Supports all Unicode characters
3. **Modern** - Industry standard
4. **ASCII-compatible** - ASCII text encodes identically (zero overhead)
5. **Compile-time in hardware** - Variable-width resolved at compile-time

### Software Context

TEXT is a runtime type with full UTF-8 support:

```boon
-- Dynamic text operations
FUNCTION render_greeting(user_name) {
    greeting: TEXT { Hello {user_name}! }  -- Runtime interpolation
    bytes: greeting |> Text/to_bytes(encoding: Utf8)
    bytes
}
```

### Hardware Context

TEXT must be compile-time constant and auto-converts to BYTES (UTF-8):

```boon
-- UART debug (clean syntax!)
FUNCTION uart_hello() {
    BLOCK {
        msg: TEXT { Hello World }  -- Compile-time constant

        -- Auto-converts to UTF-8 BYTES here
        msg |> Bytes/for_each(byte: byte => uart_tx(byte))
    }
}

-- Non-ASCII works too
FUNCTION uart_czech() {
    BLOCK {
        msg: TEXT { Dobrý den }  -- Compile-time constant

        -- Auto-converts to UTF-8 (some multi-byte chars)
        msg |> Bytes/for_each(byte: byte => uart_tx(byte))

        -- "Dobr" = 4 bytes (ASCII subset)
        -- "ý" = 2 bytes (C3 BD in UTF-8)
        -- " den" = 4 bytes (ASCII subset)
        -- Total: 10 bytes (known at compile-time)
    }
}
```

**Hardware rules:**
1. ✅ TEXT must be compile-time constant
2. ✅ Auto-converts to UTF-8 BYTES at boundaries
3. ✅ Compile-time interpolation allowed (with constants)
4. ❌ Runtime text operations not allowed (compile error)

---

## When to Use ASCII Encoding

ASCII encoding is **explicit** and used when:
- Legacy protocols require ASCII-only
- Need 1-byte-per-character guarantee
- Validation (ensure no non-ASCII characters)
- Old embedded systems/terminals

**ASCII validation:**

```boon
-- Validate before sending
command: TEXT { AT+RESET }
bytes: command |> Text/to_bytes(encoding: Ascii)  -- Validates all chars < 128

-- Check first
user_input
    |> Text/is_ascii()
    |> WHEN {
        True => user_input |> Text/to_bytes(encoding: Ascii)
        False => Error("Protocol requires ASCII-only")
    }
```

**ASCII is subset of UTF-8:**
- ASCII text encodes identically in UTF-8
- No overhead for ASCII-only text

---

## Size and Performance

### Hardware Size

**ASCII text (common case):**
```boon
TEXT { INIT }    -- UTF-8: 4 bytes, ASCII: 4 bytes (0% overhead)
TEXT { ERROR }   -- UTF-8: 5 bytes, ASCII: 5 bytes (0% overhead)
```

**Non-ASCII text:**
```boon
TEXT { Dobrý den }  -- UTF-8: 10 bytes
// ASCII: Impossible (ý not in ASCII)

TEXT { Status: ✓ }  -- UTF-8: 10 bytes (✓ = 3 bytes)
// ASCII: Impossible (✓ not in ASCII)
```

**Conclusion:** For ASCII text (common in hardware debug), zero overhead.

### Software Performance

**UTF-8 → UTF-8 (default):**
- Zero transcoding (TEXT is UTF-8 internally)
- Just copy or reference internal buffer
- **Cost: Negligible**

**UTF-8 → ASCII (validation):**
- Scan bytes, check all < 128
- ~1 nanosecond per byte on modern CPUs
- **Cost: <0.01% vs I/O latency**

**Example:** 100-byte HTTP header
- Validation: ~100 nanoseconds
- Network I/O: ~1-10 milliseconds
- **Overhead: Completely negligible**

---

## Compatibility Patterns

### Pattern 1: Validate ASCII Before Protocol

```boon
FUNCTION send_at_command(command) {
    command
        |> Text/is_ascii()
        |> WHEN {
            True => command
                |> Text/to_bytes(encoding: Ascii)
                |> serial_write()
            False => Error("AT commands must be ASCII")
        }
}
```

### Pattern 2: Length-Prefixed Protocols

```boon
-- ✅ Correct: Use byte length
message: TEXT { Hello }
bytes: message |> Text/to_bytes(encoding: Utf8)
length: bytes |> Bytes/length()  -- Byte count, not character count

send_byte(length)
send_bytes(bytes)

-- ❌ Wrong: Character length
char_length: message |> Text/length()  -- 5 characters
// But UTF-8 bytes might differ (if non-ASCII)
```

### Pattern 3: Legacy Terminal Fallback

```boon
msg: terminal_supports_utf8 |> WHEN {
    True => TEXT { Status: ✓ }
    False => TEXT { Status: OK }
}
```

### Pattern 4: Lossy ASCII Conversion

```boon
-- Replace non-ASCII with '?'
user_input |> Text/to_ascii_lossy()
// TEXT { Café™ } → BYTES { "Caf??" }
```

---

## Text Module API

```boon
-- Core conversions
Text/to_bytes(text: Text, encoding: Utf8|Ascii) -> BYTES

-- ASCII utilities
Text/is_ascii(text: Text) -> Bool
Text/to_ascii_lossy(text: Text) -> BYTES

-- Examples
TEXT { Hello } |> Text/is_ascii()         -- True
TEXT { Hello 世界 } |> Text/is_ascii()    -- False
TEXT { Café™ } |> Text/to_ascii_lossy()   -- "Caf??" (é, ™ → ?)
```

---

## Summary

| Aspect | Design | Rationale |
|--------|--------|-----------|
| **Source files** | UTF-8 required | Modern standard, international support |
| **Identifiers** | ASCII-only | Compatibility, no confusion |
| **TEXT type** | Unicode (UTF-8 internal) | Universal, modern |
| **Default encoding** | UTF-8 everywhere | Consistent, supports all languages |
| **ASCII encoding** | Explicit when needed | Legacy protocols, validation |
| **Hardware TEXT** | Compile-time → BYTES | Zero runtime overhead |
| **Performance** | <0.01% overhead | Validation negligible vs I/O |
| **Size overhead** | 0% for ASCII text | UTF-8 = ASCII for chars 0-127 |

**This design gives Boon:**
- ✅ Modern UTF-8 support everywhere
- ✅ Zero overhead for common case (ASCII)
- ✅ Explicit when encoding matters
- ✅ Works across all domains (FPGA, Web, Server, Embedded, 3D)
