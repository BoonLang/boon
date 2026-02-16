# Boon as a Systems Language: Architecture, Compilation, and Self-Hosting

*Research compiled from a deep-dive conversation exploring how Boon's reactive dataflow
architecture could run efficiently at the systems level, inspired by the
[moss-kernel](https://github.com/hexagonal-sun/moss-kernel) async Rust kernel.*

---

## Table of Contents

1. [Moss Kernel: An Async Rust OS](#1-moss-kernel-an-async-rust-os)
2. [Deep Parallels with Boon](#2-deep-parallels-with-boon)
3. [How Boon Could Run on an Async Kernel](#3-how-boon-could-run-on-an-async-kernel)
4. [Performance Bottlenecks in the Current Engines](#4-performance-bottlenecks-in-the-current-engines)
5. [What's Needed: The Compilation Design](#5-whats-needed-the-compilation-design)
6. [Speed Comparison: Boon vs Rust](#6-speed-comparison-boon-vs-rust)
7. [Self-Compiling Boon Without LLVM](#7-self-compiling-boon-without-llvm)
8. [WASM-First Compilation Strategy](#8-wasm-first-compilation-strategy)
9. [The Self-Hosting Bootstrap Chain](#9-the-self-hosting-bootstrap-chain)
10. [Writing the Parser in Boon](#10-writing-the-parser-in-boon)
11. [Compiler Passes as Dataflow](#11-compiler-passes-as-dataflow)
12. [Engine-Specific Considerations](#12-engine-specific-considerations)
13. [The Recommended Path](#13-the-recommended-path)

---

## 1. Moss Kernel: An Async Rust OS

[Moss](https://github.com/hexagonal-sun/moss-kernel) is a ~26,000 LoC experimental
Unix-like kernel written in async Rust (AArch64). It runs real Arch Linux binaries
(bash, coreutils, strace) and implements 105 Linux syscalls.

### Core Architectural Innovations

1. **All non-trivial syscalls are `async fn`** -- `sleep`, `read`, `write`, page faults --
   everything yields via `.await` instead of blocking threads on wait queues.

2. **Compile-time deadlock prevention** -- Rust's type system prevents holding a `SpinLock`
   guard across `.await` points (guards are `!Send`). Eliminates an entire class of kernel
   bugs structurally.

3. **Custom Waker -> Scheduler integration** -- When an async operation completes, the
   `Waker` directly moves the process back into the EEVDF run queue, even across CPUs
   via IPI (Inter-Processor Interrupt) messages.

4. **Per-process future slots** -- Each process carries `signal: Option<Future>` and
   `kernel: Option<Future>` -- boxed pinned futures that the scheduler polls.

5. **Inter-CPU communication is pure message-passing** -- `PutTask(OwnedTask)` and
   `WakeupTask(Waker)` messages sent through per-CPU mailboxes. No shared mutable state
   between CPUs for task migration.

### The `Interruptable` Combinator

A custom Future combinator wrapping any async operation for signal awareness:

```rust
pub async fn sys_nanosleep(rqtp: TUA<TimeSpec>, rmtp: TUA<TimeSpec>) -> Result<usize> {
    let timespec: Duration = TimeSpec::copy_from_user(rqtp).await?.into();
    match sleep(timespec).interruptable().await {
        InterruptResult::Interrupted => Err(KernelError::Interrupted),
        InterruptResult::Uninterrupted(()) => Ok(0),
    }
}
```

### Scheduling

Moss uses **EEVDF** (Earliest Eligible Virtual Deadline First) -- the same algorithm Linux
adopted in kernel 6.6. SMP load balancing via global atomic `LEAST_TASKED_CPU_INFO`
packing CPU ID + weight + task count into a single u64.

### Memory

Full MMU with Copy-on-Write pages, buddy allocator for physical addresses, slab allocator
with per-CPU caching. Page faults return `FaultResolution::Deferred(Box<dyn Future>)` --
the process sleeps while disk I/O completes asynchronously.

---

## 2. Deep Parallels with Boon

> `★ Insight ─────────────────────────────────────`
>
> Moss's core thesis is the same as Boon's, just at a different layer of the stack:
> replace shared mutable state with structured async dataflow, and let the compiler
> enforce correctness. Moss does this for kernel syscalls; Boon does it for UI reactivity.
> The patterns converge in striking ways.
>
> `─────────────────────────────────────────────────`

### 2.1 Futures ARE Dataflow Nodes

Moss demonstrates that `async/await` creates an **implicit dataflow graph**:

```rust
sleep(timespec).interruptable().await
```

This is a pipeline: `timer_future -> interruptable_wrapper -> scheduler_polling`.

**Boon parallel**: This is exactly what Boon's engines do explicitly. A Boon expression:

```boon
10 |> Stream/interval() |> THEN { state + 1 }
```

...is a dataflow pipeline: `interval_source -> then_transform -> output`.

The difference: Boon makes the graph *first-class* and *user-visible*. Moss's graph is
implicit in the async call chain.

**The opportunity**: If Boon compiled to an OS like Moss, each dataflow node could be a
lightweight kernel-level future. The kernel's scheduler would become Boon's reactive
scheduler *for free*.

### 2.2 The Waker Pattern = Reactive Notification

| Moss (Kernel)                  | Boon (Engine)                               |
|-------------------------------|---------------------------------------------|
| `Waker::wake()`              | `Mutable::set()` triggers subscribers (DD io layer)  |
| Future returns `Poll::Pending` | Node has no new input, stays dormant         |
| Scheduler polls ready futures | DD engine re-evaluates dirty nodes            |
| IPI for cross-CPU wake        | (hypothetical) cross-worker wake             |

The Waker is fundamentally a **push notification** that something changed -- which is
exactly what reactive/dataflow systems do.

### 2.3 Message-Passing Between CPUs = Actor Distribution

Moss's inter-CPU messaging:

```rust
enum Message {
    PutTask(OwnedTask),      // Migrate actor to another CPU
    WakeupTask(Waker),       // Wake actor on remote CPU
}
```

This maps directly to distributing Boon's computation across cores:
- `PutTask` = migrate a computation unit to another WebWorker/thread
- `WakeupTask` = notify a unit on another core that its input changed

Both the Actors engine (channel-based communication) and the DD engine
(event-batch-based propagation) could benefit from this pattern for multi-core
distribution.

### 2.4 Compile-Time Safety Parallels

| Moss                                             | Boon                                              |
|-------------------------------------------------|-------------------------------------------------|
| `SpinLock` guard can't cross `.await` (compile error) | DD: No `Mutable<T>` in `core/` (module boundary enforced) |
| `TUA<T>` -- typed user-space addresses           | DD: `VarId`, `LinkId`, `ListKey` type-safe IDs   |
| `Interruptable` combinator wraps any future      | Actors: `switch_map` cancels previous inner stream |
| Owned-value IPI messages (no shared state)        | Actors: channels only, no `Rc<RefCell>` |

---

## 3. How Boon Could Run on an Async Kernel

### Option A: Dataflow Nodes as Kernel Tasks (Deep Integration)

Each reactive node becomes a kernel-level async task:

- **HOLD state** -> a kernel task with an owned state variable, polled by the scheduler
- **LATEST { a, b, c }** -> a `select!`-style future combining three input futures
- **LINK events** -> kernel-level I/O futures (keyboard, mouse, network)
- **Stream/interval()** -> kernel timer future (like Moss's `sleep()`)

The kernel's EEVDF scheduler handles priority and fairness. No separate event loop.

**Challenge**: Boon's dynamic nature (runtime-evaluated expressions, hot reload) conflicts
with static future compilation.

### Option B: Engine as a Kernel Subsystem

Instead of compiling nodes to kernel tasks, implement the engine itself as a kernel
subsystem:

- Dataflow graph nodes = futures in a single kernel task
- Change propagation = Waker chain
- Multi-core = nodes distributed across CPUs via `PutTask`

This is more practical for Boon's dynamic nature. The DD engine's clean three-layer
architecture (`core/` pure computation, `io/` bridge, `render/` output) maps naturally
to kernel subsystem boundaries.

### Option C: Boon as a Systems Language (Most Ambitious)

What if Boon's dataflow constructs could express kernel-level logic?

```boon
// Hypothetical: a Boon-like kernel scheduler
tasks: System/task_queue()
timer: Hardware/timer(4ms)

scheduled_task: LATEST { tasks, timer } |> THEN {
    tasks |> List/sort_by(.deadline) |> List/first()
}

cpu_assignment: scheduled_task |> WHEN {
    .affinity => Hardware/current_cpu()
    _ => System/least_loaded_cpu()
}
```

Boon's reactive model naturally expresses "when X changes, recompute Y" -- which is
what a scheduler does. The `Interruptable` combinator maps to Boon's `WHEN` pattern
matching on signal interrupts.

---

## 4. Performance Bottlenecks in the Current Engines

### 4.1 Value Representation (The Biggest Cost)

**Affects: DD engine and Actors engine.**

Both engines use a `Value` enum as the universal runtime type. The DD engine has 7 variants
(165 lines) using `Arc<str>` for text and `BTreeMap` for objects to satisfy DD's `Ord`/`Hash`
trait bounds.

| Operation          | Current (any engine) | Equivalent Rust     | Overhead          |
|--------------------|---------------------|--------------------|--------------------|
| Number access      | Match 7-variant enum    | Direct `f64`    | ~2-5ns (branch miss) |
| Field access       | `Arc` deref -> `BTreeMap` O(log n) | Struct offset O(1) | **10-100x** |
| Object creation    | `Arc::new(BTreeMap)` + N * `Arc::from(key)` | Stack alloc | **50-500x** |
| Cell ID lookup     | `HashMap<String, Value>` | Array index `cells[3]` | **20-50x** |

**What Lustre/SCADE does**: Every node compiles to a C struct with typed fields. A counter
becomes `struct { int mem_count; bool init; }`. No heap, no tagging, no indirection.

### 4.2 Per-Event Allocation

**Affects: DD engine and Actors engine.**

The DD engine uses DD collections natively (zero `.to_vec()` calls) for true incremental
updates, but still allocates `Arc<str>` per cell update.

The Actors engine avoids bulk cloning but allocates per-message via channels.

| Engine     | Per-event cost (list toggle)      | Root cause                    |
|------------|-----------------------------------|-------------------------------|
| DD         | DD diff (O(delta))                 | Incremental by design         |
| Actors     | Channel message + Arc clone        | Message-passing overhead      |

### 4.3 Dynamic Dispatch of Transforms (Interpretive Overhead)

**Affects: DD engine (closures in CompiledProgram), Actors (stream combinator chains).**

The DD engine's `CompiledProgram` variants (Static, SingleHold, LatestSum, General) already
partially address this by specializing common patterns at compile time. The General
fallback still interprets the full AST.

| System                              | Approach              | Relative Speed |
|-------------------------------------|-----------------------|----------------|
| DD (SingleHold closure)             | Specialized closure   | Baseline       |
| DD (General / full AST interp)      | Still interpretive    | ~0.2-0.5x      |
| Actors (stream combinators)         | Compiled combinator chain | ~1-2x       |
| Lustre -> C (compiled step function) | Direct assignment     | **100-1000x faster** |

### 4.4 Event Routing

**Affects: DD engine (partially addressed), Actors engine (not affected).**

The DD engine's `compile.rs` pre-computes link bindings per `CompiledProgram` variant,
reducing runtime routing for specialized programs. The General fallback still scans
mappings.

The Actors engine avoids this entirely -- each actor subscribes to its specific sources
via channels (O(1) dispatch).

### 4.5 Framework Overhead

**Affects: DD engine (Timely/DD framework). Not applicable to Actors engine.**

Timely Dataflow's progress tracking and Differential Dataflow's arrangement maintenance
add overhead for single-threaded WASM. The DD engine mitigates this by using
`timely::execute_directly()` (single thread, no coordination overhead).

The Actors engine has its own overhead: the `ActorLoop` / `Task::start_droppable()`
machinery, channel allocation, and the Lamport clock ordering. However, this is lighter
than DD's progress tracking for simple programs.

Frank McSherry's COST paper demonstrated: a single-threaded Rust program outperformed
Spark on 128 cores. The framework overhead can exceed the parallelism benefit.

---

## 5. What's Needed: The Compilation Design

### Layer 1: Type Inference + Specialization

Infer concrete types for cells:
```
cell "count" : always Number -> compile to f64
cell "title" : always Text -> compile to &str
cell "todo_item" : always Object{title: Text, completed: Bool} -> compile to struct
```

The DD engine's `CompiledProgram` variants already move in this direction -- `SingleHold`
knows the shape of its state, `LatestSum` knows it's accumulating numbers. A full type
inference pass would extend this to all cells.

**Evidence**: Lustre/SCADE generates equivalent C with zero overhead. StreamIt achieves
1-2x *better* than hand-written C by exposing communication patterns to the compiler.

### Layer 2: Static Scheduling

Analyze the dataflow graph topology:
1. Compute dependency order (topological sort)
2. Generate specialized transform functions per cell
3. Build direct dispatch table: event -> affected cells in order
4. Generate a single `step()` function

The DD engine's `compile.rs` is the foundation for this -- it already analyzes programs
into specialized variants. Extending it to generate a static schedule for the General
variant would eliminate the DD framework overhead entirely.

**Evidence**: SDF (Synchronous Dataflow) scheduling eliminates all runtime scheduling
overhead for fixed-rate dataflow graphs. For Boon's UI case, most cells fire exactly
once per event -- a trivial SDF schedule.

> `★ Insight ─────────────────────────────────────`
>
> **A compiled Boon could be FASTER than hand-written Rust for static subgraphs.**
>
> MIT's [StreamIt](https://groups.csail.mit.edu/cag/streamit/) proved this is not
> theoretical: it achieved 1-2x *better* than hand-coded C on 4/6 benchmarks, and
> 11.2x mean speedup from 1 to 16 cores. The reason: when the compiler can see the
> entire dataflow graph (production rates, consumption rates, communication patterns),
> it simultaneously applies fusion, fission, scheduling, unrolling, and cache-aware
> tiling -- optimizations a human might do one at a time but never all at once.
>
> Boon's pipe chains (`a |> b |> c`) are StreamIt Pipelines. `LATEST { a, b }` is a
> SplitJoin. `HOLD` is a FeedbackLoop. The DD engine's `compile.rs` already analyzes
> these patterns. Extending it with StreamIt-style rate analysis and operator fusion
> would let the compiler find optimizations that a Rust programmer writing imperative
> code simply cannot -- because the dataflow structure is invisible in imperative code.
>
> The path: `compile.rs` rate analysis -> static schedule -> operator fusion ->
> WASM emission -> **faster than hand-written Rust for static reactive subgraphs**.
>
> `─────────────────────────────────────────────────`

### Layer 3: In-Place State Mutation

**Option A -- Uniqueness types (Futhark style):**
HOLD state has a single owner. `state.count + 1` compiles to in-place mutation.

**Option B -- Region-based allocation (MLKit style):**
Each cell gets a memory region. All allocations use the cell's region. When destroyed,
the region is freed in O(1).

The DD engine already achieves incremental updates via DD collections (no `.to_vec()`).
Going further with uniqueness types would eliminate the DD collection overhead for
cells with a single owner.

**Evidence**: Pony achieves C-like performance with actors via `iso` (isolated) reference
capability -- zero-copy message passing proven at the type level.

### Layer 4: Pipeline Fusion

Chains like `value |> THEN { x + 1 } |> THEN { x * 2 }` should fuse into
`|x| (x + 1) * 2` with zero intermediate `Value` allocations.

**Evidence**: Strymonas (POPL 2017) guarantees: if each operation runs without allocations,
the entire fused pipeline runs without allocations. Generates code matching hand-written
state machines.

### Layer 5: Selective Dynamism

Not everything can be compiled statically. Dynamic list items, user input, and hot reload
are inherently runtime operations.

The DD engine's four-variant `CompiledProgram` is already this approach -- Static/SingleHold/
LatestSum are compiled fast paths, General is the dynamic fallback. A compilation backend
would extend this pattern to generate native code for the fast paths.

**Evidence**: CAL (MPEG actor language) classifies subgraphs as static vs dynamic,
compiles static parts fully, uses lightweight runtime only for dynamic parts.

---

## 6. Speed Comparison: Boon vs Rust

### Current State

| Operation           | DD Engine       | Actors          | Equiv Rust | Overhead Ratio |
|---------------------|-----------------|-----------------|-----------|----------------|
| Counter increment   | ~1-10us         | ~2-20us         | ~1ns      | 1K-20Kx        |
| List item toggle    | ~5-50us         | ~10-100us       | ~5ns      | 1K-20Kx        |
| Object field update | ~0.5-5us        | ~1-5us          | ~1ns      | 500-5Kx        |

The DD engine's incremental design provides efficient list operations via DD collections.
The Actors engine uses channel-based message passing with per-message allocation.

### After Full Compilation (Theoretical)

| Approach                                               | Overhead vs Rust | Evidence |
|-------------------------------------------------------|-----------------|----------|
| Lustre-style (synchronous, typed, static schedule)    | **~0%**         | Lustre/SCADE generates equivalent C |
| StreamIt-style (streaming, fused)                     | **0.5-2x faster** | Compiler optimizations humans miss |
| Pony-style (actors, zero-copy)                        | **~1-2x**       | Actor scheduling + routing cost |
| Futhark-style (data-parallel, fused)                  | **~1x**         | Matches hand-written GPU code |
| Rust async overhead only                              | **~3.4x without I/O, ~1.09x with I/O** | 243ns per operation |

### Realistic Target for Compiled Boon

**~1.5-3x slower than equivalent Rust**, with overhead from:
- Actor/cell scheduling: ~100-500ns per dispatch
- Message passing: ~0ns (uniqueness types) to ~30-60ns (small value copy)
- Dataflow propagation: ~10-50ns per node if statically scheduled
- Dynamic subgraphs: interpretive overhead only where needed

This is **5,000-50,000x faster** than the current interpreted engines.

---

## 7. Self-Compiling Boon Without LLVM

### Why Not LLVM?

| Phase                         | Time      | % of Total |
|-------------------------------|-----------|-----------|
| Frontend (lex + parse + eval) | ~microseconds | <1%   |
| LLVM setup                    | ~2,500us  | 2%        |
| LLVM IR generation            | ~11,000us | 10%       |
| LLVM object file emission     | ~84,000us | **73%**   |
| Optimization                  | ~15,000us | 13%       |

LLVM takes >= 70% of compilation time even at -O0. The dominant cost isn't optimization --
it's object file emission through LLVM's heavy abstraction layers. This is structural.

### Alternative Backend Comparison

| Backend          | Compile Speed (us/fn) | Runtime vs LLVM -O2 | Codebase Size |
|------------------|-----------------------|---------------------|---------------|
| TCC              | **2.0**               | ~30-50%             | ~25 KLOC      |
| Zig x86_64       | **3.6**               | Varies (naive)      | 250 KLOC      |
| DMD              | **8.5**               | ~70-80%             | ~100 KLOC     |
| QBE              | 3.9x faster than Clang | **73%**            | **8 KLOC**    |
| Go (gc)          | **14.4**              | ~85-90%             | ~500 KLOC     |
| Cranelift        | 20-35% faster than LLVM | 76-98%            | ~200 KLOC     |
| Clang/LLVM -O2   | 21.6-22.8            | 100% (baseline)     | ~20M LOC      |
| Rust (LLVM)      | **69.2**              | ~100%               | ~500 KLOC     |

### Key Patterns

1. **The 70% rule**: LLVM takes ~70% of time. Replacing it gives 3-5x speedup.
2. **QBE's sweet spot**: 73% of LLVM performance in 0.1% of the code (8K lines).
3. **Linker dominates**: Jai's example shows linking at 77% of total time.
4. **Dual-backend is standard**: Jai, Zig, D all offer fast-naive + optimizing backends.

---

## 8. WASM-First Compilation Strategy

> `★ Insight ─────────────────────────────────────`
>
> WASM is the only target where you get optimization for free without building an
> optimizer. When Boon emits naive WASM, three layers of optimization happen
> automatically:
>
> 1. **wasm-opt** (Binaryen): Ahead-of-time -- constant propagation, DCE, inlining.
>    Gets within 7.7% of LLVM.
> 2. **Liftoff** (V8 baseline): Compiles WASM to native in tens of MB/s -- instant startup.
> 3. **TurboFan** (V8 optimizing): Background-compiles hot functions with full SSA
>    optimization, register allocation, bounds-check elimination.
>
> Result: naive WASM bytecode runs at 85-97% of LLVM-optimized native speed.
>
> `─────────────────────────────────────────────────`

### Why WASM-First Wins

| Advantage                    | Details                                      |
|------------------------------|----------------------------------------------|
| Trivially simple format      | Complete module is ~40 bytes                  |
| Stack machine = easy codegen | AST tree walk, one opcode per node            |
| No linker needed             | WASM is self-contained (eliminates Jai's 77% bottleneck) |
| Runs everywhere              | Browser, server, embedded, bare metal         |
| Free runtime optimization    | V8 TurboFan / Wasmtime AOT                   |
| Self-hosting via WASM        | Zig proved this works at compiler scale       |
| One target                   | Instead of x86_64 + AArch64 + RISC-V         |

### How Simple Is WASM Code Generation?

A complete, valid, executable WASM module (an `add` function) in ~40 bytes:

```
00 61 73 6d 01 00 00 00    -- magic + version
01 07 01 60 02 7f 7f 01 7f -- type: (i32, i32) -> i32
03 02 01 00                -- function 0 uses type 0
07 07 01 03 61 64 64 00 00 -- export "add" as func 0
0a 09 01 07 00 20 00 20 01 6a 0b -- body: local.get 0, local.get 1, i32.add, end
```

No linker, no relocation, no object file format.

Using Rust's `wasm-encoder`:

```rust
let mut module = Module::new();
// ~15 lines to define type, function, export, code sections
let wasm_bytes = module.finish(); // Valid .wasm file
```

A Boon expression `count + 1` compiles to:
```wasm
local.get $count    ;; push count onto stack
i32.const 1         ;; push 1
i32.add             ;; pop both, push sum
```

A code generator for Boon's core language could be **under 2,000 lines**.

### WASM Performance

| Scenario                        | Speed vs Native | Source                  |
|---------------------------------|-----------------|-------------------------|
| Browser (V8), average           | ~65% (1.55x)    | USENIX ATC 2019         |
| Browser (SpiderMonkey), average | ~69% (1.45x)    | USENIX ATC 2019         |
| Wasmtime AOT                    | 85-90%          | Bytecode Alliance        |
| Binaryen-only vs LLVM           | 92.3% (7.7% gap)| WAMI paper 2025          |
| WAMR AOT (embedded)             | ~50%            | Bytecode Alliance        |
| wasm3 interpreter (MCU)         | ~10-20%         | wasm3 benchmarks         |

### WASM Limitations (Honest Assessment)

| Limitation                    | Impact                     | Mitigation               |
|-------------------------------|---------------------------|--------------------------|
| No raw hardware access        | Can't write device drivers | WASI provides abstract interfaces |
| ~1.5x browser overhead        | Slower than native for tight loops | wasm-opt + TurboFan closes to <10% |
| No shared-everything threads (yet) | Limited parallelism    | Proposal in progress     |
| 4GB memory limit (w/o Memory64) | Large datasets           | Memory64 shipped WASM 3.0 |
| No raw syscalls               | Can't be a kernel         | WASI 0.3 adds async I/O  |

### Relevant WASM 3.0 Proposals (Shipped Sept 2025)

| Feature            | Impact for Boon                              |
|--------------------|----------------------------------------------|
| Fixed-width SIMD   | 4x for vectorizable code                     |
| Tail Calls         | Zero-cost recursion (parser, functional patterns) |
| WasmGC             | Managed structs/arrays, no linear memory GC  |
| Exception Handling | try/catch without JS interop                 |
| Memory64           | >4GB linear memory                           |
| Threads + Atomics  | Real multi-threading (multi-worker dataflow)  |

---

## 9. The Self-Hosting Bootstrap Chain

> `★ Insight ─────────────────────────────────────`
>
> No reactive or dataflow language has ever self-hosted. Not Elm (compiler in Haskell),
> not Lustre (compiler in OCaml/Coq), not LabVIEW (compiler in C++). Boon would be the
> first.
>
> But the research reveals something surprising: compilers are secretly dataflow systems
> already. Salsa (used by rust-analyzer) is essentially Boon's HOLD + LATEST semantics
> applied to compiler passes. A Boon compiler written in Boon would get Salsa-like
> incrementality for free from the language itself.
>
> `─────────────────────────────────────────────────`

### The Concrete Plan

```
Phase 0: TODAY
  Boon compiler = Rust (parser + evaluator + engine)
  Engines: Actors (legacy, /repos/boon) + DD (/repos/boon-dd-v2)
  Target: WASM via Rust/wasm-bindgen/mzoon

Phase 1: BOON-TO-WASM COMPILER (in Rust)
  Write a new backend emitting WASM directly via wasm-encoder
  Boon source -> AST -> WASM bytecode
  Still a Rust program, outputs .wasm files
  ~5-10K lines of Rust

Phase 2: BOON COMPILER IN BOON
  Rewrite the compiler in Boon itself
  Parser: WHEN for token matching, HOLD for state, WHILE for repetition
  Type checker: reactive constraint propagation
  Codegen: tree-walking WASM emitter
  Compile with Phase 1 compiler -> boon-compiler.wasm

Phase 3: SELF-HOSTING
  boon-compiler.wasm compiles its own source
  -> produces boon-compiler-v2.wasm
  Verify: v2 compiles source again -> must match v2 (reproducibility)
  Commit boon-compiler.wasm to git (~few hundred KB)

Phase 4: BOOTSTRAP FROM ANY PLATFORM
  Tiny WASI interpreter in C (~4K lines, like Zig's)
  OR use Wasmtime/wasm3 (already exists)
  -> Runs boon-compiler.wasm -> builds Boon from source
  -> No Rust toolchain needed!
```

### Precedent: Zig's WASM Bootstrap

Zig proved this exact approach works at scale:
- Committed a ~637KB compressed WASM binary to git
- A 4,000-line C WASI interpreter bootstraps from it
- Memory usage dropped from 9.6GB to 2.8GB
- Build speed improved 1.5-3.75x

### Expected Compilation Speed

| Compiler       | Speed (lines/sec) | Notes                       |
|----------------|-------------------|-----------------------------|
| TCC            | 880K              | Single-pass, no optimization |
| Jai            | 250K              | Multi-threaded, naive codegen |
| Zig (custom)   | ~125K             | Incremental, in-place patching |
| Go             | ~40K              | Package-level parallelism    |
| Rust (LLVM)    | ~15K              | Full optimization            |
| **Boon->WASM** | **100-300K**      | No register alloc, no linking, one target |

---

## 10. Writing the Parser in Boon

### Boon Construct -> Parsing Concept Mapping

| Boon Construct | Parsing Analog                                    |
|---------------|---------------------------------------------------|
| `WHEN { pattern => body }` | Pattern match on tokens (recursive descent) |
| `THEN { body }` | Sequential composition ("parse A, then B")          |
| `HOLD state { ... }` | Parser state accumulation (scope stack, prec)    |
| `LATEST { a, b, c }` | Combining results from sub-parsers              |
| `WHILE { pattern => body }` | Repetition ("parse while tokens match")       |
| Pipes (`\|>`) | Parser pipeline composition                         |

### The Backtracking Challenge

Parsers need pull-based input consumption (demand tokens) while Boon is push-based
(events push through the graph). Solutions:

1. **PEG-style ordered choice**: First match wins. Expressible as ordered `WHEN` arms.
2. **Pratt parsing** (what chumsky uses for Boon today): Precedence as `HOLD` state.
3. **Two-phase**: Lex everything first (token list), then pattern-match reactively.

### Incremental Parsing is Naturally Reactive

This is the strongest alignment. Source text is a reactive input. When a character
changes, the change propagates through the parser, updating only affected AST nodes.
This is precisely what `LATEST` semantics provide.

```boon
// Reactive incremental compilation
source: Editor/content()               // updates on every keystroke
ast: source |> Parser/incremental_parse() // only re-parses changed regions
types: ast |> TypeChecker/check()         // only re-checks affected nodes
errors: types |> Diagnostics/collect()
highlights: ast |> Highlighter/highlight()
```

---

## 11. Compiler Passes as Dataflow

### Attribute Grammars ARE Dataflow

Attribute grammars (Knuth, 1968) are the original formalization of dataflow in compilers:
- **Synthesized attributes** flow bottom-up (child to parent): computed values, types
- **Inherited attributes** flow top-down (parent to child): scope context, expected types

In Boon terms:
- Synthesized = return values flowing up through pipes
- Inherited = `PASS`/`PASSED` context flowing down

### Type Inference as Reactive Dataflow

Constraint-based type inference IS reactive constraint propagation:
1. Generate constraints from AST (each expression -> type equations)
2. Propagate solutions through constraint graph (unification)
3. When a type resolves, propagate to all dependent constraints

Each type variable is a reactive cell. Unification constraints are edges. Solving
proceeds by propagating known values until reaching a fixed point.

### Salsa: The Proof That Compilers = Reactive Dataflow

[Salsa](https://github.com/salsa-rs/salsa) (used by rust-analyzer):

```rust
#[salsa::tracked]
fn parse_file(db: &dyn Db, file: ProgramFile) -> Ast { ... }

#[salsa::tracked]
fn type_check(db: &dyn Db, ast: Ast) -> TypedAst { ... }
```

Features directly analogous to Boon:
- **Dependency tracking**: Like Boon's reactive subscriptions
- **Early cutoff**: Same input -> skip downstream (like DD's differential updates)
- **Durability levels**: Stdlib = durable, user code = volatile (optimization)

> `★ Insight ─────────────────────────────────────`
>
> Most languages must *add* incrementality to their compilers (Rust added Salsa, Scala
> proposed "functional reactive compilation"). Boon has incrementality **built into the
> language**. A Boon compiler written in Boon would be inherently incremental -- every
> variable is a reactive stream, every computation automatically tracks dependencies,
> and changes propagate minimally. This is the strongest argument for self-hosting:
> the compiler would be a showcase of the language's unique capabilities.
>
> `─────────────────────────────────────────────────`

---

## 12. Engine-Specific Considerations

### How Each Engine Relates to the Compilation Vision

#### Actors Engine (`/repos/boon`, `engine_actors/`)

**Architecture**: ~19K lines. Push-based reactive streams. ValueActor + LazyValueActor +
ActorLoop. Channel-based communication. Eager evaluation with lazy mode for HOLD bodies.

**Relevance to systems compilation**:
- The actor model maps *directly* to Moss's per-process future slots.
- Each ValueActor is conceptually a kernel task with a Waker.
- Channel-based IPC mirrors Moss's inter-CPU message passing.
- The `switch_map` / `flat_map` stream combinators are kernel-style future combinators.

**Potential improvement path**: If the Actors engine were to evolve toward compilation,
the key change would be **static actor graph analysis** -- determining at compile time
which actors exist, what their types are, and how they connect. Currently the actor graph
is built dynamically during evaluation. Static analysis would enable:
- Pre-allocated channels (no runtime `mpsc::channel()`)
- Typed messages (no `Value` enum)
- Dead actor elimination
- Actor fusion (merge sequential actors into one)

**Strength for systems work**: The actor model is the most natural fit for distribution
across threads/cores/machines. Moss's `PutTask(OwnedTask)` is exactly actor migration.
If Boon targets multi-core or distributed systems, the Actors engine's model is
architecturally closer to the hardware.

#### DD Engine (`/repos/boon-dd-v2`, `engine_dd/`)

**Architecture**: ~8K lines. Three clean layers: `core/` (pure computation), `io/`
(browser bridge), `render/` (output). Four `CompiledProgram` variants.

**Relevance to systems compilation**:
- **`compile.rs` is the seed of a real compiler**. It already analyzes Boon programs
  into specialized variants (Static, SingleHold, LatestSum, General). Extending this
  to emit WASM instead of configuring a DD runtime is the natural next step.
- The pure `core/` layer (no Mutable, no RefCell, no side effects) could be transplanted
  to a non-browser target with minimal changes.
- The 6-operator model (hold_state, hold_latest, when_match, skip, while_reactive,
  latest) maps cleanly to WASM codegen patterns.

**The compilation path from the DD engine**:

```
DD Engine Today                      DD Engine Compiled
───────────────                      ──────────────────
compile.rs analyzes AST              compile.rs analyzes AST
 -> CompiledProgram::SingleHold       -> WasmModule::SingleHold
 -> runs on Timely DD runtime         -> emits wasm step() function
                                       -> no DD framework needed

CompiledProgram::General             WasmModule::General
 -> full DD dataflow graph            -> statically scheduled node graph
 -> Timely manages operator order     -> topological-sort step() function
```

The DD engine's `CompiledProgram::Static` variant already evaluates at "compile time" (no
runtime needed). Adding `CompiledProgram::SingleHold` WASM emission would cover
counters, simple state machines, and basic reactive UIs.

> `★ Insight ─────────────────────────────────────`
>
> The DD engine's `compile.rs` (1,312 lines) is architecturally positioned as the foundation
> of a Boon compiler. It already does program analysis, specialization into variants,
> and optimization (Static programs need zero runtime). Changing its output from
> "DD runtime configuration" to "WASM bytecode" is the core transformation needed
> for Boon-as-a-systems-language.
>
> The DD engine's three-layer architecture (`core/` = pure computation logic, `io/` = platform
> bridge, `render/` = output) already separates concerns in the right way. A compiled
> Boon would keep `core/`'s logic, replace `io/` with WASI, and replace `render/` with
> the target platform's UI framework (or raw framebuffer).
>
> `─────────────────────────────────────────────────`

### Boon Construct -> DD Operator -> WASM Instruction Mapping

| Boon                 | DD Operator              | Compiled WASM                    |
|----------------------|--------------------------|----------------------------------|
| `x: 42`             | `input.insert(42)`       | `i32.const 42; local.set $x`     |
| `LATEST { a, b }`   | `.concat()` + hold_latest | `local.get $a; local.get $b; select` |
| `initial \|> HOLD state { body }` | `hold_state` unary | `loop { ... local.set $state }` |
| `event \|> THEN { body }` | `.flat_map()`       | `if (event_fired) { body_code }`  |
| `input \|> WHEN { arms }` | `.flat_map()` with match | `br_table` (WASM switch)       |
| `TEXT { {a} and {b} }` | `.join()` on refs       | `string.concat` (or manual)      |
| `BLOCK { vars, out }` | Scoped aliases           | No runtime cost (local scoping)  |

---

## 13. The Recommended Path

### WASM Serves as Both Development and Production Backend

> `★ Insight ─────────────────────────────────────`
>
> The dual-backend strategy (Jai, Zig, D) offers fast-naive for development +
> optimizing for release. But Boon has a unique advantage: **WASM can serve as BOTH**.
>
> - **Development**: Boon -> naive WASM -> V8 Liftoff (instant, tens of MB/s)
> - **Production**: Boon -> WASM + wasm-opt -> V8 TurboFan / Wasmtime AOT (85-97% native)
>
> Same target, same toolchain, two optimization levels. No dual backend to maintain.
>
> Furthermore, the self-hosting story is uniquely clean:
> 1. The Boon playground already runs in the browser via WASM
> 2. A Boon-to-WASM compiler runs in the browser via WASM
> 3. The compiler becomes a Boon program that compiles Boon programs
> 4. Meta-circular, in the browser, with reactive incremental compilation
>
> `─────────────────────────────────────────────────`

### Concrete Steps

1. **Now**: Finish DD engine (get 11/11 tests passing in `/repos/boon-dd-v2`).
   The clean three-layer architecture and `compile.rs` specialization are prerequisites.

2. **Next**: Build a minimal WASM emitter in Rust using `wasm-encoder` (~2-5K lines).
   Target Boon's core subset: numbers, text, objects, functions, HOLD, LATEST, WHEN,
   WHILE, THEN. Use the DD engine's `compile.rs` as the analysis frontend.

3. **Then**: Port the parser to Boon (using WHEN/HOLD/WHILE). This is the first
   "dogfooding" -- Boon parsing Boon.

4. **Then**: Port type inference and codegen to Boon. Type inference via reactive
   constraint propagation is the natural showcase.

5. **Finally**: Self-compile. Commit the .wasm artifact. Bootstrap from any platform.

### The Critical Design Tradeoff

| Expressiveness Restriction       | Performance Gain | Precedent              |
|----------------------------------|------------------|------------------------|
| Synchronous (no dynamic lists)   | ~0% overhead     | Lustre/SCADE (avionics) |
| No recursion on data structures  | GPU-matching     | Futhark                 |
| 6 reference capabilities         | C-like actors    | Pony                    |
| Fixed token rates only           | Zero scheduling  | SDF                     |
| **Boon's approach: classify & specialize** | **1.5-3x of Rust** | **CAL, DD engine variants** |

Boon's existing approach in the DD engine -- classify programs into specialized variants,
compile what you can, interpret the rest -- is the right strategy. The question is
extending it from "runtime configuration" to "native code emission."

### Total Compiler Size Estimate

| Component                    | Estimated Lines | Notes                     |
|------------------------------|-----------------|---------------------------|
| Parser (in Boon)             | ~2,000          | WHEN/HOLD/WHILE combinators |
| Type inference (in Boon)     | ~1,500          | Reactive constraint propagation |
| Program analysis (compile.rs) | ~2,000         | Extend DD engine's approach |
| WASM emitter                 | ~2,000          | Tree-walk + wasm-encoder   |
| Standard library             | ~3,000          | Math, List, Text, Element  |
| **Total**                    | **~10-15K**     | Oberon's OS+compiler was 12K |

---

## Appendix A: Research Sources

### Dataflow Systems Performance

- McSherry, "Scalability! But at what COST?" (USENIX HotOS 2015) -- single-threaded
  Rust vs distributed systems
- Naiad (SOSP 2013) -- sub-millisecond coordination across 64 machines
- DBSP/Feldera (VLDB 2023 Best Paper) -- 2.2x faster than Flink, 76% less memory
- StreamIt (MIT) -- 1-2x better than hand-written C for DSP
- Lustre/SCADE -- zero-overhead compilation for avionics (DO-178C Level A)

### Dataflow Machines in Hardware

- MIT Tagged-Token: ~3x overhead per core, 7x scaling on 8 cores
- WaveScalar (UW): 2-7x better than superscalar on SPEC benchmarks
- Hardware dataflow failed due to: token matching (2-4x cycles), communication
  granularity, CAM scaling, poor data locality

### Reactive Programming Overhead

- "Deprecating the Observer Pattern" (Maier, Rompf, Odersky) -- reactive is faster
  than observer pattern
- ECOOP 2018 -- glitch-free propagation cost proportional to graph depth
- Elm thesis (PLDI 2013) -- discrete FRP eliminates continuous-time sampling overhead

### Compilation Without LLVM

- Jai: 250K LOC/sec, custom x64 backend, `#run` bytecode interpreter
- Zig: 83x speedup for hello world, WASM bootstrap kernel, in-place binary patching
- QBE: 73% of LLVM performance in 8K lines of C
- TCC: 880K lines/sec, single-pass, ~100KB binary

### WASM Performance

- USENIX ATC 2019: WASM 1.45-1.55x slower than native (browser)
- WAMI 2025: Binaryen-only within 7.7% of LLVM, 1.9% faster on WAMR AOT
- V8 Liftoff: tens of MB/s compilation, 50-70% runtime of TurboFan
- Wasmtime AOT: 85-90% of native speed

### Self-Hosting

- Schism (Google): Self-hosting Scheme-to-WASM in <1K lines
- Zig: WASM bootstrap kernel, 4K-line C WASI interpreter
- Salsa/rust-analyzer: reactive incremental compilation framework
- Adapton: demand-driven incremental computation (academic foundation)

### Actor / Uniqueness Type Systems

- Pony: C-like performance via `iso`/`val`/`ref` reference capabilities, Orca GC
  (no stop-the-world, no atomics, per-actor heaps)
- Futhark: uniqueness types for in-place array updates, matches hand-written GPU
- Region-based memory (MLKit): actor lifetimes as region boundaries
