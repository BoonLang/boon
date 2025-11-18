# Queue: Lazy Sequences and Hardware FIFOs

**Date**: 2025-01-18
**Status**: Design Draft

---

## Executive Summary

Queue provides **lazy sequence generation** and **hardware buffering** in Boon:

- **Software**: Lazy infinite sequences (fibonacci, generators)
- **Hardware**: FIFOs for rate matching, clock domain crossing

**Key principles:**
- **Queue/iterate** - Lazy, pull-based sequence generation
- **Queue/generate** - Generation without self-reference
- **All queues are bounded by default** - Prevents memory leaks
- **FIFO = Queue/bounded(size: N)** - Hardware buffers

**Note:** For reactive state (counters, FSMs, accumulators), use **LATEST** instead (see [LATEST.md](./LATEST.md)).

---

## Table of Contents

1. [Core Operations](#core-operations)
2. [Bounded by Default](#bounded-by-default)
3. [API Reference](#api-reference)
4. [Software Use Cases](#software-use-cases)
5. [Hardware FIFOs](#hardware-fifos)
6. [When to Use Queue vs LATEST](#when-to-use-queue-vs-latest)

---

## Core Operations

Queue provides two core operations for sequence generation:

### Queue/iterate - Lazy sequences with self-reference

Use when next value depends on current value:

```boon
// Fibonacci sequence
LIST { 0, 1 }
    |> Queue/iterate(previous, next: previous |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    })
    |> Queue/take_nth(position: 10)
    |> List/first()
```

### Queue/generate - Generation without self-reference

Use when next value is independent of current:

```boon
// Constant stream
ones: Queue/generate(next: 1)
    |> Queue/take_nth(position: 100)

// External sampling
samples: Queue/bounded(size: 1000)
    |> Queue/generate(next: sensor.reading)
```

---

## Bounded by Default

### Why Bounded?

Prevents memory leaks and forces explicit capacity planning:

```boon
// ✅ Explicit bounds + backpressure strategy
events: Queue/bounded(size: 100, on_full: DropOldest)
    |> Queue/append(item: websocket_stream)

// ❌ Unbounded only for provably lazy cases
fib: Queue/unbounded()
    |> Queue/iterate(prev, next: ...)  // Safe: lazy evaluation
```

### Backpressure Strategies

```boon
Queue/bounded(size: N, on_full: strategy)

Strategies:
- DropOldest  // Ring buffer (keep recent)
- DropNewest  // Reject new (circuit breaker)
- Block       // Async backpressure
- Error       // Fail fast
```

---

## API Reference

### Creation

```boon
// Bounded (default)
Queue/bounded(size: N, on_full: strategy, initial: value_or_list)

// Unbounded (lazy sequences only)
Queue/unbounded(initial: value_or_list)
```

### Core Operations

```boon
// Iteration
Queue/iterate(current, next: expression)  // With self-reference
Queue/generate(next: expression)          // Without self-reference

// Status
Queue/is_full()
Queue/is_empty()
Queue/count()
```

### Producer/Consumer

```boon
// Add items
Queue/append(item: value, enable: condition)

// Take items
Queue/take(enable: condition)
Queue/take_nth(position: N)     // Software only
Queue/take_while(condition: fn) // Software only

// Peek (software only)
Queue/at(index: N)
Queue/first()
```

---

## Software Use Cases

### Lazy Sequences

```boon
// Fibonacci
LIST { 0, 1 }
    |> Queue/iterate(previous, next: previous |> WHEN {
        LIST { first, second } => LIST { second, first + second }
    })
    |> Queue/take_nth(position: 10)
```

### Task Queues

```boon
// Worker pool with backpressure
task_queue: Queue/bounded(size: 50, on_full: Block)
    |> Queue/append(item: incoming_tasks)

worker: task_queue |> Queue/take_while(condition: worker_available)
```

### Event Buffers

```boon
// Ring buffer for recent events
events: Queue/bounded(size: 100, on_full: DropOldest)
    |> Queue/append(item: click_events)
```

---

## Hardware FIFOs

### Clock Domain Crossing

```boon
// Producer: 100 MHz, Consumer: 50 MHz
cdc_fifo: Queue/bounded(size: 16)
    |> Queue/append(item: fast_data, enable: fast_valid && !fifo_full)

slow_data: cdc_fifo
    |> Queue/take(enable: slow_ready && !fifo_empty)

[
    data: slow_data
    fifo_full: cdc_fifo |> Queue/is_full()
    fifo_empty: cdc_fifo |> Queue/is_empty()
]
```

### UART Interface

```boon
// 8-byte receive FIFO
uart_fifo: Queue/bounded(size: 8)
    |> Queue/append(item: rx_byte, enable: byte_ready && !fifo_full)

data_out: uart_fifo
    |> Queue/take(enable: processor_read && !fifo_empty)

[
    data: data_out
    rx_overflow: uart_fifo |> Queue/is_full()
    rx_count: uart_fifo |> Queue/count()
]
```

### Common FIFO Sizes

| Size | Implementation | Use Case |
|------|---------------|----------|
| 2-16 | Flip-flop array | Interface buffers |
| 32-512 | Distributed RAM | Rate matching |
| 1024+ | Block RAM | DMA buffers |

---

## When to Use Queue vs LATEST

| Use Case | Tool | Why |
|----------|------|-----|
| **Lazy sequences** | Queue/iterate | Fibonacci, infinite streams |
| **Task queues** | Queue/bounded | Backpressure, buffering |
| **Hardware FIFOs** | Queue/bounded | CDC, rate matching |
| **Reactive state** | LATEST | Counters, FSMs (see [LATEST.md](./LATEST.md)) |
| **Event accumulation** | LATEST | Math/sum internally |

---

## Design Rationale

### Why Bounded by Default?

Forces explicit capacity planning and prevents memory leaks:
- Software: Prevents unbounded growth from fast producers
- Hardware: Naturally bounded (physical resources)
- Must explicitly choose `Queue/unbounded()` for lazy sequences

### Why Queue for Lazy Sequences?

Pull-based evaluation is ideal for:
- Infinite sequences (fibonacci, primes)
- Unknown iteration count
- Consumer-controlled iteration
- Minimal computation (only what's needed)

### Why Queue for Hardware FIFOs?

Natural abstraction for:
- Clock domain crossing
- Rate matching
- Interface buffering
- Pipeline decoupling

---
