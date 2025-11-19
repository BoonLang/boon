# UNFOLD: Sequence Generation from State

**Date**: 2025-01-19
**Status**: Design Proposal
**Replaces**: Queue (for sequence generation)

---

## Executive Summary

UNFOLD generates sequences by **repeatedly transforming state**.

**Key principles:**
- **Push-based** - Eager evaluation, fits Boon's dataflow model
- **Finite by default** - Explicit iteration count (no infinite sequences)
- **Pure functional** - No side effects or mutable state
- **Works in HW and SW** - Same semantics in both contexts
- **General purpose** - Fibonacci, factorial, powers, any iterative computation

**Replaces Queue for:**
- Lazy sequences (Fibonacci, generators)
- Iterative computations
- Sequence generation

**Compared to LATEST:**
- **LATEST** - Reactive state (events drive changes)
- **UNFOLD** - Sequence generation (iteration drives changes)

---

## Table of Contents

1. [Core Concept](#core-concept)
2. [Syntax](#syntax)
3. [Examples](#examples)
4. [Hardware Context](#hardware-context)
5. [API Reference](#api-reference)
6. [Comparison with Queue](#comparison-with-queue)
7. [Design Rationale](#design-rationale)

---

## Core Concept

UNFOLD repeatedly applies a transformation to generate a sequence:

```
initial → transform → state₁ → transform → state₂ → ... → stateₙ
```

**The dual of fold:**
- **Fold** - Consumes sequence → produces value (reduce)
- **Unfold** - Consumes value → produces sequence (generate)

**Visual:**
```
Fold:   [1,2,3,4,5] → sum → 15
Unfold: [0,1]       → fib → [0,1,1,2,3,5,8,13,...]
```

---

## Syntax

### Basic Form

```boon
initial_value |> UNFOLD times: N { state_name =>
    transformation_expression
}
```

### Full Form

```boon
UNFOLD {
    initial: value           // Starting state (required)
    times: N                 // Number of iterations (required if no 'while')
    next: state => expr      // Transformation function (required)

    // Optional
    collect: True/False      // Collect all states? (default: False = final only)
    while: state => bool     // Alternative termination condition
}
```

### Piped Form (Recommended)

```boon
value |> UNFOLD times: N { state => expression }
value |> UNFOLD while: condition { state => expression }
value |> UNFOLD times: N, collect: True { state => expression }
```

---

## Examples

### Software Examples

**1. Fibonacci (single value):**
```boon
FUNCTION fibonacci(n) {
    [prev: 0, current: 1]
        |> UNFOLD times: n { state =>
            [prev: state.current, current: state.prev + state.current]
        }
        |> .current
}

fibonacci(10)  // Returns: 55
```

**2. Fibonacci (sequence):**
```boon
FUNCTION fibonacci_sequence(n) {
    [prev: 0, current: 1]
        |> UNFOLD times: n, collect: True { state =>
            [prev: state.current, current: state.prev + state.current]
        }
        |> List/map(fn: FUNCTION(s) { s.current })
}

fibonacci_sequence(10)  // Returns: [1, 1, 2, 3, 5, 8, 13, 21, 34, 55]
```

**3. Factorial:**
```boon
FUNCTION factorial(n) {
    [count: 1, product: 1]
        |> UNFOLD times: n { state =>
            [count: state.count + 1, product: state.product * state.count]
        }
        |> .product
}

factorial(5)  // Returns: 120
```

**4. Powers of 2:**
```boon
FUNCTION powers_of_2(n) {
    1 |> UNFOLD times: n, collect: True { x => x * 2 }
}

powers_of_2(10)  // Returns: [1, 2, 4, 8, 16, 32, 64, 128, 256, 512]
```

**5. Countdown:**
```boon
countdown: 10 |> UNFOLD times: 10, collect: True { x => x - 1 }
// Returns: [10, 9, 8, 7, 6, 5, 4, 3, 2, 1]
```

**6. Collatz sequence (using 'while'):**
```boon
FUNCTION collatz(start) {
    start
        |> UNFOLD while: x => x != 1, collect: True { x =>
            x % 2 == 0 |> WHEN {
                True => x / 2
                False => x * 3 + 1
            }
        }
}

collatz(13)  // Returns: [13, 40, 20, 10, 5, 16, 8, 4, 2, 1]
```

**7. Compound interest:**
```boon
FUNCTION compound_interest(principal, rate, years) {
    principal
        |> UNFOLD times: years { amount => amount * (1 + rate) }
}

compound_interest(1000, 0.05, 10)  // Returns: 1628.89
```

**8. Newton's method (square root):**
```boon
FUNCTION sqrt_approx(n, iterations) {
    n / 2  // Initial guess
        |> UNFOLD times: iterations { guess =>
            (guess + n / guess) / 2
        }
}

sqrt_approx(2, 5)  // Returns: 1.414...
```

**9. Pascal's triangle row:**
```boon
FUNCTION pascal_row(n) {
    LIST { 1 }
        |> UNFOLD times: n { row =>
            row
                |> List/windows(2)
                |> List/map(fn: FUNCTION(pair) { pair |> Math/sum() })
                |> List/prepend(1)
                |> List/append(1)
        }
}

pascal_row(4)  // Returns: [1, 4, 6, 4, 1]
```

**10. Game of Life step:**
```boon
FUNCTION simulate_life(grid, steps) {
    grid |> UNFOLD times: steps { current_grid =>
        current_grid |> game_of_life_rules()
    }
}
```

---

## Hardware Context

UNFOLD works in hardware too! Compiles to iterative hardware circuits.

**1. Hardware Fibonacci generator:**
```boon
fib_result: [prev: BITS{8, 0}, current: BITS{8, 1}]
    |> UNFOLD times: 10 { state =>
        [prev: state.current, current: state.prev + state.current]
    }
    |> .current
```

**Compiles to:**
- Counter (0 to 10)
- Two registers (prev, current)
- Adder (prev + current)
- Done flag when complete
- Mux to output final result

**2. LFSR (Linear Feedback Shift Register):**
```boon
FUNCTION lfsr_sequence(taps, initial, count) {
    initial
        |> UNFOLD times: count, collect: True { lfsr =>
            feedback: lfsr |> extract_taps(taps) |> xor_reduce()
            lfsr |> shift_right() |> set_msb(feedback)
        }
}
```

**3. Iterative algorithms:**
```boon
// Multiply by repeated addition (for simple cores)
FUNCTION multiply(a, b) {
    0 |> UNFOLD times: b { sum => sum + a }
}

// Division by repeated subtraction
FUNCTION divide(a, b) {
    [remainder: a, quotient: 0]
        |> UNFOLD while: s => s.remainder >= b { s =>
            [remainder: s.remainder - b, quotient: s.quotient + 1]
        }
        |> .quotient
}
```

**4. Pipelined state machine:**
```boon
pipeline: initial_state
    |> UNFOLD times: PIPELINE_DEPTH { state =>
        state |> pipeline_stage()
    }
```

**Hardware synthesis characteristics:**
- `times: N` → Fixed iteration count (synthesizable)
- `while: condition` → Variable iterations (needs max bound or warning)
- `collect: True` → Stores all states (register array or memory)
- `collect: False` → Only final state (single register)

---

## API Reference

### Creation

```boon
// Basic - final state only
initial |> UNFOLD times: N { state => next_state }

// Collect all intermediate states
initial |> UNFOLD times: N, collect: True { state => next_state }

// While loop variant
initial |> UNFOLD while: condition { state => next_state }

// Full form
UNFOLD {
    initial: value
    times: N                    // OR while: condition
    next: state => expression
    collect: True/False         // Optional (default: False)
}
```

### Parameters

**initial** - Starting state (any type)
- Scalar: `0`, `1.5`, `"start"`
- Object: `[prev: 0, current: 1]`
- List: `LIST { 0, 1 }`
- BITS: `BITS { 8, 10u0 }`

**times** - Number of iterations (required if no 'while')
- Must be known at compile time for hardware
- Can be runtime value in software

**while** - Termination condition (alternative to 'times')
- Function: `state => boolean`
- Evaluated before each iteration
- Stops when returns False

**next** - State transformation (required)
- Function: `state => next_state`
- Pure function (no side effects)
- Must return same type as input state

**collect** - Collect all states? (optional, default: False)
- `False` - Return only final state
- `True` - Return list of all states (including initial)

### Return Type

**collect: False (default)**
- Returns: Same type as initial state
- Value: Final state after all iterations

**collect: True**
- Returns: `LIST { State }` (List of states)
- Contains: `[initial, state₁, state₂, ..., stateₙ]`
- Length: `N + 1` (includes initial state)

---

## Comparison with Queue

**Why UNFOLD is better than Queue for sequence generation:**

| Aspect | Queue/iterate | UNFOLD |
|--------|--------------|--------|
| **Model** | Pull-based (lazy) | Push-based (eager) |
| **Evaluation** | On demand | Immediate |
| **Finiteness** | Unbounded by default | Bounded by default |
| **Semantics** | Complex (HW/SW different) | Simple (same everywhere) |
| **Multi-consumer** | Ambiguous | Clear (returns List) |
| **Fit with Boon** | Conflicts with dataflow | Perfect fit |
| **State** | Implicit | Explicit |
| **Hardware** | Unclear mapping | Clear synthesis |

**Example comparison:**

**With Queue (old):**
```boon
LIST { 0, 1 }
    |> Queue/iterate(previous, next: previous |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    })
    |> Queue/take_nth(position: 10)
    |> List/first()
```

**With UNFOLD (new):**
```boon
[prev: 0, current: 1]
    |> UNFOLD times: 10 { state =>
        [prev: state.current, current: state.prev + state.current]
    }
    |> .current
```

**Improvements:**
- ✅ Clearer intent
- ✅ Simpler syntax
- ✅ No confusion about laziness
- ✅ No multi-consumer issues
- ✅ Works same in HW and SW
- ✅ More readable

---

## When to Use UNFOLD vs LATEST

| Use Case | Tool | Why |
|----------|------|-----|
| **Reactive state** | LATEST | Events drive updates |
| **Counters/FSMs** | LATEST | Clock/event triggered |
| **Accumulators** | LATEST | Stateful, reactive |
| **Sequence generation** | UNFOLD | Iterative computation |
| **Fibonacci, factorial** | UNFOLD | Known iteration count |
| **Simulations** | UNFOLD | Step-by-step evolution |
| **Iterative algorithms** | UNFOLD | Convergence, approximation |

**Key difference:**
- **LATEST** - State that **reacts to inputs** (events, signals)
- **UNFOLD** - Sequence **computed from iteration** (no external triggers)

**Example comparison:**

```boon
// LATEST - Reactive counter (event-driven)
counter: 0 |> LATEST count {
    increment |> THEN { count + 1 }
    reset |> THEN { 0 }
}
// Updates when events fire, stays idle otherwise

// UNFOLD - Count to N (computation)
count_to_n: 0 |> UNFOLD times: N { x => x + 1 }
// Eagerly computes all N steps, returns final result
```

---

## Design Rationale

### Why UNFOLD?

**1. Push-based fits Boon**
- Boon is fundamentally push-based (dataflow)
- UNFOLD eagerly computes (no lazy complexity)
- No conflict with multi-consumer semantics

**2. Finite by default prevents errors**
- Must explicitly specify `times: N` or `while: condition`
- No accidental infinite sequences
- Hardware synthesis requires bounds anyway

**3. Pure functional, no hidden state**
- State is explicit in transformation
- No mutable variables
- Easy to reason about

**4. Same semantics in HW and SW**
- Software: Loop N times, collect results
- Hardware: Counter + registers + logic
- No semantic split

**5. General purpose**
- Not just for sequences (Fibonacci)
- Any iterative computation
- Converging algorithms (Newton's method)
- Simulations (Game of Life)

### Why not lazy sequences?

**Lazy sequences (like Queue/iterate) add complexity:**
- Pull-based vs push-based conflict
- Multi-consumer ambiguity
- When does evaluation happen?
- How does caching work?
- How to synthesize in hardware?

**In practice, lazy sequences rarely needed:**
- Most use cases have finite bounds
- Eager evaluation is simple and predictable
- Hardware can't do infinite anyway

**If really needed later, add STREAM:**
```boon
// Future: Lazy infinite streams (if needed)
STREAM/iterate(initial: value, next: fn)
    |> Stream/take(N)
    |> Stream/to_list()
```

But start simple with UNFOLD.

### Why 'collect' parameter?

**Sometimes you want intermediate states:**
```boon
// All Fibonacci numbers up to N
fibonacci_sequence(10)  // [1, 1, 2, 3, 5, 8, 13, 21, 34, 55]

// Collatz sequence
collatz(13)  // [13, 40, 20, 10, 5, 16, 8, 4, 2, 1]

// Simulation history
game_of_life_history(grid, 100)  // All 100 frames
```

**Other times, only final state matters:**
```boon
fibonacci(10)           // Just 55
factorial(5)            // Just 120
sqrt_approx(2, 10)      // Just 1.414...
```

**'collect' makes this explicit:**
- `collect: False` (default) - Efficient, final state only
- `collect: True` - Store history, return list

### Naming: Why UNFOLD?

**Alternatives considered:**
- `ITERATE` - Too generic, confused with loops
- `GENERATE` - Implies side effects
- `REPEAT` - Doesn't capture transformation
- `FOLD_RIGHT` - Reversed fold (confusing)
- **`UNFOLD`** - Standard FP term, clear meaning ✅

**UNFOLD is:**
- Recognized in functional programming
- Clear dual to FOLD
- Implies "expanding from seed"
- Short, memorable

---

## Implementation Notes

### Software (JavaScript/WebAssembly)

**Compiles to simple loop:**
```javascript
function unfold(initial, times, next, collect) {
  let state = initial;
  const results = collect ? [initial] : null;

  for (let i = 0; i < times; i++) {
    state = next(state);
    if (collect) results.push(state);
  }

  return collect ? results : state;
}
```

**With 'while' condition:**
```javascript
function unfold_while(initial, condition, next, collect) {
  let state = initial;
  const results = collect ? [initial] : null;

  while (condition(state)) {
    state = next(state);
    if (collect) results.push(state);
  }

  return collect ? results : state;
}
```

### Hardware (SystemVerilog)

**Example: Fibonacci (times: 10, collect: False)**
```systemverilog
// Synthesizes to:
logic [7:0] prev, current;
logic [3:0] counter;  // 0 to 10
logic done;

always_ff @(posedge clk) begin
  if (rst) begin
    prev <= 8'd0;
    current <= 8'd1;
    counter <= 4'd0;
    done <= 1'b0;
  end else if (!done) begin
    prev <= current;
    current <= prev + current;
    counter <= counter + 1;
    if (counter == 4'd9) done <= 1'b1;
  end
end

// Output: current when done
```

**With collect: True:**
```systemverilog
// Creates register array or memory
logic [7:0] states [0:10];  // Store all states
logic [3:0] counter;

always_ff @(posedge clk) begin
  if (!done) begin
    states[counter + 1] <= next_state;
    counter <= counter + 1;
    // ...
  end
end
```

**With while: condition:**
- Synthesizable if max iterations known: `while: s => s.count < MAX`
- Warning/error if unbounded: Requires `#[max_iterations(N)]` annotation

---

## Future Enhancements

**1. Parallel UNFOLD (hardware)**
```boon
// Unroll in hardware for speed
#[parallel]
result: initial |> UNFOLD times: 8 { s => transform(s) }
// Generates pipelined or parallel stages
```

**2. Step size**
```boon
// Take every Nth state
result: initial |> UNFOLD times: 100, step: 10 { s => next(s) }
// Collect states at iterations 0, 10, 20, ..., 100
```

**3. Indexed transformation**
```boon
// Access iteration index
result: initial |> UNFOLD times: N { state, index =>
    state + index
}
```

**4. Converge variant**
```boon
// Stop when state stabilizes
result: guess |> UNFOLD converge: tolerance { state =>
    next_guess(state)
}
// Stops when |next - state| < tolerance
```

---

## Open Questions

1. **Should 'collect' include initial state?**
   - Current: `collect: True` includes initial → `[initial, s1, s2, ..., sN]`
   - Alternative: Only transformations → `[s1, s2, ..., sN]`
   - Leaning towards including initial (more complete history)

2. **Hardware synthesis for 'while'?**
   - Require `#[max_iterations(N)]` annotation?
   - Infer max from type bounds?
   - Error if unbounded?

3. **Should state type be constrained?**
   - Allow any type?
   - Or only serializable types (for hardware)?

4. **Syntax for indexed version?**
   ```boon
   // Option A: Two parameters
   UNFOLD times: N { state, index => ... }

   // Option B: Provide index in scope
   UNFOLD times: N { state => state + UNFOLD_INDEX }
   ```

---

## Next Steps

1. Implement UNFOLD in compiler
2. Update Fibonacci example to use UNFOLD
3. Remove Queue primitive entirely
4. Design FIFO primitive separately (for hardware only)
5. Add List/fold for completeness (UNFOLD's dual)
6. Write tutorial showing common patterns
7. Benchmark hardware synthesis

---

**UNFOLD: Beautiful, simple, powerful sequence generation! 🌱➡️🌳**
