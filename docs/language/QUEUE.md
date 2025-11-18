# Queue: Unified Model for Software and Hardware

**Date**: 2025-01-18
**Status**: Design Draft
**Scope**: Queue semantics across Web, Server, and FPGA contexts

---

## Executive Summary

Boon's Queue provides a **unified abstraction** for stateful iteration and buffering across software and hardware:

- **Software**: Event streams, async queues, lazy sequences
- **Hardware**: Registers (size: 1) and FIFOs (size: N)

**Key principles:**
- **All queues are bounded** - Prevents memory leaks, forces explicit backpressure
- **No self-reference** - Stateful operations maintain internal state
- **Incremental pipeline** - Stream → Stateful Operation → Result
- **Register = Queue(size: 1)** - FPGA registers are 1-element queues
- **FIFO = Queue(size: N)** - Hardware buffers for rate matching, CDC

---

## Table of Contents

1. [Core Concept](#core-concept)
2. [Software vs Hardware Semantics](#software-vs-hardware-semantics)
3. [Buffer Size Implications](#buffer-size-implications)
4. [Bounded Queues (Default)](#bounded-queues-default)
5. [API Reference](#api-reference)
6. [Use Cases](#use-cases)
7. [FPGA Register Pattern](#fpga-register-pattern)
8. [FPGA FIFO Pattern](#fpga-fifo-pattern)
9. [Transpiler Mapping](#transpiler-mapping)
10. [Design Rationale](#design-rationale)

---

## Core Concept

### The Unified Pattern

**Stream → Stateful Operation → Result**

```boon
// Software: Event stream → Accumulator
counter: LATEST { 0, increment_event |> THEN { 1 } } |> Math/sum()
//       ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ stream
//                                                       ^^^^^^^^^ stateful op

// Hardware: Control signals → Register
counter: default |> Bits/sum(delta: control_signals |> WHEN { ... })
//       ^^^^^^^    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//       initial    stateful operation (maintains register)
```

**Key insight:** No self-reference needed! Stateful operations (Math/sum, Bits/sum, Queue/iterate) maintain internal state.

---

## Software vs Hardware Semantics

### Queue Operations by Context

| Operation | Software (async) | FPGA (sync) | Notes |
|-----------|-----------------|-------------|-------|
| **`iterate(current, next)`** | ✅ Lazy sequence | ✅ Register/FIFO | Works in both! |
| **`append(item, enable)`** | ✅ Add to queue | ✅ Write to FIFO | Different timing |
| **`take(enable)`** | ✅ Consume item | ✅ Read from FIFO | Different timing |
| **`at(index)`** | ✅ Peek at index | ❌ N/A | Software only |
| **`take_nth(position)`** | ✅ Consume N items | ❌ N/A | Software only |
| **`take_while(condition)`** | ✅ Consume while true | ❌ N/A | Software only |
| **`wait_for(where)`** | ✅ Async wait | ❌ N/A | Software only |
| **`is_full()`** | ✅ Check capacity | ✅ Backpressure signal | Both |
| **`is_empty()`** | ✅ Check empty | ✅ Underflow check | Both |
| **`count()`** | ✅ Current size | ✅ Occupancy | Both |

### Execution Model

**Software:**
- **Pull-based**: Operations happen when consumed (lazy evaluation)
- **Asynchronous**: Can wait for items (`wait_for`)
- **Multi-consumer**: Multiple workers can take from shared queue

**Hardware:**
- **Clock-driven**: Operations happen every clock cycle (eager evaluation)
- **Synchronous**: Deterministic, no waiting
- **Single-consumer**: One reader, one writer per FIFO

---

## Buffer Size Implications

### size: 1 → Register

**Software:** Reactive state, latest value only
```boon
counter: LATEST { 0, events } |> Math/sum()
// Holds only current count, not event history
```

**Hardware:** Register (flip-flop)
```boon
state: REGISTER B current {
    rst |> WHEN { True => B, False => current |> next_state() }
}
// Single register holding current state
```

### size: 2-16 → Small Buffer

**Software:** Recent events buffer
```boon
recent_clicks: Queue/bounded(size: 10, on_full: DropOldest)
    |> Queue/append(item: click_events)
```

**Hardware:** Small FIFO (flip-flop array)
```boon
interface_buffer: Queue/bounded(size: 8)
    |> Queue/append(item: uart_rx_byte, enable: rx_valid)
```

### size: 32-512 → Medium Buffer

**Software:** Task queue, event buffer
```boon
task_queue: Queue/bounded(size: 100, on_full: Block)
    |> Queue/append(item: new_tasks)
```

**Hardware:** Distributed RAM FIFO
```boon
packet_buffer: Queue/bounded(size: 256)
    |> Queue/append(item: packet_data, enable: packet_valid)
```

### size: 1024+ → Large Buffer

**Software:** Message queue, log buffer
```boon
message_queue: Queue/bounded(size: 10000, on_full: Block)
```

**Hardware:** Block RAM (BRAM) FIFO
```boon
dma_buffer: Queue/bounded(size: 4096)
    |> Queue/append(item: dma_data, enable: dma_write)
```

### Unbounded (Software Only)

**Explicit unbounded for provably safe cases:**
```boon
// Fibonacci: lazy evaluation ensures safety
fib: Queue/unbounded()
    |> Queue/from_initial(LIST { 0, 1 })
    |> Queue/iterate(previous, next: previous |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    })
    |> Queue/take_nth(10)  // Only computes 10 values
```

---

## Bounded Queues (Default)

### Why Bounded by Default?

**Prevents memory leaks:**
```boon
// ❌ BAD: Unbounded queue can grow forever
events: websocket_stream |> Queue/from_stream()  // Unbounded!
// Producer faster than consumer → OOM

// ✅ GOOD: Explicit bounds + backpressure strategy
events: Queue/bounded(size: 100, on_full: DropOldest)
    |> Queue/append(item: websocket_stream)
```

### Backpressure Strategies

```boon
Queue/bounded(size: N, on_full: strategy)

Strategies:
- DropOldest  // Ring buffer (FIFO eviction)
- DropNewest  // Reject new items (circuit breaker)
- Block       // Block producer (async backpressure)
- Error       // Raise error (fail fast)
```

**Examples:**

```boon
// Ring buffer: keep most recent N events
recent_events: Queue/bounded(size: 100, on_full: DropOldest)

// Circuit breaker: reject when system overloaded
api_requests: Queue/bounded(size: 50, on_full: DropNewest)

// Async backpressure: slow down producer
database_writes: Queue/bounded(size: 20, on_full: Block)

// Fail fast: detect system issues early
critical_queue: Queue/bounded(size: 10, on_full: Error)
```

### Hardware Backpressure

```boon
// FIFO full signal = backpressure to producer
uart_fifo: Queue/bounded(size: 8)
    |> Queue/append(item: rx_byte, enable: rx_valid && !fifo_full)

fifo_full: uart_fifo |> Queue/is_full()  // Wire to producer
```

---

## API Reference

### Creation

```boon
// Bounded queue (default, recommended)
Queue/bounded(size: N, on_full: strategy, initial: value_or_list)

// Unbounded (software only, use with caution)
Queue/unbounded(initial: value_or_list)

// From existing data
Queue/from_list(list: LIST { ... })
Queue/from_stream(stream: event_stream)

// Hardware register sugar (size: 1)
REGISTER initial_value current { next_expression }
// Desugars to:
Queue/bounded(size: 1, initial: initial_value)
    |> Queue/iterate(current, next: next_expression)
```

### Core Operations (Work Everywhere)

```boon
// Iteration (software: lazy, hardware: every clock cycle)
Queue/iterate(current, next: expression)

// Status (combinational in hardware)
Queue/is_full()
Queue/is_empty()
Queue/count()
Queue/almost_full(threshold: N)
Queue/almost_empty(threshold: N)
```

### Producer Operations

```boon
// Add items (software: when called, hardware: when enable=True)
Queue/append(item: value, enable: condition)
Queue/prepend(item: value, enable: condition)  // Add to front

// Batch operations (software only)
Queue/append_list(items: LIST { ... })
```

### Consumer Operations

```boon
// Take (software: destructive consume, hardware: read + advance pointer)
Queue/take(enable: condition)
Queue/take_nth(position: N)        // Software only
Queue/take_while(condition: pred)  // Software only

// Peek (non-destructive, software only)
Queue/at(index: N)
Queue/first()
Queue/last()

// Async (software only)
Queue/wait_for(where: predicate)
Queue/watch()  // Subscribe to changes
```

### Transformation

```boon
// Map over queue (software only)
Queue/map(fn: transformer)
Queue/filter(where: predicate)

// Drain (consume all, software only)
Queue/drain()
Queue/to_list()
```

---

## Use Cases

### Software: Event Buffer

```boon
// Click events with bounded buffer
click_buffer: Queue/bounded(size: 100, on_full: DropOldest)
    |> Queue/append(item: button.click_events)

click_count: click_buffer
    |> Queue/take_while(condition: user_active)
    |> Queue/iterate(previous, next: previous + 1)
```

### Software: Task Queue

```boon
// Worker pool with backpressure
task_queue: Queue/bounded(size: 50, on_full: Block)
    |> Queue/append(item: incoming_tasks)

// Multiple workers consume
worker_a: task_queue |> Queue/take_while(condition: worker_a_available)
worker_b: task_queue |> Queue/take_while(condition: worker_b_available)
```

### Software: Fibonacci (Lazy)

```boon
// Unbounded is safe here (lazy evaluation)
fib: Queue/unbounded()
    |> Queue/from_initial(LIST { 0, 1 })
    |> Queue/iterate(previous, next: previous |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    })
    |> Queue/take_nth(position: 10)  // Only compute 10 values
    |> List/first()
```

### Hardware: Register (FSM)

```boon
// FSM state register (size: 1)
state: REGISTER B current {
    rst |> WHEN {
        True => B
        False => current |> WHEN {
            A => C
            B => D
            C => a |> WHEN { True => D, False => B }
            D => A
        }
    }
}

// Or explicit:
state: Queue/bounded(size: 1, initial: B)
    |> Queue/iterate(current, next:
        rst |> WHEN { True => B, False => current |> WHEN { ... } }
    )
```

### Hardware: FIFO (Clock Domain Crossing)

```boon
// Producer: 100 MHz clock domain
fast_fifo: Queue/bounded(size: 16)
    |> Queue/append(item: sensor_data, enable: fast_clock_valid)

// Consumer: 50 MHz clock domain
slow_data: fast_fifo
    |> Queue/take(enable: slow_clock_ready)

[
    data: slow_data
    fifo_full: fast_fifo |> Queue/is_full()    // Backpressure
    fifo_empty: fast_fifo |> Queue/is_empty()  // Underflow check
]
```

### Hardware: FIFO (UART Interface)

```boon
// UART receiver with 8-byte FIFO
uart_fifo: Queue/bounded(size: 8)
    |> Queue/append(
        item: uart_rx_byte,
        enable: uart_byte_ready && !fifo_full
    )

// Processor reads when ready
byte_to_process: uart_fifo
    |> Queue/take(enable: processor_can_read)

[
    data: byte_to_process
    rx_overflow: uart_fifo |> Queue/is_full()  // Error signal
    rx_available: uart_fifo |> Queue/is_empty() |> Bool/not()
    rx_count: uart_fifo |> Queue/count()
]
```

---

## FPGA Register Pattern

### Register = Queue(size: 1)

A hardware register is a 1-element bounded queue:
- Holds single value
- Updates every clock cycle (or when conditions met)
- No FIFO semantics (no read/write pointers)

### Counter Example

```boon
FUNCTION counter(rst, en) {
    BLOCK {
        count_width: 8
        default: BITS { count_width, 10s0 }

        control_signals: [reset: rst, enabled: en]

        // Register using Queue/iterate
        count: Queue/bounded(size: 1, initial: default)
            |> Queue/iterate(current, next:
                control_signals |> WHEN {
                    [reset: True, enabled: __] => default
                    [reset: False, enabled: True] => current + 1
                    __ => current
                }
            )

        [count: count]
    }
}

// Or using REGISTER sugar:
FUNCTION counter(rst, en) {
    BLOCK {
        count_width: 8
        default: BITS { count_width, 10s0 }
        control_signals: [reset: rst, enabled: en]

        count: REGISTER default current {
            control_signals |> WHEN {
                [reset: True, enabled: __] => default
                [reset: False, enabled: True] => current + 1
                __ => current
            }
        }

        [count: count]
    }
}
```

### FSM Example

```boon
FUNCTION fsm(rst, a) {
    BLOCK {
        state: REGISTER B current {
            rst |> WHEN {
                True => B
                False => current |> WHEN {
                    A => C
                    B => D
                    C => a |> WHEN { True => D, False => B }
                    D => A
                }
            }
        }

        // Combinational output logic
        b: state |> WHEN {
            A => False
            B => True
            C => a
            D => False
        }

        [b: b]
    }
}
```

### Key Points

- **No self-reference**: `current` is a parameter, not variable name
- **Pattern matching**: Declarative state transitions
- **Implicit clock**: Clock added by transpiler
- **Transpiles to**: `always_ff @(posedge clk)` register

---

## FPGA FIFO Pattern

### FIFO = Queue(size: N > 1)

A hardware FIFO is an N-element bounded queue:
- Multiple registers (buffer)
- Read and write pointers
- Full/empty flags
- Simultaneous read/write possible

### Common FIFO Sizes

| Size | Implementation | Use Case |
|------|---------------|----------|
| 2-16 | Flip-flop array | Small interface buffers |
| 32-512 | Distributed RAM | Medium buffers, rate matching |
| 1024+ | Block RAM (BRAM) | Packet buffers, DMA |

### Clock Domain Crossing (CDC)

```boon
FUNCTION cdc_fifo(fast_data, fast_valid, slow_ready) {
    BLOCK {
        // FIFO bridges clock domains
        // Write side: fast clock (100 MHz)
        // Read side: slow clock (50 MHz)

        cdc_fifo: Queue/bounded(size: 16)
            |> Queue/append(
                item: fast_data,
                enable: fast_valid && !fifo_full
            )

        // Read on slow clock
        slow_data: cdc_fifo
            |> Queue/take(enable: slow_ready && !fifo_empty)

        [
            data: slow_data
            fifo_full: cdc_fifo |> Queue/is_full()
            fifo_empty: cdc_fifo |> Queue/is_empty()
            fifo_count: cdc_fifo |> Queue/count()
        ]
    }
}
```

### Rate Matching

```boon
FUNCTION rate_matcher(bursty_input, bursty_valid, steady_ready) {
    BLOCK {
        // Buffer bursty input for steady consumer
        // Example: Camera sensor (bursty) → Image processor (steady)

        rate_fifo: Queue/bounded(size: 64)
            |> Queue/append(
                item: bursty_input,
                enable: bursty_valid && !fifo_full
            )

        steady_output: rate_fifo
            |> Queue/take(enable: steady_ready && !fifo_empty)

        [
            data: steady_output
            backpressure: rate_fifo |> Queue/is_full()  // Signal to slow producer
            data_available: rate_fifo |> Queue/is_empty() |> Bool/not()
        ]
    }
}
```

### UART with RX FIFO

```boon
FUNCTION uart_rx_fifo(rx_byte, byte_ready, processor_read) {
    BLOCK {
        // 8-byte receive FIFO
        rx_fifo: Queue/bounded(size: 8)
            |> Queue/append(
                item: rx_byte,
                enable: byte_ready && !overflow
            )

        overflow: rx_fifo |> Queue/is_full() && byte_ready

        data_out: rx_fifo
            |> Queue/take(enable: processor_read && data_available)

        data_available: rx_fifo |> Queue/is_empty() |> Bool/not()

        [
            data: data_out
            rx_overflow: overflow  // Error flag
            rx_empty: rx_fifo |> Queue/is_empty()
            rx_count: rx_fifo |> Queue/count()
        ]
    }
}
```

### Key Points

- **Separate read/write**: Can happen simultaneously in same clock cycle
- **Pointers wrap**: Circular buffer (read_ptr, write_ptr)
- **Status signals**: full, empty, count (combinational)
- **Backpressure**: full signal stops producer
- **Underflow protection**: empty signal stops consumer

---

## Transpiler Mapping

### Software (JavaScript/Wasm)

```boon
// Boon
events: Queue/bounded(size: 100, on_full: DropOldest)
    |> Queue/append(item: click_events)
```

**Generates:**
```javascript
// Circular buffer with DropOldest strategy
const events = new BoundedQueue({
    size: 100,
    onFull: 'drop-oldest'
});

// Reactive subscription
clickEvents.subscribe(event => {
    events.append(event);
});
```

### Hardware (SystemVerilog)

**Register (size: 1):**
```boon
// Boon
state: REGISTER B current {
    rst |> WHEN { True => B, False => current |> next_state() }
}
```

**Generates:**
```systemverilog
// Sequential logic
logic [1:0] state;

always_ff @(posedge clk or posedge rst)
    if (rst)
        state <= 2'b01;  // B
    else
        state <= next_state(state);
```

**FIFO (size: N):**
```boon
// Boon
uart_fifo: Queue/bounded(size: 8)
    |> Queue/append(item: rx_byte, enable: rx_valid)
```

**Generates:**
```systemverilog
// Parameterized FIFO module instantiation
fifo #(
    .DEPTH(8),
    .WIDTH(8)
) uart_fifo (
    .clk(clk),
    .rst(rst),
    .wr_en(rx_valid),
    .wr_data(rx_byte),
    .full(uart_fifo_full),
    .rd_en(processor_read),
    .rd_data(uart_fifo_data),
    .empty(uart_fifo_empty),
    .count(uart_fifo_count)
);
```

---

## Design Rationale

### Why Bounded by Default?

**Prevents common bugs:**
1. **Memory leaks** - Unbounded queues can grow without limit
2. **No backpressure** - Forces explicit strategy for queue full
3. **Resource awareness** - Developer must think about capacity

**Makes unbounded explicit:**
```boon
// Must consciously choose unbounded
fib: Queue/unbounded() |> Queue/iterate(...)  // I know this is safe
```

### Why Queue/iterate Works in Both Contexts?

**Same semantics, different timing:**

**Software (pull-based):**
- Iterate when consumed: `queue |> Queue/take_nth(5)` runs 5 iterations
- Lazy evaluation: Only compute what's needed
- Control flow: Developer controls when to iterate

**Hardware (clock-driven):**
- Iterate every clock: Automatic, deterministic
- Eager evaluation: Always running
- Control flow: Clock drives iteration

**Both use same syntax:**
```boon
Queue/iterate(current, next: expression)
```

### Why No Self-Reference?

**Problem with self-reference:**
```boon
// ❌ COSTLY: List recreated every update
todos: LATEST { [], add |> THEN { todos |> List/append(item) } }
//                          ^^^^^ Recreates entire list!

// ❌ OR: Requires mutation (not pure)
todos: LATEST { [], add |> THEN { todos |> List/append_mut(item) } }
//                                         ^^^^^^^^^^^ Mutates
```

**Solution: Stateful operations maintain state internally**
```boon
// ✅ INCREMENTAL: List/append maintains list internally
todos: LIST {} |> List/append(item: new_todo)
//                ^^^^^^^^^^^ Stateful operation

// ✅ INCREMENTAL: Math/sum maintains accumulator
counter: LATEST { 0, events } |> Math/sum()
//                               ^^^^^^^^^ Stateful operation
```

### Why Register = Queue(size: 1)?

**Conceptual unification:**
- Software state: Latest value from event stream
- Hardware register: Latest value from clock stream
- Both are 1-element buffers that update

**Implementation:**
- Software: Single cell, updates on events
- Hardware: Single flip-flop, updates on clock

**Same API:**
```boon
// Both use iterate with current parameter
value: initial |> Queue/iterate(current, next: ...)
```

### Why FIFO = Queue(size: N)?

**FIFOs are fundamental in hardware:**
- Clock domain crossing (different clock frequencies)
- Rate matching (producer/consumer speed mismatch)
- Interface buffering (UART, SPI, Ethernet, etc.)
- Pipeline decoupling (absorb temporary stalls)

**Same abstraction as software queues:**
- Bounded buffer
- Backpressure when full
- Status signals (empty, full, count)

---

## Open Questions

1. **REGISTER keyword vs Queue/bounded(size: 1)?**
   - Keep both? REGISTER as sugar?
   - Or always use Queue/bounded explicitly?

2. **Default backpressure strategy?**
   - Should `Queue/bounded(size: N)` have default on_full?
   - Or require explicit strategy always?

3. **FIFO simultaneous read/write in Boon syntax?**
   - How to express that read and write can happen same cycle?
   - Implicit from context?

4. **Queue/iterate in hardware: how to handle reset?**
   - Should iterate implicitly reset on rst signal?
   - Or explicit in next expression?

5. **Unbounded queues: allow at all?**
   - Only for provably lazy cases (fibonacci)?
   - Or completely disallow, use bounded with large size?

---

## Next Steps

1. Review and refine API based on feedback
2. Determine REGISTER keyword vs Queue/bounded(size: 1)
3. Update hw_examples/ to use Queue/bounded or REGISTER
4. Implement transpiler for Queue → SystemVerilog FIFO
5. Add Queue operations to standard library
6. Document common patterns and pitfalls

---

**Feedback welcome!** This is a design draft - please iterate on any section.
