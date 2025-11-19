# Queue → UNFOLD + FIFO Migration

**Date**: 2025-01-19
**Status**: Design Decision
**TL;DR**: Queue tried to be two different things. Split into UNFOLD (sequences) and FIFO (hardware buffers).

---

## The Problem with Queue

Queue attempted to unify **two fundamentally different abstractions**:

### 1. Software Lazy Sequences
```boon
// Fibonacci with Queue (old)
LIST { 0, 1 }
    |> Queue/iterate(previous, next: previous |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    })
    |> Queue/take_nth(position: 10)
```

**Intended for:**
- Lazy evaluation
- Infinite sequences
- Pull-based consumption

### 2. Hardware FIFOs
```boon
// CDC FIFO with Queue (old)
cdc_fifo: Queue/bounded(size: 16)
    |> Queue/append(item: fast_data, enable: fast_valid)

slow_data: cdc_fifo |> Queue/take(enable: slow_ready)
```

**Intended for:**
- Clock domain crossing
- Rate matching
- Stateful buffering

### Critical Issues

**1. Conflicting Semantics**
- Software: Pull-based (lazy), functional, immutable
- Hardware: Push/pull stateful, destructive reads, mutable

**2. Multi-Consumer Ambiguity**
```boon
q: Queue/bounded(size: 10, initial: [1,2,3,4,5])
worker1: q |> Queue/take()  // Gets 1? Or same as worker2?
worker2: q |> Queue/take()  // Gets 2? Or 1?
```

In normal Boon, multiple references get **same value**.
For a real queue, they should get **different values**.
**Fundamental conflict with Boon's dataflow model!**

**3. Push vs Pull Conflict**
- Boon is push-based (dataflow, reactive)
- Queue/iterate is pull-based (lazy)
- Mismatch causes complexity

**4. Unclear Execution Model**
- When is Queue/iterate evaluated?
- How does Queue/take work in software?
- What does `enable:` mean outside hardware?

---

## The Solution: Split Into Two Primitives

### UNFOLD - Sequence Generation (Replaces Queue for Software)

**Pure functional, push-based, eager evaluation:**

```boon
// Fibonacci with UNFOLD (new)
FUNCTION fibonacci(n) {
    [prev: 0, current: 1]
        |> UNFOLD times: n { state =>
            [prev: state.current, current: state.prev + state.current]
        }
        |> .current
}
```

**Benefits:**
- ✅ Push-based (fits Boon's model)
- ✅ Eager evaluation (no lazy complexity)
- ✅ Finite by default (explicit bounds)
- ✅ Pure functional (no side effects)
- ✅ Clear semantics (same in HW and SW)
- ✅ No multi-consumer issues (returns List)

**Use for:**
- Fibonacci, factorial, sequences
- Iterative algorithms
- Simulations (Game of Life, etc.)
- Any repeated state transformation

### FIFO - Hardware Buffering (Replaces Queue for Hardware)

**Explicit stateful module, hardware-only:**

```boon
// CDC FIFO with FIFO (new)
cdc_fifo: FIFO {
    depth: 16
    width: 32
    type: "async"
}

// Write side
FIFO/write(cdc_fifo, data: fast_data, enable: fast_valid && !FIFO/full(cdc_fifo))

// Read side
slow_data: FIFO/read(cdc_fifo, enable: slow_ready && !FIFO/empty(cdc_fifo))
```

**Benefits:**
- ✅ Hardware-only (no SW confusion)
- ✅ Explicit side effects
- ✅ Clear stateful semantics
- ✅ Multiple references = same module
- ✅ Standard hardware abstraction

**Use for:**
- Clock domain crossing
- Rate matching
- Interface buffers (UART, SPI, etc.)
- Pipeline decoupling

---

## Migration Guide

### Pattern 1: Fibonacci / Lazy Sequences

**Before (Queue):**
```boon
FUNCTION fibonacci(position) {
    LIST { 0, 1 }
        |> Queue/iterate(previous, next: previous |> WHEN {
            LIST { first, second } => LIST { second, first + second }
        })
        |> Queue/take_nth(position: position)
        |> List/first()
}
```

**After (UNFOLD):**
```boon
FUNCTION fibonacci(n) {
    [prev: 0, current: 1]
        |> UNFOLD times: n { state =>
            [prev: state.current, current: state.prev + state.current]
        }
        |> .current
}
```

**Changes:**
- Use object state instead of list
- `UNFOLD times: n` instead of `Queue/iterate`
- Direct access `.current` instead of `take_nth` + `List/first`
- Simpler, clearer, more efficient

---

### Pattern 2: Generate Sequence (Collect All Values)

**Before (Queue):**
```boon
// Not clear how to do this with Queue
// Maybe Queue/take_nth returns all values?
```

**After (UNFOLD):**
```boon
FUNCTION fibonacci_sequence(n) {
    [prev: 0, current: 1]
        |> UNFOLD times: n, collect: True { state =>
            [prev: state.current, current: state.prev + state.current]
        }
        |> List/map(fn: FUNCTION(s) { s.current })
}

// Returns: [1, 1, 2, 3, 5, 8, 13, 21, 34, 55]
```

**Changes:**
- Use `collect: True` to get all intermediate states
- Map to extract values
- Explicit and clear

---

### Pattern 3: Hardware FIFO

**Before (Queue):**
```boon
uart_fifo: Queue/bounded(size: 8)
    |> Queue/append(item: rx_byte, enable: byte_ready && !fifo_full)

data_out: uart_fifo
    |> Queue/take(enable: processor_read && !fifo_empty)

fifo_full: uart_fifo |> Queue/is_full()
fifo_empty: uart_fifo |> Queue/is_empty()
```

**After (FIFO):**
```boon
uart_fifo: FIFO {
    depth: 8
    width: 8
}

FIFO/write(uart_fifo,
    data: rx_byte,
    enable: byte_ready && !FIFO/full(uart_fifo)
)

data_out: FIFO/read(uart_fifo,
    enable: processor_read && !FIFO/empty(uart_fifo)
)

fifo_full: FIFO/full(uart_fifo)
fifo_empty: FIFO/empty(uart_fifo)
```

**Changes:**
- `FIFO { depth: 8 }` instead of `Queue/bounded(size: 8)`
- `FIFO/write(...)` instead of `Queue/append(...)`
- `FIFO/read(...)` instead of `Queue/take(...)`
- `FIFO/full(...)` instead of `Queue/is_full(...)`
- Explicit function calls (not piped) for side effects

---

### Pattern 4: Clock Domain Crossing

**Before (Queue):**
```boon
cdc_fifo: Queue/bounded(size: 16)
    |> Queue/append(item: fast_data, enable: fast_valid && !fifo_full)

slow_data: cdc_fifo
    |> Queue/take(enable: slow_ready && !fifo_empty)
```

**After (FIFO):**
```boon
cdc_fifo: FIFO {
    depth: 16
    width: 32
    type: "async"  // Async FIFO for CDC
}

FIFO/write(cdc_fifo,
    data: fast_data,
    enable: fast_valid && !FIFO/full(cdc_fifo)
)

slow_data: FIFO/read(cdc_fifo,
    enable: slow_ready && !FIFO/empty(cdc_fifo)
)
```

**Changes:**
- Add `type: "async"` for CDC FIFOs
- Explicit FIFO/write and FIFO/read
- Clearer that this is a hardware module

---

### Pattern 5: Iterative Algorithms

**Before (Queue):**
```boon
// Not really possible with Queue
// Queue was for infinite sequences, not finite iterations
```

**After (UNFOLD):**
```boon
// Newton's method for square root
FUNCTION sqrt_approx(n, iterations) {
    n / 2  // Initial guess
        |> UNFOLD times: iterations { guess =>
            (guess + n / guess) / 2
        }
}

// Factorial
FUNCTION factorial(n) {
    [count: 1, product: 1]
        |> UNFOLD times: n { state =>
            [count: state.count + 1, product: state.product * state.count]
        }
        |> .product
}
```

**Changes:**
- UNFOLD is perfect for bounded iterations
- Clear, simple, efficient

---

## Conceptual Comparison

| Aspect | Queue (Old) | UNFOLD (New) | FIFO (New) |
|--------|------------|--------------|------------|
| **Purpose** | Sequences + Buffers | Sequences | Buffers |
| **Scope** | HW and SW (confused) | HW and SW | HW only |
| **Model** | Pull-based | Push-based | Stateful |
| **Evaluation** | Lazy | Eager | N/A |
| **Finiteness** | Unbounded | Bounded | Bounded |
| **Side effects** | Unclear | None | Explicit |
| **Multi-consumer** | Ambiguous | Clear (List) | Shared module |
| **Fit with Boon** | Conflicts | Perfect | Natural |

---

## Why This is Better

### 1. Clear Separation of Concerns

**Queue confused two different needs:**
- Sequence generation → Now UNFOLD
- Hardware buffering → Now FIFO

**Each primitive has one clear purpose.**

### 2. Consistent with Boon's Model

**UNFOLD is push-based:**
- Fits Boon's dataflow model
- Eager evaluation (no lazy complexity)
- No conflict with multi-consumer semantics

**FIFO is explicit:**
- Obviously stateful (not functional)
- Side effects are clear
- Hardware-only (no SW confusion)

### 3. Simpler Mental Model

**Queue required understanding:**
- Pull vs push semantics
- Lazy evaluation
- When/how evaluation happens
- Different behavior in HW vs SW
- Multi-consumer sharing (or not?)

**UNFOLD + FIFO are straightforward:**
- UNFOLD: Repeat this N times, get result
- FIFO: Hardware buffer module, read/write

### 4. Better Type Safety

**Queue had unclear types:**
```boon
Queue/take()  // Returns T? Option<T>? Effect<T>?
```

**UNFOLD has clear types:**
```boon
UNFOLD { ... }  // Returns State (collect: False)
UNFOLD { collect: True ... }  // Returns LIST { State }
```

**FIFO has explicit operations:**
```boon
FIFO/write(...)  // Void (side effect)
FIFO/read(...)   // Returns data type
```

### 5. More General Purpose

**Queue was mainly for Fibonacci example.**

**UNFOLD is useful for:**
- Fibonacci, factorial, powers
- Iterative algorithms (Newton's method)
- Simulations (Game of Life)
- Convergence algorithms
- Any repeated transformation

**Much broader applicability!**

---

## What We Lose

**Nothing important!**

**Queue features we're NOT keeping:**
- ❌ Lazy evaluation - Rarely needed, adds complexity
- ❌ Unbounded sequences - Not useful (HW can't do it anyway)
- ❌ Pull-based consumption - Conflicts with Boon's model
- ❌ Software task queues - Use reactive LIST or CHANNEL (future)

**Everything useful is preserved:**
- ✅ Fibonacci and sequences → UNFOLD
- ✅ Hardware FIFOs → FIFO
- ✅ Iterative algorithms → UNFOLD
- ✅ All use cases covered!

---

## Decision Summary

**REMOVE:**
- Queue primitive entirely
- Queue/iterate
- Queue/generate
- Queue/take, Queue/take_nth, Queue/take_while
- Queue/append
- Queue/bounded, Queue/unbounded
- All Queue documentation

**ADD:**
- UNFOLD primitive (HW and SW)
- FIFO primitive (HW only)
- Documentation for both
- Examples for both

**UPDATE:**
- Fibonacci example to use UNFOLD
- Hardware examples to use FIFO
- Remove QUEUE.md
- Add UNFOLD.md and FIFO.md

---

## Next Steps

1. ✅ Design UNFOLD and FIFO (done)
2. ✅ Write documentation (done)
3. ⏳ Update Fibonacci example
4. ⏳ Remove Queue from codebase
5. ⏳ Implement UNFOLD in compiler
6. ⏳ Implement FIFO in compiler
7. ⏳ Update all examples
8. ⏳ Update tutorials

---

## Conclusion

**Queue was ambitious but flawed** - trying to unify two incompatible abstractions.

**UNFOLD + FIFO is clean and clear** - each primitive does one thing well.

**Result:**
- ✅ Simpler mental model
- ✅ Better fit with Boon
- ✅ Clearer semantics
- ✅ More general purpose
- ✅ No ambiguity
- ✅ Easier to learn
- ✅ Easier to implement
- ✅ More beautiful! 🌟

---

**From confusion to clarity! Queue → UNFOLD + FIFO** ✨
