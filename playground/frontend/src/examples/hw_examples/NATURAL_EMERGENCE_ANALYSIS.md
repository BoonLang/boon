# How Pipelines and Streaming Naturally Emerge from Boon's Reactive Core

**Date:** 2025-11-20
**Key Insight:** The top 2 missing HDL features (pipelines and streaming) are NOT foreign additions to Boon - they emerge naturally from reactive semantics already present.

---

## Executive Summary

After deep analysis of Boon's reactive/flow-based architecture, I discovered that:

1. **Pipeline stages** emerge naturally from `LATEST` + `PASSED` context + compiler stage inference
2. **Streaming interfaces** emerge naturally from `LINK` + `FLUSH` + explicit flow control signals

**The mechanisms already exist in Boon** - we just need to recognize them as HDL patterns and add minimal syntactic support.

---

## Part 1: Pipelines Emerge from LATEST + PASSED

### What Boon Already Has

From `LATEST.md` and `CLOCK_SEMANTICS.md`:

```boon
// Boon register pattern
count: BITS{8, 10u0} |> LATEST count {
    PASSED.clk |> THEN {
        rst |> WHILE {
            True => BITS{8, 10u0}
            False => count |> Bits/increment()
        }
    }
}
```

**This is already a pipeline register!**
- `LATEST` = storage element
- `PASSED.clk |> THEN` = clock-triggered
- Self-reference = access to current stage value

### The Natural Extension: Multi-Stage Pipelines

**Current (manual registers):**
```boon
FUNCTION manual_pipeline(instruction) {
    BLOCK {
        // Stage 1: Fetch
        fetch_out: instruction |> LATEST fetch {
            PASSED.clk |> THEN { fetch_logic(instruction) }
        }

        // Stage 2: Decode
        decode_out: fetch_out |> LATEST decode {
            PASSED.clk |> THEN { decode_logic(decode) }
        }

        // Stage 3: Execute
        execute_out: decode_out |> LATEST execute {
            PASSED.clk |> THEN { execute_logic(execute) }
        }

        [result: execute_out]
    }
}
```

**Natural extension (stage naming + PASSED.pipeline):**
```boon
FUNCTION natural_pipeline(instruction) {
    PIPELINE {
        // Compiler recognizes LATEST blocks as stages
        // Automatically creates PASSED.pipeline context

        'fetch: instruction |> LATEST fetch {
            PASSED.clk |> THEN {
                fetch_logic(instruction)
            }
        }

        'decode: PASSED.pipeline.fetch |> LATEST decode {
            PASSED.clk |> THEN {
                decode_logic(decode)
            }
        }

        'execute: PASSED.pipeline.decode |> LATEST execute {
            PASSED.clk |> THEN {
                // Forward reference for hazards!
                hazard: PASSED.pipeline.writeback.reg_write
                execute_logic(execute, hazard)
            }
        }

        'writeback: PASSED.pipeline.execute |> LATEST writeback {
            PASSED.clk |> THEN {
                writeback_logic(writeback)
            }
        }

        [result: PASSED.pipeline.writeback.final]
    }
}
```

### What Makes This Natural?

1. **LATEST already exists** - just recognize it as a stage
2. **PASSED context already exists** - extend it with `.pipeline`
3. **Named stages** (`'fetch`) - just labels for LATEST blocks
4. **Stage references** - read from `PASSED.pipeline.stage_name`
5. **Compiler inserts registers** - already happening with LATEST!

### The Spade-Inspired Syntax, Boon-Native Implementation

**Key insight:** Spade's `pipeline(N)` with `reg` separators is syntax sugar. Boon's `LATEST` blocks already ARE the registers!

```boon
// Spade style (explicit stage count, reg keywords)
pipeline(4) div(clk, x, y) {
    stage1_result
    reg;
    stage2_result
    reg;
    stage3_result
    reg;
    stage4_result
}

// Boon style (LATEST blocks are stages, labels optional)
PIPELINE {
    'stage1: input |> LATEST s1 {
        PASSED.clk |> THEN { compute1(input) }
    }

    'stage2: PASSED.pipeline.stage1 |> LATEST s2 {
        PASSED.clk |> THEN { compute2(s2) }
    }

    'stage3: PASSED.pipeline.stage2 |> LATEST s3 {
        PASSED.clk |> THEN { compute3(s3) }
    }

    'stage4: PASSED.pipeline.stage3 |> LATEST s4 {
        PASSED.clk |> THEN { compute4(s4) }
    }

    [result: PASSED.pipeline.stage4]
}
```

**Advantages over manual approach:**
- `PASSED.pipeline` context makes stages accessible
- Stage labels enable forward/backward references
- Compiler can verify pipeline topology
- No new primitives needed!

---

## Part 2: Streaming Emerges from LINK + FLUSH

### What Boon Already Has

From `LINK_PATTERN.md` and `FLUSH.md`:

**LINK creates bidirectional reactive channels:**
```boon
// Step 1: Declare
store: [elements: [button: LINK]]

// Step 2: Provide
Element/button(element: [event: [press: LINK]])

// Step 3: Wire
button() |> LINK { store.elements.button }

// Step 4: Multiple consumers!
store.elements.button.event.press |> THEN { action1 }
store.elements.button.event.press |> THEN { action2 }
```

**FLUSH creates transparent propagation with bypass:**
```boon
result: items
    |> List/map(item =>
        item |> process() |> WHEN {
            error => FLUSH { error }  // Bypass remaining operations
        }
    )
    |> next_operation()  // Skipped if FLUSH
```

**This is already streaming with backpressure!**
- LINK = reactive channel
- FLUSH = cancel/backpressure signal
- Multiple consumers work natively

### The Natural Extension: Ready/Valid Protocol

**Current implicit streaming:**
```boon
// Producer
data_stream: LINK  // Emits values

// Consumer
result: data_stream |> THEN { value =>
    process(value)
}
```

**Natural extension (explicit flow control):**
```boon
// Streaming interface with flow control
StreamInterface: [
    valid: Bool   // Producer has data
    ready: Bool   // Consumer can accept
    data: BITS { width, ... }
]

// Producer declares streaming capability
FUNCTION producer() {
    Element/stream_source(
        element: [
            stream: LINK  // Provides streaming interface
        ]
        // Internal: generates valid when data available
    )
}

// Consumer with backpressure
FUNCTION consumer(input_stream) {
    BLOCK {
        // Consumer signals ready
        processing_capacity: capacity_available()

        // Receive only when both valid and ready
        received: input_stream |> Stream/receive(
            when_ready: processing_capacity
        ) |> WHEN {
            data => process(data)
            SKIP => SKIP  // No data this cycle
        }

        [result: received]
    }
}

// Wire streaming connection
producer_elem() |> LINK { store.streams.data_producer }
consumer_elem(input_stream: store.streams.data_producer.stream)
```

### What Makes This Natural?

1. **LINK already creates channels** - extend to include `valid`/`ready`
2. **FLUSH already implements bypass** - use for backpressure
3. **THEN already waits for events** - extend to honor `ready` signal
4. **WHEN/SKIP already handle conditional flow** - perfect for valid/ready checking

### FLUSH as Backpressure

**The connection is beautiful:**

```boon
// Producer side
result: items
    |> List/map(item =>
        item |> WHEN {
            valid_data => send_to_stream(data)
            no_data => SKIP  // No valid data
        }
    )

// Consumer signals backpressure via FLUSH
consumer_result: input_stream
    |> process()
    |> WHEN {
        buffer_full => FLUSH { Backpressure }  // Stop producing!
    }
```

**FLUSH propagates upstream:**
- Consumer FLUSHes → Producer sees backpressure
- Producer stops sending → Respects ready=False
- Resumes when consumer ready → Automatic flow control

This is **exactly** the ready/valid handshake protocol!

---

## Part 3: Comparison to Current Approach

### Pipelines: Before and After

**Before (manual registers, no stage context):**
```boon
// From QUICK_REPORT.md "What Boon lacks"
// Manual pipeline with no stage references

FUNCTION cpu_pipeline(instruction) {
    fetch_out: fetch(instruction)
    decode_out: decode(fetch_out)
    execute_out: execute(decode_out)
    // No way to reference writeback stage from execute!
    // No pipeline flush mechanism
    // Registers implicit in each function
}
```

**After (LATEST as stages, PASSED.pipeline context):**
```boon
FUNCTION cpu_pipeline(instruction) {
    PIPELINE {
        'fetch: instruction |> LATEST fetch {
            PASSED.clk |> THEN { fetch_logic(instruction) }
        }

        'decode: PASSED.pipeline.fetch |> LATEST decode {
            PASSED.clk |> THEN { decode_logic(decode) }
        }

        'execute: PASSED.pipeline.decode |> LATEST execute {
            PASSED.clk |> THEN {
                // Forward reference works!
                hazard: PASSED.pipeline.writeback.reg_write
                execute_logic(execute, hazard)
            }
        }

        'memory: PASSED.pipeline.execute |> LATEST memory {
            PASSED.clk |> THEN { memory_logic(memory) }
        }

        'writeback: PASSED.pipeline.memory |> LATEST writeback {
            PASSED.clk |> THEN { writeback_logic(writeback) }
        }

        [result: PASSED.pipeline.writeback]
    }
}
```

**What changed:**
- ✅ Named stages (`'fetch`, `'decode`, etc.)
- ✅ `PASSED.pipeline` context for stage access
- ✅ Forward references (execute sees writeback)
- ✅ Explicit registers (LATEST blocks)
- ✅ Same LATEST semantics - just organized!

### Streaming: Before and After

**Before (manual event handling, no flow control):**
```boon
// From QUICK_REPORT.md "What Boon lacks"
// No backpressure, no ready/valid protocol

producer: generate_data()
consumer: producer |> process()
// What if consumer is slow?
// What if producer is fast?
// No flow control!
```

**After (LINK-based streaming with flow control):**
```boon
// Producer with valid signal
producer: Element/stream_source(
    element: [
        stream: LINK  // Provides [valid, data]
    ]
) |> LINK { store.streams.producer }

// Consumer with ready signal
consumer: store.streams.producer.stream
    |> Stream/receive(when_ready: buffer_has_space)
    |> WHEN {
        data => process(data)
        SKIP => SKIP  // Not ready yet
    }

// Backpressure via FLUSH
result: consumer
    |> WHEN {
        buffer_full => FLUSH { Backpressure }  // Propagates to producer!
    }
```

**What changed:**
- ✅ LINK as streaming channel
- ✅ Explicit valid/ready signals
- ✅ FLUSH for backpressure
- ✅ Same reactive semantics - just extended!

---

## Part 4: Minimal Language Additions Needed

### For Pipelines

**1. PIPELINE block** (recognizes LATEST as stages)
```boon
PIPELINE {
    'stage_name: LATEST { ... }
}
```

**2. PASSED.pipeline context** (compiler-managed)
```boon
PASSED.pipeline.stage_name  // Access stage value
```

**3. Stage labels** (optional, for named references)
```boon
'fetch: ...
'decode: ...
```

**4. Compiler verification**
- Check stage dependencies (no cycles except explicit feedback)
- Verify forward references are valid
- Insert registers automatically (already done for LATEST!)

### For Streaming

**1. Stream interface type** (bundle of valid/ready/data)
```boon
StreamInterface: [
    valid: Bool
    ready: Bool
    data: T
]
```

**2. Stream/receive operator** (honors ready signal)
```boon
stream |> Stream/receive(when_ready: condition)
```

**3. Stream/send operator** (sets valid signal)
```boon
data |> Stream/send()
```

**4. FLUSH as backpressure** (already exists!)
```boon
WHEN { buffer_full => FLUSH { Backpressure } }
```

### Summary of Additions

| Feature | Mechanism | Status in Boon |
|---------|-----------|----------------|
| **Pipeline stages** | LATEST blocks | ✅ Exists |
| **Stage registers** | LATEST inserts registers | ✅ Exists |
| **Stage naming** | Labels (`'name:`) | ⚠️ New syntax |
| **Stage context** | PASSED.pipeline | ⚠️ New namespace |
| **Stage references** | PASSED.pipeline.name | ⚠️ Compiler support |
| **Streaming channels** | LINK | ✅ Exists |
| **Valid signal** | Bool in LINK | ⚠️ Protocol extension |
| **Ready signal** | Bool in LINK | ⚠️ Protocol extension |
| **Backpressure** | FLUSH | ✅ Exists |
| **Flow control** | WHEN/SKIP | ✅ Exists |

**Result:** ~70% already exists! Need minimal additions.

---

## Part 5: Why This Approach Is Better

### Compared to Adding New Primitives

**Don't do this (SpinalHDL-style new types):**
```scala
// Completely new types
val stream = Stream(UInt(8 bits))
val pipeline = new Pipeline { ... }
```

**Do this (recognize existing patterns):**
```boon
// LATEST is already a register
// LINK is already a channel
// Just add context and naming
```

### Advantages of Natural Emergence

1. **Familiar semantics** - LATEST, LINK, FLUSH already understood
2. **Less to learn** - No new execution models
3. **Composable** - Pipelines can contain streams, streams can feed pipelines
4. **Gradual adoption** - Use LATEST manually, then adopt PIPELINE sugar
5. **Hardware-friendly** - Same compilation target (registers, wires)
6. **Software-friendly** - Same reactive semantics work in software

### Conceptual Unity

```
LATEST = reactive state
PASSED = ambient context
LINK = reactive channel
FLUSH = early exit + bypass
THEN = event trigger
WHEN/SKIP = conditional flow

LATEST + PASSED.clk = hardware register
LATEST in PIPELINE = pipeline stage
PASSED.pipeline = stage context
LINK + valid/ready = streaming interface
FLUSH in stream = backpressure
```

**Everything builds on the same foundation!**

---

## Part 6: Practical Examples

### Example 1: 5-Stage CPU Pipeline

```boon
FUNCTION risc_cpu(instruction) {
    PIPELINE {
        'fetch: instruction |> LATEST fetch {
            PASSED.clk |> THEN {
                instr: fetch_from_memory(instruction)
                pc_next: pc + 4
                [instr: instr, pc: pc_next]
            }
        }

        'decode: PASSED.pipeline.fetch |> LATEST decode {
            PASSED.clk |> THEN {
                decoded: decode_instruction(decode.instr)
                rs1: read_register(decoded.rs1)
                rs2: read_register(decoded.rs2)
                [op: decoded.op, rs1: rs1, rs2: rs2, rd: decoded.rd]
            }
        }

        'execute: PASSED.pipeline.decode |> LATEST execute {
            PASSED.clk |> THEN {
                // Forward reference for hazard detection
                wb_hazard: PASSED.pipeline.writeback.reg_write

                // Forwarding logic
                rs1_fwd: execute.rs1 |> WHEN {
                    wb_hazard.rd == execute.rd => wb_hazard.value
                    __ => execute.rs1
                }

                result: alu(execute.op, rs1_fwd, execute.rs2)
                [result: result, rd: execute.rd]
            }
        }

        'memory: PASSED.pipeline.execute |> LATEST memory {
            PASSED.clk |> THEN {
                mem_result: memory_access(memory.result)
                [result: mem_result, rd: memory.rd]
            }
        }

        'writeback: PASSED.pipeline.memory |> LATEST writeback {
            PASSED.clk |> THEN {
                write_register(writeback.rd, writeback.result)
                [reg_write: [rd: writeback.rd, value: writeback.result]]
            }
        }

        [result: PASSED.pipeline.writeback]
    }
}
```

**This is natural Boon!**
- LATEST blocks are stages
- PASSED.pipeline provides access
- Forward references for hazards
- Clock-triggered updates

### Example 2: Streaming FIFO with Backpressure

```boon
FUNCTION streaming_fifo(depth) {
    BLOCK {
        // Input stream interface
        input: LINK  // [valid: Bool, ready: Bool, data: BITS{8, ...}]

        // Output stream interface
        output: LINK  // [valid: Bool, ready: Bool, data: BITS{8, ...}]

        // FIFO storage
        buffer: MEMORY { depth, BITS{8, 10u0} }

        // Pointers
        write_ptr: BITS{8, 10u0} |> LATEST wr {
            PASSED.clk |> THEN {
                // Write when input valid AND ready
                input.valid |> Bool/and(ready_to_accept) |> WHEN {
                    True => wr |> Bits/increment()
                    False => wr
                }
            }
        }

        read_ptr: BITS{8, 10u0} |> LATEST rd {
            PASSED.clk |> THEN {
                // Read when output ready AND valid
                output.ready |> Bool/and(data_available) |> WHEN {
                    True => rd |> Bits/increment()
                    False => rd
                }
            }
        }

        // Status
        count: (write_ptr - read_ptr) % depth
        data_available: count > 0
        ready_to_accept: count < depth

        // Write to buffer
        buffer_write: buffer
            |> Memory/write_entry(entry: BLOCK {
                input.valid |> Bool/and(ready_to_accept) |> WHEN {
                    True => [address: write_ptr, data: input.data]
                    False => SKIP
                }
            })

        // Read from buffer
        output_data: buffer_write |> Memory/read(address: read_ptr)

        // Output stream
        [
            input_ready: ready_to_accept  // Backpressure signal
            output_valid: data_available   // Data available signal
            output_data: output_data
        ]
    }
}
```

**This is natural Boon!**
- LINK for streaming interface
- MEMORY for buffer
- LATEST for pointers
- Bool/and for ready/valid logic
- SKIP for no operation

### Example 3: Stream Processing Pipeline

```boon
FUNCTION video_pipeline(pixel_stream) {
    BLOCK {
        // Stage 1: Color conversion (streaming)
        rgb_to_yuv: pixel_stream
            |> Stream/map(pixel =>
                pixel |> color_convert()
            )
            |> LINK { store.streams.yuv }

        // Stage 2: Filtering (streaming with backpressure)
        filtered: store.streams.yuv
            |> Stream/map(yuv =>
                yuv |> apply_filter()
            )
            |> WHEN {
                buffer_full => FLUSH { Backpressure }
                data => data
            }
            |> LINK { store.streams.filtered }

        // Stage 3: Encoding (multi-cycle pipeline)
        encoded: store.streams.filtered
            |> PIPELINE {
                'stage1: input |> LATEST s1 {
                    PASSED.clk |> THEN { dct(input) }
                }
                'stage2: PASSED.pipeline.stage1 |> LATEST s2 {
                    PASSED.clk |> THEN { quantize(s2) }
                }
                'stage3: PASSED.pipeline.stage2 |> LATEST s3 {
                    PASSED.clk |> THEN { entropy_encode(s3) }
                }
                [result: PASSED.pipeline.stage3]
            }

        [output: encoded]
    }
}
```

**Combines both patterns:**
- Streaming between stages (LINK)
- Pipelined processing within stages (PIPELINE)
- Backpressure (FLUSH)
- All natural Boon!

---

## Part 7: Implementation Strategy

### Phase 1: Recognize Existing Patterns (No Changes)

Document that these patterns already work:

```boon
// Manual pipeline stages
stage1: input |> LATEST s1 { PASSED.clk |> THEN { ... } }
stage2: stage1 |> LATEST s2 { PASSED.clk |> THEN { ... } }

// Streaming with LINK
producer() |> LINK { store.streams.data }
consumer: store.streams.data |> process()

// Backpressure with FLUSH
result |> WHEN { error => FLUSH { error } }
```

### Phase 2: Add Pipeline Sugar

1. **PIPELINE block** - recognize LATEST as stages
2. **PASSED.pipeline** - compiler creates context
3. **Stage labels** - `'name:` before LATEST
4. **Verification** - check stage topology

### Phase 3: Add Streaming Protocol

1. **StreamInterface type** - [valid, ready, data]
2. **Stream/receive** - honors ready signal
3. **Stream/send** - sets valid signal
4. **Documentation** - FLUSH as backpressure

### Phase 4: Optimize

1. **Register merging** - eliminate redundant LATEST blocks
2. **Pipeline retiming** - move logic between stages
3. **Stream fusion** - optimize connected streams

---

## Conclusion: A Coherent Vision

**The top 2 HDL gaps are not foreign additions - they're natural extensions of Boon's core:**

### Pipelines = LATEST + PASSED + Labels
- LATEST blocks → pipeline stages
- PASSED.pipeline → stage context
- Labels (`'name:`) → stage naming
- Forward references → hazard detection

### Streaming = LINK + FLUSH + Flow Control
- LINK → streaming channel
- FLUSH → backpressure
- valid/ready → flow control signals
- Multiple consumers → already work!

### Why This Matters

1. **Coherent language** - one model for everything
2. **Less to learn** - reuse existing concepts
3. **Natural evolution** - not bolted on
4. **Hardware + software** - same semantics
5. **Gradual adoption** - use manually first, then sugar

**This is the Boon way:**
- Reactive semantics everywhere
- Explicit data flow
- Compile-time verification
- Minimal primitives, maximum expressiveness

---

**Next Steps:**
1. Prototype PIPELINE block with PASSED.pipeline context
2. Design StreamInterface type and protocol
3. Update hardware examples to use patterns
4. Write specification for compiler support

The foundation is already there. We just need to recognize it.
