# HDL Gap Analysis for Boon - Quick Report

**Date:** 2025-11-20
**Purpose:** Identify features in modern HDLs (SpinalHDL, Spade, Chisel, Amaranth, VHDL) that Boon lacks
**Goal:** Prioritize additions to make Boon production-ready for hardware design

---

## Executive Summary

After analyzing Boon's current hardware capabilities and researching SpinalHDL, Spade, Chisel, Amaranth, and VHDL-2019, the **two most critical missing features** are:

1. **First-class pipeline stage abstraction** (like Spade)
2. **Streaming interfaces with flow control** (ready/valid handshaking)

These are fundamental to modern hardware design and are present in all competitive HDLs.

---

## Boon's Current Strengths

### What Boon Does Well:
✅ **Clock semantics** - PASSED context for ambient clock signals
✅ **Register patterns** - LATEST for FSMs, Bits/sum for accumulators
✅ **BITS operations** - Comprehensive arithmetic and bit manipulation
✅ **Fixed-size LIST** - Elaboration-time unrolling (map, fold, scan, chain)
✅ **Pattern matching** - WHEN/WHILE for declarative control flow
✅ **MEMORY primitive** - Clean Block RAM abstraction
✅ **PULSES** - Counted iteration for loops
✅ **Type safety** - Explicit width tracking, compile-time constants
✅ **Reactive semantics** - Flow-based programming with pipes

---

## CRITICAL GAPS - High Priority for Hardware Design

### 1. Pipeline Stage Abstraction ⭐⭐⭐⭐⭐

**What Spade has:**
```spade
pipeline(4) X(clk: clock, a: int<32>) -> int<64> {
    'fetch: reg;
    'decode: reg;
    'execute: reg * 2;  // Multiple stages
    'writeback: result
}
```

**Features:**
- Named stages (`'fetch`, `'decode`, `'execute`, `'writeback`)
- Stage references: `stage(+1).value`, `stage(execute).result`
- Automatic register insertion by compiler
- Forward and backward references for hazard detection

**What SpinalHDL has:**
- Pipeline library with automatic retiming
- DirectLink, StageLink, S2mLink for connecting nodes
- Parametrizable pipeline locations
- Moving logic between stages without manual register management

**What Boon lacks:**
- No first-class pipeline abstraction
- Manual register placement with `LATEST` for each stage
- No stage naming or cross-stage references
- No automatic register insertion

**Impact:** Pipelines are fundamental in CPUs, DSP, and high-performance designs. Without this, complex pipelines are error-prone and hard to maintain.

**Suggested Boon syntax:**
```boon
FUNCTION pipelined_alu(instruction) {
    PIPELINE {
        stages: 5

        'fetch: BLOCK {
            instr: fetch_from_memory(instruction)
        }

        'decode: BLOCK {
            decoded: decode(PIPELINE.fetch.instr)
        }

        'execute: BLOCK {
            result: alu(PIPELINE.decode.decoded)
            -- Access future stage for hazard detection
            hazard: PIPELINE.writeback.reg_write
        }

        'memory: BLOCK {
            mem_result: memory_access(PIPELINE.execute.result)
        }

        'writeback: BLOCK {
            final: write_back(PIPELINE.memory.mem_result)
        }

        [result: PIPELINE.writeback.final]
    }
}
```

---

### 2. Streaming Interfaces with Flow Control ⭐⭐⭐⭐⭐

**What SpinalHDL has:**
```scala
val stream = Stream(UInt(8 bits))
stream.valid   // Producer has data
stream.ready   // Consumer can accept
stream.payload // Data
```

**Protocol rules:**
- `valid` cannot depend combinatorially on `ready` (prevents combinatorial loops)
- Transfer occurs only when both `valid` and `ready` are high
- Also has `Flow` (valid/payload only, no backpressure)

**What Chisel has:**
```scala
val io = IO(new Bundle {
  val in = Flipped(Decoupled(UInt(8.W)))
  val out = Decoupled(UInt(8.W))
})
```

- `Decoupled` wraps signals with ready/valid
- Standard library includes queues, arbiters built on Decoupled
- Hardware generators parameterized by Decoupled interfaces

**What Boon lacks:**
- No built-in ready/valid interface abstraction
- No backpressure mechanism
- No streaming protocol primitives
- Must manually implement handshaking logic every time

**Impact:** Modern hardware heavily uses streaming for:
- Data flow between modules
- IP core interfaces
- Network packet processing
- Video/audio pipelines

**Suggested Boon syntax:**
```boon
-- Define streaming interface type
StreamInterface: [
    valid: Bool
    ready: Bool
    data: BITS { width, ... }
]

-- Producer side
producer_stream: StreamInterface |> Stream/send(data: payload)

-- Consumer side
consumer_data: consumer_stream |> Stream/receive()

-- Queue/FIFO with backpressure
buffered_stream: input_stream |> Stream/queue(depth: 16)
```

---

### 3. Clock Domain Crossing (CDC) Primitives ⭐⭐⭐⭐

**What SpinalHDL has:**
- **Compile-time CDC violation detection** - Won't compile if you accidentally read a signal from another clock domain
- `crossClockDomain` tag for intentional crossings
- `BufferCC` - Synchronizer for bits/gray-coded signals
- `StreamFifoCC` - High-bandwidth CDC using async FIFO
- `StreamCCByToggle` - Low-resource CDC for slower transfers

**What Amaranth has:**
- Clock domain crossing primitives in standard library
- Synchronous and asynchronous FIFOs

**What Boon has:**
- `PASSED` context can hold multiple clocks
- But no CDC safety mechanisms

**What Boon lacks:**
- No compile-time CDC checking
- No CDC-safe synchronizer primitives
- No async FIFO for clock crossing
- Easy to create metastability bugs

**Impact:** CDC bugs are:
- Extremely difficult to debug (intermittent, timing-dependent)
- Can cause silent data corruption
- Often only appear in production hardware

**Suggested Boon additions:**
```boon
-- Synchronizer for single bits (2-FF or 3-FF)
synced_signal: async_signal |> CDC/synchronize(
    from: clk_domain_a,
    to: clk_domain_b,
    stages: 2  -- Number of FF stages
)

-- Async FIFO for high-bandwidth crossing
fifo: CDC/async_fifo(
    write_domain: clk_a,
    read_domain: clk_b,
    depth: 16,
    data_width: 8
)

-- Handshake-based crossing (for control signals)
handshake: CDC/handshake(
    from: clk_a,
    to: clk_b,
    data: control_signal
)

-- Compile-time checking with explicit permission
#[allow_cdc] signal: cross_domain_signal  -- Intentional crossing
```

---

### 4. Interface/Bundle Abstraction ⭐⭐⭐⭐

**What VHDL-2019 has:**
```vhdl
type AXI_Interface is record
  awvalid : std_logic;
  awready : std_logic;
  awaddr  : std_logic_vector(31 downto 0);
  -- ... many more signals
end record;
```
- Bundles ports together
- Reduces instantiation verbosity dramatically

**What Chisel has:**
```scala
class MyBundle extends Bundle {
  val data = UInt(32.W)
  val valid = Bool()
  val ready = Bool()
}
```

**What Amaranth has:**
- Python wrappers for AXI/APB that encapsulate all related signals
- Makes configuration and connection much easier

**What Boon lacks:**
- Records exist but no "interface" concept
- No standard protocol interfaces (AXI, Wishbone, APB)
- Each signal must be explicitly passed/wired

**Impact:** Complex interfaces like AXI4 have 40+ signals. Without bundles, module instantiation becomes extremely verbose and error-prone.

**Suggested Boon syntax:**
```boon
-- Define reusable interface
AXI4_LITE: INTERFACE {
    -- Write address channel
    awvalid: Bool
    awready: Bool
    awaddr: BITS { 32, ... }
    awprot: BITS { 3, ... }

    -- Write data channel
    wvalid: Bool
    wready: Bool
    wdata: BITS { 32, ... }
    wstrb: BITS { 4, ... }

    -- Write response channel
    bvalid: Bool
    bready: Bool
    bresp: BITS { 2, ... }

    -- Read address channel
    arvalid: Bool
    arready: Bool
    araddr: BITS { 32, ... }
    arprot: BITS { 3, ... }

    -- Read data channel
    rvalid: Bool
    rready: Bool
    rdata: BITS { 32, ... }
    rresp: BITS { 2, ... }
}

-- Use in function signatures
FUNCTION axi_slave(axi_master: AXI4_LITE) {
    -- Access as axi_master.awvalid, axi_master.awaddr, etc.
    [axi_slave: AXI4_LITE]
}
```

---

## IMPORTANT GAPS - Medium-High Priority

### 5. Formal Verification Support ⭐⭐⭐⭐

**What SpinalHDL has:**
- Built-in formal backend using SymbiYosys (open-source)
- `assert`, `assume`, `cover` statements
- FormalConfig similar to SimConfig
- Direct integration with formal tools

**What Chisel has:**
- ChiselTest with formal verification
- Can generate SVA (SystemVerilog Assertions)

**What Boon lacks:**
- No formal verification primitives
- No assertion language
- No integration with formal tools (like SymbiYosys, ABC, etc.)

**Suggested Boon syntax:**
```boon
FUNCTION counter(rst, en) {
    count: BITS{8, 10u0} |> LATEST count {
        PASSED.clk |> THEN {
            rst |> WHILE {
                True => BITS{8, 10u0}
                False => en |> WHILE {
                    True => count |> Bits/increment()
                    False => count
                }
            }
        }
    }

    -- Formal assertions
    #[assert] count |> Bits/less_than(BITS{8, 10u256})  -- Never overflow
    #[assume] (en |> Bool/and(count |> Bits/equal(BITS{8, 10u255}))) |> Bool/not()
    #[cover] count |> Bits/equal(BITS{8, 10u255})  -- Coverage goal

    [count: count]
}
```

---

### 6. Built-in Simulation/Testing Framework ⭐⭐⭐⭐

**What SpinalHDL has - SpinalSim:**
- Write testbenches in Scala
- `fork`/`join` simulation processes
- `sleep`, `waitUntil` primitives
- Multiple simulator backends (Verilator, GHDL, VCS, Icarus)
- Integration with Scala unit test frameworks

**What Chisel has - ChiselTest:**
- Poke/peek interface for signals
- `step()` to advance clock
- `expect()` for checking values
- Integrated with Scala test frameworks

**What Boon lacks:**
- No integrated simulation framework
- Must rely on external tools
- No standardized testbench patterns

**Suggested Boon syntax:**
```boon
TEST counter_basic {
    -- Instantiate DUT
    dut: counter(rst: test_rst, en: test_en)

    -- Test sequence
    test_rst <- True
    WAIT_CYCLES 1

    test_rst <- False
    test_en <- True

    REPEAT 10 {
        WAIT_CYCLES 1
        ASSERT dut.count |> Bits/equal(BITS{8, CYCLE_COUNT})
    }

    test_en <- False
    WAIT_CYCLES 5
    ASSERT dut.count |> Bits/equal(BITS{8, 10u10})  -- Should hold
}
```

---

### 7. Hardware Generator Patterns ⭐⭐⭐

**What Chisel has:**
- Full Scala language for generators
- For loops, conditionals at elaboration time
- Parameterized classes

**What SpinalHDL has:**
- Scala-based generation
- Component factories
- Conditional elaboration

**What Spade has:**
- Type generics: `<T>`
- Compile-time integer generics: `<#uint N>`

**What Boon has:**
- Generic parameters (compile-time constants)
- LIST operations for unrolling
- Width parameters

**What could be improved:**
- More flexible metaprogramming
- Generate statement equivalent
- Conditional instantiation

**Example need:**
```boon
-- Generate N parallel adders
FUNCTION parallel_adders<N>(inputs: LIST{N, BITS{8, ...}}) {
    -- Would be nice to have more generation constructs
    results: inputs |> List/map(input, result: input |> Bits/increment())
    [results: results]
}
```

---

### 8. Standard Protocol Libraries ⭐⭐⭐

**What SpinalHDL has:**
- AXI4, AXI4-Lite, AXI4-Stream
- APB (ARM Peripheral Bus)
- Wishbone
- AMBA protocols

**What Amaranth has:**
- Interface wrappers for AXI, APB
- Streamlined integration

**What Boon lacks:**
- No standard protocol implementations
- Each protocol must be built from scratch

**Suggested additions:**
```boon
-- Ready-to-use AXI4-Lite
axi_master: AXI4_Lite/master(
    addr_width: 32,
    data_width: 32
)

-- AXI4-Stream for data pipelines
stream_source: AXI4_Stream/source(
    data_width: 64,
    user_width: 8,
    dest_width: 4
)

-- Wishbone
wishbone_master: Wishbone/master(
    addr_width: 32,
    data_width: 32,
    granularity: 8
)
```

---

## NICE-TO-HAVE GAPS - Lower Priority

### 9. Multi-Module Hierarchy Management ⭐⭐⭐

**What VHDL has:**
- Formal entity/architecture/configuration system
- Clear component declarations

**What Verilog has:**
- Module hierarchy with parameters
- Generate blocks

**What Boon could improve:**
- More explicit module/hierarchy semantics
- Module metadata/attributes
- Hierarchical path naming

---

### 10. Advanced Type System Features ⭐⭐

**What VHDL-2019 has:**
- Type classes for generics
- Implicit operations (maximum, to_string, =)
- Enhanced generic types

**What Boon could add:**
- Type constraints/bounds
- Refinement types
- More sophisticated type inference

---

### 11. Debugging/Introspection Support ⭐⭐

**What others have:**
- Rich debugging with host language tools (Scala, Python)
- Intermediate representations (FIRRTL in Chisel)
- Waveform annotation

**What Boon could add:**
- Debug metadata
- IR for analysis/optimization
- Better error messages with hardware context

---

## PRIORITIZED ROADMAP

### TIER 1: Must-Have for Production (Immediate - 0-6 months)

**1. Streaming Interface Abstraction**
- Ready/valid protocol
- Backpressure handling
- Queue/FIFO primitives
- Compile-time protocol checking

**2. Pipeline Stage Abstraction**
- Named stages
- Automatic register insertion
- Stage references (forward/backward)
- Compiler-managed timing

**3. Clock Domain Crossing Primitives**
- Compile-time CDC checking
- Synchronizers (2-FF, 3-FF)
- Async FIFO
- Handshake mechanisms

**4. Interface/Bundle Types**
- Record-like bundles for ports
- Reusable interface definitions
- Direction modifiers (input/output)

### TIER 2: Important for Productivity (Short-term - 6-12 months)

**5. Standard Protocol Library**
- AXI4-Lite implementation
- AXI4-Stream implementation
- Wishbone implementation
- Common interface patterns

**6. Simulation Framework**
- Basic testbench support
- Clock stepping
- Signal poke/peek
- Assertions in tests

### TIER 3: Quality of Life (Medium-term - 12-18 months)

**7. Formal Verification**
- Assert/assume/cover primitives
- SymbiYosys integration
- Property language

**8. Advanced Simulation**
- Multi-threaded tests
- VCD waveform generation
- Coverage metrics

### TIER 4: Advanced Features (Long-term - 18+ months)

**9. Hardware Generator Templates**
- Standard component library
- Arbiter generators
- Cache generators
- Bus fabric generators

**10. Advanced Type System**
- Type constraints
- Dependent types
- Refinement types

---

## COMPARISON TABLE: Boon vs Others

| Feature | Boon | SpinalHDL | Spade | Chisel | Amaranth | VHDL-2019 |
|---------|------|-----------|-------|--------|----------|-----------|
| **Pipelines** | ❌ Manual | ✅ Library | ✅✅ First-class | ⚠️ Manual | ⚠️ Manual | ❌ Manual |
| **Streaming** | ❌ None | ✅✅ Stream/Flow | ⚠️ Basic | ✅ Decoupled | ⚠️ Basic | ❌ None |
| **CDC Safety** | ❌ None | ✅✅ Built-in | ⚠️ Limited | ⚠️ Limited | ✅ Primitives | ⚠️ Limited |
| **Interfaces** | ⚠️ Records | ✅ Bundles | ⚠️ Records | ✅ Bundles | ✅ Wrappers | ✅✅ Interfaces |
| **Formal** | ❌ None | ✅✅ Built-in | ⚠️ Limited | ✅ ChiselTest | ⚠️ Limited | ✅ SVA |
| **Simulation** | ❌ None | ✅✅ SpinalSim | ⚠️ Limited | ✅ ChiselTest | ✅ Built-in | ✅ Built-in |
| **Protocols** | ❌ None | ✅✅ Rich | ❌ None | ✅ Library | ✅ Wrappers | ⚠️ Community |
| **Generators** | ⚠️ Basic | ✅✅ Scala | ✅ Generics | ✅✅ Scala | ✅✅ Python | ⚠️ Generics |
| **Reactivity** | ✅✅ Core | ✅ Signals | ❌ Static | ⚠️ Limited | ⚠️ Limited | ❌ Static |
| **Type Safety** | ✅ Good | ✅ Good | ✅✅ Strong | ✅ Good | ⚠️ Python | ✅✅ Strong |

**Legend:**
- ✅✅ = Excellent, industry-leading
- ✅ = Good, production-ready
- ⚠️ = Basic, usable but limited
- ❌ = Missing or inadequate

---

## KEY DESIGN PRINCIPLES TO MAINTAIN

When adding features, preserve Boon's unique strengths:

✅ **Reactive/flow-based semantics** - PASSED context, pipe operators
✅ **Explicit over implicit** - Width tracking, signedness, clock domains
✅ **Unified software/hardware** - Same constructs in both contexts
✅ **Pattern matching** - WHEN/WHILE for declarative control
✅ **Compile-time safety** - Early error detection
✅ **Clean syntax** - Avoid verbosity while maintaining clarity

---

## SPECIFIC RECOMMENDATIONS

### Immediate Action Items:

1. **Design streaming interface syntax**
   - Compatible with Boon's reactive philosophy
   - Built on existing PASSED/THEN/LATEST patterns
   - Consider how it integrates with MEMORY, BITS

2. **Prototype pipeline abstraction**
   - Study Spade's implementation deeply
   - Determine how stages map to Boon's BLOCK/LATEST
   - Design stage reference syntax

3. **Plan CDC primitives**
   - Research synchronizer patterns
   - Design async FIFO for Boon
   - Implement compile-time checking

### Research Questions:

- How do pipelines interact with PASSED clock context?
- Can streaming interfaces leverage existing reactive semantics?
- Should interfaces be first-class types or syntactic sugar over records?
- How to implement CDC checking without violating Boon's philosophy?

---

## CONCLUSION

Boon has excellent fundamentals for hardware design:
- Clean syntax
- Strong type safety
- Reactive semantics
- Good basic primitives (BITS, MEMORY, LATEST)

**The critical missing pieces are:**

1. **Pipelines** - Every CPU, DSP, and high-performance design needs this
2. **Streaming** - Modern hardware is all about data flow
3. **CDC** - Multi-clock designs are everywhere, bugs are severe
4. **Interfaces** - Reduces verbosity, improves maintainability

**By adding these four features, Boon can compete with modern HDLs while maintaining its unique reactive philosophy.**

The pipeline syntax from Spade and streaming interfaces from SpinalHDL/Chisel should be the top two priorities. These aren't just "nice to have" - they're fundamental to how modern hardware is designed.

---

**Priority Order:**
1. Streaming interfaces (most universally needed)
2. Pipeline stages (differentiator, Spade-inspired)
3. CDC primitives (safety-critical)
4. Interface bundles (productivity)
5. Standard protocols (interoperability)
6. Formal verification (quality)
7. Simulation framework (testing)

---

*End of Report*
