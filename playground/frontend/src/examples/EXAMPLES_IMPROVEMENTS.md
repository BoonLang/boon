# Examples Improvement Analysis

**Date**: 2025-01-18
**Context**: Analysis of all Boon examples with updated LATEST, QUEUE, and BITS semantics

---

## Executive Summary

After reviewing all examples with the new LATEST piped syntax (`value |> LATEST param { ... }`), QUEUE semantics, and BITS patterns, here are the key findings:

**✅ Perfect Examples (No Changes Needed):**
- fibonacci.bn - Queue/iterate is correct for lazy sequences
- alu.bn - Pure combinational logic
- prio_encoder.bn - Pattern matching with LIST { Bool }
- fulladder.bn - Pure combinational Boolean logic
- serialadder.bn - List/fold for hardware generation
- latest.bn - Good demo of LATEST event merging

**🔄 Should Update (Use Piped LATEST):**
- counter.bn (software) - Use `0 |> LATEST count { ... }`
- complex_counter.bn (software) - Use piped LATEST
- interval.bn (software) - Use piped LATEST
- then.bn (software) - Use piped LATEST
- when.bn (software) - Use piped LATEST for counter
- lfsr.bn (hardware) - Use piped LATEST
- fsm.bn (hardware) - Use piped LATEST
- ram.bn (hardware) - Use piped LATEST

**⚡ Could Enhance (Show Alternative Patterns):**
- counter.bn (hardware) - Show piped LATEST alternative to Bits/sum
- cycleadder_arst.bn (hardware) - Show piped LATEST alternative

---

## Software Examples (UI/Frontend)

### 1. counter.bn

**Current Pattern:**
```boon
counter:
    LATEST {
        0
        increment_button.event.press |> THEN { 1 }
    }
    |> Math/sum()
```

**Improvement:** Use piped LATEST with self-reference
```boon
counter: 0 |> LATEST count {
    increment_button.event.press |> THEN { count + 1 }
}
```

**Why Better:**
- More explicit: shows initial value and current value
- More direct: no Math/sum indirection
- More teachable: "count becomes count + 1" is clearer than "sum deltas"
- Matches new LATEST.md documentation

**Impact:** High - This is a foundational example

---

### 2. complex_counter.bn

**Current Pattern:**
```boon
counter:
    LATEST {
        0
        elements.decrement_button.event.press |> THEN { -1 }
        elements.increment_button.event.press |> THEN { 1 }
    }
    |> Math/sum()
```

**Improvement:** Use piped LATEST
```boon
counter: 0 |> LATEST count {
    elements.decrement_button.event.press |> THEN { count - 1 }
    elements.increment_button.event.press |> THEN { count + 1 }
}
```

**Why Better:**
- Shows decrement/increment operations directly
- No delta accumulation abstraction
- More intuitive for beginners

**Impact:** High - Shows multi-button pattern

---

### 3. interval.bn

**Current Pattern:**
```boon
document:
    Duration[seconds: 1]
    |> Timer/interval()
    |> THEN { 1 }
    |> Math/sum()
    |> Document/new()
```

**Improvement:** Use piped LATEST
```boon
counter: 0 |> LATEST count {
    Duration[seconds: 1] |> Timer/interval() |> THEN { count + 1 }
}

document: counter |> Document/new()
```

**Why Better:**
- Explicit counter variable with name
- Clear separation of state and view
- More explicit initial value

**Impact:** Medium - Shows timer integration

---

### 4. then.bn

**Current Pattern:**
```boon
FUNCTION sum_of_steps(step, seconds) {
    Duration[seconds: seconds]
    |> Timer/interval()
    |> THEN { step }
    |> Math/sum()
}
```

**Improvement:** Use piped LATEST
```boon
FUNCTION sum_of_steps(step, seconds) {
    0 |> LATEST sum {
        Duration[seconds: seconds]
        |> Timer/interval()
        |> THEN { sum + step }
    }
}
```

**Why Better:**
- Shows accumulation pattern explicitly
- More teachable: "add step to current sum"

**Impact:** Medium - Shows function with state

---

### 5. when.bn

**Current Pattern:**
```boon
FUNCTION sum_of_steps(step, seconds) {
    Duration[seconds: seconds]
    |> Timer/interval()
    |> THEN { step }
    |> Math/sum()
}
```

**Improvement:** Same as then.bn - use piped LATEST

**Impact:** Medium

---

### 6. latest.bn

**Status:** ✅ PERFECT - No changes needed

**Why:**
- Demonstrates LATEST event merging (multiple events into one value)
- Good teaching example for "latest value wins"
- Shows Math/sum as example of stateful operation

**Keep as-is** for demonstrating the simple LATEST form

---

### 7. fibonacci.bn

**Status:** ✅ PERFECT - No changes needed

**Why:**
- Queue/iterate is exactly right for lazy sequences
- Shows when to use Queue vs LATEST
- Demonstrates pull-based evaluation
- This is the canonical example from docs

**Keep as-is** - it's a reference implementation

---

## Hardware Examples (FPGA/ASIC)

### 8. hw_examples/counter.bn

**Current Pattern:** Uses Bits/sum with delta accumulation
```boon
count: default
    |> Bits/set(control_signals |> WHEN {
        [reset: True, load: __, up: __, enabled: __] => default
        __ => SKIP
    })
    |> Bits/set(control_signals |> WHEN {
        [reset: False, load: True, up: __, enabled: True] => load_value
        __ => SKIP
    })
    |> Bits/sum(delta: control_signals |> WHEN {
        [reset: False, load: False, up: True, enabled: True] =>
            BITS { count_width, 10s1 }
        [reset: False, load: False, up: False, enabled: True] =>
            BITS { count_width, 10s-1 }
        __ => SKIP
    })
```

**Status:** ✅ GOOD - But could ADD alternative

**Recommendation:** Keep current version, but ADD commented alternative showing piped LATEST:

```boon
-- Alternative: Using LATEST with self-reference (equivalent to above)
-- Commented out: For educational comparison
--
-- count: default |> LATEST count {
--     control_signals |> WHEN {
--         [reset: True, load: __, up: __, enabled: __] => default
--         [reset: False, load: True, up: __, enabled: True] => load_value
--         [reset: False, load: False, up: True, enabled: True] => count |> Bits/increment()
--         [reset: False, load: False, up: False, enabled: True] => count |> Bits/decrement()
--         __ => count  -- Hold
--     }
-- }
```

**Why Keep Both:**
- Bits/sum pattern is more declarative (truth table with deltas)
- Piped LATEST is more imperative (next state from current)
- Both valid hardware patterns - show both approaches
- Bits/sum better matches README.md documentation

**Impact:** Medium - Educational value in showing alternatives

---

### 9. hw_examples/cycleadder_arst.bn

**Current Pattern:** Uses Bits/sum (good!)

**Status:** ✅ PERFECT for its purpose

**Could Add:** Commented alternative with piped LATEST (similar to counter.bn)

**Impact:** Low - Simple accumulator example

---

### 10. hw_examples/lfsr.bn

**Current Pattern:**
```boon
out: LATEST {
    BITS { 8, 10u0 }  -- Reset value

    reset |> WHEN {
        True => BITS { 8, 10u0 }
        False => BLOCK {
            feedback: out |> Bits/get(index: 7) |> Bool/xor(
                out |> Bits/get(index: 3)
            ) |> Bool/not()

            out
                |> Bits/shift_right(by: 1)
                |> Bits/set(index: 7, value: feedback)
        }
    }
}
```

**Problem:** Uses `out` inside LATEST body (old self-reference pattern)

**Improvement:** Use piped LATEST
```boon
out: BITS { 8, 10u0 } |> LATEST out {
    reset |> WHEN {
        True => BITS { 8, 10u0 }
        False => BLOCK {
            feedback: out |> Bits/get(index: 7) |> Bool/xor(
                out |> Bits/get(index: 3)
            ) |> Bool/not()

            out
                |> Bits/shift_right(by: 1)
                |> Bits/set(index: 7, value: feedback)
        }
    }
}
```

**Why Better:**
- Explicit initial value: `BITS { 8, 10u0 } |>`
- Explicit parameter: `|> LATEST out {`
- Matches new LATEST.md documentation
- Shows non-self-reactive semantics clearly

**Impact:** High - Important example of transformation pattern

---

### 11. hw_examples/fsm.bn

**Current Pattern:**
```boon
state: LATEST {
    B  -- Reset state

    rst |> WHEN {
        True => B
        False => state |> WHEN {
            A => C
            B => D
            C => a |> WHEN { True => D, False => B }
            D => A
        }
    }
}
```

**Problem:** Uses `state` inside LATEST body (old pattern)

**Improvement:** Use piped LATEST
```boon
state: B |> LATEST state {
    rst |> WHEN {
        True => B
        False => state |> WHEN {
            A => C
            B => D
            C => a |> WHEN { True => D, False => B }
            D => A
        }
    }
}
```

**Why Better:**
- Explicit initial state: `B |>`
- Explicit parameter: `|> LATEST state {`
- More symmetric with counter pattern
- Shows FSM pattern with new syntax

**Impact:** High - FSM is fundamental hardware pattern

---

### 12. hw_examples/ram.bn

**Current Pattern:**
```boon
mem: LATEST {
    List/range(start: 0, end: 16)  -- 2^4 = 16 entries
    mem |> List/set(index: wraddr, value: wrdata)
}
```

**Problem:** Uses `mem` inside LATEST body (old pattern)

**Improvement:** Use piped LATEST
```boon
mem: List/range(start: 0, end: 16) |> LATEST mem {
    mem |> List/set(index: wraddr, value: wrdata)
}
```

**Why Better:**
- Explicit initial value
- Explicit parameter
- Consistent with other examples
- Shows memory update pattern clearly

**Impact:** High - RAM is common hardware pattern

---

### 13. hw_examples/alu.bn

**Status:** ✅ PERFECT - No changes needed

**Why:**
- Pure combinational logic
- No state, no LATEST needed
- Perfect use of pattern matching
- Great showcase of BITS operations

**Keep as-is**

---

### 14. hw_examples/prio_encoder.bn

**Status:** ✅ PERFECT - No changes needed

**Why:**
- Perfect demonstration of LIST { Bool } pattern matching
- Shows wildcard patterns elegantly
- Matches BITS.md documentation
- Pure combinational

**Keep as-is**

---

### 15. hw_examples/fulladder.bn

**Status:** ✅ PERFECT - No changes needed

**Why:**
- Pure Boolean combinational logic
- Shows function composition
- Clean pedagogical example

**Keep as-is**

---

### 16. hw_examples/serialadder.bn

**Status:** ✅ PERFECT - No changes needed

**Why:**
- Great use of List/fold for hardware generation
- Shows carry propagation
- Demonstrates parameterized hardware

**Keep as-is**

---

## Summary of Changes

### High Priority (Update Now)

**Software:**
1. counter.bn - Use piped LATEST
2. complex_counter.bn - Use piped LATEST
3. interval.bn - Use piped LATEST

**Hardware:**
1. lfsr.bn - Use piped LATEST
2. fsm.bn - Use piped LATEST
3. ram.bn - Use piped LATEST

### Medium Priority (Update Soon)

**Software:**
1. then.bn - Use piped LATEST
2. when.bn - Use piped LATEST

### Low Priority (Enhancement)

**Hardware:**
1. counter.bn - Add commented piped LATEST alternative
2. cycleadder_arst.bn - Add commented alternative

### No Changes Needed (Perfect!)

- fibonacci.bn - Queue/iterate
- latest.bn - Event merging demo
- alu.bn - Combinational
- prio_encoder.bn - Pattern matching
- fulladder.bn - Combinational
- serialadder.bn - List/fold generation

---

## Pattern Summary

### LATEST Piped Pattern (New Standard)

**Form:** `initial_value |> LATEST parameter_name { next_value_expression }`

**Use for:**
- Counters (software and hardware)
- FSMs (state transitions)
- Accumulators
- RAMs (memory updates)
- LFSRs (transformations)

**Examples:**
```boon
-- Software counter
count: 0 |> LATEST count {
    increment |> THEN { count + 1 }
}

-- Hardware FSM
state: Idle |> LATEST state {
    reset |> WHEN { True => Idle, False => state |> next_state() }
}

-- Hardware RAM
mem: initial_data |> LATEST mem {
    mem |> List/set(index: addr, value: data)
}
```

### Bits/sum Pattern (Keep for Hardware)

**Use for:** Hardware counters with delta accumulation (truth table style)

**Why Keep:**
- More declarative (matches truth table)
- Clear separation of absolute (Bits/set) vs relative (Bits/sum) updates
- Matches current README.md documentation
- Some hardware designers prefer this style

**Example:**
```boon
count: default
    |> Bits/set(control |> WHEN { reset => default, __ => SKIP })
    |> Bits/sum(delta: control |> WHEN { up => +1, down => -1, __ => SKIP })
```

### Queue/iterate Pattern (Keep!)

**Use for:** Lazy sequences, infinite streams

**Example:**
```boon
fibonacci: LIST { 0, 1 }
    |> Queue/iterate(prev, next: prev |> WHEN {
        LIST { a, b } => LIST { b, a + b }
    })
```

---

## BITS Usage Analysis

All hardware examples correctly use BITS:

✅ **counter.bn** - BITS for signed arithmetic (10s1, 10s-1)
✅ **cycleadder_arst.bn** - BITS with parameterized width
✅ **lfsr.bn** - BITS with shift operations (perfect use case!)
✅ **alu.bn** - BITS for all arithmetic/logic ops
✅ **prio_encoder.bn** - Uses LIST { Bool } for pattern matching (correct choice!)

**Key Insight:** Examples show exactly when to use BITS vs LIST { Bool }:
- BITS for arithmetic/shifts (counter, lfsr, alu)
- LIST { Bool } for wildcard patterns (prio_encoder)
- This matches BITS.md decision tree perfectly

---

## Documentation Cross-References

### Examples Demonstrate Concepts From:

1. **LATEST.md** - All counter examples, FSM, LFSR, RAM
2. **QUEUE.md** - fibonacci.bn (Queue/iterate)
3. **BITS.md** - All hw_examples
4. **README.md** (hw_examples) - Counter pattern, two register patterns

### Gaps to Fill:

None! Examples cover all documented patterns. After updates, examples will fully align with documentation.

---

## Next Steps

1. **Phase 1:** Update high-priority software examples (counter, complex_counter, interval)
2. **Phase 2:** Update high-priority hardware examples (lfsr, fsm, ram)
3. **Phase 3:** Update medium-priority (then, when)
4. **Phase 4:** Add commented alternatives (hw counter)
5. **Phase 5:** Update hw_examples/README.md with piped LATEST pattern

---

## Design Consistency Check

After reviewing all examples, the three-way split is well-justified:

- **LATEST** - Reactive state (counters, FSMs, accumulators)
- **QUEUE** - Lazy sequences (fibonacci, streams)
- **BITS** - Hardware bit manipulation (counters, ALU, LFSR)

Each abstraction has clear use cases demonstrated in examples. No overlap or confusion.

✅ Documentation and examples are aligned (after proposed updates).
