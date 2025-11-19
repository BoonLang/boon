# PULSES: Counted Iteration in Boon

**Date**: 2025-01-19
**Status**: Language Design
**Replaces**: Queue (for sequences), UNFOLD

---

## Executive Summary

PULSES generates **counted events** to drive iteration in LATEST.

**Core idea:**
```boon
PULSES { N }  // Generates N pulses: [], [], [], ... (N times)
```

**Used with LATEST for iteration:**
```boon
result: initial |> LATEST state {
    PULSES { 10 } |> THEN {
        transform(state)
    }
}
// Transforms state 10 times
```

**Key principles:**
- **Counted iteration** - Explicit number of pulses (no infinite sequences)
- **Unique to Boon** - Not overloaded with clock ticks or event loop ticks
- **Works with LATEST** - Drives state updates through iteration
- **Same syntax, different execution** - Loop in SW, counter in HW
- **Simple and safe** - Bounded by default, no infinite loops
- **Filter intermediate values** - Track iteration in state, use WHEN to filter

**Replaces:**
- Queue/iterate (for sequences like Fibonacci)
- UNFOLD (iteration primitive)
- REPEAT (too ambiguous)

---

## Table of Contents

1. [Quick Start](#quick-start)
2. [Core Concept](#core-concept)
3. [Syntax](#syntax)
4. [How PULSES Works](#how-pulses-works)
5. [Integration with LATEST](#integration-with-latest)
6. [Examples](#examples)
7. [Hardware Context](#hardware-context)
8. [Software Context](#software-context)
9. [Common Patterns](#common-patterns)
10. [Comparison with Alternatives](#comparison-with-alternatives)
11. [Design Rationale](#design-rationale)

---

## Quick Start

### **Fibonacci**
```boon
FUNCTION fibonacci(position) {
    BLOCK {
        state: [previous: 0, current: 1, iteration: 0] |> LATEST state {
            PULSES { position } |> THEN {
                [
                    previous: state.current,
                    current: state.previous + state.current,
                    iteration: state.iteration + 1
                ]
            }
        }
        // Only emit when iteration reaches target position
        state.iteration = position |> WHEN {
            True => state.current
            False => SKIP
        }
    }
}

fibonacci(10)  // Returns 55
```

### **Factorial**
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

factorial(5)  // Returns 120
```

### **Powers of 2**
```boon
FUNCTION power_of_2(n) {
    1 |> LATEST value {
        PULSES { n } |> THEN { value * 2 }
    }
}

power_of_2(10)  // Returns 1024
```

---

## Core Concept

**PULSES is an event source that fires N times:**

```
PULSES { 5 }  →  [], [], [], [], []
             ↓   ↓   ↓   ↓   ↓
          pulse pulse pulse pulse pulse
             1    2    3    4    5
```

**Each pulse:**
- Is a unit value `[]` (empty object)
- Triggers LATEST to update
- Counts toward the total N

**After N pulses:**
- Stops firing
- LATEST holds final state

---

## Syntax

### **Basic Form**
```boon
PULSES { N }
```

Where:
- `N` is the number of pulses (integer)
- Can be literal: `PULSES { 10 }`
- Can be variable: `PULSES { count }`
- Can be expression: `PULSES { n * 2 }`

### **Type**
```boon
PULSES { N }  // Type: Event source producing [] (unit values)
```

### **Used with LATEST**
```boon
value: initial |> LATEST state {
    PULSES { N } |> THEN {
        expression
    }
}
```

---

## How PULSES Works

### **Software Execution**

```boon
result: 0 |> LATEST count {
    PULSES { 10 } |> THEN { count + 1 }
}
```

**Compiles to:**
```javascript
let count = 0;
for (let __pulse = 0; __pulse < 10; __pulse++) {
    count = count + 1;
}
// result = 10
```

**Execution model:**
1. Initialize state: `count = 0`
2. Loop 10 times:
   - Pulse fires (value: `[]`)
   - THEN expression evaluates: `count + 1`
   - State updates: `count = 1, 2, 3, ...`
3. Return final state: `10`

### **Hardware Execution**

```boon
counter: BITS{8, 0} |> LATEST count {
    clk |> THEN {
        PULSES { 10 } |> THEN { count + 1 }
    }
}
```

**Compiles to:**
```systemverilog
logic [3:0] pulse_counter;
logic [7:0] count;

always_ff @(posedge clk) begin
    if (pulse_counter < 10) begin
        count <= count + 1;
        pulse_counter <= pulse_counter + 1;
    end
end
```

**Execution model:**
1. Initialize: `pulse_counter = 0, count = 0`
2. Every clock edge:
   - Check: `pulse_counter < 10`?
   - If yes: pulse fires, count increments, pulse_counter increments
   - If no: stop (done)
3. Final: `count = 10, pulse_counter = 10`

**Key difference:**
- Software: Synchronous loop (blocks until done)
- Hardware: Asynchronous counter (counts over clock cycles)

---

## Integration with LATEST

PULSES is an **event source** that works with LATEST:

### **Pattern:**
```boon
result: initial_value |> LATEST state_name {
    PULSES { count } |> THEN {
        state_transformation
    }
}
```

### **How it works:**
1. **Initial state:** `state = initial_value`
2. **PULSES fires:** First pulse (value: `[]`)
3. **THEN triggers:** Expression evaluates with current state
4. **State updates:** New value assigned to state
5. **Repeat:** Until N pulses fired
6. **Result:** Final state value

### **Multiple events in LATEST:**
```boon
state: initial |> LATEST state {
    // Automatic pulses
    PULSES { 100 } |> THEN {
        state |> transform()
    }

    // Manual reset
    reset_button.click |> THEN {
        initial
    }
}
```

PULSES can coexist with other events!

---

## Examples

### **1. Fibonacci (with iteration tracking)**
```boon
FUNCTION fibonacci(position) {
    BLOCK {
        state: [previous: 0, current: 1, iteration: 0] |> LATEST state {
            PULSES { position } |> THEN {
                [
                    previous: state.current,
                    current: state.previous + state.current,
                    iteration: state.iteration + 1
                ]
            }
        }
        // Filter: only emit when complete
        state.iteration = position |> WHEN {
            True => state.current
            False => SKIP
        }
    }
}

// Usage
fib_10: fibonacci(10)  // 55 (emits once when iteration = 10)
fib_20: fibonacci(20)  // 6765 (emits once when iteration = 20)
```

### **2. Factorial**
```boon
FUNCTION factorial(n) {
    BLOCK {
        final: [count: 1, product: 1] |> LATEST state {
            PULSES { n } |> THEN {
                [
                    count: state.count + 1,
                    product: state.product * state.count
                ]
            }
        }
        final.product
    }
}

factorial(5)  // 120
factorial(10) // 3628800
```

### **3. Sum 1 to N**
```boon
FUNCTION sum_to_n(n) {
    BLOCK {
        result: [index: 1, sum: 0] |> LATEST state {
            PULSES { n } |> THEN {
                [index: state.index + 1, sum: state.sum + state.index]
            }
        }
        result.sum
    }
}

sum_to_n(10)  // 55 (1+2+3+...+10)
```

### **4. Powers of 2 (simple state)**
```boon
FUNCTION power_of_2(n) {
    1 |> LATEST value {
        PULSES { n } |> THEN { value * 2 }
    }
}

power_of_2(0)  // 1
power_of_2(5)  // 32
power_of_2(10) // 1024
```

### **5. Newton's Method (with early stop)**
```boon
FUNCTION sqrt_approx(n, max_iterations) {
    BLOCK {
        final: [guess: n / 2, prev: 0] |> LATEST state {
            PULSES { max_iterations } |> THEN {
                // Stop if converged
                Math/abs(state.guess - state.prev) < 0.0001 |> WHEN {
                    True => SKIP
                    False => [
                        guess: (state.guess + n / state.guess) / 2,
                        prev: state.guess
                    ]
                }
            }
        }
        final.guess
    }
}

sqrt_approx(2, 10)   // 1.414...
sqrt_approx(100, 10) // 10.0
```

### **6. Countdown**
```boon
FUNCTION countdown(start) {
    start |> LATEST count {
        PULSES { start } |> THEN { count - 1 }
    }
}

countdown(10)  // 0
countdown(5)   // 0
```

### **7. Accumulate Values**
```boon
FUNCTION accumulate_values(values) {
    0 |> LATEST sum {
        PULSES { values |> List/length() } |> THEN {
            sum + (values |> List/at(sum))  // Use sum as index
        }
    }
}

accumulate_values(LIST { 1, 2, 3, 4, 5 })  // 15
```

Wait, that's wrong - sum is not the index. Let me fix:

```boon
FUNCTION accumulate_values(values) {
    BLOCK {
        final: [index: 0, sum: 0] |> LATEST state {
            PULSES { values |> List/length() } |> THEN {
                [
                    index: state.index + 1,
                    sum: state.sum + (values |> List/at(state.index))
                ]
            }
        }
        final.sum
    }
}
```

### **8. Compound Interest**
```boon
FUNCTION compound_interest(principal, rate, years) {
    principal |> LATEST amount {
        PULSES { years } |> THEN {
            amount * (1 + rate)
        }
    }
}

compound_interest(1000, 0.05, 10)  // 1628.89
```

### **9. Collatz Sequence (with early termination)**
```boon
FUNCTION collatz_steps(n, max_steps) {
    BLOCK {
        final: [value: n, steps: 0] |> LATEST state {
            PULSES { max_steps } |> THEN {
                state.value == 1 |> WHEN {
                    True => SKIP  // Already at 1
                    False => [
                        value: state.value % 2 == 0 |> WHEN {
                            True => state.value / 2
                            False => state.value * 3 + 1
                        },
                        steps: state.steps + 1
                    ]
                }
            }
        }
        final.steps
    }
}

collatz_steps(13, 100)  // Number of steps to reach 1
```

### **10. Simulation Step**
```boon
FUNCTION simulate_physics(config) {
    BLOCK {
        final: [position: 0, velocity: config.v0] |> LATEST state {
            PULSES { config.timesteps } |> THEN {
                [
                    position: state.position + state.velocity * config.dt,
                    velocity: state.velocity + config.gravity * config.dt
                ]
            }
        }
        final.position
    }
}

result: simulate_physics([
    timesteps: 100,
    dt: 0.016,
    v0: 10,
    gravity: -9.8
])
```

---

## Hardware Context

### **PULSES in Hardware Requires clk**

In hardware, LATEST must be driven by clock:

```boon
counter: BITS{8, 0} |> LATEST count {
    clk |> THEN {
        PULSES { 10 } |> THEN {
            count + 1
        }
    }
}
```

**Execution:**
- Every clock edge: `clk` fires
- PULSES checks: `pulse_counter < 10`?
- If yes: pulse fires, count increments
- After 10 pulses: stops updating

**Synthesizes to:**
```systemverilog
logic [3:0] pulse_counter;
logic [7:0] count;

always_ff @(posedge clk) begin
    if (rst) begin
        pulse_counter <= 0;
        count <= 0;
    end else if (pulse_counter < 10) begin
        count <= count + 1;
        pulse_counter <= pulse_counter + 1;
    end
end
```

### **Hardware Examples**

**1. Pulse Counter:**
```boon
FUNCTION pulse_counter_module(max_count) {
    BLOCK {
        count: BITS{8, 0} |> LATEST count {
            clk |> THEN {
                PULSES { max_count } |> THEN {
                    count + 1
                }
            }
        }

        done: count >= max_count

        [count: count, done: done]
    }
}
```

**2. LFSR Iterations:**
```boon
FUNCTION lfsr_n_cycles(initial, cycles) {
    initial |> LATEST lfsr {
        clk |> THEN {
            PULSES { cycles } |> THEN {
                feedback: lfsr |> Bits/get(7) |> Bool/xor(lfsr |> Bits/get(3))
                lfsr |> Bits/shift_right(1) |> Bits/set(7, feedback)
            }
        }
    }
}
```

**3. Fibonacci in Hardware:**
```boon
FUNCTION fibonacci_hw(position) {
    BLOCK {
        state: [
            previous: BITS{16, 0},
            current: BITS{16, 1},
            iteration: BITS{16, 0}
        ] |> LATEST state {
            clk |> THEN {
                PULSES { position } |> THEN {
                    [
                        previous: state.current,
                        current: state.previous + state.current,
                        iteration: state.iteration + 1
                    ]
                }
            }
        }
        // Filter: only output when complete
        state.iteration = position |> WHEN {
            True => state.current
            False => SKIP
        }
    }
}
```

**4. Mixed with Conditions:**
```boon
counter: BITS{8, 0} |> LATEST count {
    clk |> THEN {
        PULSES { 100 } |> THEN {
            enable |> WHEN {
                True => count + 1
                False => SKIP
            }
        }
    }
}
// Counts to 100, but only when enabled
```

---

## Software Context

### **PULSES in Software: Asynchronous Iteration**

**PULSES fires asynchronously to maintain Boon's actor model:**

```boon
result: 0 |> LATEST count {
    PULSES { 10 } |> THEN { count + 1 }
}
```

**Execution:**
- PULSES fires N async events (doesn't block actor queues)
- Each pulse updates LATEST
- LATEST emits on each update (normal reactive behavior)
- All intermediate values propagate downstream

**To filter intermediate values, track iteration in state:**

```boon
FUNCTION fibonacci(position) {
    BLOCK {
        state: [previous: 0, current: 1, iteration: 0] |> LATEST state {
            PULSES { position } |> THEN {
                [
                    previous: state.current,
                    current: state.previous + state.current,
                    iteration: state.iteration + 1
                ]
            }
        }
        // Filter: only emit when complete
        state.iteration = position |> WHEN {
            True => state.current
            False => SKIP
        }
    }
}

position: 5
result: fibonacci(position)
TEXT { "{position}. Fibonacci number is {result}" } |> Console/log()
// ✅ Logs ONCE: "5. Fibonacci number is 5"
// The WHEN filter prevents intermediate values from propagating
```

**Key insight:**
- LATEST emits all updates (normal behavior)
- Use WHEN + SKIP to filter unwanted intermediate emissions
- Track iteration count in state for filtering logic

### **Software Examples**

**1. Generate Sequence:**
```boon
FUNCTION generate_sequence(n, fn) {
    LIST {} |> LATEST items {
        PULSES { n } |> THEN {
            items |> List/push(fn(items |> List/length()))
        }
    }
}

// Usage
squares: generate_sequence(10, FUNCTION(i) { i * i })
// [0, 1, 4, 9, 16, 25, 36, 49, 64, 81]
```

**2. Retry Logic:**
```boon
FUNCTION retry_operation(operation, max_retries) {
    BLOCK {
        result: [success: False, value: None, attempts: 0] |> LATEST state {
            PULSES { max_retries } |> THEN {
                state.success |> WHEN {
                    True => SKIP  // Already succeeded
                    False => BLOCK {
                        outcome: operation()
                        outcome.success |> WHEN {
                            True => [
                                success: True,
                                value: outcome.value,
                                attempts: state.attempts + 1
                            ]
                            False => [
                                success: False,
                                value: None,
                                attempts: state.attempts + 1
                            ]
                        }
                    }
                }
            }
        }
        result
    }
}
```

**3. Animation Frames:**
```boon
FUNCTION animate(initial_frame, frame_count, animate_fn) {
    initial_frame |> LATEST frame {
        PULSES { frame_count } |> THEN {
            animate_fn(frame)
        }
    }
}

// Usage
final_frame: animate(initial_state, 60, FUNCTION(frame) {
    frame |> apply_physics(dt: 1.0 / 60.0)
})
```

---

## Common Patterns

### **Pattern 1: Transform N Times**
```boon
result: initial |> LATEST state {
    PULSES { n } |> THEN { transform(state) }
}
```

### **Pattern 2: Accumulate with Index**
```boon
result: [index: 0, accumulator: initial] |> LATEST state {
    PULSES { n } |> THEN {
        [
            index: state.index + 1,
            accumulator: update(state.accumulator, state.index)
        ]
    }
}
```

### **Pattern 3: Early Termination**
```boon
result: initial |> LATEST state {
    PULSES { max } |> THEN {
        done(state) |> WHEN {
            True => SKIP
            False => continue_transform(state)
        }
    }
}
```

### **Pattern 4: Conditional Update**
```boon
result: initial |> LATEST state {
    PULSES { n } |> THEN {
        condition(state) |> WHEN {
            True => state_a(state)
            False => state_b(state)
        }
    }
}
```

### **Pattern 5: Mixed Events (Software/Hardware)**
```boon
state: initial |> LATEST state {
    // Automatic iteration
    PULSES { max } |> THEN {
        auto_update(state)
    }

    // Manual control
    user_event |> THEN {
        manual_update(state)
    }

    // Reset
    reset_event |> THEN {
        initial
    }
}
```

---

## Comparison with Alternatives

### **vs UNFOLD (Previous Design)**

**UNFOLD:**
```boon
result: initial |> UNFOLD times: 10 { state => transform(state) }
```

**PULSES + LATEST:**
```boon
result: initial |> LATEST state {
    PULSES { 10 } |> THEN { transform(state) }
}
```

**Why PULSES is better:**
- ✅ Reuses LATEST (no new primitive)
- ✅ Can mix with other events
- ✅ More composable
- ✅ Same pattern everywhere
- ❌ Slightly more verbose (needs BLOCK for field extraction)

### **vs Queue/iterate (Old Design)**

**Queue:**
```boon
LIST { 0, 1 }
    |> Queue/iterate(previous, next: previous |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    })
    |> Queue/take_nth(position: 10)
```

**PULSES:**
```boon
BLOCK {
    state: [previous: 0, current: 1, iteration: 0] |> LATEST state {
        PULSES { 10 } |> THEN {
            [
                previous: state.current,
                current: state.previous + state.current,
                iteration: state.iteration + 1
            ]
        }
    }
    // Filter to return only when complete
    state.iteration = 10 |> WHEN {
        True => state.current
        False => SKIP
    }
}
```

**Why PULSES is better:**
- ✅ Clear semantics (counted pulses)
- ✅ No pull vs push confusion
- ✅ Works same in HW and SW
- ✅ No multi-consumer ambiguity
- ✅ Bounded by default

### **vs For Loop (Traditional)**

**Traditional for loop (not in Boon):**
```javascript
for (let i = 0; i < 10; i++) {
    state = transform(state);
}
```

**PULSES:**
```boon
state |> LATEST state {
    PULSES { 10 } |> THEN { transform(state) }
}
```

**PULSES advantages:**
- ✅ Functional (no mutation)
- ✅ Works in hardware
- ✅ Composable with events
- ✅ Explicit state flow

---

## Design Rationale

### **Why "PULSES"?**

**Alternatives considered:**
- REPEAT - Ambiguous (repeat what? value or iteration?)
- TICKS - Overloaded (clock ticks, event loop ticks)
- UNFOLD - Cryptic for beginners
- ITERATE - Generic, doesn't convey "counted"

**PULSES chosen because:**
1. **Unique to Boon** - Not overloaded with other meanings
2. **Plural** - Clear it's a count, not repeating a value
3. **Evocative** - Heartbeats, drumbeats, signals
4. **Searchable** - "PULSES Boon" finds docs immediately
5. **Clear** - "10 pulses" is instantly understood

### **Why Counted Only (No PULSES { if: ... })?**

**Decision:** Only support `PULSES { N }` (counted), not `PULSES { if: condition }`

**Rationale:**

1. **Conditional is redundant in hardware:**
   ```boon
   // These are identical:
   PULSES { if: !done } |> THEN { ... }
   !done |> WHEN { True => ..., False => SKIP }
   ```

2. **Conditional has simple workaround in software:**
   ```boon
   // Instead of: PULSES { if: !done(state) }
   // Just use:
   PULSES { max } |> THEN {
       done(state) |> WHEN { True => SKIP, False => continue }
   }
   ```

3. **Keep it simple:**
   - One mode: counted
   - Add conditional later if truly needed
   - Start simple, grow later

### **Why With LATEST (Not Standalone)?**

**PULSES is an event source, not a loop construct:**
- Fits Boon's event-driven model
- Composes with other events
- Reuses LATEST (no new primitive)
- Unified pattern across language

**Could have been standalone:**
```boon
// Alternative (rejected):
result: PULSES(10, initial, FUNCTION(state) { transform(state) })
```

**But LATEST integration is better:**
- More composable
- Can mix with other events
- Consistent with rest of language

### **Why Unit Values `[]`?**

**Each pulse produces `[]` (empty object):**
- Simple - just a signal, not data
- Type-safe - no ambiguity about Number vs BITS
- Extensible - can add indexed PULSES later if needed

**Could have produced indices:**
```boon
PULSES { 5 }  // Could produce: 0, 1, 2, 3, 4
```

**But unit values are cleaner:**
- Most use cases don't need index
- Can track index in state if needed
- Simpler semantics

---

## Future Enhancements

### **Possible: Conditional PULSES**

If compelling use cases emerge:

```boon
// Future syntax:
PULSES {
    while: state => !done(state),
    max: 1000
}

// Or:
PULSES {
    until: state => converged(state),
    max: 100
}
```

**When to add:**
- If workaround proves too verbose
- If common pattern emerges
- If users consistently request it

### **Possible: Indexed PULSES**

If index access is frequently needed:

```boon
// Future helper:
PULSES_indexed { 10 }  // Produces: 0, 1, 2, ..., 9

// Usage:
result: initial |> LATEST state {
    PULSES_indexed { 10 } |> THEN { index =>
        transform(state, index)
    }
}
```

### **Possible: Rate-Based PULSES (Hardware)**

For hardware timing:

```boon
// Future: pulses at specific rate
PULSES {
    rate: 1000000,  // 1 MHz
    count: 100
}
```

---

## Best Practices

### **DO:**

✅ **Use PULSES for counted iteration:**
```boon
PULSES { n } |> THEN { transform(state) }
```

✅ **Track iteration and filter:**
```boon
FUNCTION fibonacci(position) {
    BLOCK {
        state: [previous: 0, current: 1, iteration: 0] |> LATEST state {
            PULSES { position } |> THEN {
                [
                    previous: state.current,
                    current: state.previous + state.current,
                    iteration: state.iteration + 1
                ]
            }
        }
        state.iteration = position |> WHEN {
            True => state.current
            False => SKIP
        }
    }
}
```

✅ **Use SKIP for early termination:**
```boon
PULSES { max } |> THEN {
    done |> WHEN { True => SKIP, False => continue }
}
```

✅ **Use explicit clk in hardware:**
```boon
counter: 0 |> LATEST count {
    clk |> THEN {
        PULSES { 10 } |> THEN { count + 1 }
    }
}
```

### **DON'T:**

❌ **Don't use PULSES without bound:**
```boon
// ❌ No infinite PULSES
// If you need unbounded iteration, use event-driven LATEST
```

❌ **Don't confuse with clock ticks:**
```boon
// ❌ PULSES is not the same as clk
// clk is external, PULSES is internal counter
```

❌ **Don't try to access pulse values:**
```boon
// ❌ Pulses are unit values [], not indexed
// Track index in state if needed
```

---

## Summary

**PULSES is Boon's way to iterate N times:**

```boon
PULSES { N }  // Generates N pulses

// Used with LATEST:
result: initial |> LATEST state {
    PULSES { count } |> THEN {
        transform(state)
    }
}
```

**Key Points:**
- ✅ Counted iteration (finite, bounded)
- ✅ Works with LATEST (event-driven)
- ✅ Async execution (doesn't block actor queues)
- ✅ Filter intermediate values with WHEN + iteration tracking
- ✅ Same syntax for HW and SW (different execution)
- ✅ Unique to Boon (not overloaded)
- ✅ Simple and safe (explicit bounds)
- ✅ Composable (mix with other events)

**Replaces:** Queue, UNFOLD, REPEAT

**Use for:** Fibonacci, factorial, simulations, iterations, hardware counters

---

**PULSES: Simple, powerful, unique! ⚡⚡⚡**
