# WHEN vs WHILE: Pattern Matching in Boon

**Date**: 2025-01-19
**Status**: Design Guideline
**Scope**: Software and Hardware pattern matching semantics

---

## Overview

Boon provides two pattern matching constructs with distinct evaluation semantics:
- **WHEN** - Frozen evaluation (evaluated once when input value changes)
- **WHILE** - Flowing dependencies (re-evaluated as dependencies change)

Understanding when to use each makes code clearer and more correct.

### Core Semantic Difference

**WHEN - Frozen Evaluation:**
- Pattern matching and branches are **evaluated once** when the input value changes
- Dependencies inside branches are **captured/frozen** at that moment
- Use for: Pure pattern matching on the input value only

**WHILE - Flowing Dependencies:**
- Pattern matching is **re-evaluated** as dependencies change
- Branches can **access and react to** external dependencies (fields, outer scope)
- Use for: Pattern matching that needs reactive access to dependencies

**Example:**
```boon
-- Record with Bool fields
signals: [reset: rst, enable: en]

-- ❌ WRONG: Fields frozen when record created
signals |> WHEN {
    [reset: True, enable: __] => ...  -- Won't react to rst/en changes!
}

-- ✅ CORRECT: Fields flow reactively
signals |> WHILE {
    [reset: True, enable: __] => ...  -- Reacts to rst/en changes!
}
```

---

## Quick Reference

| Use | For | Example |
|-----|-----|---------|
| **WHILE** | Flowing dependencies (fields, outer scope) | `signals \|> WHILE { [reset: True, __] => ... }` |
| **WHILE** | Bool signals (level-sensitive) | `reset \|> WHILE { True => ..., False => ... }` |
| **WHILE** | Tag matching with dependencies | `filter \|> WHILE { Active => item.completed \|> Bool/not() }` |
| **WHEN** | Pure value matching (no dependencies) | `state \|> WHEN { StateA => StateB, StateB => StateC }` |
| **WHEN** | Outer scope variable comparison | `iteration \|> WHEN { target => result, __ => SKIP }` |

---

## WHEN - Frozen Evaluation (Pure Pattern Matching)

**Semantics:** "When the value **IS** X, do Y" (evaluated once when value changes)

**Use for:**
- ✅ State machine states (pure state transitions, no external dependencies)
- ✅ Tagged union variants
- ✅ Discrete value matching (constants, enums without accessing outer scope)
- ✅ Pattern decomposition (when not accessing dependencies)
- ✅ Comparing against outer scope variables (iteration termination, equality guards)

**Examples:**

```boon
-- State machine transitions
state \|> WHEN {
    Idle => Running
    Running => Paused
    Paused => Running
    Stopped => Idle
}

-- Tag matching
result \|> WHEN {
    Ok(value) => value
    Err(msg) => default_value
}

-- List pattern matching
list \|> WHEN {
    LIST { first, second, rest... } => process(first, second)
    LIST {} => empty_case
}

-- Object pattern matching
point \|> WHEN {
    [x: 0, y: 0] => origin
    [x: x, y: y] => cartesian(x, y)
}
```

### Outer Scope Variable Comparison

When a pattern alias matches an existing variable name from outer scope, WHEN **compares** against that variable's value instead of creating a new binding:

```boon
-- Fibonacci: emit result when iteration equals position
position: 5
state: [iteration: 0, current: 1, ...] |> HOLD state { ... }

state.iteration |> WHEN {
    position => state.current  -- Compares iteration against position (5)
    __ => SKIP                 -- Skip until they match
}
```

**How it works:**
1. Pattern `position` matches the outer scope variable name
2. Instead of binding `state.iteration` to a new `position`, it compares values
3. When `state.iteration == 5`, the pattern matches → emits `state.current`
4. When values differ, pattern fails → falls through to wildcard `__`

This enables iteration termination, equality guards, and comparing computed values against targets without explicit comparison operators in patterns.

---

## WHILE - Flowing Dependencies (Reactive Pattern Matching)

**Semantics:** "**While** condition holds, do X" (re-evaluated as dependencies change)

**Use for:**
- ✅ Record pattern matching (fields are dependencies that need to flow)
- ✅ Bool signals in hardware (reset, enable, clock_enable)
- ✅ Tag/enum matching when branches access outer scope dependencies
- ✅ Any pattern matching that needs to react to changing dependencies
- ✅ Level-sensitive logic

**Examples:**

```boon
-- ✅ Record pattern matching (flowing dependencies)
control_signals: [reset: rst, enable: en]
control_signals \|> WHILE {
    [reset: True, enable: __] => reset_state  -- Fields react to rst/en changes
    [reset: False, enable: True] => active
    __ => idle
}

-- ✅ Tag matching with outer scope dependencies
selected_filter \|> WHILE {
    All => True
    Active => item.completed \|> Bool/not()  -- Accesses item from outer scope
    Completed => item.completed             -- Flowing dependency
}

-- ✅ Hardware reset signal
rst \|> WHILE {
    True => reset_state       -- While reset is asserted
    False => normal_operation  -- While reset is not asserted
}

-- ✅ Combined with state machines
state \|> HOLD state {
    reset \|> WHILE {
        True => InitialState
        False => state \|> WHEN {
            StateA => StateB
            StateB => StateC
        }
    }
}
```

---

## Hardware FSM Example

Perfect example showing both WHEN and WHILE:

```boon
FUNCTION fsm(rst, a) {
    BLOCK {
        state: B \|> HOLD state {
            rst \|> WHILE {                    -- ✅ WHILE: level-sensitive reset
                True => B                     -- While reset high, stay in B
                False => state \|> WHEN {      -- ✅ WHEN: state pattern matching
                    A => C                    -- When in state A, go to C
                    B => D                    -- When in state B, go to D
                    C => a \|> WHILE {         -- ✅ WHILE: level-sensitive input
                        True => D             -- While input high, go to D
                        False => B            -- While input low, go to B
                    }
                    D => A                    -- When in state D, go to A
                }
            }
        }

        -- Combinational output
        b: state \|> WHEN {                    -- ✅ WHEN: state pattern matching
            A => False
            B => True
            C => a                            -- Direct assignment
            D => False
        }

        [b: b]
    }
}
```

**Key observations:**
- `rst \|> WHILE` - Reset is a **signal** (level-sensitive)
- `state \|> WHEN` - State is a **value** (discrete matching)
- `a \|> WHILE` - Input is a **signal** (level-sensitive)

---

## Software Examples

### WHEN for Enums
```boon
-- Event handling
event \|> WHEN {
    Click => handle_click()
    Hover => handle_hover()
    Scroll => handle_scroll()
}
```

### WHILE for Bool Conditions
```boon
-- UI state
is_loading \|> WHILE {
    True => show_spinner()
    False => show_content()
}

-- Feature flags
debug_mode \|> WHILE {
    True => verbose_logging()
    False => normal_logging()
}
```

---

## Record Pattern Matching: Always Use WHILE

**Critical Rule:** When pattern matching on records (objects), **always use WHILE**.

**Why?** Record fields are dependencies that need to flow reactively.

```boon
-- ❌ WRONG: WHEN freezes field values
signals: [reset: rst, enable: en, load: ld]
signals |> WHEN {
    [reset: True, enable: __, load: __] => ...  -- Won't react to rst/en/ld changes!
}

-- ✅ CORRECT: WHILE allows fields to flow
signals |> WHILE {
    [reset: True, enable: __, load: __] => ...  -- Reacts to rst/en/ld changes!
}
```

**How it works:**
1. Record is created: `signals: [reset: rst, enable: en, load: ld]`
2. With WHEN: Fields are frozen at creation time - no reactivity
3. With WHILE: Fields flow through - pattern re-evaluated as rst/en/ld change

**This applies to:**
- ✅ Hardware control signal bundles: `[reset: rst, enable: en, ...]`
- ✅ Software filter/config patterns: `[active: is_active, filter: current_filter]`
- ✅ Any record where you need to react to field value changes

---

## Decision Tree

```
Does pattern matching access dependencies (record fields or outer scope)?
│
├─ YES → Use WHILE
│  │
│  ├─ Record pattern? ([reset: True, ...])
│  ├─ Branches access outer scope? (item.completed in branch)
│  ├─ Bool signal? (reset, enable)
│  └─ Tag with dependencies? (Active => item.completed)
│
└─ NO → Use WHEN
   │
   ├─ Pure state transitions? (StateA => StateB)
   ├─ Constant mapping? (Red => 0xFF0000)
   ├─ Compare against outer scope variable? (iteration |> WHEN { target => ... })
   └─ No external dependencies in branches
```

---

## Common Patterns

### Pattern 1: FSM with Reset
```boon
state \|> HOLD state {
    reset \|> WHILE {              -- WHILE: signal check
        True => InitialState
        False => state \|> WHEN {   -- WHEN: state match
            StateA => StateB
            StateB => StateC
        }
    }
}
```

### Pattern 2: Conditional State Transition
```boon
state \|> HOLD state {
    state \|> WHEN {                      -- WHEN: state match
        StateA => input \|> WHILE {        -- WHILE: signal check
            True => StateB
            False => StateA
        }
        StateB => StateC
    }
}
```

### Pattern 3: Multi-Signal Control
```boon
state \|> HOLD state {
    reset \|> WHILE {                     -- WHILE: reset signal
        True => InitialState
        False => enable \|> WHILE {        -- WHILE: enable signal
            True => state \|> WHEN {       -- WHEN: state match
                Running => process()
                Idle => waiting()
            }
            False => Disabled
        }
    }
}
```

---

## Anti-Patterns

### ❌ DON'T: Use WHEN for record patterns
```boon
-- Bad: Record fields won't react!
control: [reset: rst, enable: en]
control \|> WHEN {
    [reset: True, enable: __] => ...  -- Fields frozen, no reactivity!
}
```

### ✅ DO: Use WHILE for record patterns
```boon
-- Good: Record fields flow reactively
control: [reset: rst, enable: en]
control \|> WHILE {
    [reset: True, enable: __] => ...  -- Fields react to changes!
}
```

### ❌ DON'T: Use WHEN when branches access dependencies
```boon
-- Bad: item.completed is a flowing dependency
filter \|> WHEN {
    Active => item.completed \|> Bool/not()  -- Won't react to item changes!
}
```

### ✅ DO: Use WHILE when branches access dependencies
```boon
-- Good: Branches can access outer scope
filter \|> WHILE {
    Active => item.completed \|> Bool/not()  -- Reacts to item changes!
}
```

### ✅ DO: Use WHEN for pure state transitions
```boon
-- Good: No dependencies, pure state mapping
state \|> WHEN {
    StateA => StateB  -- Pure transition, no external dependencies
    StateB => StateC
}
```

---

## Transpilation to SystemVerilog

### WHILE for Bool signals
```boon
rst \|> WHILE {
    True => reset_state
    False => normal_state
}
```

**Transpiles to:**
```systemverilog
if (rst)
    next_state = reset_state;
else
    next_state = normal_state;
```

### WHEN for state matching
```boon
state \|> WHEN {
    StateA => StateB
    StateB => StateC
}
```

**Transpiles to:**
```systemverilog
case (state)
    StateA: next_state = StateB;
    StateB: next_state = StateC;
endcase
```

---

## Key Takeaways

1. **WHILE = Flowing dependencies** (reactive evaluation)
   - Pattern matching and branches **re-evaluate** as dependencies change
   - Use for: Record patterns, Bool signals, tag matching with outer scope access
   - Example: `signals \|> WHILE { [reset: True, __] => ... }` - fields flow reactively

2. **WHEN = Frozen evaluation** (static pattern matching)
   - Pattern matching and branches **evaluated once** when input value changes
   - Use for: Pure state transitions, constant mappings, no external dependencies
   - Example: `state \|> WHEN { StateA => StateB }` - pure mapping
   - **Outer scope comparison**: Pattern aliases matching existing variable names compare values
   - Example: `iteration \|> WHEN { target => result, __ => SKIP }` - matches when `iteration == target`

3. **Critical rule for records**: Always use WHILE for record pattern matching
   - Record fields are dependencies that need to flow
   - WHEN would freeze field values - breaking reactivity
   - `[reset: rst, enable: en] \|> WHILE { ... }` ✅
   - `[reset: rst, enable: en] \|> WHEN { ... }` ❌

4. **Branching with dependencies**: Use WHILE when branches access outer scope
   - `filter \|> WHILE { Active => item.completed \|> Bool/not() }` ✅
   - Branches can reference variables from enclosing scope

5. **Visual clarity**: The right keyword makes intent obvious
   - `reset \|> WHILE { True => ... }` - reactive to signal changes
   - `state \|> WHEN { Idle => Running }` - pure state transition

---

## Related Documentation

- [LATEST.md](./LATEST.md) - Reactive state with LATEST
- [BITS.md](./BITS.md) - Hardware bit manipulation
- [hw_examples/fsm.bn](../../playground/frontend/src/examples/hw_examples/fsm.bn) - Reference FSM implementation

---

**WHEN vs WHILE: Clear semantics for clear code! 🎯**
