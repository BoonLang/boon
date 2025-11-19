# Queue → PULSES Migration Guide

**Date**: 2025-01-19
**Status**: Migration Guide
**Supersedes**: QUEUE_TO_UNFOLD_MIGRATION.md

---

## Executive Summary

**Queue is being replaced by PULSES + LATEST.**

**Why:**
- Queue tried to be two things: lazy sequences AND hardware FIFOs
- Pull-based semantics conflicted with Boon's push-based model
- Multi-consumer semantics were ambiguous
- Only used for Fibonacci example

**New approach:**
- **PULSES + LATEST** - For iteration and sequences
- **FIFO** (future) - For hardware buffers (if needed)

---

## The Problem with Queue

```boon
// Queue tried to do lazy sequences:
LIST { 0, 1 }
    |> Queue/iterate(previous, next: previous |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    })
    |> Queue/take_nth(position: 10)

// Problems:
// - Pull-based (conflicts with Boon's push model)
// - Lazy evaluation (adds complexity)
// - Ambiguous multi-consumer semantics
// - Only ever used for Fibonacci
```

---

## The Solution: PULSES + LATEST

```boon
// PULSES: Counted iteration
PULSES { N }  // Generates N pulses

// Used with LATEST for stateful iteration
result: initial |> LATEST state {
    PULSES { count } |> THEN {
        transform(state)
    }
}
```

**Benefits:**
- ✅ Push-based (fits Boon)
- ✅ Eager evaluation (simpler)
- ✅ Clear semantics
- ✅ Reuses LATEST (no new primitive)
- ✅ Works same in HW and SW

---

## Migration Examples

### **Example 1: Fibonacci**

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

**After (PULSES):**
```boon
FUNCTION fibonacci(n) {
    BLOCK {
        final: [prev: 0, current: 1] |> LATEST state {
            PULSES { n } |> THEN {
                [prev: state.current, current: state.prev + state.current]
            }
        }
        final.current
    }
}
```

**Changes:**
- Use object state `[prev: 0, current: 1]` instead of list
- Use `PULSES { n }` instead of `Queue/iterate`
- Use LATEST for state management
- Use BLOCK to extract final field
- Simpler, clearer, more direct!

---

### **Example 2: Factorial (Not Possible with Queue)**

**With PULSES:**
```boon
FUNCTION factorial(n) {
    BLOCK {
        final: [count: 1, product: 1] |> LATEST state {
            PULSES { n } |> THEN {
                [count: state.count + 1, product: state.product * state.count]
            }
        }
        final.product
    }
}
```

PULSES is more general than Queue - works for any iteration!

---

### **Example 3: Powers of 2**

**With Queue (awkward):**
```boon
// Not really expressible with Queue
```

**With PULSES:**
```boon
FUNCTION power_of_2(n) {
    1 |> LATEST value {
        PULSES { n } |> THEN { value * 2 }
    }
}
```

---

### **Example 4: Hardware Counter**

**Before (Queue - confusing):**
```boon
counter: Queue/bounded(size: 10)
    |> Queue/append(item: clk, enable: enable)
// What does this even mean?
```

**After (PULSES - clear):**
```boon
counter: BITS{8, 0} |> LATEST count {
    clk |> THEN {
        PULSES { 10 } |> THEN {
            enable |> WHEN {
                True => count + 1
                False => SKIP
            }
        }
    }
}
```

---

## Migration Checklist

### **Step 1: Find Queue Usage**

Search codebase for:
- `Queue/iterate`
- `Queue/generate`
- `Queue/bounded`
- `Queue/take`
- `Queue/take_nth`

### **Step 2: Determine Pattern**

**If using Queue for sequences (like Fibonacci):**
→ Migrate to PULSES + LATEST

**If using Queue for hardware buffers:**
→ Wait for FIFO primitive (or use other approach)

### **Step 3: Rewrite with PULSES**

**Pattern:**
```boon
// Old:
value |> Queue/iterate(state, next: transform(state))
    |> Queue/take_nth(n)

// New:
BLOCK {
    final: value |> LATEST state {
        PULSES { n } |> THEN { transform(state) }
    }
    final  // Or extract field: final.field
}
```

### **Step 4: Test**

Verify same results with PULSES implementation.

### **Step 5: Remove Queue**

After all migrations:
- Remove `QUEUE.md`
- Remove Queue examples
- Remove Queue from compiler

---

## Common Patterns

### **Pattern 1: Simple State Transformation**

**Queue:**
```boon
value |> Queue/iterate(state, next: transform(state))
    |> Queue/take_nth(n)
```

**PULSES:**
```boon
value |> LATEST state {
    PULSES { n } |> THEN { transform(state) }
}
```

---

### **Pattern 2: Complex State with Fields**

**Queue:**
```boon
LIST { initial_a, initial_b }
    |> Queue/iterate(state, next: state |> WHEN {
        LIST { a, b } => LIST { transform_a(a, b), transform_b(a, b) }
    })
    |> Queue/take_nth(n)
    |> List/first()
```

**PULSES:**
```boon
BLOCK {
    final: [a: initial_a, b: initial_b] |> LATEST state {
        PULSES { n } |> THEN {
            [a: transform_a(state.a, state.b), b: transform_b(state.a, state.b)]
        }
    }
    final.a  // Or final.b, or entire final
}
```

---

### **Pattern 3: Collect All Intermediate States**

**Queue (not clear):**
```boon
// How to get all intermediate states?
```

**PULSES:**
```boon
BLOCK {
    all_states: LIST {} |> LATEST states {
        PULSES { n } |> THEN {
            states |> List/push(compute_next_state(states |> List/last()))
        }
    }
    all_states
}
```

Or track in state:
```boon
BLOCK {
    final: [current: initial, history: LIST { initial }] |> LATEST state {
        PULSES { n } |> THEN {
            next: transform(state.current)
            [current: next, history: state.history |> List/push(next)]
        }
    }
    final.history
}
```

---

## Conceptual Differences

| Aspect | Queue | PULSES + LATEST |
|--------|-------|-----------------|
| **Model** | Pull-based | Push-based |
| **Evaluation** | Lazy | Eager |
| **State** | Implicit | Explicit (LATEST) |
| **Bounds** | Unbounded (default) | Bounded (always) |
| **Multi-consumer** | Ambiguous | Clear (single result) |
| **Hardware** | Unclear | Clear (counter + FSM) |
| **Composability** | Limited | High (mix with events) |

---

## Benefits of Migration

### **1. Simpler Mental Model**

**Queue required understanding:**
- Pull vs push semantics
- Lazy evaluation
- When evaluation happens
- Take vs iterate vs generate

**PULSES requires understanding:**
- PULSES generates N pulses
- LATEST updates on each pulse
- That's it!

### **2. More Powerful**

**Queue was limited to sequences.**

**PULSES + LATEST can:**
- Iterate any transformation
- Mix with other events
- Early termination (SKIP)
- Work in HW and SW
- Compose with entire LATEST ecosystem

### **3. Clearer Code**

**Queue (nested, complex):**
```boon
LIST { 0, 1 }
    |> Queue/iterate(previous, next: previous |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    })
    |> Queue/take_nth(position: 10)
    |> List/first()
```

**PULSES (direct, clear):**
```boon
BLOCK {
    final: [prev: 0, current: 1] |> LATEST state {
        PULSES { 10 } |> THEN {
            [prev: state.current, current: state.prev + state.current]
        }
    }
    final.current
}
```

---

## What About Hardware FIFOs?

**Queue also tried to be hardware FIFOs:**
```boon
fifo: Queue/bounded(size: 16)
    |> Queue/append(item: data, enable: valid)
```

**This is being replaced by dedicated FIFO primitive:**
```boon
fifo: FIFO {
    depth: 16
    width: 32
}

FIFO/write(fifo, data: data, enable: valid)
data_out: FIFO/read(fifo, enable: ready)
```

**See FIFO.md for details** (when implemented).

---

## Timeline

1. ✅ Design PULSES (done)
2. ✅ Write PULSES.md (done)
3. ⏳ Implement PULSES in compiler
4. ⏳ Update Fibonacci example
5. ⏳ Migrate any other Queue usage
6. ⏳ Remove Queue from language
7. ⏳ Archive QUEUE.md as deprecated

---

## Questions?

**Q: Can I still use Queue?**
A: For now, yes. But it's deprecated and will be removed.

**Q: What if I need lazy sequences?**
A: PULSES is eager but bounded. For most use cases, this is better. If you truly need lazy, open an issue.

**Q: What about hardware FIFOs?**
A: Coming soon as dedicated FIFO primitive.

**Q: Is PULSES slower than Queue?**
A: No! PULSES is eager (faster in most cases). Queue's lazy evaluation added overhead.

**Q: Can PULSES do everything Queue could?**
A: Yes, and more! PULSES is more general and composable.

---

## Summary

**Migration is simple:**

```boon
// Before: Queue
LIST { 0, 1 }
    |> Queue/iterate(prev, next: transform(prev))
    |> Queue/take_nth(n)

// After: PULSES
BLOCK {
    final: [prev: 0, current: 1] |> LATEST state {
        PULSES { n } |> THEN { transform(state) }
    }
    final.current
}
```

**Benefits:**
- ✅ Clearer semantics
- ✅ More powerful
- ✅ Better composability
- ✅ Same in HW and SW
- ✅ Fits Boon's model

**Welcome to PULSES!** ⚡
