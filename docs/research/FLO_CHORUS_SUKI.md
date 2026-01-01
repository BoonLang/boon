# Stream Processing Research: Flo, Suki, and ChoRus

This document summarizes three academic papers/projects relevant to Boon's design and future architecture.

## Overview

| Paper | Venue | Focus | Relevance to Boon |
|-------|-------|-------|-------------------|
| **Flo** | POPL 2025 | Formal semantics for streaming | Validates Boon's `TypedStream` design |
| **Suki** | CP 2024 | Distributed dataflow compilation | Blueprint for multi-target (FPGA, distributed) |
| **ChoRus** | CP 2024 | Choreographic programming library | Patterns for distributed conditionals |

All three originate from the intersection of reactive programming, distributed systems, and formal methods. They share a common goal: **making distributed reactive systems correct by construction**.

---

## 1. Flo: A Semantic Foundation for Progressive Stream Processing

**Authors**: Shadaj Laddad, Mae Milano, Alvin Cheung, Joseph Hellerstein (UC Berkeley)
**Venue**: POPL 2025 (Principles of Programming Languages)
**Paper**: https://arxiv.org/abs/2411.08274
**Project**: [Hydro](https://github.com/hydro-project/hydro)

### Core Contribution

Flo identifies two fundamental semantic properties for streaming systems:

1. **Streaming Progress**: Outputs advance as inputs arrive—no arbitrary stagnation
2. **Eager Execution**: Outputs remain deterministic and fresh with respect to inputs

These properties together ensure that streaming computations behave predictably.

### Key Technical Ideas

#### Bounded vs Unbounded Streams

Flo's type system distinguishes:

- **Bounded streams**: Can have operators that block on termination (e.g., `collect()`, `fold()`)
- **Unbounded streams**: Must be processed progressively, cannot block on termination

This distinction is critical because mixing them incorrectly causes deadlocks or starvation.

#### Nested Graphs with Cycles

Flo provides constructs for dataflow composition including cycles (feedback loops). This is essential for stateful computations where output feeds back into input.

#### Unification of Streaming Models

The paper shows how existing systems map to Flo's semantics:

| System | Domain | How Flo Models It |
|--------|--------|-------------------|
| Apache Flink | Stream processing | Windowed aggregations as bounded sub-streams |
| LVars | Parallel programming | Monotonic lattice operations |
| DBSP | Incremental computation | Differential dataflow operators |

### Relevance to Boon

**Boon already implements the bounded/unbounded distinction!** In `engine.rs`:

```rust
pub struct TypedStream<S, Lifecycle> {
    inner: S,
    _marker: PhantomData<Lifecycle>,
}

pub struct Infinite;  // Unbounded - safe for ValueActor
pub struct Finite;    // Bounded - needs .keep_alive()
```

Flo provides formal justification for this design choice. The `Infinite` marker maps to Flo's unbounded streams, and `Finite` maps to bounded.

| Boon Construct | Flo Property |
|----------------|--------------|
| `ValueActor` with `TypedStream<_, Infinite>` | Streaming progress guaranteed |
| `LazyValueActor` in HOLD body | Sequential evaluation for cycles |
| `constant()` helper | Converts bounded to unbounded safely |

### What to Adopt

1. **Formal documentation**: Use Flo's terminology when documenting Boon's guarantees
2. **Cycle handling patterns**: Study how Flo composes nested graphs—may improve HOLD
3. **DBSP techniques**: Incremental computation could optimize reactive updates

---

## 2. Suki: Choreographed Distributed Dataflow in Rust

**Authors**: Shadaj Laddad, Alvin Cheung, Joseph Hellerstein (UC Berkeley)
**Venue**: CP 2024 (Choreographic Programming Workshop at PLDI)
**Paper**: https://arxiv.org/abs/2406.14733

### Core Contribution

Suki is an embedded Rust DSL for streaming dataflow with **explicit placement of computation**. Key innovation: **two-phase staged compilation**.

### Key Technical Ideas

#### Two-Phase Staged Compilation

```
Phase 1: Build Global Dataflow Graph
         (runs on developer machine)
              │
              ▼
Phase 2: Compile Per-Location Binaries
         (generates optimized Rust for each node)
```

This separation enables:
- Global optimization across the entire dataflow
- Per-location code that compiles to efficient native binaries
- Zero runtime scheduling overhead

#### Location Types

Suki introduces location-aware types:

```rust
// Process: exactly one computational instance
let server: Process = ...;

// Cluster: SIMD-style distributed computation
let workers: Cluster = ...;

// Streams are located
let data: Stream<Data, Server> = ...;
```

Network crossings are **explicit operators**, not implicit:

```rust
// Data moves from server to browser explicitly
let browser_data = server_data.send_bincode(browser);
```

#### Zero-Overhead Choreography

Because scheduling decisions happen at compile time (Phase 1), the generated binaries:
- Have no runtime scheduler overhead
- Are amenable to autovectorization
- Can be optimized by LLVM like any Rust code

### Relevance to Boon

Boon's roadmap includes FPGA and RISC-V targets (see `docs/new_boon/5.x`). Suki's approach solves exactly this problem.

**Proposed Boon Extension**:

```boon
@browser
ui_state: LATEST { clicks, inputs }

@server
validated: form_data |> validate()

@fpga
dsp: audio_in |> lowpass(cutoff: 1000) |> amplify(gain: 2.0)
```

The location annotation (`@browser`, `@server`, `@fpga`) becomes part of the type, and network crossings are explicit.

#### Two-Phase for Boon's New Engine

```
Boon Source (.bn)
      │
      ▼
┌─────────────────────┐
│  Phase 1: Parse     │
│  Build Arena Graph  │
└──────────┬──────────┘
           │
     ┌─────┴─────┬─────────────┐
     ▼           ▼             ▼
┌─────────┐ ┌─────────┐ ┌───────────┐
│  WASM   │ │ Verilog │ │  RISC-V   │
│ (browser)│ │ (FPGA)  │ │  (embed)  │
└─────────┘ └─────────┘ └───────────┘
```

### What to Adopt

1. **Location annotations**: Add `@location` syntax to Boon
2. **Explicit network operators**: Make data crossing visible in syntax
3. **Two-phase architecture**: New arena engine should support graph extraction for ahead-of-time compilation
4. **The `q!` macro pattern**: For capturing code during staging (if Boon adds Rust-level metaprogramming)

---

## 3. ChoRus: Library-Level Choreographic Programming in Rust

**Authors**: Lindsey Kuper et al. (UC Santa Cruz)
**Venue**: CP 2024
**Documentation**: https://lsd-ucsc.github.io/ChoRus/
**Crate**: https://crates.io/crates/chorus_lib

### Core Contribution

ChoRus implements choreographic programming as a Rust library (not a separate language). Key technique: **EPP-as-DI** (End-Point Projection as Dependency Injection).

### Key Technical Ideas

#### Located Values

Values are tagged with their location:

```rust
// A value that exists at location Alice
let data: Located<String, Alice> = ...;

// Communication creates located values at the destination
let received: Located<String, Bob> = data.comm(Bob);
```

#### Choreographic Enclaves

Sub-choreographies that only involve specific participants:

```rust
// Only Alice and Bob participate in this sub-computation
enclave!(participants: [Alice, Bob]) {
    let secret = alice_data.combine(bob_data);
    // Carol doesn't need to know about this
}
```

**Key benefit**: "Knowledge of choice" only propagates to participants. Other nodes don't need to be informed of decisions they're not involved in.

#### EPP-as-DI

Instead of compiling choreography to per-location code, ChoRus uses dependency injection:
- Each location gets a different implementation of choreographic operators
- At runtime, the correct implementation is injected based on which node is executing

This is less relevant for Boon (which has its own compiler), but the concepts are transferable.

### Relevance to Boon

#### Enclaves Map to BLOCK

Boon's `BLOCK` construct creates a local scope. In a distributed setting, this could become an enclave:

```boon
@server enclave
validated: BLOCK {
    temp: input |> parse()
    result: temp |> validate()
    output: result
}

@browser
ui: validated |> render()  // Browser only sees the result
```

The browser doesn't need to know about `temp` or the validation logic—it just receives `validated`.

#### Distributed WHEN Optimization

When WHEN/WHILE involves distributed actors, enclaves can reduce network traffic:

```boon
// Without enclaves: browser must know about all patterns
input |> WHEN {
    Valid(data) => process(data)    // Both branches need
    Invalid(err) => handle(err)     // to propagate to browser
}

// With server enclave: browser only gets the result
@server enclave
result: input |> WHEN {
    Valid(data) => process(data)
    Invalid(err) => handle(err)
}

@browser
ui: result |> render()  // Just receives the outcome
```

### What to Adopt

1. **Enclave semantics for BLOCK**: In distributed Boon, BLOCK could limit knowledge propagation
2. **Knowledge-of-choice patterns**: How to efficiently propagate conditional decisions
3. **Less urgent than Flo/Suki**: ChoRus is library-level, Boon has its own syntax

---

## Synthesis: Priority for Boon

### Validates Current Design

| Boon Feature | Academic Validation |
|--------------|---------------------|
| `TypedStream<_, Infinite/Finite>` | Flo's bounded/unbounded types (POPL'25) |
| `ValueActor` eager evaluation | Flo's eager execution property |
| `HOLD` feedback loops | Flo's nested graphs with cycles |
| `LazyValueActor` for HOLD body | Flo's streaming progress guarantee |

### Informs Future Architecture

| Feature | Source | Priority | Target |
|---------|--------|----------|--------|
| Location types (`@browser`, `@fpga`) | Suki | HIGH | New engine |
| Two-phase compilation | Suki | HIGH | New engine |
| Explicit network operators | Suki | HIGH | New engine |
| Formal semantic docs | Flo | MEDIUM | Documentation |
| Enclave optimization | ChoRus | LOW | Distributed Boon |

### Reading Priority

1. **Flo paper** — Read now for formal understanding of stream lifecycle
2. **Suki paper** — Read before implementing new engine's multi-target support
3. **DBSP paper** (referenced by Flo) — For incremental computation optimization
4. **ChoRus docs** — Reference when implementing distributed features

---

## Architecture Diagram

```
                         ┌───────────────────────────────────────┐
                         │            BOON LANGUAGE              │
                         │   (LATEST, HOLD, WHEN, WHILE, LINK)   │
                         └──────────────────┬────────────────────┘
                                            │
              ┌─────────────────────────────┼─────────────────────────────┐
              │                             │                             │
              ▼                             ▼                             ▼
   ┌────────────────────┐      ┌────────────────────┐      ┌────────────────────┐
   │   FLO SEMANTICS    │      │   SUKI STAGING     │      │  CHORUS ENCLAVES   │
   │                    │      │                    │      │                    │
   │ • Bounded/unbounded│      │ • Location types   │      │ • Sub-scopes       │
   │ • Progress property│      │ • Two-phase compile│      │ • Knowledge prop.  │
   │ • Cycle handling   │      │ • Zero-overhead    │      │ • Limited audience │
   └─────────┬──────────┘      └─────────┬──────────┘      └─────────┬──────────┘
             │                           │                           │
             │  Current Engine           │  New Engine               │  Future
             │  Validation               │  Architecture             │  Distributed
             │                           │                           │
             └───────────────────────────┴───────────────────────────┘
                                         │
                          ┌──────────────▼──────────────┐
                          │      BOON NEW ENGINE        │
                          │       (Arena-based)         │
                          │                             │
                          │  ┌───────────────────────┐  │
                          │  │   Graph Extraction    │◀─┼── Phase 1
                          │  │   (location-aware)    │  │
                          │  └───────────┬───────────┘  │
                          │              │              │
                          │  ┌───────────▼───────────┐  │
                          │  │   Target Compilers    │◀─┼── Phase 2
                          │  │   • WASM (browser)    │  │
                          │  │   • Verilog (FPGA)    │  │
                          │  │   • RISC-V (embedded) │  │
                          │  └───────────────────────┘  │
                          └─────────────────────────────┘
```

---

## References

- Laddad, S., Milano, M., Cheung, A., & Hellerstein, J. (2025). *Flo: A Semantic Foundation for Progressive Stream Processing*. POPL 2025. https://arxiv.org/abs/2411.08274

- Laddad, S., Cheung, A., & Hellerstein, J. (2024). *Suki: Choreographed Distributed Dataflow in Rust*. CP 2024. https://arxiv.org/abs/2406.14733

- Kuper, L., et al. (2024). *ChoRus: Library-Level Choreographic Programming in Rust*. CP 2024. https://lsd-ucsc.github.io/ChoRus/

- Hydro Project. https://github.com/hydro-project/hydro

- Budiu, M., et al. *DBSP: Automatic Incremental View Maintenance*. (Referenced by Flo for incremental computation)
