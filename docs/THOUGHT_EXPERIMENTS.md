# Thought Experiments

Deep architectural explorations of Boon's runtime design and its potential for alternative execution environments.

---

## 1. Stack-Only Runtime (No Heap Allocations)

**Question:** Would it be possible to keep the entire runtime and most values on the stack? No heap allocations, even for Arc and similar constructs?

**Short answer: Yes, but with constraints that align well with Boon's target domains.**

### Current Heap Usage

The current runtime uses heap for:
- `Arc<ValueActor>` - shared ownership for subscriptions
- `mpsc` channels - actor communication
- `Vec`, `HashMap` - dynamic collections
- Boxed futures - async task state
- `String`, `Value` variants - variable-sized data

### Path to Stack-Only

**Arena/Region-Based Allocation**

Instead of per-object heap allocation, use a pre-allocated memory region:

```rust
struct Runtime<const ACTOR_CAPACITY: usize, const VALUE_CAPACITY: usize> {
    actors: [MaybeUninit<ActorSlot>; ACTOR_CAPACITY],
    actor_count: usize,
    values: [MaybeUninit<ValueSlot>; VALUE_CAPACITY],
    value_cursor: usize,
}
```

This is "stack-like" - bump allocation, bulk deallocation per scope.

**Generational Indices Instead of Arc**

Replace `Arc<ValueActor>` with typed indices:

```rust
#[derive(Copy, Clone)]
struct ActorId {
    index: u16,
    generation: u16,  // Prevents use-after-free bugs
}

impl Runtime {
    fn get_actor(&self, id: ActorId) -> Option<&ActorSlot> {
        let slot = &self.actors[id.index as usize];
        (slot.generation == id.generation).then_some(slot)
    }
}
```

This is Copy (no refcounting), bounds-checked, and ABA-safe.

**Static Subscription Topology**

The key insight: Boon programs have a *statically known* dataflow graph. At compile time, we can:
1. Count exact number of actors needed
2. Determine subscription relationships
3. Pre-wire all connections

```rust
// Compile-time generated
const PROGRAM_ACTORS: usize = 47;
const SUBSCRIPTIONS: [(ActorId, ActorId); 62] = [...];
```

**Bounded Data Structures**

Replace dynamic collections with fixed-capacity:

```rust
struct BoundedList<T, const N: usize> {
    items: [MaybeUninit<T>; N],
    len: usize,
}

struct BoundedString<const N: usize> {
    bytes: [u8; N],
    len: usize,
}
```

For Boon programs, bounds come from:
- UI lists: Reasonable max (1000 items?)
- Text: Known max lengths
- Objects: Fixed field count

**No-Async Alternative**

The async runtime is the trickiest part. Options:

1. **Cooperative scheduling without futures**: Each actor is a state machine with explicit `poll()` method:

```rust
enum ActorState {
    WaitingForInput,
    Computing { step: u8 },
    Ready { value: Value },
}

impl Actor {
    fn poll(&mut self, runtime: &mut Runtime) -> Poll<Value> {
        match &mut self.state {
            WaitingForInput => {
                if let Some(input) = runtime.check_input(self.input_id) {
                    self.state = Computing { step: 0 };
                    Poll::Pending
                } else {
                    Poll::Pending
                }
            }
            // ...
        }
    }
}
```

2. **Stackless coroutines**: Rust's async/await compiles to state machines - we could do the same transformation explicitly.

### Constraints for Stack-Only

| Constraint | Implication |
|------------|-------------|
| Bounded actor count | Static analysis or runtime limit |
| Bounded recursion | No unbounded recursive functions |
| Bounded collections | Max list/object sizes |
| No dynamic actor creation | All actors pre-allocated |
| Scope-based lifetimes | Actors tied to lexical scopes |

**These constraints are natural for Boon's target domains:**
- UI: Bounded screen size, bounded widget count
- Hardware: Fixed resources
- Embedded: Known memory limits

### Implementation Sketch

```rust
// Fully stack-allocated runtime
struct StackRuntime<'scope> {
    actors: ActorArena<'scope, 256>,
    subscriptions: SubscriptionTable<512>,
    values: ValueArena<'scope, 1024>,
    scheduler: StaticScheduler<256>,
}

impl<'scope> StackRuntime<'scope> {
    // All operations are in-place, no allocation
    fn evaluate(&mut self, expr: &Expr) -> ActorId {
        let id = self.actors.alloc();
        // ...
        id
    }

    fn run_until_stable(&mut self) {
        while let Some(actor_id) = self.scheduler.next_ready() {
            self.actors[actor_id].step(&mut self.subscriptions);
        }
    }
}
```

---

## 2. FPGA Circuit Design for Boon

**Question:** How would you design a runtime as an FPGA circuit that would be able to process Boon programs naturally and efficiently?

**This is where Boon's design really shines - it maps almost directly to hardware.**

### Fundamental Mapping

| Boon Concept | FPGA Equivalent |
|--------------|-----------------|
| Variable | Wire bundle + register |
| Actor | Finite State Machine (FSM) |
| Subscription | Physical wire connection |
| Value change | Signal transition |
| LATEST | Multiplexer + change detector |
| HOLD | Register with feedback loop |
| THEN | Triggered latch |
| WHEN/WHILE | Conditional enable logic |
| LINK | I/O port binding |

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        BOON FPGA CORE                           │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐         │
│  │  Actor 0    │───▶│  Actor 1    │───▶│  Actor 2    │         │
│  │  (Counter)  │    │  (LATEST)   │    │  (Display)  │         │
│  │             │    │             │◀───│             │         │
│  │ ┌─────────┐ │    │ ┌─────────┐ │    │ ┌─────────┐ │         │
│  │ │ state   │ │    │ │ mux     │ │    │ │ output  │ │         │
│  │ │ register│ │    │ │ logic   │ │    │ │ encoder │ │         │
│  │ └─────────┘ │    │ └─────────┘ │    │ └─────────┘ │         │
│  └─────────────┘    └─────────────┘    └─────────────┘         │
│         ▲                                    │                  │
│         │          Backpressure              │                  │
│         └────────────(ready)─────────────────┘                  │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                    Global Clock & Reset                   │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐         │
│  │   BRAM 0    │    │   BRAM 1    │    │   BRAM 2    │         │
│  │  (Lists)    │    │  (Objects)  │    │  (Text)     │         │
│  └─────────────┘    └─────────────┘    └─────────────┘         │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### Actor FSM Template

Each Boon actor compiles to an FSM with this structure:

```verilog
module actor_template #(
    parameter VALUE_WIDTH = 64,
    parameter INPUT_COUNT = 2
) (
    input  wire                     clk,
    input  wire                     rst,

    // Input channels (valid/ready handshaking)
    input  wire [VALUE_WIDTH-1:0]   in_data  [INPUT_COUNT],
    input  wire                     in_valid [INPUT_COUNT],
    output wire                     in_ready [INPUT_COUNT],

    // Output channel
    output reg  [VALUE_WIDTH-1:0]   out_data,
    output reg                      out_valid,
    input  wire                     out_ready,

    // Change detection
    output reg                      out_changed
);

    // FSM states
    localparam IDLE      = 2'b00;
    localparam COMPUTING = 2'b01;
    localparam EMIT      = 2'b10;

    reg [1:0] state;
    reg [VALUE_WIDTH-1:0] stored_value;

    always @(posedge clk) begin
        if (rst) begin
            state <= IDLE;
            out_valid <= 0;
            out_changed <= 0;
        end else begin
            case (state)
                IDLE: begin
                    // Wait for any input to become valid
                    if (|in_valid) begin
                        state <= COMPUTING;
                    end
                end

                COMPUTING: begin
                    // Compute output (combinatorial or multi-cycle)
                    // ... actor-specific logic ...
                    state <= EMIT;
                end

                EMIT: begin
                    out_valid <= 1;
                    out_changed <= (out_data != stored_value);
                    if (out_ready) begin
                        stored_value <= out_data;
                        out_valid <= 0;
                        out_changed <= 0;
                        state <= IDLE;
                    end
                end
            endcase
        end
    end
endmodule
```

### LATEST Combinator Circuit

```verilog
module latest_combinator #(
    parameter VALUE_WIDTH = 64,
    parameter INPUT_COUNT = 3
) (
    input  wire                     clk,
    input  wire                     rst,

    input  wire [VALUE_WIDTH-1:0]   in_data  [INPUT_COUNT],
    input  wire                     in_valid [INPUT_COUNT],
    input  wire                     in_changed [INPUT_COUNT],
    output wire                     in_ready [INPUT_COUNT],

    output wire [VALUE_WIDTH*INPUT_COUNT-1:0] out_data,  // Tuple of all inputs
    output wire                     out_valid,
    output wire                     out_changed,
    input  wire                     out_ready
);

    // Store latest value from each input
    reg [VALUE_WIDTH-1:0] stored [INPUT_COUNT];
    reg                   has_value [INPUT_COUNT];

    // Any input changed → output changed
    assign out_changed = |in_changed;

    // Output valid when all inputs have values
    assign out_valid = &has_value;

    // Pack all values into output tuple
    generate
        for (genvar i = 0; i < INPUT_COUNT; i++) begin
            assign out_data[VALUE_WIDTH*(i+1)-1 : VALUE_WIDTH*i] = stored[i];
        end
    endgenerate

    // Update stored values
    always @(posedge clk) begin
        if (rst) begin
            for (int i = 0; i < INPUT_COUNT; i++) begin
                has_value[i] <= 0;
            end
        end else begin
            for (int i = 0; i < INPUT_COUNT; i++) begin
                if (in_valid[i]) begin
                    stored[i] <= in_data[i];
                    has_value[i] <= 1;
                end
            end
        end
    end
endmodule
```

### HOLD Combinator Circuit

```verilog
module hold_combinator #(
    parameter VALUE_WIDTH = 64
) (
    input  wire                     clk,
    input  wire                     rst,

    // Initial value
    input  wire [VALUE_WIDTH-1:0]   initial_value,
    input  wire                     initial_valid,

    // Body input (feedback from body evaluation)
    input  wire [VALUE_WIDTH-1:0]   body_data,
    input  wire                     body_valid,
    output wire                     body_ready,

    // State output (to body and downstream)
    output reg  [VALUE_WIDTH-1:0]   state_data,
    output reg                      state_valid,
    output reg                      state_changed
);

    localparam WAIT_INIT = 2'b00;
    localparam RUNNING   = 2'b01;

    reg [1:0] fsm_state;

    always @(posedge clk) begin
        if (rst) begin
            fsm_state <= WAIT_INIT;
            state_valid <= 0;
            state_changed <= 0;
        end else begin
            case (fsm_state)
                WAIT_INIT: begin
                    if (initial_valid) begin
                        state_data <= initial_value;
                        state_valid <= 1;
                        state_changed <= 1;
                        fsm_state <= RUNNING;
                    end
                end

                RUNNING: begin
                    state_changed <= 0;  // Clear after one cycle
                    if (body_valid) begin
                        state_data <= body_data;
                        state_changed <= 1;
                    end
                end
            endcase
        end
    end

    assign body_ready = (fsm_state == RUNNING);
endmodule
```

### Value Encoding

```verilog
// Tagged union for Boon values
// 4-bit tag + 60-bit payload (fits in 64 bits)

localparam TAG_NIL     = 4'h0;
localparam TAG_BOOL    = 4'h1;
localparam TAG_INT     = 4'h2;
localparam TAG_FLOAT   = 4'h3;  // Use BRAM index for IEEE754
localparam TAG_TEXT    = 4'h4;  // BRAM base + length
localparam TAG_LIST    = 4'h5;  // BRAM base + length
localparam TAG_OBJECT  = 4'h6;  // BRAM base + field count
localparam TAG_ACTOR   = 4'h7;  // Actor index

typedef struct packed {
    logic [3:0]  tag;
    logic [59:0] payload;
} boon_value_t;

// For complex values, payload is BRAM pointer:
// payload[59:44] = BRAM bank (for parallel access)
// payload[43:16] = address
// payload[15:0]  = length/count
```

### Memory Architecture

```
┌────────────────────────────────────────────────────────────┐
│                    BRAM Organization                        │
├────────────────────────────────────────────────────────────┤
│                                                            │
│  Bank 0: Lists                                             │
│  ┌──────────────────────────────────────────────────────┐ │
│  │ Addr 0-99:   List A [10 items]                       │ │
│  │ Addr 100-199: List B [50 items]                      │ │
│  │ ...                                                   │ │
│  └──────────────────────────────────────────────────────┘ │
│                                                            │
│  Bank 1: Objects                                           │
│  ┌──────────────────────────────────────────────────────┐ │
│  │ Addr 0-7:   Object X {a, b, c}                       │ │
│  │ Addr 8-15:  Object Y {x, y}                          │ │
│  │ ...                                                   │ │
│  └──────────────────────────────────────────────────────┘ │
│                                                            │
│  Bank 2: Text (UTF-8 bytes)                               │
│  ┌──────────────────────────────────────────────────────┐ │
│  │ Addr 0-255:  String pool                             │ │
│  │ ...                                                   │ │
│  └──────────────────────────────────────────────────────┘ │
│                                                            │
└────────────────────────────────────────────────────────────┘
```

### Backpressure & Flow Control

The valid/ready handshake provides natural backpressure:

```
Producer                    Consumer
   │                           │
   ├──────data─────────────────▶
   ├──────valid────────────────▶
   ◀──────ready────────────────┤
   │                           │

Transaction occurs when: valid && ready
Producer waits when:     valid && !ready
Consumer waits when:     !valid && ready
```

This maps directly to Boon's actor model - a slow consumer naturally slows the producer without buffer overflow.

### Compiler Pipeline

```
Boon Source (.bn)
       │
       ▼
   ┌───────────┐
   │  Parser   │
   └───────────┘
       │
       ▼
   ┌───────────┐
   │   AST     │
   └───────────┘
       │
       ▼
   ┌────────────────┐
   │ Static Analysis│ ─── Count actors, determine topology
   └────────────────┘
       │
       ▼
   ┌────────────────┐
   │ Resource Alloc │ ─── Assign BRAM regions, FSM states
   └────────────────┘
       │
       ▼
   ┌────────────────┐
   │  HDL Codegen   │ ─── Generate Verilog/VHDL
   └────────────────┘
       │
       ▼
   ┌────────────────┐
   │ Synthesis/P&R  │ ─── Vivado/Quartus/Yosys
   └────────────────┘
       │
       ▼
   Bitstream (.bit)
```

### Example: Counter in FPGA

```boon
button: LINK { click }
count: [0] |> HOLD state {
    button.click |> THEN { [state.0 + 1] }
}
document: TEXT { Count: {count.0} } |> Document/new()
```

Compiles to:

```verilog
module counter_program (
    input  wire        clk,
    input  wire        rst,
    input  wire        button_click,  // External I/O
    output wire [31:0] display_value  // To 7-segment or HDMI
);

    // Actor 0: button.click detector
    wire click_pulse;
    edge_detector button_edge (
        .clk(clk),
        .in(button_click),
        .rising_edge(click_pulse)
    );

    // Actor 1: HOLD state (counter)
    reg [31:0] count;
    always @(posedge clk) begin
        if (rst)
            count <= 0;
        else if (click_pulse)
            count <= count + 1;
    end

    // Actor 2: Display (passthrough)
    assign display_value = count;

endmodule
```

This is **incredibly efficient** - the entire program becomes ~50 lines of Verilog, runs at hundreds of MHz, and uses minimal FPGA resources.

### Why Boon Maps Well to FPGA

1. **No shared mutable state** - No need for arbitration logic
2. **Explicit dataflow** - Direct wire connections
3. **Actors as FSMs** - Natural hardware mapping
4. **Bounded resources** - Can verify fits on target FPGA
5. **Deterministic execution** - Predictable timing
6. **Backpressure** - Valid/ready handshaking

### Challenges & Solutions

| Challenge | Solution |
|-----------|----------|
| Dynamic actor creation | Pre-allocate max actors, use enable signals |
| Variable-size lists | BRAM with max capacity, length register |
| Recursive functions | Unroll at compile time or use call stack in BRAM |
| Floating point | Use FPGA DSP slices or fixed-point |
| Complex text ops | Soft-core CPU for text processing |
| External I/O | AXI/Wishbone bus adapters |

---

## Synthesis: The Unified Vision

Both questions point to the same insight: **Boon's actor model is fundamentally a hardware description language in disguise.**

The constraints for stack-only runtime and FPGA synthesis are nearly identical:
- Static topology
- Bounded resources
- No dynamic allocation
- Explicit ownership/lifetime

A Boon program could have three compilation targets:

```
                    Boon Program
                         │
         ┌───────────────┼───────────────┐
         ▼               ▼               ▼
    ┌─────────┐    ┌─────────┐    ┌─────────┐
    │  WASM   │    │  Native │    │  FPGA   │
    │(browser)│    │(no heap)│    │(Verilog)│
    └─────────┘    └─────────┘    └─────────┘
```

All three share:
- Arena-based memory (or registers)
- Static subscription topology
- Cooperative scheduling (or parallel hardware)
- Bounded data structures

This is why the CLAUDE.md mentions "portability to HVM, hardware synthesis, and different runtimes" - it's the core design goal.

---

## 3. Most Performant Rust Runtime

**Question:** What would be the most performant Rust runtime for executing Boon programs?

This approach optimizes the interpreter/runtime while keeping interpretation overhead.

### Current Runtime Bottlenecks

The existing runtime has these performance costs:

| Bottleneck | Cost |
|------------|------|
| `Arc` reference counting | Atomic increments/decrements on every clone/drop |
| Dynamic dispatch (`dyn Stream`) | Virtual function calls, no inlining |
| Channel overhead | Lock contention, memory allocation per message |
| Async runtime | Waker registration, task scheduling, future boxing |
| Value boxing | Heap allocation for each value, cache misses |
| HashMap lookups | Variable resolution at runtime |
| Type checking | Runtime type discrimination on every operation |

### Optimization Strategy 1: Memory Layout

**NaN-Boxing for Values**

Pack most values into 64 bits without heap allocation:

```rust
// IEEE 754 double has 52-bit mantissa, 11-bit exponent, 1-bit sign
// Quiet NaN: exponent = all 1s, top mantissa bit = 1
// This leaves 51 bits for payload + tag

#[derive(Copy, Clone)]
struct NanBoxedValue(u64);

impl NanBoxedValue {
    const NAN_MASK: u64 = 0x7FF8_0000_0000_0000;  // Quiet NaN prefix
    const TAG_MASK: u64 = 0x0007_0000_0000_0000;  // 3 bits for tag
    const PTR_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;  // 48 bits for pointer/value

    const TAG_INT: u64    = 0x0001_0000_0000_0000;
    const TAG_TRUE: u64   = 0x0002_0000_0000_0000;
    const TAG_FALSE: u64  = 0x0003_0000_0000_0000;
    const TAG_NIL: u64    = 0x0004_0000_0000_0000;
    const TAG_PTR: u64    = 0x0005_0000_0000_0000;  // Heap pointer

    #[inline(always)]
    fn is_float(&self) -> bool {
        // If not a NaN, it's a valid float
        (self.0 & Self::NAN_MASK) != Self::NAN_MASK
    }

    #[inline(always)]
    fn as_float(&self) -> Option<f64> {
        self.is_float().then(|| f64::from_bits(self.0))
    }

    #[inline(always)]
    fn from_int(i: i48) -> Self {
        Self(Self::NAN_MASK | Self::TAG_INT | (i as u64 & Self::PTR_MASK))
    }

    #[inline(always)]
    fn from_float(f: f64) -> Self {
        Self(f.to_bits())
    }
}
```

Benefits:
- No heap allocation for numbers, booleans, nil
- 48-bit integers inline (covers most use cases)
- Cache-friendly (8 bytes per value)
- Copy semantics (no refcounting)

**Arena Allocation for Complex Values**

```rust
struct ValueArena {
    // Separate arenas for different value types (better cache locality)
    lists: TypedArena<ListData>,
    objects: TypedArena<ObjectData>,
    strings: StringInterner,

    // Bump allocator for temporary values
    temp: BumpAllocator,
}

// Values reference arena slots, not heap
struct ListData {
    items: Box<[NanBoxedValue]>,  // Single allocation
    len: u32,
}
```

### Optimization Strategy 2: Actor Scheduling

**Custom Single-Threaded Executor**

Replace tokio with a minimal executor optimized for Boon's patterns:

```rust
struct BoonExecutor {
    // Ready queue (circular buffer, no allocation)
    ready: ArrayQueue<ActorId, 1024>,

    // Actors stored in contiguous array
    actors: Vec<ActorSlot>,

    // Subscription table (static after initialization)
    subscriptions: SubscriptionTable,

    // Topologically sorted update order
    update_order: Vec<ActorId>,
}

impl BoonExecutor {
    fn run_until_stable(&mut self) {
        // Process in topological order to minimize re-computation
        while let Some(actor_id) = self.ready.pop() {
            let actor = &mut self.actors[actor_id.0];

            if let Some(new_value) = actor.compute() {
                if new_value != actor.cached_value {
                    actor.cached_value = new_value;

                    // Notify subscribers (direct array access, no indirection)
                    for &subscriber_id in self.subscriptions.get(actor_id) {
                        self.ready.push(subscriber_id);
                    }
                }
            }
        }
    }
}
```

**Batched Change Propagation**

Instead of propagating each change immediately:

```rust
struct BatchedPropagator {
    // Dirty flags (bit vector for cache efficiency)
    dirty: BitVec,

    // Changes to process this frame
    pending_changes: Vec<(ActorId, NanBoxedValue)>,
}

impl BatchedPropagator {
    fn mark_dirty(&mut self, id: ActorId) {
        self.dirty.set(id.0, true);
    }

    fn flush(&mut self, actors: &mut [ActorSlot], topo_order: &[ActorId]) {
        // Process in topological order
        for &id in topo_order {
            if self.dirty.get(id.0) {
                self.dirty.set(id.0, false);
                actors[id.0].recompute();
            }
        }
    }
}
```

### Optimization Strategy 3: Subscription Model

**Static Subscription Tables**

Pre-compute subscription relationships at parse time:

```rust
// Generated at parse time, immutable at runtime
struct SubscriptionTable {
    // For each actor, list of subscribers (flattened for cache)
    subscriber_ranges: Vec<(u32, u32)>,  // (start, end) into subscribers
    subscribers: Vec<ActorId>,
}

impl SubscriptionTable {
    #[inline]
    fn get(&self, actor: ActorId) -> &[ActorId] {
        let (start, end) = self.subscriber_ranges[actor.0];
        &self.subscribers[start as usize..end as usize]
    }
}
```

**Inline Small Subscriber Lists**

```rust
enum Subscribers {
    // Most actors have few subscribers - inline up to 3
    Inline1(ActorId),
    Inline2(ActorId, ActorId),
    Inline3(ActorId, ActorId, ActorId),
    // Fall back to heap for many subscribers
    Heap(Box<[ActorId]>),
}
```

### Optimization Strategy 4: Type Specialization

**Monomorphic Fast Paths**

```rust
impl ActorSlot {
    fn compute_add(&mut self) -> NanBoxedValue {
        // Fast path: both operands are inline integers
        let a = self.input_a.load();
        let b = self.input_b.load();

        if a.is_inline_int() && b.is_inline_int() {
            // No heap access, no type checking
            return NanBoxedValue::from_int(a.as_inline_int() + b.as_inline_int());
        }

        // Slow path: handle floats, heap integers, type coercion
        self.compute_add_slow(a, b)
    }
}
```

**Specialized Actor Variants**

```rust
enum ActorKind {
    // Specialized implementations for common patterns
    ConstantInt(i64),
    ConstantFloat(f64),
    AddInts { a: ActorId, b: ActorId },
    AddFloats { a: ActorId, b: ActorId },
    LatestTwo { a: ActorId, b: ActorId },
    LatestThree { a: ActorId, b: ActorId, c: ActorId },
    HoldInt { state: i64, body: ActorId },

    // Generic fallback
    Generic(Box<GenericActor>),
}
```

### Optimization Strategy 5: Memory Access Patterns

**Structure of Arrays (SoA)**

```rust
// Instead of Array of Structs (AoS):
// struct Actor { value: Value, dirty: bool, subscribers: Vec<ActorId> }
// actors: Vec<Actor>

// Use Structure of Arrays:
struct ActorStorage {
    values: Vec<NanBoxedValue>,      // Contiguous values
    dirty_flags: BitVec,              // Packed bits
    actor_kinds: Vec<ActorKind>,      // Computation logic
    // subscribers stored separately in SubscriptionTable
}
```

Benefits:
- Better cache utilization when iterating
- SIMD-friendly for batch operations
- Dirty flag checking is a single cache line

**SIMD for Batch Operations**

```rust
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

fn batch_check_changes(old: &[u64], new: &[u64], changed: &mut BitVec) {
    // Process 4 values at a time with AVX2
    let chunks = old.len() / 4;
    for i in 0..chunks {
        unsafe {
            let old_vec = _mm256_loadu_si256(old[i*4..].as_ptr() as *const __m256i);
            let new_vec = _mm256_loadu_si256(new[i*4..].as_ptr() as *const __m256i);
            let cmp = _mm256_cmpeq_epi64(old_vec, new_vec);
            let mask = _mm256_movemask_epi8(cmp);
            // Update changed bits based on mask
        }
    }
}
```

### Runtime Architecture

```rust
pub struct OptimizedRuntime {
    // Actor storage (SoA layout)
    actors: ActorStorage,

    // Static topology (immutable after init)
    topology: SubscriptionTable,
    topo_order: Box<[ActorId]>,

    // Value storage
    arena: ValueArena,

    // Scheduling
    ready_queue: ArrayQueue<ActorId, 1024>,
    dirty: BitVec,

    // Frame batching
    frame_changes: Vec<ActorId>,
}

impl OptimizedRuntime {
    pub fn process_event(&mut self, event: ExternalEvent) {
        // 1. Apply external input
        let affected = self.apply_input(event);

        // 2. Mark affected actors dirty
        for id in affected {
            self.dirty.set(id.0, true);
        }

        // 3. Propagate in topological order
        for &id in &*self.topo_order {
            if self.dirty.get(id.0) {
                self.dirty.set(id.0, false);

                let old_value = self.actors.values[id.0];
                let new_value = self.compute(id);

                if old_value != new_value {
                    self.actors.values[id.0] = new_value;

                    // Mark subscribers dirty
                    for &sub in self.topology.get(id) {
                        self.dirty.set(sub.0, true);
                    }
                }
            }
        }
    }
}
```

### Expected Performance Gains

| Optimization | Estimated Speedup |
|--------------|-------------------|
| NaN-boxing (no heap for primitives) | 2-3x |
| Static subscriptions | 1.5-2x |
| Custom executor (no tokio overhead) | 2-3x |
| Topological update order | 1.5-2x |
| Type specialization | 2-4x for numeric code |
| SoA + cache optimization | 1.3-1.5x |
| **Combined** | **10-20x** over current |

### Trade-offs

| Benefit | Cost |
|---------|------|
| No async overhead | Single-threaded only |
| Static topology | No dynamic actor creation |
| NaN-boxing | 48-bit integer limit |
| Type specialization | More complex codebase |
| Custom executor | No ecosystem integration |

---

## 4. Boon → Rust Transpilation

**Question:** What if we transpile Boon source code to Rust source code, then compile with rustc/LLVM?

This eliminates interpretation overhead entirely - the Boon program becomes a native Rust program.

### Core Insight

Boon's static dataflow graph means we can generate specialized Rust code where:
- Each actor becomes a struct with typed fields
- Subscriptions become direct function calls
- Values are statically typed (no boxing)
- The update loop is unrolled/specialized
- LLVM optimizes across actor boundaries

### Simple Example

```boon
x: 10
y: 20
sum: x + y
product: x * y
result: sum + product
```

Transpiles to:

```rust
// Generated from example.bn
// DO NOT EDIT - Generated by boon-transpile

pub struct Program {
    x: i64,
    y: i64,
    sum: i64,
    product: i64,
    result: i64,
}

impl Program {
    pub fn new() -> Self {
        let x = 10;
        let y = 20;
        let sum = x + y;
        let product = x * y;
        let result = sum + product;
        Self { x, y, sum, product, result }
    }

    pub fn set_x(&mut self, new_x: i64) {
        if self.x != new_x {
            self.x = new_x;
            self.update_from_x();
        }
    }

    pub fn set_y(&mut self, new_y: i64) {
        if self.y != new_y {
            self.y = new_y;
            self.update_from_y();
        }
    }

    #[inline]
    fn update_from_x(&mut self) {
        // x affects: sum, product
        self.sum = self.x + self.y;
        self.product = self.x * self.y;
        self.update_from_sum();
        // product has no dependents
    }

    #[inline]
    fn update_from_y(&mut self) {
        // y affects: sum, product
        self.sum = self.x + self.y;
        self.product = self.x * self.y;
        self.update_from_sum();
    }

    #[inline]
    fn update_from_sum(&mut self) {
        self.result = self.sum + self.product;
    }
}
```

### HOLD Transpilation

```boon
count: [0] |> HOLD state {
    button.click |> THEN { [state.0 + 1] }
}
```

Transpiles to:

```rust
pub struct Counter {
    state: i64,
}

impl Counter {
    pub fn new() -> Self {
        Self { state: 0 }  // Initial value
    }

    /// Called when button.click fires
    pub fn on_click(&mut self) -> Option<i64> {
        let old_state = self.state;
        self.state += 1;

        // Return new value if changed (for subscriber notification)
        if self.state != old_state {
            Some(self.state)
        } else {
            None
        }
    }
}
```

### LATEST Transpilation

```boon
merged: LATEST { x, y, z }
```

Transpiles to:

```rust
pub struct LatestXYZ {
    x: Option<i64>,
    y: Option<i64>,
    z: Option<i64>,
    cached_output: Option<(i64, i64, i64)>,
}

impl LatestXYZ {
    pub fn new() -> Self {
        Self { x: None, y: None, z: None, cached_output: None }
    }

    pub fn update_x(&mut self, value: i64) -> Option<(i64, i64, i64)> {
        self.x = Some(value);
        self.try_emit()
    }

    pub fn update_y(&mut self, value: i64) -> Option<(i64, i64, i64)> {
        self.y = Some(value);
        self.try_emit()
    }

    pub fn update_z(&mut self, value: i64) -> Option<(i64, i64, i64)> {
        self.z = Some(value);
        self.try_emit()
    }

    #[inline]
    fn try_emit(&mut self) -> Option<(i64, i64, i64)> {
        match (self.x, self.y, self.z) {
            (Some(x), Some(y), Some(z)) => {
                let output = (x, y, z);
                if self.cached_output != Some(output) {
                    self.cached_output = Some(output);
                    Some(output)
                } else {
                    None  // No change
                }
            }
            _ => None,  // Not all inputs ready
        }
    }
}
```

### Type Inference

The transpiler must infer types from Boon code:

```rust
enum InferredType {
    Int,
    Float,
    Bool,
    Text,
    List(Box<InferredType>),
    Object(HashMap<String, InferredType>),
    Tuple(Vec<InferredType>),
    Actor(Box<InferredType>),  // Stream of T
    Unknown,  // Needs more context
}

fn infer_type(expr: &Expr, context: &TypeContext) -> InferredType {
    match expr {
        Expr::Literal(Literal::Int(_)) => InferredType::Int,
        Expr::Literal(Literal::Float(_)) => InferredType::Float,
        Expr::Literal(Literal::Bool(_)) => InferredType::Bool,
        Expr::Literal(Literal::Text(_)) => InferredType::Text,

        Expr::BinaryOp { op: Op::Add, left, right } => {
            match (infer_type(left, context), infer_type(right, context)) {
                (InferredType::Int, InferredType::Int) => InferredType::Int,
                (InferredType::Float, _) | (_, InferredType::Float) => InferredType::Float,
                (InferredType::Text, _) | (_, InferredType::Text) => InferredType::Text,
                _ => InferredType::Unknown,
            }
        }

        Expr::List(items) => {
            let item_type = items.first()
                .map(|item| infer_type(item, context))
                .unwrap_or(InferredType::Unknown);
            InferredType::List(Box::new(item_type))
        }

        Expr::Variable(name) => context.get(name).cloned().unwrap_or(InferredType::Unknown),

        // ... etc
    }
}
```

### Generated Type Mappings

| Boon Type | Rust Type |
|-----------|-----------|
| Integer literal | `i64` |
| Float literal | `f64` |
| Boolean | `bool` |
| Text | `String` or `&'static str` |
| `[a, b, c]` list | `Vec<T>` or `[T; N]` |
| `{x: 1, y: 2}` object | Named struct |
| Actor/stream | Custom struct with update methods |

### Reactive Update Strategies

**Strategy 1: Immediate Push**

Each change immediately propagates through the graph:

```rust
impl Program {
    fn set_x(&mut self, x: i64) {
        self.x = x;
        // Immediately update all dependents
        self.sum = self.x + self.y;
        self.result = self.sum * 2;
        self.render();
    }
}
```

Pros: Simple, low latency
Cons: May recompute same node multiple times (diamond problem)

**Strategy 2: Mark-and-Sweep**

Mark dirty, then recompute in topological order:

```rust
impl Program {
    fn set_x(&mut self, x: i64) {
        self.x = x;
        self.dirty |= DIRTY_SUM | DIRTY_PRODUCT;
    }

    fn flush(&mut self) {
        if self.dirty & DIRTY_SUM != 0 {
            self.sum = self.x + self.y;
            self.dirty |= DIRTY_RESULT;
        }
        if self.dirty & DIRTY_PRODUCT != 0 {
            self.product = self.x * self.y;
            self.dirty |= DIRTY_RESULT;
        }
        if self.dirty & DIRTY_RESULT != 0 {
            self.result = self.sum + self.product;
        }
        self.dirty = 0;
    }
}
```

Pros: No redundant computation
Cons: Delayed updates, more complex

**Strategy 3: Incremental with Memoization**

Only recompute if inputs actually changed:

```rust
impl Program {
    fn update_sum(&mut self) {
        let new_sum = self.x + self.y;
        if new_sum != self.sum {
            self.sum = new_sum;
            self.update_result();
        }
        // If sum unchanged, don't propagate
    }
}
```

This is what the generated code typically uses.

### Complex Example: Todo App

```boon
todos: [] |> HOLD state {
    LATEST {
        add_todo.submit |> THEN { [...state, {text: input.value, done: false}] },
        toggle.click |> THEN {
            state |> List/map_at(index, todo => {...todo, done: !todo.done})
        }
    }
}

active_count: todos |> List/filter(todo => !todo.done) |> List/length()
```

Transpiles to:

```rust
#[derive(Clone, PartialEq)]
pub struct Todo {
    pub text: String,
    pub done: bool,
}

pub struct TodoApp {
    todos: Vec<Todo>,
    active_count: usize,

    // Subscriber callbacks (set by UI layer)
    on_todos_change: Option<Box<dyn Fn(&[Todo])>>,
    on_active_count_change: Option<Box<dyn Fn(usize)>>,
}

impl TodoApp {
    pub fn new() -> Self {
        Self {
            todos: Vec::new(),
            active_count: 0,
            on_todos_change: None,
            on_active_count_change: None,
        }
    }

    pub fn add_todo(&mut self, text: String) {
        self.todos.push(Todo { text, done: false });
        self.on_todos_changed();
    }

    pub fn toggle(&mut self, index: usize) {
        if let Some(todo) = self.todos.get_mut(index) {
            todo.done = !todo.done;
            self.on_todos_changed();
        }
    }

    fn on_todos_changed(&mut self) {
        // Recompute derived values
        let new_active_count = self.todos.iter().filter(|t| !t.done).count();

        // Notify todos subscribers
        if let Some(cb) = &self.on_todos_change {
            cb(&self.todos);
        }

        // Notify active_count subscribers (only if changed)
        if new_active_count != self.active_count {
            self.active_count = new_active_count;
            if let Some(cb) = &self.on_active_count_change {
                cb(self.active_count);
            }
        }
    }
}
```

### Compiler Architecture

```
Boon Source (.bn)
       │
       ▼
┌─────────────────┐
│ Parse + Resolve │ → Typed AST with resolved scopes
└─────────────────┘
       │
       ▼
┌─────────────────┐
│  Type Inference │ → Fully typed AST
└─────────────────┘
       │
       ▼
┌─────────────────┐
│ Dataflow Graph  │ → Static actor topology
└─────────────────┘
       │
       ▼
┌─────────────────┐
│ Topo Sort       │ → Update order
└─────────────────┘
       │
       ▼
┌─────────────────┐
│ Rust Codegen    │ → .rs file
└─────────────────┘
       │
       ▼
┌─────────────────┐
│ rustc + LLVM    │ → Native binary or WASM
└─────────────────┘
```

### Codegen Optimizations

**Constant Folding**

```boon
x: 2 + 3
y: x * 4
```

Generates:

```rust
// Constants evaluated at compile time
const X: i64 = 5;
const Y: i64 = 20;
```

**Dead Code Elimination**

Actors with no subscribers and no side effects are removed.

**Inlining**

Small update functions are marked `#[inline]` or `#[inline(always)]`.

**Loop Unrolling for List Operations**

```boon
sum: [1, 2, 3, 4, 5] |> List/reduce(0, acc, x => acc + x)
```

Generates:

```rust
const SUM: i64 = 1 + 2 + 3 + 4 + 5;  // Computed at compile time
```

### Advantages

| Advantage | Explanation |
|-----------|-------------|
| Maximum performance | Native code, LLVM optimizations |
| Zero interpretation overhead | No decode-dispatch loop |
| Static typing | No runtime type checks |
| Inlining | LLVM inlines across actor boundaries |
| Dead code elimination | Unused code removed at compile time |
| Memory layout control | LLVM optimizes struct layout |

### Disadvantages

| Disadvantage | Explanation |
|--------------|-------------|
| Slow compilation | rustc is notoriously slow |
| Large binaries | LLVM-generated code is larger |
| No runtime flexibility | Can't modify topology at runtime |
| Debugging | Generated code is hard to trace |
| Implementation complexity | Full type inference needed |

---

## 5. Boon → WASM Direct Compilation

**Question:** What if we compile Boon directly to WASM bytecode, bypassing Rust and LLVM?

This gives us a middle ground: ahead-of-time compilation without LLVM's overhead.

### Why Direct WASM?

| Benefit | Explanation |
|---------|-------------|
| Fast compilation | No LLVM passes (seconds vs minutes) |
| Small output | WASM is compact (~10-50KB for typical apps) |
| Portable | Browser, Node, edge workers, embedded |
| Sandboxed | Memory-safe execution environment |
| Predictable | No JIT surprises, consistent performance |

### WASM Architecture Primer

**Linear Memory**

Single contiguous memory space (like C's heap):

```
┌─────────────────────────────────────────────────────────┐
│ 0x0000: Static data (strings, constants)               │
│ 0x1000: Actor state (fixed layout)                     │
│ 0x2000: Value heap (lists, objects, text)              │
│ 0x8000: Stack (grows down)                             │
└─────────────────────────────────────────────────────────┘
```

**Stack Machine**

WASM instructions operate on a virtual stack:

```wat
;; Compute: (a + b) * c
local.get $a      ;; stack: [a]
local.get $b      ;; stack: [a, b]
i64.add           ;; stack: [a+b]
local.get $c      ;; stack: [a+b, c]
i64.mul           ;; stack: [(a+b)*c]
```

### Value Representation

**Option A: NaN-Boxing (64-bit)**

```
┌─────────────────────────────────────────────────────────┐
│ Float: IEEE 754 double (not a NaN)                     │
├─────────────────────────────────────────────────────────┤
│ Tagged value: 0x7FF8 | tag(3 bits) | payload(48 bits)  │
│                                                         │
│ Tags:                                                   │
│   000 = Pointer to heap                                │
│   001 = Signed integer (48-bit)                        │
│   010 = True                                           │
│   011 = False                                          │
│   100 = Nil                                            │
│   101 = Symbol (interned string index)                 │
└─────────────────────────────────────────────────────────┘
```

**Option B: Tagged 64-bit (simpler)**

```
┌────────────┬────────────────────────────────────────────┐
│ Tag (8 bit)│ Payload (56 bits)                         │
├────────────┼────────────────────────────────────────────┤
│ 0x00       │ Float (f64 stored in next 8 bytes)        │
│ 0x01       │ Integer (signed 56-bit)                   │
│ 0x02       │ True                                      │
│ 0x03       │ False                                     │
│ 0x04       │ Nil                                       │
│ 0x05       │ Heap pointer (address)                    │
│ 0x06       │ String pointer                            │
│ 0x07       │ List pointer                              │
│ 0x08       │ Object pointer                            │
└────────────┴────────────────────────────────────────────┘
```

### Memory Layout for Program State

```rust
// Compile-time layout calculation
struct MemoryLayout {
    // Fixed offsets for each actor's state
    actor_offsets: Vec<u32>,

    // Heap starts after actors
    heap_start: u32,

    // Total static size
    static_size: u32,
}

// Example layout for: x: 10, y: 20, sum: x + y
//
// Offset 0x0000: x (8 bytes)
// Offset 0x0008: y (8 bytes)
// Offset 0x0010: sum (8 bytes)
// Offset 0x0018: heap start
```

### WASM Codegen: Simple Variables

```boon
x: 10
y: 20
sum: x + y
```

Generates WASM (in WAT text format):

```wat
(module
  ;; Memory: 1 page (64KB) minimum
  (memory (export "memory") 1)

  ;; Global state offsets
  (global $x_offset i32 (i32.const 0))
  (global $y_offset i32 (i32.const 8))
  (global $sum_offset i32 (i32.const 16))

  ;; Initialize program
  (func $init (export "init")
    ;; x = 10
    global.get $x_offset
    i64.const 10
    i64.store

    ;; y = 20
    global.get $y_offset
    i64.const 20
    i64.store

    ;; sum = x + y
    call $update_sum
  )

  ;; Update x and propagate
  (func $set_x (export "set_x") (param $new_x i64)
    ;; Store new value
    global.get $x_offset
    local.get $new_x
    i64.store

    ;; Propagate to dependents
    call $update_sum
  )

  ;; Update sum (internal)
  (func $update_sum
    (local $x i64)
    (local $y i64)

    ;; Load x
    global.get $x_offset
    i64.load
    local.set $x

    ;; Load y
    global.get $y_offset
    i64.load
    local.set $y

    ;; Compute and store sum
    global.get $sum_offset
    local.get $x
    local.get $y
    i64.add
    i64.store
  )

  ;; Getter for sum
  (func $get_sum (export "get_sum") (result i64)
    global.get $sum_offset
    i64.load
  )
)
```

### WASM Codegen: HOLD

```boon
count: [0] |> HOLD state {
    click |> THEN { [state.0 + 1] }
}
```

Generates:

```wat
(module
  (memory (export "memory") 1)

  ;; State offset
  (global $count_offset i32 (i32.const 0))

  ;; Initialize with [0]
  (func $init (export "init")
    global.get $count_offset
    i64.const 0
    i64.store
  )

  ;; Handle click event
  (func $on_click (export "on_click") (result i64)
    (local $old_state i64)
    (local $new_state i64)

    ;; Load current state
    global.get $count_offset
    i64.load
    local.set $old_state

    ;; Compute new state: state.0 + 1
    local.get $old_state
    i64.const 1
    i64.add
    local.set $new_state

    ;; Store new state
    global.get $count_offset
    local.get $new_state
    i64.store

    ;; Return new state (for JS to update UI)
    local.get $new_state
  )

  ;; Get current count
  (func $get_count (export "get_count") (result i64)
    global.get $count_offset
    i64.load
  )
)
```

### WASM Codegen: LATEST

```boon
merged: LATEST { a, b }
output: merged.0 + merged.1
```

Generates:

```wat
(module
  (memory (export "memory") 1)

  ;; Memory layout:
  ;; 0x00: a value (8 bytes)
  ;; 0x08: a_valid (1 byte)
  ;; 0x10: b value (8 bytes)
  ;; 0x18: b_valid (1 byte)
  ;; 0x20: output (8 bytes)

  (global $a_offset i32 (i32.const 0))
  (global $a_valid_offset i32 (i32.const 8))
  (global $b_offset i32 (i32.const 16))
  (global $b_valid_offset i32 (i32.const 24))
  (global $output_offset i32 (i32.const 32))

  ;; Update a
  (func $set_a (export "set_a") (param $value i64)
    ;; Store value
    global.get $a_offset
    local.get $value
    i64.store

    ;; Mark valid
    global.get $a_valid_offset
    i32.const 1
    i32.store8

    ;; Try to emit
    call $try_emit_latest
  )

  ;; Update b
  (func $set_b (export "set_b") (param $value i64)
    global.get $b_offset
    local.get $value
    i64.store

    global.get $b_valid_offset
    i32.const 1
    i32.store8

    call $try_emit_latest
  )

  ;; Check if both valid and update output
  (func $try_emit_latest
    (local $a i64)
    (local $b i64)

    ;; Check a_valid
    global.get $a_valid_offset
    i32.load8_u
    i32.eqz
    if
      return
    end

    ;; Check b_valid
    global.get $b_valid_offset
    i32.load8_u
    i32.eqz
    if
      return
    end

    ;; Both valid - load values
    global.get $a_offset
    i64.load
    local.set $a

    global.get $b_offset
    i64.load
    local.set $b

    ;; Compute output = a + b
    global.get $output_offset
    local.get $a
    local.get $b
    i64.add
    i64.store
  )
)
```

### Heap Management

For dynamic values (lists, objects, text), we need a heap allocator:

```wat
(module
  (memory (export "memory") 1 10)  ;; 1-10 pages

  ;; Heap state
  (global $heap_ptr (mut i32) (i32.const 0x1000))  ;; Start at 4KB

  ;; Simple bump allocator
  (func $alloc (param $size i32) (result i32)
    (local $ptr i32)

    ;; Save current pointer
    global.get $heap_ptr
    local.set $ptr

    ;; Bump pointer
    global.get $heap_ptr
    local.get $size
    i32.add
    global.set $heap_ptr

    ;; Return allocated address
    local.get $ptr
  )

  ;; Allocate list: [length: i32, items: i64...]
  (func $alloc_list (param $length i32) (result i32)
    (local $ptr i32)
    (local $size i32)

    ;; Size = 4 (length) + 8 * length (items)
    i32.const 4
    local.get $length
    i32.const 8
    i32.mul
    i32.add
    local.set $size

    ;; Allocate
    local.get $size
    call $alloc
    local.set $ptr

    ;; Store length
    local.get $ptr
    local.get $length
    i32.store

    local.get $ptr
  )

  ;; Get list item
  (func $list_get (param $list_ptr i32) (param $index i32) (result i64)
    local.get $list_ptr
    i32.const 4          ;; Skip length field
    i32.add
    local.get $index
    i32.const 8
    i32.mul
    i32.add
    i64.load
  )

  ;; Set list item
  (func $list_set (param $list_ptr i32) (param $index i32) (param $value i64)
    local.get $list_ptr
    i32.const 4
    i32.add
    local.get $index
    i32.const 8
    i32.mul
    i32.add
    local.get $value
    i64.store
  )
)
```

### Compiler Pipeline

```
Boon Source (.bn)
       │
       ▼
┌─────────────────┐
│  Parse          │ → AST
└─────────────────┘
       │
       ▼
┌─────────────────┐
│ Type Inference  │ → Typed AST
└─────────────────┘
       │
       ▼
┌─────────────────┐
│ Dataflow Graph  │ → Actor topology + dependency order
└─────────────────┘
       │
       ▼
┌─────────────────┐
│ Memory Layout   │ → Offset assignments for all values
└─────────────────┘
       │
       ▼
┌─────────────────┐
│ WASM Codegen    │ → WAT text or WASM binary
└─────────────────┘
       │
       ▼
┌─────────────────┐
│ Optimization    │ → (optional) wasm-opt passes
└─────────────────┘
       │
       ▼
.wasm binary (10-50KB typical)
```

### WASM Optimizations

**1. Constant Propagation**

```boon
x: 2 + 3
y: x * 4
```

Compiles directly to:

```wat
;; x and y are compile-time constants, stored in data section
(data (i32.const 0) "\05\00\00\00\00\00\00\00")   ;; x = 5
(data (i32.const 8) "\14\00\00\00\00\00\00\00")   ;; y = 20
```

**2. Dead Code Elimination**

Actors with no subscribers are not generated.

**3. Function Inlining**

Small update functions can be inlined at call sites:

```wat
;; Before inlining
call $update_sum

;; After inlining
global.get $x_offset
i64.load
global.get $y_offset
i64.load
i64.add
global.get $sum_offset
i64.store
```

**4. Register Allocation**

Use WASM locals effectively to minimize memory loads/stores:

```wat
;; Bad: repeated memory access
global.get $x_offset
i64.load
global.get $y_offset
i64.load
i64.add
global.get $x_offset  ;; Loading x again!
i64.load
i64.mul

;; Good: use locals
(local $x i64)
global.get $x_offset
i64.load
local.tee $x          ;; Save to local AND leave on stack
global.get $y_offset
i64.load
i64.add
local.get $x          ;; Reuse from local
i64.mul
```

### JavaScript Integration

```javascript
// Load and instantiate WASM
const response = await fetch('app.wasm');
const bytes = await response.arrayBuffer();
const { instance } = await WebAssembly.instantiate(bytes);

// Initialize
instance.exports.init();

// Handle events
document.getElementById('button').onclick = () => {
  const newCount = instance.exports.on_click();
  document.getElementById('count').textContent = newCount;
};

// Read state
const count = instance.exports.get_count();
```

### Advanced: WASM GC Integration

WASM GC (garbage collection) proposal allows structured types:

```wat
;; Define a Todo struct type
(type $Todo (struct
  (field $text (ref string))
  (field $done i32)
))

;; Create a Todo
(func $create_todo (param $text (ref string)) (result (ref $Todo))
  (struct.new $Todo
    (local.get $text)
    (i32.const 0)  ;; done = false
  )
)

;; Access fields
(func $is_done (param $todo (ref $Todo)) (result i32)
  (struct.get $Todo $done (local.get $todo))
)
```

This eliminates manual memory management for objects.

### Performance Characteristics

| Operation | Cycles (approx) |
|-----------|-----------------|
| i64 add/sub/mul | 1 |
| i64 load/store | 1-3 (cached) |
| Function call | 3-5 |
| Indirect call | 5-10 |
| Memory grow | 1000+ |

### Comparison: Direct WASM vs Rust→WASM

| Aspect | Direct WASM | Rust → WASM |
|--------|-------------|-------------|
| Compilation speed | ~100ms | ~30-60s |
| Output size | 10-50KB | 100-500KB |
| Runtime performance | 80-90% of native | 95-100% of native |
| Memory management | Manual/arena | Rust's system |
| Debugging | Basic (DWARF) | Full Rust tooling |
| Implementation effort | High | Medium |

### When to Use Each

| Scenario | Best Choice |
|----------|-------------|
| Fast iteration, debugging | Rust runtime |
| Maximum performance | Rust transpilation |
| Web deployment, small size | Direct WASM |
| Embedded/IoT | Direct WASM |
| Server-side heavy compute | Rust transpilation |

---

## 6. Unified Compilation Architecture

All three approaches share common infrastructure:

```
                         Boon Source
                              │
                              ▼
                    ┌─────────────────┐
                    │  Parse + Type   │
                    │   Inference     │
                    └─────────────────┘
                              │
                              ▼
                    ┌─────────────────┐
                    │  Dataflow Graph │
                    │   + Topology    │
                    └─────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
     ┌─────────────┐  ┌─────────────┐  ┌─────────────┐
     │ Interpreter │  │   Rust      │  │   WASM      │
     │   Codegen   │  │   Codegen   │  │   Codegen   │
     └─────────────┘  └─────────────┘  └─────────────┘
              │               │               │
              ▼               ▼               ▼
     ┌─────────────┐  ┌─────────────┐  ┌─────────────┐
     │  Runtime    │  │   rustc     │  │  (wasm-opt) │
     │  Execute    │  │   + LLVM    │  │             │
     └─────────────┘  └─────────────┘  └─────────────┘
              │               │               │
              ▼               ▼               ▼
          Dynamic         Native          .wasm
         Execution        Binary          Binary
```

### Shared Components

1. **Parser** - Same for all backends
2. **Type inference** - Same algorithm
3. **Dataflow graph** - Same representation
4. **Topological sort** - Same algorithm
5. **Dead code elimination** - Same analysis
6. **Constant folding** - Same evaluation

### Backend-Specific Code

| Component | Runtime | Rust Codegen | WASM Codegen |
|-----------|---------|--------------|--------------|
| Value repr | NaN-boxed | Native types | NaN-boxed or typed |
| Actors | Rust structs | Rust structs | Memory regions |
| Scheduling | Custom executor | Generated code | Generated code |
| Memory | Arena | Rust ownership | Linear memory |
| GC | Manual/scope | Rust drop | Manual/arena |

### Recommended Development Workflow

1. **Development**: Use Rust runtime for fast iteration
   - Instant reload
   - Full debugging
   - Error messages with source locations

2. **Testing**: Use WASM compilation for integration tests
   - Catches codegen bugs
   - Tests real deployment artifact

3. **Production**: Use Rust transpilation OR WASM based on needs
   - Rust for maximum performance
   - WASM for smallest size / broadest compatibility

4. **Embedded/FPGA**: Use WASM or custom backend
   - WASM for soft-core processors
   - Verilog for true hardware
