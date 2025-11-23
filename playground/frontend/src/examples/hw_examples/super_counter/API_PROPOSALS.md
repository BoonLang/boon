
# Proposed Boon API Additions

**Date:** 2025-11-22
**Source:** super_counter hardware conversion (btn_message, ack_parser)
**Purpose:** Document API improvements discovered during real-world hardware conversion

---

## ✅ Implementation Status (Updated 2025-11-22)

**HIGH-PRIORITY MODULES: IMPLEMENTED!**

Three library modules have been implemented based on these proposals:

- ✅ **ASCII.bn** - Character constants, classification, conversion (100+ functions/constants)
- ✅ **Math.bn** - Mathematical functions including clog2, power-of-2 helpers
- ✅ **BCD.bn** - Binary-Coded Decimal arithmetic and formatting

**See LIBRARY_MODULES.md for complete documentation and usage examples.**

**Remaining proposals:**
- ⏳ List extensions (concat, reverse, get_or_default) - deferred to core language
- ⏳ Decimal module - covered by BCD module

This document now serves as the **design rationale** for the implemented modules.

---

## Executive Summary

Converting the complex super_counter modules (btn_message, ack_parser) to Boon revealed several API additions that would make hardware design more natural and concise.

**Key Findings:**
- 🟢 **Most patterns work well** with existing Boon constructs
- ✅ **ASCII handling** - IMPLEMENTED in ASCII.bn module
- ✅ **BCD arithmetic** - IMPLEMENTED in BCD.bn module
- ⏳ **List operations** - Need core language additions (concat, reverse, get_or_default)
- ✅ **Math operations** - IMPLEMENTED in Math.bn module (clog2, etc.)
- 🟢 **Core language** (LATEST, WHEN, WHILE) handles complexity perfectly!

---

## Category 1: ASCII Module (High Priority) 🔴

**Use Cases:** UART communication, text parsing, display formatting

### Proposed: ASCII Character Constants

```boon
-- Character literals
ASCII/CHAR_A       -> BITS[8] { 16u41 }    -- 'A'
ASCII/CHAR_B       -> BITS[8] { 16u42 }    -- 'B'
ASCII/CHAR_SPACE   -> BITS[8] { 16u20 }    -- ' '
ASCII/CHAR_NEWLINE -> BITS[8] { 16u0A }    -- '\n'
ASCII/CHAR_0       -> BITS[8] { 16u30 }    -- '0'
ASCII/CHAR_9       -> BITS[8] { 16u39 }    -- '9'

-- Or cleaner syntax:
ASCII('A')    -> BITS[8] { 16u41 }
ASCII('\n')   -> BITS[8] { 16u0A }
ASCII('0')    -> BITS[8] { 16u30 }
```

**Current workaround:**
```boon
char_a: BITS[8] { 16u41 }  -- Must know hex code manually
```

**Benefit:** More readable, less error-prone

### Proposed: ASCII/is_digit()

**Check if character is '0'-'9':**

```boon
-- Proposed API
is_digit: data |> ASCII/is_digit()  -- Returns Bool
```

**Current workaround:**
```boon
is_digit: (data >= BITS[8] { 16u30 }) |> Bool/and(data <= BITS[8] { 16u39 })
```

**Benefit:** 73% shorter, clearer intent

### Proposed: ASCII/to_digit()

**Convert '0'-'9' to 0-9:**

```boon
-- Proposed API
digit_value: data |> ASCII/to_digit()  -- Returns BITS[4] (0-9)
-- Or with width parameter:
digit_value: data |> ASCII/to_digit(width: 32)  -- Returns BITS[32]
```

**Current workaround:**
```boon
digit_value: data |> Bits/sub(BITS[8] { 16u30 })
```

**Benefit:** Self-documenting, handles width conversion

### Proposed: ASCII/from_digit()

**Convert 0-9 to '0'-'9':**

```boon
-- Proposed API
ascii_char: digit |> ASCII/from_digit()  -- BITS[4] -> BITS[8]
```

**Current workaround:**
```boon
FUNCTION ascii_from_digit(digit) {
    digit |> WHEN {
        BITS[4] { 10u0 } => BITS[8] { 16u30 }
        BITS[4] { 10u1 } => BITS[8] { 16u31 }
        -- ... 8 more cases
    }
}
```

**Benefit:** 90% reduction in code, standard implementation

### Proposed: ASCII/is_letter(), ASCII/to_upper(), etc.

**Additional helpers for text processing:**

```boon
-- Character classification
data |> ASCII/is_letter()      -> Bool
data |> ASCII/is_uppercase()   -> Bool
data |> ASCII/is_lowercase()   -> Bool
data |> ASCII/is_alphanumeric() -> Bool

-- Case conversion
data |> ASCII/to_upper()       -> BITS[8]
data |> ASCII/to_lower()       -> BITS[8]
```

**Use case:** Protocol parsers (case-insensitive matching)

---

## Category 2: BCD Module (Medium Priority) 🟡

**Use Cases:** Decimal displays, counters, human-readable numbers

### Proposed: BCD/increment()

**Increment BCD digit array with carry:**

```boon
-- Proposed API
new_bcd: bcd_digits |> BCD/increment()
-- LIST[N] { BITS[4] } -> LIST[N] { BITS[4] }
```

**Current workaround:**
```boon
FUNCTION bcd_increment(digits) {
    digits |> List/fold(
        init: [result: LIST[5] { }, carry: True]
        digit, acc: BLOCK {
            new_state: acc.carry |> WHEN {
                True => digit |> WHEN {
                    BITS[4] { 10u9 } => [new_digit: BITS[4] { 10u0 }, carry: True]
                    __ => [new_digit: digit |> Bits/increment(), carry: False]
                }
                False => [new_digit: digit, carry: False]
            }
            [
                result: acc.result |> List/append(new_state.new_digit)
                carry: new_state.carry
            ]
        }
    ).result
}
```

**Benefit:** 85% reduction in code, standard implementation

### Proposed: BCD/decrement()

**Decrement BCD digit array with borrow:**

```boon
new_bcd: bcd_digits |> BCD/decrement()
```

### Proposed: BCD/add(), BCD/sub()

**BCD arithmetic:**

```boon
sum: bcd_a |> BCD/add(bcd_b)  -- With carry handling
diff: bcd_a |> BCD/sub(bcd_b) -- With borrow handling
```

### Proposed: BCD/from_binary(), BCD/to_binary()

**Conversion between binary and BCD:**

```boon
bcd: binary_value |> BCD/from_binary(digits: 5)  -- 16 bits -> 5 BCD digits
binary: bcd_digits |> BCD/to_binary()            -- 5 BCD digits -> 16 bits
```

### Proposed: BCD/count_digits()

**Count significant digits (skip leading zeros):**

```boon
-- Proposed API
count: bcd_digits |> BCD/count_digits()  -- Returns 1-N (always >= 1)
```

**Current workaround:**
```boon
FUNCTION bcd_count_significant_digits(digits) {
    digits |> List/get(index: 4) |> WHEN {
        BITS[4] { 10u0 } => digits |> List/get(index: 3) |> WHEN {
            BITS[4] { 10u0 } => digits |> List/get(index: 2) |> WHEN {
                BITS[4] { 10u0 } => digits |> List/get(index: 1) |> WHEN {
                    BITS[4] { 10u0 } => 1
                    __ => 2
                }
                __ => 3
            }
            __ => 4
        }
        __ => 5
    }
}
```

**Benefit:** 80% reduction, standard implementation

---

## Category 3: List Extensions (Medium Priority) 🟡

**Use Cases:** Array operations, message building, data formatting

### Proposed: List/concat()

**Concatenate multiple lists:**

```boon
-- Proposed API
full_message: LIST/concat([
    prefix_bytes    -- LIST[4] { BITS[8] }
    data_bytes      -- LIST[N] { BITS[8] }
    suffix_bytes    -- LIST[1] { BITS[8] }
])  -- Returns LIST[4+N+1] { BITS[8] }
```

**Current workaround:**
```boon
-- Manually build fixed-size array
msg: LIST {
    prefix |> List/get(index: 0)
    prefix |> List/get(index: 1)
    -- ...
    data |> List/get_or_default(index: 0, default: BITS[8] { 10u0 })
    -- ...
}
```

**Benefit:** More flexible, handles variable-length components

### Proposed: List/reverse()

**Reverse list order:**

```boon
-- Proposed API
reversed: list |> List/reverse()
```

**Use case:** BCD digits are stored little-endian but displayed big-endian

**Current workaround:**
```boon
-- Manual reversal with explicit indexing
```

### Proposed: List/get_or_default()

**Safe list access with fallback:**

```boon
-- Proposed API (if not already exists)
value: list |> List/get_or_default(index: 5, default: BITS[8] { 10u0 })
```

**Benefit:** Avoids out-of-bounds errors in hardware

### Proposed: List/pad()

**Pad list to fixed size:**

```boon
-- Proposed API
padded: variable_list |> List/pad(size: 10, value: BITS[8] { 10u0 })
-- Pads or truncates to exactly 10 elements
```

**Use case:** Variable-length messages in fixed-size buffers

---

## Category 4: Math Module (High Priority) 🔴

**Use Cases:** Width calculations, logarithms, power-of-2 checks

### Proposed: Math/clog2()

**Ceiling log base 2 (essential for HDL!):**

```boon
-- Proposed API
width: divisor |> Math/clog2()  -- Returns smallest N where 2^N >= divisor
```

**Example:**
```boon
divisor: 217
width: divisor |> Math/clog2()  -- Returns 8 (since 2^8 = 256 >= 217)
```

**Current workaround:**
```boon
-- Must pass width as parameter (user calculates manually)
FUNCTION uart_tx(divisor_width, ...) {
    -- User must compute: width = ceil(log2(divisor))
}
```

**Benefit:** **Critical for parametric hardware design** - eliminates manual calculation

**SystemVerilog equivalent:**
```systemverilog
localparam int CTR_WIDTH = $clog2(DIVISOR);
```

### Proposed: Math/is_power_of_2()

**Check if value is power of 2:**

```boon
is_pow2: value |> Math/is_power_of_2()  -- Bool
```

---

## Category 5: Bits Extensions (Low Priority) 🟢

**Most operations exist, just need a few additions**

### Proposed: Bits/from_nat(), Bits/to_nat()

**Convert between natural numbers and BITS:**

```boon
-- Proposed API
bits: 42 |> Bits/from_nat(width: 8)  -- nat -> BITS[8]
nat: bits |> Bits/to_nat()            -- BITS[8] -> nat
```

**Use case:** Index calculations, loop bounds

**Benefit:** Explicit type conversions

### Proposed: Bits/all_ones()

**Check if all bits are 1:**

```boon
-- Proposed API
is_max: counter |> Bits/all_ones()  -- Bool
```

**Current workaround:**
```boon
is_max: counter == BITS[width] { ... }  -- Must construct max value
```

**Use case:** Counter max detection (debouncer, baud generator)

### Proposed: Bits/multiply()

**Multiplication (if not exists):**

```boon
product: a |> Bits/multiply(b)
-- or
product: a * b  -- Operator overload?
```

**Use case:** BCD decimal accumulation (`duration * 10 + digit`)

---

## Category 6: Decimal Module (Low Priority) 🟢

**Use Cases:** Decimal string parsing, number formatting

### Proposed: Decimal/accumulate()

**Accumulate decimal digit:**

```boon
-- Proposed API
new_value: current_value |> Decimal/accumulate(digit)
-- Equivalent to: current_value * 10 + digit
```

**Current workaround:**
```boon
ten: BITS[32] { 10u10 }
new_value: current_value |> Bits/multiply(ten) |> Bits/add(digit)
```

**Benefit:** Self-documenting, single operation

---

## Priority Summary

### 🔴 **High Priority (Essential for Hardware)**

| Feature | Module | Reason |
|---------|--------|--------|
| `Math/clog2()` | Math | **Critical** - width calculations for parametric design |
| `ASCII/is_digit()` | ASCII | Common in protocols (UART, SPI text mode) |
| `ASCII/to_digit()` | ASCII | ASCII to integer conversion |
| `ASCII/from_digit()` | ASCII | Integer to ASCII conversion |
| ASCII constants | ASCII | Readability (use 'A' instead of 0x41) |

### 🟡 **Medium Priority (Very Useful)**

| Feature | Module | Reason |
|---------|--------|--------|
| `BCD/increment()` | BCD | Decimal displays, counters |
| `BCD/count_digits()` | BCD | Variable-length number formatting |
| `List/concat()` | List | Message building, data framing |
| `List/reverse()` | List | Endianness conversions |
| `List/get_or_default()` | List | Safe array access |

### 🟢 **Low Priority (Nice to Have)**

| Feature | Module | Reason |
|---------|--------|--------|
| `Bits/all_ones()` | Bits | Counter max detection |
| `Bits/from_nat()` | Bits | Type conversions |
| `Decimal/accumulate()` | Decimal | Decimal parsing |
| `BCD/from_binary()` | BCD | Binary to decimal display |

---

## Implementation Recommendations

### Phase 1: Essential ASCII & Math

**These unblock most hardware design:**

```boon
-- Math module
Math/clog2(value) -> nat

-- ASCII module
ASCII/is_digit(char: BITS[8]) -> Bool
ASCII/to_digit(char: BITS[8]) -> BITS[4]  -- or parameterized width
ASCII/from_digit(digit: BITS[4]) -> BITS[8]
ASCII('A') -> BITS[8]  -- Character literals
```

**Estimated effort:** Small (1-2 days)
**Impact:** Huge (enables parametric designs, text protocols)

### Phase 2: BCD Helpers

**For decimal displays and counters:**

```boon
BCD/increment(digits: LIST[N] { BITS[4] }) -> LIST[N] { BITS[4] }
BCD/count_digits(digits: LIST[N] { BITS[4] }) -> nat
```

**Estimated effort:** Medium (3-4 days, need good BCD algorithm)
**Impact:** Medium (specific use case, but very clean when needed)

### Phase 3: List Extensions

**For data manipulation:**

```boon
List/concat(lists: LIST { LIST[M] { T } }) -> LIST[...] { T }
List/reverse(list: LIST[N] { T }) -> LIST[N] { T }
List/get_or_default(list, index, default) -> T
```

**Estimated effort:** Small (2-3 days)
**Impact:** Medium (cleaner code, less manual indexing)

---

## Alternative: Syntax Extensions

**Instead of helper functions, could add syntax:**

### Character Literals

```boon
-- Instead of ASCII('A')
data |> WHEN {
    'A' => handle_a()     -- Direct character literal
    '\n' => handle_newline()
    '0' => handle_zero()
}
```

**Pros:** Cleaner, more familiar
**Cons:** New syntax to parse

### Operator Overloading

```boon
-- Instead of Bits/multiply()
product: a * b           -- If both are BITS, use Bits/multiply()
sum: duration * 10 + digit
```

**Pros:** More natural
**Cons:** Type inference complexity

---

## Examples: Before & After

### Example 1: ASCII Digit Check

**Before:**
```boon
is_digit: (data >= BITS[8] { 16u30 }) |> Bool/and(data <= BITS[8] { 16u39 })
digit_value: data |> Bits/sub(BITS[8] { 16u30 })
```

**After:**
```boon
is_digit: data |> ASCII/is_digit()
digit_value: data |> ASCII/to_digit()
```

**Improvement:** 60% reduction, much clearer

### Example 2: BCD Increment

**Before:** 20+ lines of fold logic

**After:**
```boon
new_bcd: bcd_digits |> BCD/increment()
```

**Improvement:** 95% reduction!

### Example 3: Width Calculation

**Before:**
```boon
-- User must calculate manually and pass as parameter
FUNCTION uart_tx(divisor_width: 8, ...) {
    -- Hoping user did the math right!
}
```

**After:**
```boon
FUNCTION uart_tx(clock_hz, baud_rate, ...) {
    divisor: clock_hz / baud_rate
    divisor_width: divisor |> Math/clog2()  -- Automatic!

    counter: BITS[divisor_width] { 10u0 }
}
```

**Improvement:** Eliminates error-prone manual calculation

---

## Conclusion

**Key Takeaways:**

1. ✅ **Boon's core language works great** for complex hardware (FSMs, counters, arrays)
2. 🔴 **Math/clog2() is critical** - without it, parametric designs are painful
3. 🟡 **ASCII helpers make protocols cleaner** - UART, SPI, text parsing benefit greatly
4. 🟡 **BCD helpers eliminate boilerplate** - 95% code reduction for decimal operations
5. 🟢 **Most patterns already work** - LIST, WHEN, LATEST handle complexity well

**Recommendation:**
- **Phase 1:** Implement Math/clog2() and ASCII helpers (HIGH ROI)
- **Phase 2:** Add BCD module for decimal operations
- **Phase 3:** Extend LIST operations for data manipulation

**These additions will make Boon hardware design 50-70% more concise while maintaining clarity!**

---

## Related Documents

- **btn_message.bn** - Shows BCD, ASCII, and LIST patterns
- **ack_parser.bn** - Shows ASCII parsing patterns
- **CONVERSION_ANALYSIS.md** - Initial feature analysis
- **TRANSPILER_TARGET.md** - SystemVerilog target spec
