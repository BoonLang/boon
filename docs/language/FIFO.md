# FIFO: Hardware Buffering and Clock Domain Crossing

**Date**: 2025-01-19
**Status**: Design Proposal
**Scope**: Hardware Only

---

## Executive Summary

FIFO provides **hardware buffering** for Boon - explicit stateful modules for:
- Clock domain crossing (CDC)
- Rate matching between producers and consumers
- Interface buffering (UART, SPI, etc.)
- Pipeline decoupling

**Key principles:**
- **Hardware-only primitive** - No software equivalent
- **Explicit stateful module** - Clear that it's not a functional value
- **Multiple ports** - Different operations read different module signals
- **Clear semantics** - No confusion with functional constructs

**Replaces Queue for:**
- Hardware FIFOs and buffers
- CDC applications
- Interface protocols

---

## Quick Example

```boon
// Create FIFO module
uart_rx: FIFO {
    depth: 8
    width: 8
}

// Write side (producer)
FIFO/write(uart_rx,
    data: rx_byte,
    enable: byte_valid && !FIFO/full(uart_rx)
)

// Read side (consumer)
data_out: FIFO/read(uart_rx,
    enable: cpu_read && !FIFO/empty(uart_rx)
)

// Status signals
status: [
    full: FIFO/full(uart_rx)
    empty: FIFO/empty(uart_rx)
    count: FIFO/count(uart_rx)
]
```

---

## Core Concept

A FIFO is a **stateful hardware module** with:
- Internal buffer storage
- Write port (producer)
- Read port (consumer)
- Status signals (full, empty, count)

**Not a functional value:**
- Multiple references access the **same module instance**
- Different operations read **different ports**
- Writes and reads have **side effects** (state changes)

---

## API Reference

### Creation

```boon
FIFO {
    depth: N                // Number of entries (required)
    width: M                // Bits per entry (optional, inferred from data)
    type: "sync"/"async"    // Sync or async (CDC) FIFO (default: "sync")
    almost_full: K          // Threshold for almost_full flag (optional)
    almost_empty: J         // Threshold for almost_empty flag (optional)
}
```

### Write Operations

```boon
// Write to FIFO (side effect!)
FIFO/write(fifo, data: value, enable: condition)

// Returns: Nothing (void operation, updates module state)
```

### Read Operations

```boon
// Read from FIFO (side effect!)
data: FIFO/read(fifo, enable: condition)

// Returns: Data value (type inferred from FIFO width)
// Side effect: Removes entry from FIFO when enabled
```

### Status Operations

```boon
// Status flags (pure reads, no side effects)
is_full: FIFO/full(fifo)
is_empty: FIFO/empty(fifo)
count: FIFO/count(fifo)
almost_full: FIFO/almost_full(fifo)
almost_empty: FIFO/almost_empty(fifo)
overflow: FIFO/overflow(fifo)      // Sticky error flag
underflow: FIFO/underflow(fifo)    // Sticky error flag
```

---

## Examples

### 1. UART Receive Buffer

```boon
FUNCTION uart_receiver(rx_bit, byte_ready) {
    BLOCK {
        // 8-byte receive FIFO
        rx_fifo: FIFO {
            depth: 8
            width: 8
        }

        // Write received bytes
        FIFO/write(rx_fifo,
            data: rx_bit,
            enable: byte_ready && !FIFO/full(rx_fifo)
        )

        // Read to processor
        data_out: FIFO/read(rx_fifo,
            enable: processor_read && !FIFO/empty(rx_fifo)
        )

        [
            data: data_out
            data_valid: !FIFO/empty(rx_fifo)
            overflow: FIFO/overflow(rx_fifo)
            count: FIFO/count(rx_fifo)
        ]
    }
}
```

### 2. Clock Domain Crossing (CDC)

```boon
FUNCTION clock_domain_bridge(fast_data, fast_valid, slow_ready) {
    BLOCK {
        // Async FIFO for CDC
        cdc_fifo: FIFO {
            depth: 16
            width: 32
            type: "async"
        }

        // Fast clock domain (write side)
        write_enable: fast_valid && !FIFO/full(cdc_fifo)
        FIFO/write(cdc_fifo,
            data: fast_data,
            enable: write_enable
        )

        // Slow clock domain (read side)
        read_enable: slow_ready && !FIFO/empty(cdc_fifo)
        slow_data: FIFO/read(cdc_fifo, enable: read_enable)

        [
            // Fast clock domain outputs
            fast_ready: !FIFO/full(cdc_fifo)

            // Slow clock domain outputs
            slow_data: slow_data
            slow_valid: !FIFO/empty(cdc_fifo)
        ]
    }
}
```

### 3. Rate Matching Buffer

```boon
FUNCTION rate_matcher(burst_data, burst_valid, steady_ready) {
    BLOCK {
        // Buffer bursts, output steady stream
        buffer: FIFO {
            depth: 64
            width: 16
            almost_empty: 8    // Keep minimum 8 entries
        }

        // Write bursts
        FIFO/write(buffer,
            data: burst_data,
            enable: burst_valid && !FIFO/full(buffer)
        )

        // Read steady stream (only if enough data buffered)
        can_read: !FIFO/almost_empty(buffer) && steady_ready
        steady_data: FIFO/read(buffer, enable: can_read)

        [
            burst_ready: !FIFO/full(buffer)
            steady_data: steady_data
            steady_valid: can_read
            buffer_level: FIFO/count(buffer)
        ]
    }
}
```

### 4. DMA Buffer

```boon
FUNCTION dma_buffer(mem_data, mem_valid, stream_ready) {
    BLOCK {
        // Large FIFO for DMA transfers
        dma_fifo: FIFO {
            depth: 1024
            width: 64
            almost_full: 960   // Request more when 64 free
        }

        // Memory writes
        FIFO/write(dma_fifo,
            data: mem_data,
            enable: mem_valid && !FIFO/full(dma_fifo)
        )

        // Stream reads
        stream_data: FIFO/read(dma_fifo,
            enable: stream_ready && !FIFO/empty(dma_fifo)
        )

        [
            mem_request: FIFO/almost_full(dma_fifo)  // Request refill
            stream_data: stream_data
            stream_valid: !FIFO/empty(dma_fifo)
        ]
    }
}
```

### 5. Pipeline Decoupling

```boon
FUNCTION decoupled_pipeline(stage1_out, stage1_valid, stage2_ready) {
    BLOCK {
        // Small FIFO to decouple pipeline stages
        stage_fifo: FIFO {
            depth: 4
            width: 32
        }

        // Stage 1 writes
        FIFO/write(stage_fifo,
            data: stage1_out,
            enable: stage1_valid && !FIFO/full(stage_fifo)
        )

        // Stage 2 reads
        stage2_in: FIFO/read(stage_fifo,
            enable: stage2_ready && !FIFO/empty(stage_fifo)
        )

        [
            stage1_ready: !FIFO/full(stage_fifo)
            stage2_in: stage2_in
            stage2_valid: !FIFO/empty(stage_fifo)
        ]
    }
}
```

---

## Hardware Synthesis

### Synchronous FIFO (type: "sync")

**Compiles to:**
```systemverilog
module sync_fifo #(
    parameter DEPTH = 8,
    parameter WIDTH = 8
) (
    input  logic clk,
    input  logic rst,

    // Write port
    input  logic             wr_en,
    input  logic [WIDTH-1:0] wr_data,

    // Read port
    input  logic             rd_en,
    output logic [WIDTH-1:0] rd_data,

    // Status
    output logic full,
    output logic empty,
    output logic [$clog2(DEPTH):0] count
);

    // Internal storage
    logic [WIDTH-1:0] mem [0:DEPTH-1];
    logic [$clog2(DEPTH)-1:0] wr_ptr, rd_ptr;

    // Write logic
    always_ff @(posedge clk) begin
        if (wr_en && !full) begin
            mem[wr_ptr] <= wr_data;
            wr_ptr <= wr_ptr + 1;
        end
    end

    // Read logic
    always_ff @(posedge clk) begin
        if (rd_en && !empty) begin
            rd_data <= mem[rd_ptr];
            rd_ptr <= rd_ptr + 1;
        end
    end

    // Status logic
    assign empty = (count == 0);
    assign full = (count == DEPTH);

endmodule
```

### Asynchronous FIFO (type: "async")

**Compiles to:**
- Dual-port memory
- Gray code counters (for CDC safety)
- Synchronizer chains (2-FF)
- Empty/full logic in each clock domain

```systemverilog
module async_fifo #(
    parameter DEPTH = 16,
    parameter WIDTH = 32
) (
    // Write clock domain
    input  logic             wr_clk,
    input  logic             wr_rst,
    input  logic             wr_en,
    input  logic [WIDTH-1:0] wr_data,
    output logic             full,

    // Read clock domain
    input  logic             rd_clk,
    input  logic             rd_rst,
    input  logic             rd_en,
    output logic [WIDTH-1:0] rd_data,
    output logic             empty
);
    // Gray code pointers + synchronizers
    // (Standard async FIFO implementation)
endmodule
```

### Implementation Trade-offs

| Depth | Implementation | Use Case |
|-------|---------------|----------|
| 2-16 | Flip-flop array | Interface buffers, small FIFOs |
| 32-512 | Distributed RAM | Rate matching, CDC |
| 1024+ | Block RAM (BRAM) | DMA buffers, large queues |

Compiler chooses implementation based on depth and target FPGA.

---

## Design Rationale

### Why FIFO is Hardware-Only

**Software doesn't need FIFOs:**
- Use reactive LIST for dynamic queues
- Use CHANNEL for async message passing (future)
- Use arrays for fixed buffers

**Hardware needs FIFOs because:**
- Physical clock domain crossing
- Rate matching between independent modules
- Pipeline decoupling for timing
- Interface protocols (UART, SPI, etc.)

### Why Separate from UNFOLD

**UNFOLD** - Functional sequence generation
- Pure, no side effects
- Returns List of states
- Compiles to iterative logic

**FIFO** - Stateful buffering module
- Side effects (write/read change state)
- Multiple references share same module
- Compiles to memory + pointers

**Completely different semantics!** Separating them keeps both clean.

### Why Explicit FIFO/write and FIFO/read

**Makes side effects obvious:**
```boon
// Clear that this modifies FIFO state
FIFO/write(fifo, data: value, enable: condition)

// Clear that this consumes from FIFO
data: FIFO/read(fifo, enable: condition)
```

**Alternative (rejected):**
```boon
// Looks too functional, hides side effects
fifo |> write(data: value)
data: fifo |> read()
```

Explicit syntax makes stateful nature clear.

---

## Safety and Best Practices

### 1. Always Check full/empty Before Write/Read

```boon
// ❌ BAD - Can overflow
FIFO/write(fifo, data: data, enable: valid)

// ✅ GOOD - Check full flag
FIFO/write(fifo, data: data, enable: valid && !FIFO/full(fifo))
```

### 2. Monitor Overflow/Underflow Flags

```boon
status: [
    overflow: FIFO/overflow(fifo)
    underflow: FIFO/underflow(fifo)
]

// Use in simulation/debug to catch errors
```

### 3. Size FIFOs Appropriately

```boon
// Rule of thumb for CDC:
// depth >= 2 * max_burst_size

// For rate matching:
// depth >= average_burst_size * safety_margin
```

### 4. Use almost_full/almost_empty for Flow Control

```boon
// Request more data when FIFO getting empty
dma_request: FIFO/almost_empty(buffer)

// Stop accepting when FIFO getting full
backpressure: FIFO/almost_full(buffer)
```

---

## Comparison with Queue (Old Design)

| Aspect | Queue (Old) | FIFO (New) |
|--------|------------|-----------|
| **Scope** | HW and SW | Hardware only |
| **Semantics** | Ambiguous (pull-based?) | Clear (stateful module) |
| **Creation** | Queue/bounded(...) | FIFO {...} |
| **Write** | Queue/append(...) | FIFO/write(...) |
| **Read** | Queue/take(...) | FIFO/read(...) |
| **Side effects** | Unclear | Explicit |
| **Multi-consumer** | Undefined | Shared module (same instance) |
| **Confusion** | High (two different things) | None (hardware only) |

**FIFO is better because:**
- ✅ Clear that it's hardware-only
- ✅ Explicit side effects
- ✅ No confusion with functional constructs
- ✅ Obvious stateful semantics
- ✅ Better separation of concerns

---

## Future Enhancements

### 1. FIFO with Commit/Abort

```boon
// Speculative writes with rollback
FIFO/write_spec(fifo, data: value, tag: transaction_id)
FIFO/commit(fifo, tag: transaction_id)
FIFO/abort(fifo, tag: transaction_id)
```

### 2. Multi-Clock FIFOs

```boon
FIFO {
    depth: 16
    type: "multi_clock"
    write_clock: clk_a
    read_clock: clk_b
}
```

### 3. Scatter/Gather FIFOs

```boon
// Multiple writers, single reader
FIFO {
    depth: 32
    write_ports: 4  // Round-robin arbitration
    read_ports: 1
}
```

### 4. Priority FIFOs

```boon
FIFO {
    depth: 16
    priority_levels: 4  // High priority exits first
}
```

---

## Open Questions

1. **Should FIFO/read return Option<T> when empty?**
   - Current: Returns value (undefined when empty, rely on enable)
   - Alternative: Return Some(value) or None
   - Leaning towards current (hardware doesn't have Option)

2. **How to handle errors (overflow/underflow)?**
   - Current: Sticky flags (FIFO/overflow, FIFO/underflow)
   - Alternative: Raise warnings, block writes/reads
   - Leaning towards sticky flags (hardware standard)

3. **Should width be required or inferred?**
   - Current: Optional, inferred from data type
   - Alternative: Always required
   - Leaning towards inferred (less boilerplate)

---

## Next Steps

1. Implement FIFO primitive in compiler
2. Update hardware examples to use FIFO
3. Remove Queue primitive entirely (replaced by UNFOLD + FIFO)
4. Write FIFO synthesis guide
5. Add simulation support (testbench helpers)

---

**FIFO: Clear, explicit, hardware buffering! 📦➡️📦**
