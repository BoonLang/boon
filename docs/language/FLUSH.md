# FLUSH Error Handling Pattern

**Date:** 2025-11-16
**Status:** Design Specification
**Audience:** Language designers, compiler implementers, runtime developers

---

## Table of Contents

1. [Introduction](#introduction)
2. [Core Mechanism](#core-mechanism)
3. [Semantics and Behavior](#semantics-and-behavior)
4. [Dataflow Model](#dataflow-model)
5. [Hardware/FPGA Implementation](#hardwarefpga-implementation)
6. [Parallel Processing](#parallel-processing)
7. [Streaming/Continuous Processing](#streamingcontinuous-processing)
8. [Actor Model Implementation](#actor-model-implementation)
9. [Type System Integration](#type-system-integration)
10. [Examples and Patterns](#examples-and-patterns)
11. [Guarantees and Properties](#guarantees-and-properties)
12. [Comparison with Alternatives](#comparison-with-alternatives)

---

## Introduction

### What is FLUSH?

**FLUSH** is a control flow operator for early exit from pipeline expressions. It creates a hidden wrapper that propagates transparently through pipelines, enabling fail-fast error handling without special collection functions.

### Why FLUSH?

- **Fail-fast error handling** - Stop processing on first error without special functions
- **Hardware-friendly** - Standard pipeline flush pattern used in CPUs
- **Actor-friendly** - Clean message passing with cancellation semantics
- **Diagram-friendly** - Clear dataflow visualization with bypass paths
- **Type-safe** - Hidden from user namespace, cannot clash with user types
- **Composable** - Works in nested contexts without special handling

### Basic Example

```boon
result: items
    |> List/map(item =>
        item |> process() |> WHEN {
            Ok[value] => value
            error => FLUSH { error }  -- Exit early, stop List/map
        }
    )
    |> next_operation()
```

When `FLUSH` executes:
1. Exits the current expression immediately
2. Stops `List/map` processing (fail-fast)
3. Skips `next_operation()`
4. `result` equals the error

---

## Core Mechanism

### FLUSHED[value] - Hidden Internal Wrapper

When `FLUSH { value }` executes, the runtime creates an internal wrapper:

```
FLUSH { error }  =>  FLUSHED[value: error]
```

**Properties:**

- **Hidden from users** - Internal compiler/runtime construct
- **Uppercase convention** - Indicates special/internal status
- **Cannot clash** - Not in user namespace
- **Carries semantics** - "This value should propagate immediately"
- **Unwraps at boundaries** - Variable bindings, function returns, BLOCK returns

**Important:** Users never write `FLUSHED[...]` in their code. It's purely internal.

---

## Semantics and Behavior

### FLUSH Has Three Effects

#### 1. Local Exit

Exits the current pipeline expression immediately. Remaining steps in that expression are skipped.

```boon
result: input
    |> step1()
    |> step2() |> WHEN {
        error => FLUSH { error }  -- Exits here
    }
    |> step3()  -- SKIPPED if FLUSH occurs
```

#### 2. Wrapper Creation

Creates `FLUSHED[value]` that propagates through the pipeline.

```boon
operation() |> WHEN {
    error => FLUSH { error }  -- Returns: FLUSHED[error] internally
}
```

#### 3. Transparent Propagation

`FLUSHED[value]` automatically bypasses functions until it reaches a boundary.

```boon
value
    |> function1()  -- Receives FLUSHED[T], bypasses, returns FLUSHED[T]
    |> function2()  -- Receives FLUSHED[T], bypasses, returns FLUSHED[T]
    |> function3()  -- Receives FLUSHED[T], bypasses, returns FLUSHED[T]
```

Each function:
1. Checks if input is `FLUSHED[T]`
2. If yes: bypass processing, return `FLUSHED[T]` unchanged
3. If no: process normally

### Boundary Unwrapping

`FLUSHED[value]` automatically unwraps to `value` at:

**Variable bindings:**
```boon
result: items |> process()  -- Returns FLUSHED[error] internally
-- Boundary unwrapping: result = error
```

**Function returns:**
```boon
FUNCTION helper(x) {
    x |> operation() |> WHEN {
        error => FLUSH { error }  -- Returns FLUSHED[error]
    }
}
-- Caller sees: error (unwrapped)
```

**BLOCK returns:**
```boon
BLOCK {
    logged: message |> Log/error()
    operation() |> WHEN {
        error => FLUSH { error }  -- BLOCK returns FLUSHED[error]
    }
}
-- Unwrapped: error
```

---

## Dataflow Model

### Complete Flow Example

Using BUILD.bn as reference:

```boon
generation_result: svg_files
    |> List/map(old, new:
        old |> icon_code() |> WHEN {
            Ok[text] => text                    -- Normal: text
            error => FLUSH { error }             -- Flushed: FLUSHED[error]
        }
    )
    -- List/map sees FLUSHED[error] in iteration
    -- Stops processing remaining items (fail-fast)
    -- Returns FLUSHED[error]

    |> Text/join_lines()
    -- Signature: [TEXT] -> TEXT
    -- Receives: FLUSHED[error]
    -- Runtime: is FLUSHED? Yes → bypass
    -- Returns: FLUSHED[error] (unchanged)

    |> WHEN { code => TEXT {
        -- Generated from {icons_directory}
        icon: [ {code} ]
    } }
    -- Receives: FLUSHED[error]
    -- Runtime: is FLUSHED? Yes → bypass WHEN
    -- Returns: FLUSHED[error]

    |> File/write_text(path: output_file)
    -- Signature: TEXT -> Ok | WriteError
    -- Receives: FLUSHED[error]
    -- Runtime: is FLUSHED? Yes → bypass
    -- Returns: FLUSHED[error]

-- Variable binding boundary - UNWRAP
-- generation_result = error  (FLUSHED wrapper removed)

-- Error handling at variable level (no CATCH needed)
generation_error_handling: generation_result |> WHEN {
    Ok => BLOCK {
        count: svg_files |> List/count()
        logged: TEXT { Included {count} icons } |> Log/info()
        Build/succeed()
    }
    error => BLOCK {
        error_message: error |> WHEN {
            ReadError[message] => TEXT { Cannot read icon: {message} }
            EncodeError[message] => TEXT { Cannot encode icon: {message} }
            WriteError[message] => TEXT { Cannot write {output_file}: {message} }
        }
        logged: error_message |> Log/error()
        Build/fail()
    }
}
```

### Flow Visualization

```
[svg_files]
    ↓
[List/map] ← FLUSH happens in item processing
    ↓ FLUSHED[error]
[Text/join_lines] → BYPASS → FLUSHED[error]
    ↓
[WHEN (template)] → BYPASS → FLUSHED[error]
    ↓
[File/write_text] → BYPASS → FLUSHED[error]
    ↓
[generation_result] = error (unwrapped)
    ↓
[generation_error_handling] → matches error → Build/fail()
```

---

## Hardware/FPGA Implementation

### Memory Representation

`FLUSHED[value]` is represented as:

```
[value_data (N bits) | FLUSHED_bit (1 bit)]
```

- **Value data:** The actual value being carried
- **FLUSHED bit:** Single bit flag indicating flushed status
- **Minimal overhead:** Only 1 bit per value

### Component Bypass Logic

Each pipeline component implements bypass logic:

```
Input → Check FLUSHED bit
         ↓
    [Is FLUSHED set?]
         ↓
    Yes ─┴─ No
     ↓      ↓
  Bypass  Process
     ↓      ↓
     └─ MUX ─┘
         ↓
      Output
```

**Bypass path:** Input passes through unchanged (no processing)
**Process path:** Normal component operation

### Component Diagram

```
┌─────────────────────────────────┐
│  Text/join_lines Component      │
├─────────────────────────────────┤
│                                 │
│  Input ──→ FLUSHED? ──→ MUX    │
│              │           ↓↓     │
│              No         Yes     │
│              ↓          ↓       │
│           Process    Bypass     │
│              ↓          ↓       │
│              └──→ MUX ←─┘      │
│                   ↓             │
│                Output           │
└─────────────────────────────────┘
```

### Pipeline Flush Signal (Parallel Lanes)

**This is a standard hardware pattern** - CPU pipelines use the same mechanism for branch misprediction or exceptions.

```
Lane1 → FLUSHED? → Set global FLUSH signal
Lane2 → Check signal → Stop if set
Lane3 → Check signal → Stop if set
Lane4 → Check signal → Stop if set
```

**Control unit behavior:**
1. Monitors FLUSHED bits from all lanes
2. On first FLUSHED detection: broadcast FLUSH signal
3. Stops feeding new items to lanes
4. Drains in-flight items
5. Outputs first FLUSHED value (by input order)

### FPGA Resource Cost

- **Per-value overhead:** 1 bit (FLUSHED flag)
- **Per-component overhead:** 1 MUX (2:1, bypass vs process)
- **Global overhead:** 1 FLUSH signal (broadcast line)
- **Total:** Minimal, standard practice in pipeline designs

---

## Parallel Processing

### Semantic Rule

**"First FLUSHED in INPUT ORDER wins"**

This ensures deterministic behavior regardless of parallel execution timing.

### Parallel Execution Example

```
Input: [A, B, C, D, E, F, G, H]

4 parallel threads process items:
  Thread 1: A, E
  Thread 2: B, F
  Thread 3: C, G
  Thread 4: D, H

Results (by completion time):
  T0: Thread 3: C → FLUSHED[error_C]  ← First by time!
  T1: Thread 1: A → ok, E → FLUSHED[error_E]
  T2: Thread 2: B → ok, F → ok
  T3: Thread 4: D → ok, H → processing...

First by INPUT ORDER: C (index 2)
Action: Return FLUSHED[error_C], cancel threads processing F, G, H
```

**Result is deterministic:** Always fails on C if C flushes, regardless of thread timing.

### Implementation Strategy

**Order-preserving parallel processing:**

1. **Process items in parallel** (non-deterministic timing)
2. **Collect results in order buffer** (maintains input order)
3. **Emit results in input order**
4. **On first FLUSHED in order:**
   - Stop emitting further results
   - Cancel remaining workers
   - Return FLUSHED value

### Coordination Mechanism

**Shared state:**
- Atomic `FLUSH_FLAG` (initially false)
- `FIRST_FLUSHED_VALUE` (protected by lock)
- `FLUSH_INDEX` (which input position flushed)

**Worker behavior:**
```
1. Before starting new item: check FLUSH_FLAG
2. If flag set: stop processing
3. On FLUSHED result from callback:
   - Atomic compare-and-swap on FLUSH_FLAG
   - If first to set: store value and index
4. Return result to coordinator
```

**Coordinator behavior:**
```
1. Distribute work to workers
2. Collect results in order buffer
3. Emit results in input order
4. When FLUSHED encountered in order:
   - Set global FLUSH_FLAG
   - Send cancel to all workers
   - Return FLUSHED value
```

### Determinism Guarantee

- Result is **always** first FLUSHED in input order
- **Independent** of thread timing or execution order
- Parallel execution is **optimization only** - semantics match sequential
- Same program produces same result every time

### Trade-offs

✅ **Pros:**
- Deterministic (input order preserved)
- Parallel speedup (process multiple items simultaneously)
- Standard pattern (well-understood coordination)

⚠️ **Cons:**
- Some wasted work (out-of-order items may be discarded)
- Coordination overhead (order buffer, FLUSH flag checks)

✅ **Overall:** Acceptable for fail-fast semantics. Benefits outweigh costs.

---

## Streaming/Continuous Processing

### FLUSH as Stream Cancellation

In streaming context, `FLUSHED[value]` acts as a cancellation signal.

```boon
result: input_stream
    |> Stream/map(item =>
        item |> process() |> WHEN {
            error => FLUSH { error }  -- Cancel stream
        }
    )
    |> Stream/filter(predicate)
    |> Stream/collect()
```

### Streaming Behavior

When `FLUSH` occurs during streaming:

1. **Stop consuming** from source (no new items)
2. **Propagate FLUSHED downstream** (as value)
3. **Send cancel signal upstream** (backpressure)
4. **Close stream resources** (cleanup)

### Backpressure Propagation

`FLUSHED` propagates in two directions:

- **Downstream:** `FLUSHED[error]` flows through pipeline as value
- **Upstream:** Cancel message to source (stop producing items)

```
Source → Map → Filter → Sink
  ↑      ↓
  |    FLUSH
  |      ↓
  └── Cancel
```

### Resource Cleanup

On FLUSH, streaming components:
- Close file handles
- Release network connections
- Free buffers
- Notify dependent streams
- Deallocate temporary resources

### Advantages for Streaming

✅ **Natural cancellation semantics** - FLUSH = stop stream
✅ **Immediate resource cleanup** - No waiting for stream end
✅ **Backpressure support** - Upstream stops producing
✅ **Composable** - Works with nested streams
✅ **No buffering waste** - Stop consuming immediately

---

## Actor Model Implementation

### Actor Types

- **Source actors:** Produce items
- **Worker actors:** Process items
- **Coordinator actors:** Distribute work, collect results
- **Sink actors:** Consume final results

### Message Types

```
NormalValue[data: T]           -- Regular value
FLUSHED[value: E]              -- Hidden wrapper in message (internal)
Cancel                         -- Explicit cancel command
Result[value: T | FLUSHED[E]]  -- Result wrapper
```

### Worker Actor Behavior

```boon
ACTOR Worker {
    state: [processing, idle, cancelled]

    receive message:
        message |> WHEN {
            WorkItem[item] => BLOCK {
                checked: state |> WHEN {
                    cancelled => send_to_coordinator(Cancelled)
                    __ => process_item(item)
                }
                result: checked |> WHEN {
                    FLUSHED[value] => BLOCK {
                        sent: send_to_coordinator(FLUSHED[value])
                        state: cancelled
                    }
                    value => send_to_coordinator(Result[value])
                }
            }

            Cancel => BLOCK {
                state: cancelled
            }
        }
}
```

### Coordinator Actor Behavior

```boon
ACTOR Coordinator {
    state: [
        items: [A, B, C, D, E]
        workers: [Worker1, Worker2, Worker3]
        results: []
        flush_detected: FALSE
    ]

    receive message:
        message |> WHEN {
            Start => distribute_work()

            Result[value] => BLOCK {
                updated_results: [...results, value]
                check_order_and_emit(updated_results)
            }

            FLUSHED[value] => BLOCK {
                state.flush_detected: TRUE
                broadcast_cancel_to_workers()
                emit_to_sink(FLUSHED[value])
            }
        }
}
```

### Advantages for Actors

✅ **Standard message passing** - FLUSHED is just a message type
✅ **Clean cancellation** - Send Cancel message to workers
✅ **Supervisor trees** - Propagate FLUSH to supervisor for error handling
✅ **Location transparency** - Works locally or distributed
✅ **Fault tolerance** - FLUSHED can represent actor failure

---

## Type System Integration

### Type Signatures

**User perspective (FLUSHED is hidden):**

```boon
function_signature: TEXT -> TEXT
```

**With potential FLUSH:**

```boon
function_with_flush: TEXT -> TEXT | Error
-- If function contains FLUSH, return type includes error type
```

**Internal representation (compiler sees FLUSHED):**

```boon
function_internal: TEXT -> TEXT | FLUSHED[Error]
-- FLUSHED[Error] unwraps to Error at boundary
```

### Type Inference

Compiler infers FLUSH possibility from code analysis:

```boon
FUNCTION process(x) {
    x |> operation() |> WHEN {
        error => FLUSH { error }  -- Compiler detects FLUSH
    }
}
-- Inferred type: T -> U | Error
-- (FLUSHED[Error] unwrapped at function boundary)
```

### List/map Type Signature

**Without FLUSH in callback:**
```boon
List/map: ([A], (A -> B)) -> [B]
```

**With FLUSH in callback:**
```boon
List/map: ([A], (A -> B | E)) -> [B] | E
-- Where E might be FLUSHED[error] internally
-- Unwrapped to error at binding boundary
```

### Boundary Unwrapping Rules

**Variable binding:**
```boon
result: operation()  -- Returns FLUSHED[error] internally
-- Unwrapped: result = error
```

**Function return:**
```boon
FUNCTION helper() {
    operation() |> WHEN {
        error => FLUSH { error }  -- Returns FLUSHED[error]
    }
}
-- Caller sees: error (unwrapped)
```

**BLOCK return:**
```boon
BLOCK {
    logged: message |> Log/error()
    operation() |> WHEN {
        error => FLUSH { error }  -- BLOCK returns FLUSHED[error]
    }
}
-- Unwrapped: error
```

### Type Safety Guarantees

✅ **No namespace collision** - FLUSHED[T] is internal, cannot clash with user types
✅ **Type inference** - Compiler tracks FLUSH possibility automatically
✅ **Automatic unwrapping** - No manual unwrapping needed at boundaries
✅ **Clean signatures** - User functions don't mention FLUSHED explicitly

---

## Examples and Patterns

### Pattern 1: Fail-Fast Collection Processing

```boon
result: items
    |> List/map(item =>
        item |> risky_operation() |> WHEN {
            Ok[value] => value
            error => FLUSH { error }  -- Stop on first error
        }
    )
    |> next_operation()
```

**Behavior:** Stops processing on first error, skips remaining items and `next_operation()`.

### Pattern 2: Nested Pipelines

```boon
result: outer_items
    |> List/map(outer =>
        inner_items
            |> List/map(inner =>
                inner |> process() |> WHEN {
                    error => FLUSH { error }  -- Exits to outer List/map
                }
            )
            |> combine_with(outer)
    )
```

**Behavior:** FLUSH in inner List/map stops inner processing, propagates to outer.

### Pattern 3: Multi-Step Pipeline with FLUSH

```boon
result: input
    |> step1() |> WHEN {
        error => FLUSH { error }
        Ok[v1] => v1
    }
    |> step2() |> WHEN {
        error => FLUSH { error }
        Ok[v2] => v2
    }
    |> step3() |> WHEN {
        error => FLUSH { error }
        Ok[v3] => v3
    }
```

**Behavior:** Each step can fail and exit early, skipping remaining steps.

### Pattern 4: Conditional FLUSH on Validation

```boon
result: item
    |> validate() |> WHEN {
        Invalid[reason] => FLUSH { ValidationError[reason: reason] }
        Valid[data] => data
    }
    |> process()
```

**Behavior:** Invalid input FLUSHes immediately, skips processing.

### Pattern 5: FLUSH with Error Transformation

```boon
result: item
    |> operation() |> WHEN {
        DatabaseError[message] => FLUSH {
            UserFacingError[message: TEXT { Database unavailable }]
        }
        Ok[value] => value
    }
```

**Behavior:** Transforms technical error to user-friendly error before flushing.

### Pattern 6: Resource Cleanup with FLUSH

```boon
FUNCTION process_with_cleanup(resource) {
    resource
        |> acquire()
        |> WHEN {
            error => FLUSH { error }
            Ok[handle] => handle
        }
        |> use_resource()
        |> WHEN {
            error => BLOCK {
                cleanup: resource |> release()
                FLUSH { error }
            }
            Ok[result] => BLOCK {
                cleanup: resource |> release()
                result
            }
        }
}
```

**Behavior:** Ensures resource cleanup even when FLUSH occurs.

### Pattern 7: BUILD.bn Complete Example

```boon
generation_result: svg_files
    |> List/retain(item, if: item.extension = TEXT { svg })
    |> List/sort_by(item, key: item.path)
    |> List/map(old, new:
        old |> icon_code() |> WHEN {
            Ok[text] => text
            error => FLUSH { error }
        }
    )
    |> Text/join_lines()
    |> WHEN { code => TEXT {
        -- Generated from {icons_directory}
        icon: [ {code} ]
    } }
    |> File/write_text(path: output_file)

generation_error_handling: generation_result |> WHEN {
    Ok => BLOCK {
        count: svg_files |> List/count()
        logged: TEXT { Included {count} icons } |> Log/info()
        Build/succeed()
    }
    error => BLOCK {
        error_message: error |> WHEN {
            ReadError[message] => TEXT { Cannot read icon: {message} }
            EncodeError[message] => TEXT { Cannot encode icon: {message} }
            WriteError[message] => TEXT { Cannot write {output_file}: {message} }
        }
        logged: error_message |> Log/error()
        Build/fail()
    }
}

FUNCTION icon_code(item) {
    item.path
        |> File/read_text()
        |> WHEN {
            Ok[text] => text
            error => FLUSH { error }
        }
        |> Url/encode()
        |> WHEN {
            Ok[encoded] => encoded
            error => FLUSH { error }
        }
        |> WHEN { encoded =>
            Ok[text: TEXT { {item.file_stem}: data:image/svg+xml;utf8,{encoded} }]
        }
}
```

---

## Guarantees and Properties

### Determinism Guarantee

- **Sequential execution:** Deterministic (obvious - single thread)
- **Parallel execution:** First FLUSHED in INPUT ORDER (deterministic)
- **Streaming execution:** First FLUSHED in STREAM ORDER (deterministic)
- **Result:** Same input always produces same output

### Ordering Guarantee

- FLUSHED values maintain **input order semantics**
- Parallel processing preserves **input order** for result selection
- **No race conditions** affect which error is returned
- **Time-independent:** Result doesn't depend on thread timing

### Resource Safety

- FLUSHED triggers **immediate cleanup** in streaming contexts
- Functions can **intercept FLUSHED** for cleanup logic
- **No resource leaks** from early exit
- **Deterministic cleanup** order (follows data flow)

### Type Safety

- `FLUSHED[T]` is **hidden**, cannot clash with user types
- Type system **tracks FLUSH possibility** via inference
- **Automatic unwrapping** prevents type confusion
- **Compile-time checking** of error paths

### Composability

- FLUSH works in **nested contexts** (pipelines in pipelines)
- Functions **don't need to know** about FLUSHED mechanism
- Pipelines **compose naturally** (no special wiring)
- **Works with any collection function** (List/map, List/fold, etc.)

### Performance

- **Fail-fast:** Stops processing immediately on first error
- **No unnecessary work** after FLUSH
- **Minimal overhead:** 1-bit check per value
- **Parallel speedup preserved** with deterministic coordination
- **Hardware-friendly:** Standard bypass pattern

### Correctness

- FLUSH semantics **match sequential execution** exactly
- Parallel optimization **doesn't change semantics** (only performance)
- Streaming cancellation is **immediate** (no buffering delay)
- **No hidden state** - all effects visible in data flow

---

## Comparison with Alternatives

### Alternative 1: THROW/CATCH (Exception-like)

**Code example:**
```boon
result: items
    |> List/map(item =>
        item |> process() |> WHEN {
            error => THROW { error }
        }
    )
    |> CATCH { error => handle(error) }
```

**Problems:**

❌ **Requires CATCH blocks** - More boilerplate, mandatory catch
❌ **Non-local jump semantics** - Confusing control flow
❌ **Compilation error if missing** - CATCH is mandatory
❌ **Less hardware-friendly** - Exception handling is complex in hardware

**FLUSH advantages:**

✅ No CATCH needed - Handle at variable level
✅ Transparent propagation - Simpler mental model
✅ Hardware-friendly - Standard bypass logic
✅ Optional handling - Can choose where to handle errors

### Alternative 2: List/try_map (Special Function)

**Code example:**
```boon
result: items
    |> List/try_map(item =>
        item |> process()
    )
```

**Problems:**

❌ **Need special functions** - `try_map`, `try_fold`, `try_filter`, etc.
❌ **More functions to learn** - Cognitive overhead
❌ **Cannot use regular List/map** for fail-fast
❌ **Less flexible** - FLUSH can be anywhere in expression

**FLUSH advantages:**

✅ One `List/map` works for both - With or without FLUSH
✅ Fewer functions to maintain - Simpler API
✅ More flexible - FLUSH anywhere in pipeline
✅ Explicit intent - FLUSH shows where failure happens

### Alternative 3: Result Monad (Explicit Chaining)

**Code example:**
```boon
result: items
    |> List/map(item =>
        item
            |> process()
            |> Result/and_then(v1 => step2(v1))
            |> Result/and_then(v2 => step3(v2))
    )
```

**Problems:**

❌ **Verbose** - Explicit `and_then` everywhere
❌ **Callback nesting** - Harder to read
❌ **Still need special List/try_map** - For fail-fast collection processing
❌ **More mental overhead** - Need to think about monad laws

**FLUSH advantages:**

✅ Cleaner syntax - Just `|> WHEN`
✅ No nesting - Flat pipelines
✅ Works with regular `List/map` - No special functions
✅ Simpler mental model - Just data flow + bypass

### Alternative 4: Transparent Propagation Only (No FLUSH)

**Code example:**
```boon
result: items
    |> List/map(item =>
        item |> process()  -- Returns T | Error
    )
-- Returns: [T | Error, T | Error, ...]
```

**Problems:**

❌ **No fail-fast** - Processes all items even after error
❌ **Mixed results** - List contains both successes and errors
❌ **Wasted processing** - Unnecessary work after first error
❌ **Harder to handle** - Need to filter errors from successes

**FLUSH advantages:**

✅ Fail-fast - Stop on first error
✅ Clean result - Not a mixed list
✅ Efficient - No wasted work
✅ Simpler handling - Single error or success

---

## Summary: Why FLUSH?

### Design Goals Achieved

✅ **Simple** - No special functions, no mandatory CATCH blocks
✅ **Efficient** - Fail-fast, minimal overhead (1-bit check)
✅ **Hardware-friendly** - Standard bypass pattern, easy to implement in FPGA
✅ **Actor-friendly** - Message passing with clean cancellation
✅ **Type-safe** - Hidden wrapper, automatic unwrapping, no namespace collision
✅ **Composable** - Works in nested contexts without special handling
✅ **Deterministic** - Input order preserved, same result every time
✅ **Flexible** - Use with any collection function, FLUSH anywhere in pipeline

### Key Innovation

**FLUSH combines:**
1. **Local control flow** (exit expression)
2. **Transparent propagation** (bypass intermediate steps)
3. **Boundary unwrapping** (clean variable assignment)

**Result:** Fail-fast error handling that is simple, efficient, and hardware-friendly.

### When to Use FLUSH

**Use FLUSH when:**
- Processing collections and want fail-fast behavior
- Building multi-step pipelines with potential failures
- Need deterministic error handling in parallel contexts
- Want clean error propagation without CATCH blocks
- Targeting hardware/FPGA implementation

**Don't use FLUSH when:**
- Want to accumulate all errors (just return error without FLUSH)
- Need to continue processing after errors
- Error handling is better done locally (use WHEN without FLUSH)

---

## Related Documents

- `ERROR_HANDLING.md` - General error handling patterns in Boon
- `BUILD_SYSTEM.md` - BUILD.bn error handling specifics
- `TAGGED_UNIONS.md` - Tagged union types (Ok[T], Error[E])
- `BOON_SYNTAX.md` - Core language syntax

---

**Last Updated:** 2025-11-16
**Status:** Design Specification
**Next Review:** After initial implementation

