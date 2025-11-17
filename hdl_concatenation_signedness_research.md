# HDL Concatenation Signedness Research

Comprehensive analysis of how major hardware description languages handle signedness when concatenating bit vectors.

## Table of Contents
1. [SystemVerilog](#systemverilog)
2. [VHDL](#vhdl)
3. [Chisel](#chisel)
4. [Rust (for comparison)](#rust)
5. [Summary and Rationale](#summary-and-rationale)

---

## SystemVerilog

### Concatenation Operator Behavior

**Operator**: `{a, b, c}`

**Key Rule**: **Concatenation results are ALWAYS unsigned**, regardless of operand signedness.

This is explicitly specified in IEEE 1800 (SystemVerilog Language Reference Manual), Section 11.4.12 on concatenation operators.

### Mixed Signedness

When concatenating signed and unsigned operands:
- The operation succeeds without error
- All operands are treated as bit patterns
- The result is **always unsigned**
- Result width = sum of all operand widths

### Code Examples

```systemverilog
// Example 1: Concatenating signed values produces unsigned result
logic signed [3:0] a = 4'sb1111;  // -1 in signed representation
logic signed [3:0] b = 4'sb0001;  // +1 in signed representation
logic [7:0] result = {a, b};       // result is unsigned 8'b11110001

// Example 2: Mixed signed/unsigned concatenation
logic signed [31:0] s1;
logic [15:0] u1;
logic [47:0] concat_result = {s1, u1};  // Always unsigned 48-bit result

// Example 3: Using signed' cast to restore signedness
logic signed [31:0] a;
logic [15:0] b;
logic signed [47:0] c;

if (c < signed'{a, b}) ...  // Cast concatenation result to signed
```

### Impact on Subsequent Operations

```systemverilog
logic signed [31:0] s1, s2;
logic [31:0] u1;
logic [63:0] result;

// The concatenation is unsigned, making the entire operation unsigned
result = {s1, s2};  // Result is unsigned, even though both operands are signed

// If used in arithmetic, the unsigned nature propagates
assign diff = {s1, s2} - 64'd100;  // Unsigned subtraction
```

### Explicit Conversion

To use concatenated values as signed:

```systemverilog
// Method 1: Using signed' cast operator
logic signed [47:0] result = signed'{a, b};

// Method 2: Using $signed() system function (Verilog-2001 compatible)
logic signed [47:0] result = $signed({a, b});
```

### Design Rationale

1. **Bit-level operation**: Concatenation is fundamentally a structural operation that joins bits together without arithmetic interpretation
2. **Sign bit ambiguity**: When concatenating two signed values, which bit represents the sign? The semantics become unclear
3. **Predictable behavior**: Making all concatenations unsigned ensures consistent, tool-independent behavior
4. **Hardware reality**: At the gate level, concatenation is just wire arrangement - signedness is a software interpretation layer

---

## VHDL

### Concatenation Operator Behavior

**Operator**: `&` (ampersand)

**Key Rule**: **Strong typing - operands must be of compatible types**. The result type matches the operand type.

VHDL is strongly typed and requires explicit type handling. The `numeric_std` package (IEEE 1076.3, now part of IEEE 1076 base standard as of VHDL-2008) defines concatenation for signed and unsigned types.

### Type System

The `numeric_std` package defines:
- `unsigned`: array of `std_logic` representing unsigned numbers
- `signed`: array of `std_logic` representing signed numbers (two's complement)
- Both are distinct types that cannot be mixed without explicit conversion

### Concatenation Operator Overloads

The `&` operator is overloaded for different type combinations:

**For unsigned types:**
```vhdl
function "&"(L: unsigned; R: unsigned) return unsigned;
function "&"(L: unsigned; R: std_logic) return unsigned;
function "&"(L: std_logic; R: unsigned) return unsigned;
```

**For signed types:**
```vhdl
function "&"(L: signed; R: signed) return signed;
function "&"(L: signed; R: std_logic) return signed;
function "&"(L: std_logic; R: signed) return signed;
```

**Key observation**: The result type **preserves the signedness** of the operand type.

### Mixed Signedness

**VHDL does NOT allow mixing signed and unsigned types** in concatenation without explicit conversion.

Attempting to concatenate signed and unsigned values produces a compilation error:
```
Error: type mismatch in concatenation - cannot concatenate 'signed' with 'unsigned'
```

### Code Examples

```vhdl
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

-- Example 1: Concatenating unsigned values
signal a_uns : unsigned(3 downto 0) := "1010";
signal b_uns : unsigned(3 downto 0) := "0101";
signal result_uns : unsigned(7 downto 0);

result_uns <= a_uns & b_uns;  -- OK: Result is unsigned(7 downto 0)

-- Example 2: Concatenating signed values
signal a_sig : signed(3 downto 0) := "1010";  -- -6 in signed
signal b_sig : signed(3 downto 0) := "0101";  -- +5 in signed
signal result_sig : signed(7 downto 0);

result_sig <= a_sig & b_sig;  -- OK: Result is signed(7 downto 0)

-- Example 3: Mixing signed and unsigned - COMPILATION ERROR
signal mixed : unsigned(7 downto 0);
-- mixed <= a_uns & b_sig;  -- ERROR: Type mismatch!
```

### Type Conversion Methods

To concatenate mixed signedness, explicit conversion is required:

**Method 1: Convert to common type**
```vhdl
-- Convert signed to unsigned
signal result : unsigned(7 downto 0);
result <= a_uns & unsigned(b_sig);

-- Convert unsigned to signed
signal result2 : signed(7 downto 0);
result2 <= signed(a_uns) & b_sig;
```

**Method 2: Convert both to std_logic_vector**
```vhdl
signal result : unsigned(7 downto 0);
result <= unsigned(std_logic_vector(a_sig) & std_logic_vector(b_uns));
```

**Method 3: Type qualification for std_logic concatenation**
```vhdl
-- Concatenating individual std_logic bits into unsigned
signal a, b, c : std_logic;
signal decoded : unsigned(2 downto 0);

-- Direct assignment (type inference)
decoded <= a & b & c;

-- With explicit type qualification
decoded <= unsigned'(a & b & c);
```

### Design Rationale

1. **Type safety**: VHDL's strong typing prevents accidental mixing of signed/unsigned, catching potential bugs at compile time
2. **Explicit intent**: Forcing explicit conversion makes the designer's intent clear
3. **Preserves semantics**: Result type matches operand type, so signed concatenation produces signed results
4. **Hardware synthesis**: Clear type information helps synthesis tools generate correct hardware

---

## Chisel

### Concatenation Operator Behavior

**Operator**: `Cat(a, b, ...)` function from `chisel3.util` package

**Key Rule**: **Returns UInt (unsigned) always**. Operands must be same base type (`Bits` or subtypes).

### Type Signature

```scala
def apply[T <: Bits](a: T, r: T*): UInt
def apply[T <: Bits](r: Seq[T]): UInt
```

**Return type**: Always `UInt` (unsigned integer)

**Type parameter**: `T <: Bits` - all arguments must be subtypes of `Bits` (which includes `UInt`, `SInt`, and raw `Bits`)

### Type Hierarchy

```
Bits (abstract)
├── UInt (unsigned)
└── SInt (signed)
```

### Mixed Signedness

Chisel's type system appears to allow `Cat` with mixed `UInt` and `SInt` operands since both extend `Bits`, but this is **not the common practice** and may produce type errors in some contexts.

The safer approach is to **convert all operands to the same type** before concatenation.

### Code Examples

```scala
import chisel3._
import chisel3.util._

// Example 1: Concatenating UInt values
val a = 5.U(4.W)        // 4-bit UInt
val b = 3.U(4.W)        // 4-bit UInt
val result = Cat(a, b)   // Returns 8-bit UInt (0b01010011)

// Example 2: Concatenating SInt values - requires conversion
val s1 = (-1).S(4.W)    // 4-bit SInt (0b1111)
val s2 = 1.S(4.W)       // 4-bit SInt (0b0001)
// val bad = Cat(s1, s2) // May cause type issues

// Convert to UInt first
val result2 = Cat(s1.asUInt, s2.asUInt)  // Returns 8-bit UInt (0b11110001)

// Example 3: Mixed UInt/SInt - must convert
val u = 15.U(4.W)
val s = (-1).S(4.W)
val mixed = Cat(u, s.asUInt)  // Explicit conversion required
```

### Type Conversion Methods

**asUInt**: Reinterpret as unsigned (same width)
```scala
val sint = 3.S(4.W)      // 4-bit signed
val uint = sint.asUInt    // 4-bit unsigned, same bit pattern
```

**asSInt**: Reinterpret as signed (same width)
```scala
val uint = 15.U(4.W)     // 4-bit unsigned (0b1111)
val sint = uint.asSInt   // 4-bit signed (-1 in two's complement)
```

**Note**: The arithmetic value is NOT preserved if MSB is set. For example:
- `7.U(3.W)` (binary `111`, unsigned value 7)
- `.asSInt` gives `-1.S(3.W)` (binary `111`, signed value -1)

### Working with Concatenation Results

Since `Cat` returns `UInt`, to use the result as signed:

```scala
val s1 = (-5).S(8.W)
val s2 = 3.S(8.W)
val concatenated = Cat(s1.asUInt, s2.asUInt)  // UInt(16.W)
val asSigned = concatenated.asSInt             // Reinterpret as SInt(16.W)
```

### Design Rationale

1. **Simplicity**: Single return type (`UInt`) simplifies API and type inference
2. **Bit manipulation focus**: Concatenation is a bit-level operation, unsigned interpretation is most natural
3. **Explicit conversion**: Forces users to be explicit about signed/unsigned interpretation
4. **Hardware semantics**: Reflects that concatenation at hardware level is just wire bundling
5. **Type safety**: Strong typing (requiring `Bits` subtypes) prevents invalid operations

---

## Rust

While Rust is not an HDL, it provides useful comparison for how a modern systems language handles bit manipulation with strong type safety.

### No Direct Concatenation Operator

Rust does NOT have a built-in bit concatenation operator. Instead, bit manipulation uses:
- Shift operators: `<<`, `>>`
- Bitwise operators: `&`, `|`, `^`, `!`
- Explicit type casting: `as`

### Type System

**Key Rule**: **No implicit type conversion**. All type conversions must be explicit using `as`.

Rust has distinct signed and unsigned integer types:
- Unsigned: `u8`, `u16`, `u32`, `u64`, `u128`, `usize`
- Signed: `i8`, `i16`, `i32`, `i64`, `i128`, `isize`

### Bit Concatenation Pattern

```rust
// Concatenating two u8 into u16
fn concat_u8_to_u16(high: u8, low: u8) -> u16 {
    ((high as u16) << 8) | (low as u16)
}

let result = concat_u8_to_u16(0xAB, 0xCD);  // 0xABCD
```

### Mixed Signedness

Rust **requires explicit casting** when mixing signed and unsigned types:

```rust
// Example 1: Cannot mix without casting
let unsigned: u32 = 42;
let signed: i32 = -10;
// let bad = unsigned | signed;  // ERROR: mismatched types

// Example 2: Explicit casting required
let result = unsigned | (signed as u32);  // OK

// Example 3: Bit concatenation with mixed types
fn concat_mixed(a: i8, b: u8) -> u16 {
    // Must cast both to common type
    (((a as u8) as u16) << 8) | (b as u16)
}
```

### Casting Semantics

**Casting to unsigned (e.g., `i32 as u32`)**:
- Preserves bit pattern (no-op for same-size types)
- For larger-to-smaller: truncates to least significant bits
- For negative values: reinterprets bit pattern as unsigned

```rust
let signed: i8 = -1;           // 0b11111111
let unsigned = signed as u8;   // 255 (0b11111111)
```

**Casting to signed (e.g., `u32 as i32`)**:
- First treats as unsigned, then interprets as signed
- If MSB is 1, result is negative (two's complement)

```rust
let unsigned: u8 = 255;        // 0b11111111
let signed = unsigned as i8;   // -1 (two's complement)
```

### Shift Operator Signedness

**Critical difference**: Shift right behavior depends on signedness:

```rust
let unsigned: u8 = 0b10000000;  // 128
let shifted_u = unsigned >> 1;   // 0b01000000 (64) - logical shift

let signed: i8 = -128;           // 0b10000000
let shifted_s = signed >> 1;     // 0b11000000 (-64) - arithmetic shift (sign extension)
```

### Code Examples

```rust
// Example 1: Concatenating bytes
fn concat_bytes(b1: u8, b2: u8, b3: u8, b4: u8) -> u32 {
    ((b1 as u32) << 24) |
    ((b2 as u32) << 16) |
    ((b3 as u32) << 8) |
    (b4 as u32)
}

// Example 2: Mixed signed/unsigned concatenation
fn concat_signed_unsigned(signed: i16, unsigned: u16) -> u32 {
    // Preserve bit patterns, concatenate as unsigned
    (((signed as u16) as u32) << 16) | (unsigned as u32)
}

// Example 3: Extracting and concatenating with sign preservation
fn sign_extend_and_concat(high: i8, low: u8) -> i16 {
    // Sign-extend high byte to 16 bits, then combine
    let extended_high = (high as i16) << 8;
    let extended_low = low as i16;
    extended_high | extended_low
}
```

### Wrapping Behavior

```rust
// Casting wraps values for smaller types
let large: u32 = 256;
let wrapped = large as u8;  // 0 (256 % 256)

let large2: u32 = 1000;
let wrapped2 = large2 as u8;  // 232 (keeps 8 LSB, discards rest)
```

### Design Rationale

1. **Explicit is better than implicit**: Rust forces explicit type conversions to prevent bugs
2. **Type safety**: Strong typing catches type mismatches at compile time
3. **Predictable behavior**: Casting rules are well-defined in the language specification
4. **Performance**: Explicit conversions often compile to no-ops for same-size types
5. **Safety**: No implicit conversions means no silent data loss or unexpected sign extension

---

## Summary and Rationale

### Comparison Table

| Language | Concat Operator | Mixed Signed/Unsigned? | Result Type | Requires Explicit Conversion? |
|----------|----------------|------------------------|-------------|------------------------------|
| **SystemVerilog** | `{a, b}` | Allowed (no error) | Always unsigned | No for concat, yes to use as signed |
| **VHDL** | `&` | NOT allowed | Preserves operand type | Yes (compile error without) |
| **Chisel** | `Cat(a, b)` | Discouraged | Always `UInt` | Yes (best practice) |
| **Rust** | `(a << n) \| b` | NOT allowed | Depends on cast | Yes (compile error without) |

### Design Philosophy Comparison

#### SystemVerilog: Permissive but Opinionated
- **Allows** mixed signedness without error
- **Forces** result to be unsigned
- **Rationale**: Concatenation is bit manipulation, not arithmetic; unsigned is the natural representation for bit patterns
- **Trade-off**: Convenient but can lead to subtle bugs if signed arithmetic is expected

#### VHDL: Strict Type Safety
- **Prevents** mixed signedness at compile time
- **Preserves** operand signedness in result
- **Rationale**: Strong typing catches errors early; explicit conversion documents intent
- **Trade-off**: More verbose but safer and clearer

#### Chisel: Pragmatic Uniformity
- **Returns** always unsigned (`UInt`)
- **Requires** explicit conversion for mixed types (best practice)
- **Rationale**: Simple, predictable API; concatenation is bit-bundling; hardware is agnostic to signedness
- **Trade-off**: Extra conversions needed but type system guides correct usage

#### Rust: Maximum Explicitness
- **Requires** explicit casting for all type conversions
- **Preserves** bit patterns in same-size casts
- **Rationale**: Prevent silent bugs; make programmer intent explicit; performance-conscious
- **Trade-off**: Verbose but completely transparent and safe

### Common Design Rationale Across All Languages

Despite different approaches, all languages share these underlying principles:

1. **Concatenation is a bit-level operation**: It joins bit patterns, not arithmetic values

2. **Sign bit ambiguity**: When concatenating signed values, which bit represents the sign? Semantics are unclear without explicit specification

3. **Hardware reality**: At the gate level, wires carry bit patterns. "Signedness" is an interpretation layer in software

4. **Explicitness prevents bugs**: Whether enforced by the type system (VHDL, Rust) or by returning unsigned (SystemVerilog, Chisel), all approaches push developers to be explicit about signed/unsigned interpretation

5. **Synthesis concerns**: Hardware synthesis tools need clear, unambiguous specifications. Type safety and explicit conversions help generate correct hardware

### Recommendations for HDL Design

Based on this research:

1. **Default to unsigned for concatenation**: Treat concatenation results as bit patterns (unsigned) by default

2. **Require explicit conversion when mixing**: Force users to explicitly convert between signed/unsigned before concatenation

3. **Preserve operand type when homogeneous**: If all operands are the same type (all signed or all unsigned), consider preserving that type in the result (VHDL approach)

4. **Provide clear conversion methods**: Offer well-documented conversion functions (like Chisel's `asUInt`/`asSInt`)

5. **Strong typing catches errors early**: Compile-time type checking (VHDL/Rust style) prevents runtime bugs

6. **Document sign extension behavior**: Make it clear whether conversions preserve bit patterns or arithmetic values

---

## References

### SystemVerilog
- IEEE 1800-2017: IEEE Standard for SystemVerilog—Unified Hardware Design, Specification, and Verification Language
- Section 11.4.12: Concatenation operators
- Chipverify Verilog Concatenation: https://www.chipverify.com/verilog/verilog-concatenation

### VHDL
- IEEE 1076: VHDL Language Reference Manual
- IEEE 1076.3: VHDL Synthesis Packages (numeric_std, now part of base standard in VHDL-2008)
- GHDL numeric_std source: https://github.com/ghdl/ghdl/blob/master/libraries/ieee/numeric_std.vhdl

### Chisel
- Chisel Documentation: https://www.chisel-lang.org/docs/
- Chisel API - Cat: https://www.chisel-lang.org/api/latest/chisel3/util/Cat$.html
- Chisel Data Types: https://www.chisel-lang.org/docs/explanations/data-types

### Rust
- The Rust Programming Language - Casting: https://doc.rust-lang.org/rust-by-example/types/cast.html
- Rust Reference - Type Coercions and Casts
- Stack Overflow: How do I convert between numeric types safely and idiomatically?

---

*Research compiled: 2025-11-17*
*For the BOON hardware description language project*
