# LIST: Universal Collection Type in Boon

**Date**: 2025-01-19
**Status**: Design Specification
**Scope**: Dynamic lists (software) and fixed-size lists (hardware)

---

## Overview

LIST is Boon's universal collection type, working seamlessly across domains:
- **Software**: Dynamic, reactive lists (grow/shrink, event streams)
- **Hardware**: Fixed-size lists (compile-time known size, elaboration-time unrolling)

**Key principle:** Same syntax, same operations - behavior adapts based on compile-time size knowledge.

---

## Construction Syntax

**LIST uses different syntax for dynamic vs fixed-size:**

- **Dynamic (no size parameter):** `LIST {}` or `LIST { item1, item2, ... }`
- **Fixed-size (size parameter):** `LIST[size, content]`

### Dynamic Lists (Software)

```boon
-- Empty dynamic list
todos: LIST {}

-- Dynamic list with initial items (no size parameter)
todos: LIST { item1, item2, item3 }  -- Can grow/shrink later

-- ⚠️ NOT the same as fixed-size with inferred size:
fixed: LIST[__] {  item1, item2, item3  }  -- Fixed at 3 elements, cannot grow
```

**Important distinction:**
- `LIST { item1, item2, item3 }` → **Dynamic** (no size parameter, can grow/shrink)
- `LIST[__] {  item1, item2, item3  }` → **Fixed-size** (size inferred from content, cannot grow)

**Note:** This is consistent across all collection types:
- Dynamic: `LIST { items }`, `BYTES { bytes }` (no size parameter)
- Fixed-size: `LIST[size, items]`, `BYTES { size, bytes }` (with size parameter)

### Fixed-Size Lists (Hardware Compatible)

```boon
-- Empty fixed-size list (all elements get default value)
bits: LIST[8] {  }  -- 8 elements, all False (for Bool)

-- Partial initialization (remaining get default)
flags: LIST[4] {  True, False  }  -- [True, False, False, False]

-- Full initialization
data: LIST[3] {  10, 20, 30  }

-- Size inference
coords: LIST[__] {  x, y, z  }  -- Size = 3

-- Examples
bits: LIST[8] {  }             -- 8 elements (default values)
signals: LIST[width] {  }      -- width elements (width is compile-time constant)
```

**Default values:**
- `Bool` → `False`
- `Number` → `0`
- `Text` → `TEXT {}`
- `BITS[N] { ...  }` → `BITS[N] { 10u0  }`
- Records → All fields get their defaults
- Tags → First variant (lexicographically)

---

## Pattern Matching

### Match by Size

```boon
my_list |> WHEN {
    LIST[8, items] => process_8(items)     -- Match size 8
    LIST[4, items] => process_4(items)     -- Match size 4
    LIST[__, items] => process_any(items)  -- Match any size
}
```

### Destructure Elements

```boon
-- Extract specific elements
my_list |> WHEN {
    LIST[__] {  a, b  } => combine(a, b)           -- Size 2, bind both
    LIST[__] {  first, __, third  } => ...         -- Size 3, ignore middle
    LIST[4] {  a, b, __, __  } => pair(a, b)       -- Size 4, bind first two
}

-- Pattern with literals
input |> WHEN {
    LIST[3] {  True, __, False  } => ...  -- Size 3, first=True, last=False
    LIST[__] {  0, x, y  } => ...         -- Size 3, first=0, bind rest
}

-- Nested patterns
points |> WHEN {
    LIST[__] { 
        [x: 0, y: 0]      -- Origin
        [x: x, y: y]      -- Some point
     } => distance(x, y)
}
```

### Size Wildcards

```boon
-- Match minimum size with rest
list |> WHEN {
    LIST[__] {  first, second, rest...  } => process(first, second, rest)
}
```

---

## Core Operations

### Indexed Access

```boon
-- Get element (0-indexed)
item: list |> List/get(index: 3)

-- Set element (returns new list)
updated: list |> List/set(index: 3, value: new_item)

-- Slice (extract range)
subset: list |> List/slice(from: 2, to: 5)  -- Elements 2, 3, 4
```

### Transformations (Size-Preserving)

```boon
-- Map: transform each element
doubled: numbers |> List/map(old, new: old * 2)
-- LIST[3, Number] → LIST[3, Number]

-- Reverse
reversed: list |> List/reverse()

-- Zip: combine two lists element-wise
pairs: a_list |> List/zip(with: b_list)
-- LIST[N, A] + LIST[N, B] → LIST { N, [A, B] }
-- Compile error if sizes don't match (for fixed-size)

-- Enumerate: add indices
indexed: items |> List/enumerate()
-- LIST[N, T] → LIST { N, [index: Number, item: T] }
```

### Reductions

```boon
-- Fold: reduce to single value
sum: numbers |> List/fold(
    init: 0
    item, acc: acc + item
)
-- LIST[N, Number] → Number

-- Chain: thread state through list, collect outputs
result: numbers |> List/chain(
    item
    initial_state: 0
    state
    iteration: [
        output_value: state + item
        next_state: state + item
    ]
)
-- Returns: [values: LIST[N, Number], final_state: Number]

-- Count
length: list |> List/count()

-- Any / All
has_completed: todos |> List/any(item, if: item.completed)
all_valid: items |> List/all(item, if: item.valid)
```

### Dynamic Operations (Software Only)

**❌ Not allowed in hardware context (compile error for fixed-size lists)**

```boon
-- Append (grows list)
updated: list |> List/append(item: new_item)
-- LIST { element_type } → LIST { element_type }

-- Filter (shrinks list)
active: todos |> List/filter(item, if: item.completed |> Bool/not())

-- Take / Drop
first_three: list |> List/take(count: 3)
rest: list |> List/drop(count: 3)

-- Flatten
flat: nested_lists |> List/flatten()
```

### List/retain: Permanent Removal vs Filtered View

`List/retain` has **two distinct behaviors** depending on how it's used:

#### 1. Source of Truth (Permanent Removal)

When `List/retain` is part of the **definition chain**, items that don't match the condition are **permanently removed**:

```boon
-- Items are permanently filtered - 1 is destroyed, not hidden
todos: LIST {}
    |> List/append(item: 123)
    |> List/append(item: 345)
    |> List/append(item: 1)
    |> List/retain(item, if: item > 1)
-- Result: todos contains [123, 345] - the 1 is gone forever
```

This is useful for:
- Removing items based on reactive conditions (e.g., "Clear completed" button)
- Building pipelines where you want to actually discard items

**Example: Clear completed todos on button press**
```boon
todos: LIST { todo1, todo2 }
    |> List/append(item: new_todo)
    |> List/retain(item, if: LATEST {
        True  -- Default: keep all items
        clear_button.event.press |> THEN { item.completed |> Bool/not() }
    })
-- When button is pressed: completed items are permanently removed
-- New items added after: kept (LATEST emits True immediately for them)
```

#### 2. Filtered View (Non-Destructive)

When `List/retain` is applied to an **existing list variable**, it creates a **filtered view** without modifying the source:

```boon
-- Source list (source of truth)
todos: LIST {}
    |> List/append(item: 123)
    |> List/append(item: 345)
    |> List/append(item: 1)
-- todos contains [123, 345, 1]

-- Filtered view (does NOT modify todos)
large_todos: todos |> List/retain(item, if: item > 200)
-- large_todos contains [345]
-- todos still contains [123, 345, 1]
```

This is useful for:
- Creating derived/computed lists for display
- Filtering for different views (All/Active/Completed tabs)
- Analytics and counting without modifying data

**Example: Filter tabs in todo app**
```boon
-- Source of truth
todos: LIST {} |> List/append(item: new_todo)

-- Filtered views (non-destructive)
active_todos: todos |> List/retain(item, if: item.completed |> Bool/not())
completed_todos: todos |> List/retain(item, if: item.completed)

-- Display based on selected tab
visible_todos: selected_tab |> WHILE {
    All => todos
    Active => active_todos
    Completed => completed_todos
}
```

#### Summary

| Context | Behavior | Items matching `False` |
|---------|----------|------------------------|
| Definition chain (`x: LIST {} \|> retain`) | Permanent removal | Destroyed |
| Derived from variable (`y: x \|> retain`) | Filtered view | Hidden but still in source |

---

## Domain-Specific Behavior

### Software (Dynamic Lists)

**Characteristics:**
- No compile-time size
- Can grow/shrink at runtime
- Reactive (VecDiff operations)
- Efficient structural sharing

```boon
-- Dynamic list
todos: LIST {}
    |> List/append(item: new_todo)           -- Runtime operation
    |> List/filter(item, if: item.active)    -- Runtime operation
    |> List/map(old, new: render_todo(old))  -- Runtime operation
```

**Reactive behavior:**
```boon
filtered_todos: PASSED.todos
    |> List/filter(item, if: item.completed |> Bool/not())
-- Updates automatically when PASSED.todos changes
```

### Hardware (Fixed-Size Lists)

**Characteristics:**
- Compile-time known size
- Elaboration-time unrolling
- Operations become parallel/sequential hardware
- No dynamic allocation

```boon
-- Fixed-size list
bits: LIST[8] {  }
    |> List/map(old, new: old |> Bool/not())  -- Transpiler unrolls to 8 NOT gates
    |> List/fold(init: False, item, acc: item |> Bool/or(that: acc))
                                              -- Transpiler creates OR tree
```

**Elaboration-time semantics:**
- `List/map` → Parallel instances (N copies of logic)
- `List/fold` → Sequential chain (N-stage pipeline)
- `List/chain` → Chain with outputs (N stages, N outputs)
- `List/zip` → Structural pairing (N pairs)

---

## Elaboration-Time Transpilation

### How the Transpiler Decides

**Rule:** Operations on `LIST[size, T]` where `size` is compile-time constant are **elaboration-time** (unrolled).

```boon
-- ✅ Elaboration-time (size known)
a: BITS[8] { 10u42  }
a_bits: a |> Bits/to_bool_list()     -- LIST[8, Bool]
result: a_bits |> List/map(old, new: old)     -- Transpiler unrolls to 8 operations

-- ❌ Runtime (size unknown) - ERROR in hardware context
dynamic: get_items()                  -- LIST { TodoItem }
result: dynamic |> List/map(...)      -- Software: OK, Hardware: ERROR
```

### Transpilation Examples

**List/map (parallel):**
```boon
-- Boon
bits: LIST[3] {  a, b, c  }
inverted: bits |> List/map(old, new: old |> Bool/not())

-- Transpiles to SystemVerilog
inverted[0] = ~a;
inverted[1] = ~b;
inverted[2] = ~c;
```

**List/fold (sequential chain):**
```boon
-- Boon
values: LIST[4] {  a, b, c, d  }
sum: values |> List/fold(init: 0, item, acc: acc + item)

-- Transpiles to
temp0 = 0;
temp1 = temp0 + a;
temp2 = temp1 + b;
temp3 = temp2 + c;
sum = temp3 + d;
```

**List/chain (chain with outputs):**
```boon
-- Boon
bits: LIST[3] {  a, b, c  }
result: bits |> List/chain(
    item
    initial_state: False
    state
    iteration: [
        output_value: item |> Bool/xor(that: state)
        next_state: item |> Bool/and(that: state)
    ]
)

-- Transpiles to (3 half-adders in chain)
ha0_sum = a ^ false;
ha0_carry = a & false;

ha1_sum = b ^ ha0_carry;
ha1_carry = b & ha0_carry;

ha2_sum = c ^ ha1_carry;
ha2_carry = c & ha1_carry;

-- Result structure:
result.values = {ha0_sum, ha1_sum, ha2_sum};
result.final_state = ha2_carry;
```

**List/zip (structural pairing):**
```boon
-- Boon
a: LIST[3] {  a0, a1, a2  }
b: LIST[3] {  b0, b1, b2  }
pairs: a |> List/zip(with: b)

-- Transpiles to
pairs[0] = {a0, b0};
pairs[1] = {a1, b1};
pairs[2] = {a2, b2};
```

---

## Conversions

### BITS ↔ LIST

```boon
-- BITS → Fixed-size LIST
bits: BITS[8] { 10u42  }
bool_list: bits |> Bits/to_bool_list()
-- Result: LIST[8, Bool]

-- Fixed-size LIST → BITS
list: LIST[8] {  True, False, True, False, True, False, True, False  }
bits: list |> List/to_u_bits()  -- Unsigned BITS
bits: list |> List/to_s_bits()  -- Signed BITS

-- ERROR: Dynamic LIST → BITS
dynamic: LIST { Bool }
dynamic |> List/to_u_bits()  -- Compile ERROR: "Cannot convert dynamic LIST to BITS"
```

### Between LIST types

```boon
-- Dynamic → Fixed (if size known at runtime in software)
dynamic: LIST { 1, 2, 3 }
fixed: dynamic |> List/to_fixed(size: 3)  -- Software only

-- Fixed → Dynamic
fixed: LIST[3] {  a, b, c  }
dynamic: fixed |> List/to_dynamic()  -- Software only
```

---

## Type System

### Type Notation

```boon
-- Dynamic list
LIST { element_type }

-- Fixed-size list
LIST[size, element_type]

-- Examples
LIST { TodoItem }           -- Dynamic list of TodoItem
LIST[8, Bool]            -- Fixed list of 8 booleans
LIST[width, Bool]        -- Fixed list of width booleans (width is constant)
LIST[__, Bool]           -- Size inferred from construction
```

### Type Inference

```boon
-- Infer element type from usage
items: LIST {}
items |> List/append(item: [x: 1, y: 2])
-- Type: LIST { [x: Number, y: Number] }

-- Infer size from construction
coords: LIST[__] {  0, 1, 2  }
-- Type: LIST[3, Number]

-- Infer both from BITS conversion
bits: BITS[8] { 10u42  }
bool_list: bits |> Bits/to_bool_list()
-- Type: LIST[8, Bool]
```

### Compile-Time Size Requirements

**Critical principle:** If LIST size is specified, it MUST be compile-time known, never runtime.

This matches the design philosophy of BITS width and MEMORY size - **explicit sizes are always compile-time constants.**

#### Why Compile-Time Size?

1. **Hardware Reality** - Fixed-size arrays map to hardware registers and are unrollable at elaboration time
2. **Type Safety** - Size is part of the type, enabling compile-time verification
3. **Performance** - Zero runtime overhead for size checking, enables optimizations
4. **Clarity** - Function signatures explicitly declare collection sizes

#### Size as Part of Type

When specified, size becomes part of the LIST type:

```boon
-- These are DIFFERENT types
list3: LIST[3, Number] = LIST[3] {  1, 2, 3  }
list5: LIST[5, Number] = LIST[5] {  1, 2, 3, 4, 5  }

-- ❌ Type mismatch
list3: LIST[3, Number] = LIST[5] {  1, 2, 3, 4, 5  }  -- ERROR

-- ✅ Functions specify size in type signature
process_triple: FUNCTION(data: LIST[3, Number]) -> Result {
    -- Compiler knows data has exactly 3 elements
}

-- ❌ Can't pass wrong size
list5: LIST[5, Number]
process_triple(list5)  -- ERROR: Expected LIST(3), got LIST(5)
```

#### What's Allowed: Compile-Time Constants

```boon
-- ✅ Literal size (most common)
LIST[8, Bool]                     -- Size: 8 (compile-time known)

-- ✅ Compile-time constant parameter
width: 8  -- Compile-time constant
LIST[width, Bool]                 -- Size: 8 (compile-time known)

-- ✅ Compile-time expression
LIST { width * 2, Number }           -- Size: 16 (compile-time known)

-- ✅ Type parameter in generic functions
FUNCTION create_array<size>() -> LIST[size, Bool] {
    LIST[size] {  }                -- size is compile-time parameter
}

-- ✅ Size inferred from construction
coords: LIST[__] {  x, y, z  }      -- Size: 3 (inferred at compile-time)
```

#### What's NOT Allowed: Runtime Size

```boon
-- ❌ Runtime variable size
user_count: get_count_from_user()
LIST[user_count, Number]          -- ERROR: Size must be compile-time constant

-- ❌ Conditional size
size: if condition { 8 } else { 16 }
LIST[size, Bool]                  -- ERROR: Size unknown at compile-time

-- ❌ Signal-dependent size (hardware)
FUNCTION process(size_signal) {
    LIST[size_signal, Bool]       -- ERROR: size must be compile-time constant
}

-- ✅ Use dynamic LIST instead
FUNCTION process() {
    LIST { Number }                  -- Dynamic (no size specified)
}
```

#### Compile-Time Size Across Domains

Size is compile-time known in ALL contexts where specified:

**Hardware (Fixed-size required):**
```boon
-- Register file (8 registers, hardware-defined)
registers: LIST[8] { BITS[32] { 10u0 } }

-- Elaboration-time unrolling
result: registers |> List/map(old, new: process(old))  -- Unrolls to 8 operations
```

**Software (Fixed-size optional):**
```boon
-- Fixed-size buffer (stack-allocated)
buffer: LIST[256, Number]

-- Dynamic collection (heap-allocated)
todos: LIST { TodoItem }  -- No size = dynamic
```

#### Benefits of Compile-Time Size

1. **Early error detection** - Size mismatches caught at compile-time
2. **Optimized operations** - Unrolling, vectorization, stack allocation
3. **Self-documenting** - Function signatures show exact element counts
4. **No runtime overhead** - No dynamic size tracking needed
5. **Pattern matching safety** - Size constraints enforced

```boon
-- Compile-time size checking in pattern matching
parse_triple: FUNCTION(data: LIST[3, Number]) {
    data |> WHEN {
        LIST[3] {  a, b, c  } => process(a, b, c)  -- ✅ Size matches
        LIST[2] {  a, b  } => invalid              -- ❌ ERROR: Size mismatch
    }
}
```

#### Dynamic vs Fixed: Clear Distinction

```boon
-- Dynamic LIST (no size) - can grow/shrink
todos: LIST { TodoItem }
todos: todos |> List/append(item: new_todo)  -- ✅ OK: dynamic

-- Fixed-size LIST (size specified) - cannot grow/shrink
buffer: LIST[16, Number]
buffer: buffer |> List/append(item: 42)      -- ❌ ERROR: Fixed size, cannot grow
```

---

## Hardware Examples

### Parallel Operations (Combinational)

```boon
-- Invert all bits (8 parallel NOT gates)
FUNCTION invert_byte(input) {
    -- input: BITS[8] { ...  }
    bits: input |> Bits/to_bool_list()  -- LIST[8, Bool]
    inverted: bits |> List/map(old, new: old |> Bool/not())
    [output: inverted |> List/to_u_bits()]
}
```

### Sequential Chains (Pipelined)

```boon
-- Ripple-carry adder chain
FUNCTION adder_chain(a, b) {
    a_bits: a |> Bits/to_bool_list()  -- LIST[width, Bool]
    b_bits: b |> Bits/to_bool_list()

    result: a_bits |> List/zip(with: b_bits)
        |> List/chain(
            item
            initial_state: False  -- Initial carry
            state
            iteration: BLOCK {
                sum: item.first |> Bool/xor(that: item.second) |> Bool/xor(that: state)
                carry_out: (item.first |> Bool/and(that: item.second))
                    |> Bool/or(that: item.first |> Bool/and(that: state))
                    |> Bool/or(that: item.second |> Bool/and(that: state))
                [
                    output_value: sum
                    next_state: carry_out
                ]
            }
        )

    [
        sum: result.values |> List/to_u_bits()
        carry: result.final_state
    ]
}
```

---

## Software Examples

### Reactive UI Lists

```boon
-- Dynamic todo list
todos: LIST {}
    |> List/append(item: new_todo_event)
    |> List/filter(item, if: item.completed |> Bool/not())
    |> List/map(old, new: render_todo(old))
```

### Event Streams

```boon
-- Last N events (bounded buffer)
events: LATEST {
    LIST {}
    new_event |> THEN {
        events
            |> List/append(item: new_event)
            |> List/take_last(count: 100)
    }
}
```

---

## Key Differences Summary

| Aspect | Dynamic (Software) | Fixed (Hardware) |
|--------|-------------------|------------------|
| **Size** | Unknown at compile-time | Known at compile-time |
| **Type** | `LIST { T }` | `LIST[N, T]` |
| **Growth** | ✅ append, filter, take | ❌ Compile error |
| **Operations** | Runtime iteration | Elaboration-time unrolling |
| **Memory** | Structural sharing | Static allocation |
| **List/map** | Runtime map | N parallel instances |
| **List/fold** | Runtime fold | N-stage sequential chain |

---

## Common Patterns

### Pattern 1: BITS to LIST, Process, Back to BITS

```boon
FUNCTION process_bits(input) {
    input
        |> Bits/to_bool_list()           -- BITS → LIST[N, Bool]
        |> List/map(old, new: old |> Bool/not())  -- Process as LIST
        |> List/to_u_bits()              -- LIST → BITS
}
```

### Pattern 2: Parameterized Hardware Generation

```boon
FUNCTION generate_adders(width) {
    inputs: LIST[width] {  }  -- width adder inputs
        |> List/enumerate()
        |> List/map(old, new: create_adder(index: old.index))
}
```

### Pattern 3: Reactive Software List

```boon
items: LIST {}
    |> List/append(item: new_item_event)
    |> List/filter(item, if: filter_predicate)
    |> List/map(old, new: transform(old))
```

---

## Related Documentation

- [BITS.md](./BITS.md) - Bit-level hardware data
- [LATEST.md](./LATEST.md) - Why LIST is not supported in LATEST
- [MEMORY.md](./MEMORY.md) - Fixed-size indexed storage (alternative to LIST in hardware)

---

**LIST: One type, two modes, infinite possibilities! 🚀**
