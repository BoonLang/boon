# Bugs to Resolve

Discovered while testing the fibonacci example. These bugs prevent the fibonacci.bn example from producing the expected output "5. Fibonacci number is 8".

## Bug 1: HOLD with Object/Record State Doesn't Emit Values

**Status:** PENDING

**Symptoms:**
- `state: 0 |> HOLD state { PULSES { 15 } |> THEN { state + 1 } }` works (returns 5)
- `state: [current: 1] |> HOLD state { PULSES { 15 } |> THEN { [current: state.current + 1] } }` returns empty

**Test Case:**
```boon
state: [current: 1, iter: 0] |> HOLD state {
    PULSES { 15 } |> THEN {
        [current: state.current + 1, iter: state.iter + 1]
    }
}
document: state |> Document/new()
-- Expected: some object value
-- Actual: empty
```

**Location:** Likely in `crates/boon/src/platform/browser/evaluator.rs` or `engine.rs` where HOLD processes Object values.

**Root Cause Analysis Needed:** Check how HOLD handles Object vs Number values in the value stream.

---

## Bug 2: HOLD/PULSES Loses ~60% of Iterations

**Status:** PENDING

**Symptoms:**
- `PULSES { 5 }` alone correctly emits 0,1,2,3,4
- `state: 0 |> HOLD state { PULSES { 5 } |> THEN { state + 1 } }` returns 3 instead of 5
- `PULSES { 10 }` with HOLD returns 4
- `PULSES { 15 }` with HOLD returns 5
- `PULSES { 20 }` with HOLD returns 7

**Test Case:**
```boon
state: 0 |> HOLD state { PULSES { 10 } |> THEN { state + 1 } }
document: state |> Document/new()
-- Expected: 10
-- Actual: 4
```

**Location:** Likely in `crates/boon/src/platform/browser/engine.rs` - backpressure permit handling in HOLD.

**Root Cause Analysis Needed:** HOLD's backpressure mechanism is dropping pulses before they can be processed.

---

## Bug 3: state.field Access Returns Initial Value Instead of Reactive Stream

**Status:** PENDING

**Symptoms:**
- When accessing a field of an object inside HOLD (like `state.current`), it returns the initial value instead of subscribing to updates.

**Test Case:**
```boon
state: [current: 1, iter: 0] |> HOLD state {
    PULSES { 15 } |> THEN {
        [current: state.current + 1, iter: state.iter + 1]
    }
}
document: state.current |> Document/new()
-- Expected: updated current value
-- Actual: 1 (initial value)
```

**Location:** `crates/boon/src/platform/browser/evaluator.rs` - field access on Object values.

**Root Cause Analysis Needed:** Field access may be evaluated eagerly instead of creating a reactive subscription.

---

## Bug 4: WHILE with SKIP Inside BLOCK Doesn't Emit

**Status:** PENDING

**Symptoms:**
- WHILE with SKIP works in isolation
- Inside BLOCK, when condition becomes True, WHILE doesn't emit

**Test Case:**
```boon
FUNCTION fibonacci(position) {
    BLOCK {
        state: [iteration: 0] |> HOLD state { ... }
        (state.iteration == position) |> WHILE {
            True => state.current
            __ => SKIP
        }
    }
}
-- Expected: emits state.current when iteration == position
-- Actual: never emits
```

**Location:** `crates/boon/src/platform/browser/evaluator.rs` - WHILE/SKIP evaluation in BLOCK context.

**Root Cause Analysis Needed:** BLOCK output may not wait for WHILE to eventually emit a non-SKIP value.

---

## Bug 5: Extra Quotes Around TEXT Output in Preview

**Status:** PENDING

**Symptoms:**
- TEXT values display with surrounding quotes in preview
- Expected: `5. Fibonacci number is 8`
- Actual: `"5. Fibonacci number is 8"`

**Test Case:**
```boon
message: TEXT { "Hello world" }
document: message |> Document/new()
-- Expected in preview: Hello world
-- Actual in preview: "Hello world"
```

**Location:** `crates/boon/src/platform/browser/bridge.rs:34` - `value_to_element` function for Text values.

**Fix:** The Text variant should render without quotes. Check if `text.text()` includes quotes or if they're added during rendering.

---

## Bug 6: Underscore Variable Assignment Syntax Not Supported

**Status:** PENDING

**Symptoms:**
- `_log: message |> Log/info()` causes lexer error
- `_: message |> Log/info()` also causes lexer error
- Error: "found ':' expected '_', or something else"

**Test Case:**
```boon
message: TEXT { "test" }
_log: message |> Log/info()
-- Expected: evaluates Log/info() and discards result
-- Actual: Lexer error
```

**Location:** `crates/boon/src/parser/lexer.rs` - underscore token handling.

**Root Cause Analysis Needed:** Lexer may treat `_` specially and not allow it as a variable name prefix.

---

## Priority Order

1. **Bug 1 & 3** - HOLD with Object state (these are likely related)
2. **Bug 2** - PULSES iteration loss
3. **Bug 4** - WHILE/SKIP in BLOCK
4. **Bug 5** - Extra quotes (cosmetic but affects test matching)
5. **Bug 6** - Underscore variable syntax (workaround: don't use _ prefix)

## Test Command

After fixes, verify with:
```bash
./target/release/boon-tools exec inject "$(cat playground/frontend/src/examples/fibonacci/fibonacci.bn)" && \
./target/release/boon-tools exec run && sleep 4 && \
./target/release/boon-tools exec preview
```

Expected output: `5. Fibonacci number is 8`
