# MEMORY: Stateful Random-Access Storage in Boon

**Date**: 2025-01-19
**Status**: Design Document
**Scope**: MEMORY for Hardware and Software

---

## Overview

MEMORY provides stateful, random-access storage in Boon, working seamlessly across hardware (Block RAM) and software (buffers) contexts.

**Key principle:** MEMORY is stateful by design - no need for LATEST wrapping. Each memory location is independently reactive.

---

## Quick Examples

### Hardware: Block RAM

```boon
// Simple RAM
mem: MEMORY { 16, BITS { 8, 2u0 } }
    |> Memory/write(address: wraddr, data: wrdata)

data: mem |> Memory/read(address: addr)
```

### Software: Pixel Buffer

```boon
// Frame buffer
screen: MEMORY { 1920 * 1080, Pixel { r: 0, g: 0, b: 0 } }
    |> Memory/write_entry(entry: draw_event |> THEN {
        [address: x + y * 1920, data: color]
    })

pixel: screen |> Memory/read(address: x + y * 1920)
```

---

## Syntax

### Construction

```boon
MEMORY { size, default_value }
```

**Parameters:**
- `size` - Fixed size (compile-time constant)
- `default_value` - Initial value for all locations

**Examples:**
```boon
// 8-bit RAM, 16 locations, initialized to 0
mem: MEMORY { 16, BITS { 8, 2u0 } }

// Pixel buffer, 1920x1080, initialized to black
screen: MEMORY { 1920 * 1080, Pixel { r: 0, g: 0, b: 0 } }

// Cache entries, 256 slots, initialized to invalid
cache: MEMORY { 256, [valid: False, data: 0] }
```

---

## Size Must Be Compile-Time Known

**Critical design principle:** MEMORY size is ALWAYS known at compile-time, never at runtime.

This matches the design philosophy of BITS width and LIST size - **explicit sizes are always compile-time constants.**

### Why Compile-Time Size?

1. **Hardware Reality** - Block RAM has fixed size determined at synthesis time
2. **Type Safety** - Size is part of the memory specification, enabling verification
3. **Performance** - Zero runtime overhead for size tracking or dynamic allocation
4. **Clarity** - Memory specifications explicitly declare capacity
5. **Safety** - Prevents out-of-bounds access at compile-time when possible

### What's Allowed: Compile-Time Constants

```boon
-- ✅ Literal size (most common)
MEMORY { 16, BITS { 8, 2u0 } }       -- Size: 16 (compile-time known)

-- ✅ Compile-time constant parameter
size: 256  -- Compile-time constant
MEMORY { size, Entry }               -- Size: 256 (compile-time known)

-- ✅ Compile-time expression
width: 1920
height: 1080
MEMORY { width * height, Pixel }     -- Size: 2073600 (compile-time known)

-- ✅ Type parameter in generic functions
FUNCTION create_buffer<size>() -> MEMORY<size, Byte> {
    MEMORY { size, BITS { 8, 2u0 } }    -- size is compile-time parameter
}
```

### What's NOT Allowed: Runtime Size

```boon
-- ❌ Runtime variable size
buffer_size: get_size_from_config()
MEMORY { buffer_size, Data }         -- ERROR: Size must be compile-time constant

-- ❌ Conditional size
size: if use_large_buffer { 1024 } else { 256 }
MEMORY { size, Byte }                -- ERROR: Size unknown at compile-time

-- ❌ Signal-dependent size (hardware)
FUNCTION create_mem(size_signal) {
    MEMORY { size_signal, Data }     -- ERROR: size must be compile-time constant
}
```

### Compile-Time Size Across Domains

Size is compile-time known in ALL contexts:

**Hardware (Block RAM):**
```boon
-- Block RAM (size fixed at synthesis)
ram: MEMORY { 256, BITS { 32, 2u0 } }
-- Synthesizer knows exact size, allocates BRAM blocks
```

**Software (Buffers):**
```boon
-- Fixed-size buffer (can be stack-allocated)
buffer: MEMORY { 4096, Sample { value: 0.0 } }
-- Compiler knows size, can optimize allocation

-- Screen buffer (large, heap-allocated but fixed size)
screen: MEMORY { 1920 * 1080, Pixel { r: 0, g: 0, b: 0 } }
-- Size known at compile-time, never changes
```

### Benefits of Compile-Time Size

1. **Hardware synthesis** - FPGA tools know exact BRAM requirements
2. **Memory safety** - Static bounds checking when address is constant
3. **Optimized allocation** - Fixed size enables stack allocation or efficient heap layout
4. **Clear capacity** - Size is visible in type, self-documenting
5. **No runtime tracking** - No need to store size at runtime

```boon
-- Compile-time bounds checking (when address is constant)
mem: MEMORY { 16, BITS { 8, 2u0 } }

data: mem |> Memory/read(address: 5)   -- ✅ OK: 5 < 16
data: mem |> Memory/read(address: 20)  -- ❌ ERROR: 20 >= 16 (if constant)

-- Runtime bounds checking (when address is variable)
data: mem |> Memory/read(address: user_input)  -- Runtime check needed
```

### MEMORY vs Dynamic Collections

Unlike LIST which can be dynamic, MEMORY is ALWAYS fixed-size:

```boon
-- ❌ MEMORY cannot be dynamic
dynamic_mem: MEMORY { ... }  -- ERROR: Size required, no dynamic MEMORY

-- ✅ Use LIST for dynamic collections
dynamic_list: LIST { Data }  -- OK: Dynamic list (no size specified)

-- ✅ Use MEMORY for fixed-size random access
fixed_mem: MEMORY { 256, Data }  -- OK: Fixed-size memory
```

**Why MEMORY is always fixed-size:**
- Maps directly to hardware Block RAM (fixed size)
- Random access requires allocated space
- Per-address reactivity needs pre-allocated cells
- Growing/shrinking would require reallocation (expensive)

### Function Signatures with MEMORY

```boon
-- Size in function parameter type
process_buffer: FUNCTION(buf: MEMORY<256, Byte>) -> Result {
    -- Compiler knows buffer has exactly 256 locations
}

-- Generic over size
process_any_buffer: FUNCTION<size>(buf: MEMORY<size, Byte>) -> Result {
    -- size is compile-time parameter
    -- Each instantiation has specific size
}

-- ❌ Can't pass wrong size
buf16: MEMORY { 16, Byte }
buf256: MEMORY { 256, Byte }

process_buffer(buf256)  -- ✅ OK: size matches
process_buffer(buf16)   -- ❌ ERROR: Expected MEMORY<256>, got MEMORY<16>
```

---

## Operations

### Memory/write - Direct Write

```boon
Memory/write(address: addr, data: data)
```

Writes `data` to `address` on every clock cycle (hardware) or when inputs change (software).

**Example:**
```boon
mem: MEMORY { 16, BITS { 8, 2u0 } }
    |> Memory/write(address: wraddr, data: wrdata)
```

### Memory/write_entry - Conditional Write

```boon
Memory/write_entry(entry: entry_or_skip)
```

Writes entry if provided, skips if SKIP. Useful for conditional writes.

**Example:**
```boon
mem: MEMORY { 16, BITS { 8, 2u0 } }
    |> Memory/write_entry(entry: write_enable |> WHEN {
        True => [address: wraddr, data: wrdata]
        False => SKIP
    })
```

### Memory/read - Read from Address

```boon
Memory/read(address: addr)
```

Reads value at `address`. Returns reactive signal that updates when that address is written.

**Example:**
```boon
data: mem |> Memory/read(address: 5)
// Updates automatically when mem[5] is written
```

### Memory/initialize - Custom Initialization

```boon
Memory/initialize(index, data: expression)
```

Initializes each location with custom value based on index.

**Example:**
```boon
// Initialize mem[i] = i
mem: MEMORY { 16, BITS { 8, 2u0 } }
    |> Memory/initialize(i, data: i |> Int/to_bits(width: 8))

// Initialize with lookup table
rom: MEMORY { 256, BITS { 16, 2u0 } }
    |> Memory/initialize(angle, data: SineLUT/get(angle))
```

---

## Reactivity Model

### Per-Address Reactivity

Each memory location is independently reactive:

```boon
mem: MEMORY { 16, Int }
    |> Memory/write(address: 5, data: 42)

value_at_5: mem |> Memory/read(address: 5)
value_at_10: mem |> Memory/read(address: 10)

// When address 5 is written: value_at_5 updates ✅
// value_at_10 doesn't update (different address) ✅
```

### Different from LIST

| Aspect | MEMORY | LIST |
|--------|--------|------|
| **Reactivity** | Per-address (cell-level) | Structural (collection-level) |
| **Updates** | Value changes at specific address | Add/remove/replace items |
| **Diffs** | No (just new value) | Yes (VecDiff enum) |
| **Use case** | Random access, fixed size | Sequential, dynamic size |

**Example comparison:**
```boon
// LIST: Structural reactivity
todos: LIST {}
    |> List/append(item: new_todo)
// Subscribers get: VecDiff::Push { value: new_todo }

// MEMORY: Per-address reactivity
pixels: MEMORY { 1920 * 1080, Pixel { ... } }
    |> Memory/write(address: 100, data: red_pixel)
// Subscribers at address 100 get: new Pixel value
// Other addresses not notified
```

---

## Hardware Context (FPGA)

### Block RAM

```boon
FUNCTION ram(addr, wraddr, wrdata) {
    BLOCK {
        mem: MEMORY { 16, BITS { 8, 2u0 } }
            |> Memory/write(address: wraddr, data: wrdata)

        data: mem |> Memory/read(address: addr)

        [data: data]
    }
}
```

**Transpiles to SystemVerilog:**
```systemverilog
module ram(
    input clk,
    input [3:0] addr,
    input [3:0] wraddr,
    input [7:0] wrdata,
    output [7:0] data
);
    logic [7:0] mem [0:15];

    // Initialize to 0
    initial begin
        for (int i = 0; i < 16; i++) begin
            mem[i] = 8'b0;
        end
    end

    // Synchronous write
    always_ff @(posedge clk) begin
        mem[wraddr] <= wrdata;
    end

    // Asynchronous read
    assign data = mem[addr];
endmodule
```

### Write Enable

```boon
mem: MEMORY { 16, BITS { 8, 2u0 } }
    |> Memory/write_entry(entry: write_enable |> WHEN {
        True => [address: wraddr, data: wrdata]
        False => SKIP
    })
```

**Transpiles to:**
```systemverilog
always_ff @(posedge clk) begin
    if (write_enable) begin
        mem[wraddr] <= wrdata;
    end
end
```

### Custom Initialization

```boon
// Initialize with sequential values
mem: MEMORY { 16, BITS { 8, 2u0 } }
    |> Memory/initialize(i, data: i |> Int/to_bits(width: 8))
```

**Transpiles to:**
```systemverilog
initial begin
    for (int i = 0; i < 16; i++) begin
        mem[i] = i[7:0];
    end
end
```

---

## Software Context (Event-Driven)

### Pixel Buffer

```boon
screen: MEMORY { 1920 * 1080, Pixel { r: 0, g: 0, b: 0 } }
    |> Memory/write_entry(entry: draw_event |> THEN {
        [address: x + y * 1920, data: color]
    })

// Each pixel is independently reactive
pixel_at_100_50: screen |> Memory/read(address: 100 + 50 * 1920)
// Only updates when that specific pixel is drawn
```

**Implementation:**
```rust
struct Memory<T> {
    cells: Vec<Mutable<T>>,  // Each cell is reactive
}

impl Memory<T> {
    fn write(&self, address: usize, data: T) {
        self.cells[address].set(data);  // Triggers reactivity
    }

    fn read(&self, address: usize) -> Signal<T> {
        self.cells[address].signal()  // Reactive signal
    }
}
```

### Circular Buffer

```boon
BLOCK {
    buffer: MEMORY { 4096, Sample { value: 0.0 } }
        |> Memory/write_entry(entry: push_event |> THEN {
            [address: write_ptr, data: audio_sample]
        })

    write_ptr: 0 |> LATEST wr {
        push_event |> THEN { (wr + 1) % 4096 }
    }

    read_ptr: 0 |> LATEST rd {
        pop_event |> THEN { (rd + 1) % 4096 }
    }

    sample: buffer |> Memory/read(address: read_ptr)
}
```

### Lookup Table / Cache

```boon
lru_cache: MEMORY { 256, [valid: False, tag: 0, data: 0] }
    |> Memory/write_entry(entry: cache_miss |> THEN {
        [
            address: evict_index,
            data: [valid: True, tag: tag, data: fetched_data]
        ]
    })

cache_entry: lru_cache |> Memory/read(address: hash(key))
hit: cache_entry.valid |> Bool/and(cache_entry.tag = key)
```

---

## Complete Examples

### Example 1: Simple RAM with Write Enable

```boon
FUNCTION ram_with_enable(addr, wraddr, wrdata, write_enable) {
    BLOCK {
        mem: MEMORY { 16, BITS { 8, 2u0 } }
            |> Memory/write_entry(entry: write_enable |> WHEN {
                True => [address: wraddr, data: wrdata]
                False => SKIP
            })

        data: mem |> Memory/read(address: addr)

        [data: data]
    }
}
```

### Example 2: ROM with Lookup Table

```boon
FUNCTION sine_rom(angle) {
    BLOCK {
        rom: MEMORY { 256, BITS { 16, 2u0 } }
            |> Memory/initialize(i, data: SineLUT/get(i))
        // No write operations (read-only)

        sine_value: rom |> Memory/read(address: angle)

        [sine: sine_value]
    }
}
```

### Example 3: Dual-Port RAM

```boon
FUNCTION dual_port_ram(addr_a, addr_b, wraddr, wrdata, write_enable) {
    BLOCK {
        mem: MEMORY { 16, BITS { 8, 2u0 } }
            |> Memory/write_entry(entry: write_enable |> WHEN {
                True => [address: wraddr, data: wrdata]
                False => SKIP
            })

        // Two independent read ports
        data_a: mem |> Memory/read(address: addr_a)
        data_b: mem |> Memory/read(address: addr_b)

        [data_a: data_a, data_b: data_b]
    }
}
```

### Example 4: True Dual-Port (Two Write Ports)

```boon
FUNCTION true_dual_port_ram(addr_a, addr_b, wdata_a, wdata_b, we_a, we_b) {
    BLOCK {
        mem: MEMORY { 16, BITS { 8, 2u0 } }
            |> Memory/write_entry(entry: we_a |> WHEN {
                True => [address: addr_a, data: wdata_a]
                False => SKIP
            })
            |> Memory/write_entry(entry: we_b |> WHEN {
                True => [address: addr_b, data: wdata_b]
                False => SKIP
            })
        // If addr_a == addr_b: Port B write wins (second write overwrites)

        data_a: mem |> Memory/read(address: addr_a)
        data_b: mem |> Memory/read(address: addr_b)

        [data_a: data_a, data_b: data_b]
    }
}
```

### Example 5: Priority-Based Write

```boon
FUNCTION priority_ram(addr, data_high, data_med, data_low, we_high, we_med, we_low) {
    BLOCK {
        mem: MEMORY { 16, BITS { 8, 2u0 } }
            |> Memory/write_entry(entry: BLOCK {
                [we_high, we_med, we_low] |> WHEN {
                    [True, __, __] => [address: addr, data: data_high]
                    [False, True, __] => [address: addr, data: data_med]
                    [False, False, True] => [address: addr, data: data_low]
                    [False, False, False] => SKIP
                }
            })

        data: mem |> Memory/read(address: addr)

        [data: data]
    }
}
```

### Example 6: FIFO using MEMORY

```boon
FUNCTION fifo(push, pop, push_data) {
    BLOCK {
        // Data storage
        fifo_mem: MEMORY { 16, BITS { 8, 2u0 } }
            |> Memory/write_entry(entry: push |> WHEN {
                True => [address: write_ptr, data: push_data]
                False => SKIP
            })

        // Write pointer
        write_ptr: 0 |> LATEST wr {
            push |> THEN { (wr + 1) % 16 }
        }

        // Read pointer
        read_ptr: 0 |> LATEST rd {
            pop |> THEN { (rd + 1) % 16 }
        }

        // Output data
        data: fifo_mem |> Memory/read(address: read_ptr)

        // Status
        empty: read_ptr = write_ptr
        full: (write_ptr + 1) % 16 = read_ptr

        [data: data, empty: empty, full: full]
    }
}
```

### Example 7: Cache with Dirty Bits

```boon
FUNCTION cache_memory(addr, write_data, write) {
    BLOCK {
        cache: MEMORY { 256, [data: BITS { 32, 2u0 }, dirty: False] }
            |> Memory/write_entry(entry: write |> WHEN {
                True => [
                    address: addr,
                    data: [data: write_data, dirty: True]
                ]
                False => SKIP
            })

        entry: cache |> Memory/read(address: addr)

        [data: entry.data, dirty: entry.dirty]
    }
}
```

---

## MEMORY vs LATEST

### No LATEST Needed!

**Before (with LATEST):**
```boon
mem: ARRAY[16] { BITS { 8, 2u0 } } |> LATEST mem {
    write_enable |> WHEN {
        True => mem |> Array/set(index: wraddr, value: wrdata)
        False => mem
    }
}
```

**After (with MEMORY):**
```boon
mem: MEMORY { 16, BITS { 8, 2u0 } }
    |> Memory/write_entry(entry: write_enable |> WHEN {
        True => [address: wraddr, data: wrdata]
        False => SKIP
    })
```

### Why MEMORY is Better for RAM

| Aspect | LATEST | MEMORY |
|--------|--------|--------|
| **Self-reference** | Required (`mem` parameter) | Not needed |
| **Complexity** | Explicit state management | Implicit statefulness |
| **Clarity** | General pattern | Domain-specific |
| **Syntax** | More verbose | Cleaner |
| **Use case** | General stateful updates | Specifically for memory |

### When to Use Each

**Use MEMORY for:**
- ✅ Hardware RAM (Block RAM, register files)
- ✅ Software buffers (pixels, audio, circular buffers)
- ✅ Caches and lookup tables
- ✅ Fixed-size random-access storage

**Use LATEST for:**
- ✅ Counters
- ✅ FSM state
- ✅ Pointers and indices
- ✅ Flags and enables
- ✅ General stateful computations

**Example: FIFO uses both**
```boon
// MEMORY for data storage
data: MEMORY { 16, BITS { 8, 2u0 } }

// LATEST for pointers
write_ptr: 0 |> LATEST wr { ... }
read_ptr: 0 |> LATEST rd { ... }
```

---

## MEMORY vs LIST

| Aspect | MEMORY | LIST |
|--------|--------|------|
| **Size** | Fixed at creation | Dynamic (grows/shrinks) |
| **Access** | Random (O(1) by address) | Sequential or indexed |
| **Mutation** | In-place (per-cell) | Structural sharing |
| **Reactivity** | Per-address updates | Structural diffs (VecDiff) |
| **Use cases** | Buffers, caches, RAM | Collections, UI lists |
| **In LATEST?** | No (stateful by design) | No (use reactive ops) |

**Example:**
```boon
// LIST: Dynamic collection with structural diffs
todos: LIST {}
    |> List/append(item: new_todo)
    |> List/retain(item, if: predicate)
// Sends VecDiff::Push, VecDiff::Remove, etc.

// MEMORY: Fixed storage with per-cell updates
pixels: MEMORY { 1920 * 1080, Pixel { ... } }
    |> Memory/write(address: x + y * 1920, data: color)
// Updates only the specific pixel at that address
```

---

## Design Rationale

### Why MEMORY?

**1. Hides LATEST Complexity**
- RAM is common in hardware
- No need to expose LATEST self-reference
- Simpler, cleaner code

**2. Domain-Specific**
- MEMORY is clearly for storage
- Different semantics from LIST
- Matches hardware terminology (Block RAM)

**3. Compositional API**
- Pipeline pattern (`|>`) fits Boon
- Works with WHEN, SKIP, BLOCK
- Chainable operations

**4. Type-Safe**
- Required default value
- Compiler knows element type
- No undefined memory

**5. Works in Both Contexts**
- Hardware: Block RAM synthesis
- Software: Mutable buffers
- Same API, different implementation

### Why Not ARRAY or FIXEDLIST?

**ARRAY:**
- Ambiguous (mutable or immutable?)
- Generic term (doesn't convey statefulness)
- Could conflict with functional arrays

**FIXEDLIST:**
- Misleading (implies it's like LIST)
- LIST has structural reactivity (diffs)
- MEMORY has per-cell reactivity (values)
- Fundamentally different semantics

**MEMORY:**
- Clear statefulness (persistent storage)
- Hardware-friendly (Block RAM, memory buffer)
- Distinct from LIST (different reactivity)
- Matches actual use cases (RAM, buffers, caches)

---

## Implementation Notes

### Hardware (SystemVerilog)

```boon
mem: MEMORY { 16, BITS { 8, 2u0 } }
    |> Memory/write(address: wraddr, data: wrdata)
```

**Transpiles to:**
```systemverilog
logic [7:0] mem [0:15];

initial begin
    for (int i = 0; i < 16; i++) begin
        mem[i] = 8'b0;
    end
end

always_ff @(posedge clk) begin
    mem[wraddr] <= wrdata;
end
```

### Software (Rust)

```boon
mem: MEMORY { 16, Int }
    |> Memory/write(address: wraddr, data: wrdata)
```

**Implements as:**
```rust
struct Memory<T> {
    cells: Vec<Mutable<T>>,
}

impl<T: Clone> Memory<T> {
    fn new(size: usize, default: T) -> Self {
        Memory {
            cells: (0..size)
                .map(|_| Mutable::new(default.clone()))
                .collect()
        }
    }

    fn write(&self, address: usize, data: T) {
        self.cells[address].set(data);
    }

    fn read(&self, address: usize) -> Signal<T> {
        self.cells[address].signal()
    }
}
```

---

## Common Patterns

### Pattern 1: Simple RAM
```boon
mem: MEMORY { size, BITS { width, 2u0 } }
    |> Memory/write(address: wraddr, data: wrdata)
```

### Pattern 2: RAM with Write Enable
```boon
mem: MEMORY { size, default }
    |> Memory/write_entry(entry: we |> WHEN {
        True => [address: addr, data: data]
        False => SKIP
    })
```

### Pattern 3: ROM (Read-Only)
```boon
rom: MEMORY { size, default }
    |> Memory/initialize(i, data: LUT/get(i))
// No write operations
```

### Pattern 4: Dual-Port RAM
```boon
mem: MEMORY { size, default }
    |> Memory/write(address: wraddr, data: wrdata)

data_a: mem |> Memory/read(address: addr_a)
data_b: mem |> Memory/read(address: addr_b)
```

### Pattern 5: FIFO with Pointers
```boon
data: MEMORY { size, default }
    |> Memory/write(address: write_ptr, data: input)

write_ptr: 0 |> LATEST wr { ... }
read_ptr: 0 |> LATEST rd { ... }
```

---

## Summary

**MEMORY provides:**
- ✅ Stateful random-access storage
- ✅ Per-address reactivity
- ✅ No LATEST needed (stateful by design)
- ✅ Works in hardware (Block RAM) and software (buffers)
- ✅ Clean, compositional API
- ✅ Type-safe with required default

**Use MEMORY for:**
- Hardware RAM and ROM
- Pixel and audio buffers
- Caches and lookup tables
- Any fixed-size random-access storage

**Use LATEST for:**
- Counters, FSM state, pointers
- General stateful computations

**Use LIST for:**
- Dynamic collections
- Structural changes (add/remove)
- UI lists and reactive data

---

**MEMORY: Stateful storage made simple!**
