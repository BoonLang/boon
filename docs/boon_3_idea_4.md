# Boon 3 – Idea 4: “Graph IR + Host Adapters” Architecture

This document describes a **simpler and more maintainable architecture** for Boon **without changing Boon source syntax**.

It is written as an implementation guide for another agent: definitions, invariants, concrete data structures, and a step-by-step plan.

---

## 0) Executive Summary

**One sentence:** Parse Boon to AST, compile AST into a **platform-agnostic Reactive Graph IR**, then either **run** that IR in a deterministic event loop (browser/CLI) or **transpile** it (FPGA/RISC‑V), using thin **host adapters** for IO and effects.

**Core design move:** Separate the world into:

1. **Syntax** (AST): what the user wrote
2. **Semantics** (IR): what the program *means* (reactive graph + stable identity)
3. **Runtime** (Executor): how the graph propagates values deterministically
4. **Host** (Platform adapters): how external events enter and effects leave

This gives a single “source of truth” for:
- reactivity semantics (LATEST/WHEN/WHILE/HOLD/LINK)
- hot reload + state evolution
- monitoring/graph visualization
- cross-platform targets

---

## 1) Goals and Non‑Goals

### Goals
- **No Boon syntax changes.** (Parser/lexer behavior stays compatible.)
- **Single semantic representation** reusable by:
  - browser runtime
  - CLI runtime
  - testing harness
  - monitor/visualization
  - future FPGA/RISC‑V backends
- **Deterministic execution** (replayable; stable ordering).
- **State persistence + hot reload** as a first‑class flow:
  - serialize only **runtime state**, not closures/futures
  - reattach old state to “same” nodes via stable identity
- **Clear IO boundary**:
  - all nondeterminism comes from *host inputs*
  - all side effects are explicit as *effect nodes*

### Non‑Goals (for initial implementation)
- Type system / inference (can be layered later on IR).
- Optimizations beyond correctness and determinism.
- Multi-threading (can be added after semantics are stable).

---

## 2) Current Pain Points This Fixes

These are typical failure modes of “runtime-first” designs where compilation/evaluation and platform concerns are interleaved:
- Hot reload is hard because runtime nodes are identified by “incidental” allocation order.
- Persistence is hard because runtime state is entangled with platform objects and async tasks.
- Browser/CLI/FPGA semantics drift because each target re-implements parts of evaluation.
- Debugging is hard because “meaning” is distributed across evaluator + runtime + platform bridge.

**Graph IR** makes “meaning” explicit and portable.

---

## 3) Architecture Overview

### 3.1 Layers

1) **Syntax** (`parser`):
- Input: Boon source text
- Output: AST with spans
- No IO, no persistence, no platform types

2) **Semantics** (`compiler`):
- Input: AST (+ optional previous AST/IR metadata for state evolution)
- Output: **Reactive Graph IR**
- Responsibilities:
  - name resolution / scope rules
  - persistence identity assignment (stable ids)
  - desugaring (PASS/PASSED, function calls, patterns)
  - building a typed graph shape (ports/edges) without executing it

3) **Runtime** (`runtime`):
- Input: Graph IR + initial RuntimeState snapshot (optional)
- Output: evolving RuntimeState + emitted effects
- Responsibilities:
  - create runtime nodes from IR
  - deterministic propagation (dirty queue)
  - track node-local state (registers, combiner last values, list contents…)

4) **Host** (`host` / platform adapters):
- Browser adapter: DOM events in, UI rendering out, timers, storage
- CLI adapter: stdin/filesystem clock, console output
- FPGA adapter: IO pins, memory-mapped peripherals (future)

### 3.2 “Only the Host Touches the World”

All external interaction is via two mechanisms:

- **Host Inputs**: push values into specific input nodes (LINK events, timers, IO pins…)
- **Effects**: runtime produces a FIFO list of effect requests that host executes

Everything else is pure, deterministic graph propagation.

---

## 4) The Reactive Graph IR (Core Concept)

### 4.1 Mental model

The IR is a directed graph of **nodes** connected by **edges** between **ports**.

Nodes are *small* and *explicit*:
- pure nodes compute outputs from inputs (possibly with node-local state)
- router-like nodes expose fields/items
- effect nodes queue effect requests
- host nodes represent external sources (events, timers, IO)

### 4.2 Two identities: Stable vs Runtime

**Stable ID** (semantic identity):
- derived from source spans + persistence rules
- survives recompilation / hot reload
- used to reattach state

**Runtime handle** (execution identity):
- e.g. arena slot id or integer index allocated when instantiating IR
- cheap and local

Never use runtime allocation order as persistence identity.

### 4.3 Suggested core types (pseudo‑Rust)

```rust
// Stable ID (semantic identity)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeStableId(pub u128); // ULID or derived hash

// Runtime ID (execution identity)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeRtId(pub u32); // arena index + generation if needed

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PortId(pub u16);

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum EdgeKind {
    Data,           // continuous dependency
    Trigger,        // arrival-only (WHEN/THEN semantics)
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub from: (NodeStableId, PortId),
    pub to:   (NodeStableId, PortId),
    pub kind: EdgeKind,
}

#[derive(Clone, Debug)]
pub struct GraphIr {
    pub nodes: Vec<NodeSpec>,
    pub edges: Vec<Edge>,
    pub exports: GraphExports, // roots like `document`, `main`, etc.
    pub debug: GraphDebugInfo, // spans, names, source maps
}

#[derive(Clone, Debug)]
pub struct NodeSpec {
    pub id: NodeStableId,
    pub kind: NodeKindSpec,
    pub debug_name: Option<String>,
}
```

### 4.4 NodeKindSpec: “shape”, not state

**Rule:** `NodeKindSpec` is derived from source code and is *not persisted*.

```rust
#[derive(Clone, Debug)]
pub enum NodeKindSpec {
    // values
    Const { value: Payload },
    Wire  { source: NodeStableId }, // alias/reference

    // structure
    Object { fields: Vec<(FieldId, NodeStableId)> }, // routers by field
    List   { items: Vec<(ItemKey, NodeStableId)> },  // bus of items

    // combinators
    Latest { inputs: Vec<NodeStableId> },
    Hold   { initial: NodeStableId, update: NodeStableId }, // or explicit ports
    Then   { trigger: NodeStableId, body: NodeStableId },
    When   { input: NodeStableId, arms: Vec<(RuntimePattern, NodeStableId)> },
    While  { input: NodeStableId, arms: Vec<(RuntimePattern, NodeStableId)> },

    // builtins (pure)
    MathSum { input: NodeStableId },
    Add { a: NodeStableId, b: NodeStableId },
    // ...

    // host integration
    HostInput { input_kind: HostInputKind },   // LINK events, timers, IO
    Effect    { effect_kind: EffectKind, input: NodeStableId },
}
```

**Note:** You do *not* have to hardcode every builtin as a special node kind.
Another valid approach is:
- `NodeKindSpec::Call { callee: CalleeId, args: Vec<...> }`
- and then register callee handlers either as “pure templates” or host calls.

Pick one; keep the boundary crisp.

---

## 5) Mapping Boon Constructs → IR (No Syntax Changes)

Below is the *semantic mapping*. It must match today’s Boon behavior.

### 5.1 Variables

`counter: 0` becomes:
- a `Const(0)` node
- a `Wire` node for the variable name (optional but useful for debug / identity)
- exports/bindings record that the global name `counter` refers to that stable node id

**Recommendation:** make “named variables” explicit nodes so:
- monitor can show them
- persistence identity has a home
- state evolution can reason about “variable renamed” separately (optional)

### 5.2 Objects and field access

`my: OBJECT { a: 1, b: 2 }` becomes:
- `Const(1)`, `Const(2)`
- `Object { fields: a->id1, b->id2 }`

`my.a` compiles to either:
- direct edge to field node (preferred if object fields are static)
- or a `FieldExtractor` node if fields can be dynamic

### 5.3 Function calls

Two categories:

1) **User-defined functions** (`FUNCTION ...`):
  - compile body into a subgraph template
  - instantiate on each call with a stable *call site id*
  - connect arguments via wires/ports

2) **Builtins** (`Math/sum`, `Element/button`, `Document/new`, `List/map` …):
  - either compile into dedicated node kinds (fast/simple)
  - or compile into `Call` nodes with a registry

Important: keep compile-time PASS/PASSED behavior *out of runtime*.

### 5.4 PASS/PASSED (compile-time only)

PASS/PASSED should desugar entirely during compilation.

Runtime should never need a “passed stack”.

### 5.5 LATEST

`LATEST { a, b }` becomes `Latest` node:
- N data inputs
- emits whenever any input changes
- uses deterministic tie-breaking when “simultaneous”

State (runtime only): last seen values per input.

### 5.6 HOLD

`HOLD { initial, update }` becomes `Hold` node:
- stores a value (register)
- updates on update-events depending on semantics (arrival vs continuous)
- supports “initial value” rules

State: stored value + “initial received” flag (if needed).

### 5.7 THEN / WHEN / WHILE

These are the semantic heart. The IR must make the difference explicit:

- **WHILE**: continuous dependency *while an arm is selected*
  - the selected arm output updates as its dependencies update
  - changing selector changes which arm is currently routed

- **WHEN**: snapshot/copy semantics *when input arrives*
  - on each matching input event, copy the arm’s current value at that instant
  - after that, dependencies changing do not retroactively change the previously copied result

- **THEN**: WHEN with wildcard pattern (trigger-only)

**IR representation suggestion:**
- Use `EdgeKind::Trigger` for arrival-only edges feeding WHEN/THEN triggers.
- Use `EdgeKind::Data` for continuous dependencies inside arm bodies.
- WHEN nodes keep a local “latched” output (the snapshot).
- WHILE nodes keep a local “current arm” index and forward continuously from that arm.

### 5.8 LINK

`press: LINK` is a “future input” placeholder.

Two things must happen:

1) compile-time: create a stable node id for `press` (so it persists)
2) runtime: host binds it to a specific host input source (DOM event, IO pin…)

In IR terms:
- `press` is a `HostInput { kind: DomEvent { element_id, "press" } }` node
- or `IOPad`+`Link` arrangement if you want to preserve your current mental model

But the key invariant: LINK values are not normal computed values; they are **host sources**.

---

## 6) Runtime: Deterministic Executor

### 6.1 Inputs, propagation, effects

At runtime, the engine repeatedly:
1) receives host inputs (events/timers/IO)
2) marks affected nodes dirty
3) propagates updates deterministically until quiescence
4) emits queued effects in FIFO order

This is compatible with the existing `engine_v2` idea: arena + synchronous event loop.

### 6.2 Deterministic ordering

Define a stable ordering for processing dirty nodes:
- primarily by `NodeStableId` (or a derived `order_key`)
- secondarily by port id / edge index when needed

This enables:
- replay debugging
- consistent tests
- consistent cross-platform behavior

### 6.3 Split NodeSpec vs NodeState

**NodeSpec** is compiled from code (Graph IR).
**NodeState** is runtime-local and is the only thing that should be snapshotted/persisted.

Example state shapes:

```rust
#[derive(Serialize, Deserialize)]
pub enum NodeState {
    Unit,
    Latest { last: Vec<Option<Payload>> },
    Hold { stored: Payload, initial_received: bool },
    When { latched: Payload, current_arm: Option<usize> },
    While { current_arm: Option<usize> },
    List { items: Vec<PersistedItem> },
    Timer { next_fire_tick: u64, active: bool },
    // ...
}
```

**Implementation note:** not every node needs a dedicated state variant; many are stateless.

---

## 7) Persistence & Hot Reload (State Evolution)

### 7.1 Principle

On code change, do **not** try to serialize “the whole runtime”.

Instead:
1) compile new code → new Graph IR
2) compute node stable ids for new IR
3) load previous snapshot (map stable id → node state)
4) instantiate runtime nodes for new IR
5) for each new node, reattach state from snapshot if stable id matches

### 7.2 Stable identity sources

You need stable ids that are robust across edits.

Suggested hierarchy:
- **primary**: existing persistence resolver output (`PersistenceId`)
- **secondary**: source span hash + syntactic path (for nodes that aren’t “persisted variables”)
- **tertiary**: deterministic derivation using:
  - parent stable id
  - child index (field name, arm index, list item key)

The exact policy matters less than being:
- deterministic
- stable under “small edits”
- predictable for migrations (see below)

### 7.3 State migration is already a language feature

Boon already supports migration by wiring old values into new ones with `LATEST { old, new }`.

The architecture must preserve this by:
- reusing stable ids for variables that remain
- removing state for nodes that are removed
- allowing code to explicitly “adopt” old state via wiring patterns

### 7.4 Snapshot format

Store:
- graph version hash (optional)
- map: `NodeStableId -> NodeState`
- optional host metadata (e.g., localStorage keys)

Do **not** store:
- NodeSpec (it is derived)
- edges
- platform objects
- closures/futures

---

## 8) Host Adapter API (Browser/CLI/FPGA)

### 8.1 The minimal host contract

Host must provide:
- a way to deliver inputs to runtime nodes (by stable id)
- a way to execute effect requests
- a scheduling mechanism for ticks (microtask / loop)

Pseudo‑Rust:

```rust
pub trait Host {
    fn now_tick(&self) -> u64;
    fn schedule_tick(&mut self); // e.g., queueMicrotask

    fn deliver_input(&mut self, target: NodeStableId, value: Payload);

    fn execute_effects(&mut self, effects: Vec<EffectRequest>);
}
```

### 8.2 Effects are data

Effects must be explicit data packets:

```rust
pub enum EffectRequest {
    ConsoleLog { message: String },
    DomPatch { /* ... */ },
    StorageSet { key: String, value: String },
    Navigate { to: String },
    // ...
}
```

Browser runtime turns `DomPatch` into actual DOM operations (or Zoon calls).
CLI runtime turns `ConsoleLog` into stdout/stderr, etc.

### 8.3 Timers

Timers are tricky because they create events “later”.

Recommended model:
- Timer nodes are host inputs managed by the host scheduler
- runtime asks host: “schedule next fire at time T” (effect request)
- host fires by delivering an input pulse to the timer node

Alternative model:
- keep timers inside runtime with a timer queue; host only calls `run_tick()`

Either is fine; prefer whichever yields simplest browser integration.

---

## 9) Implementation Plan (Incremental, Low-Risk)

This is written for the current repo structure where `engine_v2` exists.

### Phase A: Introduce IR as an internal module (no behavior change)
- Create `crates/boon/src/ir/` (or `compiler/ir/`) with:
  - `GraphIr`, `NodeSpec`, `Edge`, stable id types
- Write tests: parse tiny Boon → IR has expected nodes/edges (golden JSON ok)

### Phase B: Make evaluator_v2 compile to IR (instead of directly to runtime nodes)
- Extract “build nodes + connect routes” into an IR builder
- Keep existing runtime executor; add a new “instantiate IR” step:
  - IR → runtime arena nodes + routing table
- Tests: same example programs produce same observable outputs

### Phase C: Formalize NodeSpec vs NodeState
- Add explicit `NodeState` serialization
- Replace ad-hoc persistence with snapshot maps keyed by stable id
- Tests:
  - run program, mutate state, snapshot
  - rebuild runtime from same IR + snapshot
  - verify outputs

### Phase D: Host adapter boundary
- Convert browser runtime and CLI to implement:
  - input delivery
  - effect execution
- Remove remaining platform dependencies from core

### Phase E: Hot reload / state evolution
- Compile new IR on code change
- Reuse snapshot attachment rules
- Validate with existing “counter rename” migration story

---

## 10) Testing Strategy

Aim to make failures local and debuggable:

1) **Parser tests**: source → AST (existing)
2) **Compiler/IR tests**:
   - AST → IR structure
   - stable ids remain stable across “whitespace-only” edits
3) **Runtime tests**:
   - IR + inputs → outputs (deterministic)
   - effect ordering FIFO
4) **Persistence tests**:
   - snapshot/restore round trip
   - state evolution across IR changes

Add a tiny harness that:
- compiles `.bn` to IR JSON (debug)
- runs IR with a scripted input sequence
- compares produced effects and final exported values

This becomes the backbone for CLI tests and later cross-platform validation.

---

## 11) Debuggability / Monitor Integration

Graph IR should contain:
- node stable id
- debug name (variable name/function path)
- source span (file + byte offsets)
- node kind + ports
- edges (with kind: Data vs Trigger)

This makes the monitor a pure “IR/Runtime inspector”:
- visualize graph shape from IR
- visualize current values/state from runtime state map
- allow replay by feeding recorded host inputs

---

## 12) Key Invariants (Write These Down in Code Too)

1) **IR is platform-agnostic.**
   - No DOM types, no storage handles, no async tasks inside IR or runtime core.

2) **Only NodeState is persisted.**
   - NodeSpec is always derived from code (IR).

3) **Stable identity does not depend on runtime allocation order.**

4) **Deterministic propagation order.**
   - Same inputs → same outputs/effects.

5) **All nondeterminism enters through host inputs.**

6) **All side effects are explicit effect nodes and are FIFO ordered.**

---

## 13) Open Questions (Decide Early)

These choices affect implementation but not the overall architecture:

1) Do builtins compile to dedicated node kinds, or generic `Call` nodes with a registry?
2) Are timers managed in runtime (timer queue) or in host (schedule + pulse)?
3) How exactly are stable ids derived for non-persisted nodes?
   - choose a deterministic scheme and document it
4) Do we want first-class “subgraphs” / “modules” in IR now, or flatten everything?
   - flattening is simplest for v1

---

## 14) Minimal Checklist for “Done”

This architecture is “real” when:
- The compiler can emit a Graph IR for any existing example.
- The runtime can instantiate Graph IR and run it deterministically.
- Browser and CLI runtimes share the same core runtime.
- Hot reload works by reattaching NodeState via stable ids.
- Monitor can render IR + runtime state without special casing.

---

## 15) Mapping This Design onto the Current Boon Repo

This section is intentionally concrete: it names the existing modules and suggests how they map to the 4-layer architecture.

### 15.1 Syntax layer (keep as-is)

Already exists:
- `crates/boon/src/parser.rs` + `crates/boon/src/parser/*`

Output types to reuse:
- `parser::Spanned<Expression>` (already carries `persistence: Option<Persistence>`)
- `parser::PersistenceId` (`u128`) (already designed for durable identity)

### 15.2 Semantics layer (new “IR builder” module)

Today, `crates/boon/src/evaluator_v2/mod.rs` compiles AST directly into `engine_v2` arena nodes.

Change direction:
- Introduce `crates/boon/src/ir/` (or `compiler/ir/`) with `GraphIr` + `NodeSpec` + `Edge`.
- Refactor `evaluator_v2` into (or add a sibling) “compiler” that outputs **Graph IR**, not runtime slots.

Practical approach:
- Keep the existing `CompileContext` structure, but change the “emit” operations from:
  - `alloc node in arena` + `routing.add_route(...)`
  to:
  - `push NodeSpec` + `push Edge`

This keeps semantics in one place and makes the runtime a pure instantiator/executor of IR.

### 15.3 Runtime layer (reuse `engine_v2`, but make it IR-driven)

Already exists:
- `crates/boon/src/engine_v2/*` (arena + routing + event_loop + node kinds)

Recommended refactor:
- Add an “instantiation” step: `GraphIr -> (Arena nodes, RoutingTable routes, export slots)`.
- Make persistence and hot reload attach to runtime nodes by **stable id**, not by slot index.

Important: split **spec** vs **state**:
- Today `engine_v2::node::NodeKind` mixes the static wiring info (inputs, fields, arms) with runtime state (last_values, stored_value, mapped_items…).
- For the new architecture, treat that mixed enum as an internal implementation detail, but add a clear persistence boundary:
  - define `NodeState` (serializable)
  - define “state extraction” and “state injection” functions that translate between `NodeKind` and `NodeState`

This allows snapshot/restore and hot reload without serializing arena internals.

### 15.4 Host layer (move platform responsibilities out of core)

Already exists:
- Browser integration: `crates/boon/src/platform/browser/bridge_v2.rs`
- CLI integration: `crates/boon/src/platform/cli/*`

What to move to “host” explicitly:
- **State persistence storage**:
  - `bridge_v2` currently loads/saves state via localStorage and “marks nodes dirty”.
  - In the new architecture, the host owns persistence IO; the runtime only reads/writes a `Snapshot` struct.
- **Real timers**:
  - `bridge_v2` currently calls `EventLoop::take_pending_timers()` and schedules `Timer::sleep`.
  - In the new architecture, scheduling is a host concern; runtime expresses “please schedule timer X”.

Recommended end state:
- Core runtime exposes: `run_tick(inputs) -> { snapshot_delta, effects }`.
- Browser host consumes effects and performs rendering/timers/storage.

---

## 16) Stable IDs in *This* Codebase (Suggested Policy)

You already have two useful identity sources:
- `parser::PersistenceId(u128)` (durable, hierarchical with `with_child_index`, `with_child`)
- `engine_v2::address::SourceId { stable_id: u64, parse_order: u32 }` (structural hash + tiebreaker)

Suggested policy (simple, works with Boon’s existing “explicit migration via code” story):

1) **Nodes that correspond to persisted bindings** (variables/objects/tagged objects/list items) use `PersistenceId` as their primary stable id.
2) **All other nodes** use a deterministic id derived from source structure (e.g. `SourceId`).
3) **Derived/internal nodes** (field routers, arm bodies, template subgraphs) derive their stable id from the parent via `PersistenceId::with_child_index(...)` (or a hash combine), so the entire subgraph has stable identity when the parent does.

A pragmatic key type:

```rust
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct StableNodeKey {
    pub base: StableBaseId, // persistence-first, source fallback
    pub scope: ScopeId,     // distinguishes function instantiations / templates
    pub role: u32,          // child discriminator (field, arm index, etc.)
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum StableBaseId {
    Persist(PersistenceId), // u128
    Source(SourceId),       // { stable_id, parse_order }
}
```

Where `role` can be:
- `hash("field", field_name)`
- `hash("arm", arm_index)`
- `hash("template_input")`, `hash("template_output")`

This is more explicit (and less collision-prone) than trying to squeeze everything into a single u64.

