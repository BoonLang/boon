# M0 + M1 Implementation Plan: New Arena-Based Engine + CLI

## Goal

Build a **completely new engine** based on the design in `docs/new_boon/`. This is NOT extraction of existing code - it's a clean rewrite with arena-based architecture that enables snapshotting, hardware synthesis, and multi-threading.

---

## Key Decisions

| Decision | Choice |
|----------|--------|
| CLI output format | **JSON serialization** of final value |
| `boon serve` command | **Defer to M4** (Super-Counter) |
| Platform abstraction | **Generics** (`Platform<R: Runtime, T: Timer, ...>`) |
| Engine approach | **Clean rewrite** in `engine_v2/` alongside existing |

---

## Architecture Comparison

| Aspect | OLD Engine | NEW Engine |
|--------|------------|------------|
| Node storage | `Arc<ValueActor>` (heap) | `Arena<ReactiveNode>` (contiguous) |
| References | Arc pointers | `SlotId` (index + generation) |
| Scheduling | Async `ActorLoop` with streams | Sync `EventLoop.run_tick()` |
| Subscriptions | Channel-based | Explicit `RoutingTable` |
| Values | `Value` enum with metadata | `Payload` enum (simpler) |
| Timer | `zoon::Timer` async | Timer queue + microtasks |
| Serialization | Hard (closures, channels) | **Easy** (plain data) |
| Hardware model | No mapping | Maps to register/wire |

---

## New Core Types (from docs/new_boon/)

### SlotId & Arena (§2.2)
```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotId {
    index: u32,
    generation: u32,
}

pub struct Arena {
    nodes: Vec<ReactiveNode>,
    free_list: Vec<u32>,
}
```

### ReactiveNode (§2.2.5)
```rust
#[repr(C, align(64))]
pub struct ReactiveNode {
    generation: u32,
    version: u32,
    dirty: bool,
    kind_tag: u8,
    input_count: u8,
    subscriber_count: u8,
    inputs: [SlotId; 4],
    subscribers: [SlotId; 2],
    extension: Option<Box<NodeExtension>>,
}
```

### Message & Payload (§2.3)
```rust
pub struct Message {
    pub source: NodeAddress,
    pub payload: Payload,
    pub version: u64,
    pub idempotency_key: u64,
}

pub enum Payload {
    Number(f64),
    Text(Arc<str>),
    Tag(u32),
    Bool(bool),
    ListHandle(SlotId),
    ObjectHandle(SlotId),
    Flushed(Box<Payload>),
    ListDelta(ListDelta),
    ObjectDelta(ObjectDelta),
}
```

### EventLoop (§2.4)
```rust
pub struct EventLoop {
    arena: Arena,
    timer_queue: BinaryHeap<TimerEvent>,
    dirty_nodes: Vec<SlotId>,
    dom_events: VecDeque<DomEvent>,
    current_tick: u64,
    pending_effects: Vec<NodeEffect>,
}

impl EventLoop {
    pub fn run_tick(&mut self) {
        self.current_tick += 1;
        self.process_timers();
        self.process_dom_events();
        while !self.dirty_nodes.is_empty() {
            // Process until quiescence
        }
        self.execute_pending_effects();
    }
}
```

### Node Kinds (§2.3.2)
| Boon Construct | Node Kind | Hardware Equivalent |
|----------------|-----------|---------------------|
| Constant | `Producer` | Tied signal |
| Variable | `Wire` | Named wire |
| Object | `Router` | Demultiplexer |
| List | `Bus` | Address decoder |
| LATEST | `Combiner` | Multiplexer |
| HOLD | `Register` | D flip-flop |
| THEN | `Transformer` | Combinational logic |
| WHEN | `PatternMux` | Pattern decoder |
| WHILE | `SwitchedWire` | Tri-state buffer |
| LINK | `IOPad` | I/O port |

---

## Target Directory Structure

```
crates/boon/src/
├── lib.rs                           # Feature-gated exports
├── parser/                          # Unchanged
│
├── engine_v2/                       # NEW: Platform-agnostic core
│   ├── mod.rs                       # Re-exports
│   ├── arena.rs                     # Arena, SlotId, ReactiveNode
│   ├── node.rs                      # NodeKind variants (Producer, Wire, Router, etc.)
│   ├── message.rs                   # Message, Payload, ListDelta, ObjectDelta
│   ├── event_loop.rs                # EventLoop, run_tick(), timer queue
│   ├── routing.rs                   # RoutingTable, static/dynamic routes
│   ├── address.rs                   # SourceId, ScopeId, NodeAddress, Domain, Port
│   └── snapshot.rs                  # GraphSnapshot serialization
│
├── evaluator_v2/                    # NEW: Evaluator for new engine
│   ├── mod.rs
│   ├── compile.rs                   # AST → nodes in arena
│   └── api.rs                       # API function bindings
│
├── platform/
│   ├── traits.rs                    # Platform abstraction traits
│   ├── browser/
│   │   ├── engine.rs                # OLD engine (kept during migration)
│   │   ├── engine_v2_adapter.rs     # NEW: Microtask integration for new engine
│   │   ├── bridge_v2.rs             # NEW: Arena → Zoon
│   │   └── ...
│   └── cli/                         # NEW: CLI platform
│       ├── mod.rs
│       ├── runtime.rs               # Tokio-based EventLoop driver
│       └── storage.rs               # File-based persistence

crates/boon-cli/                     # NEW: CLI binary
├── Cargo.toml
└── src/main.rs
```

---

## Platform Traits (Generics-Based)

```rust
// platform/traits.rs

pub trait TimerBackend: Clone + 'static {
    type Sleep: Future<Output = ()> + 'static;
    fn schedule_tick(&self, delay_ms: u32, callback: impl FnOnce() + 'static);
    fn schedule_microtask(&self, callback: impl FnOnce() + 'static);
}

pub trait StorageBackend: Clone + 'static {
    fn load(&self, key: &str) -> Option<String>;
    fn save(&self, key: &str, value: &str);
}

pub trait LogBackend: Clone + 'static {
    fn info(&self, msg: &str);
    fn error(&self, msg: &str);
}
```

---

## Implementation Phases (from §4.9)

### PHASE 1: Core Types & Arena
**Create `engine_v2/` module with foundational types**

| Step | Action | File |
|------|--------|------|
| 1.1 | Create module structure | `src/engine_v2/mod.rs` |
| 1.2 | Implement SourceId, ScopeId, NodeAddress | `src/engine_v2/address.rs` |
| 1.3 | Implement SlotId (generational index) | `src/engine_v2/arena.rs` |
| 1.4 | Implement Arena (alloc, free, get) | `src/engine_v2/arena.rs` |
| 1.5 | Implement ReactiveNode struct | `src/engine_v2/node.rs` |
| 1.6 | Add EventLoop skeleton | `src/engine_v2/event_loop.rs` |
| 1.7 | Unit tests for arena operations | `src/engine_v2/arena.rs` |

**Validation:** `cargo test -p boon` passes

---

### PHASE 2: Message & Routing
**Implement message passing infrastructure**

| Step | Action | File |
|------|--------|------|
| 2.1 | Implement Payload enum | `src/engine_v2/message.rs` |
| 2.2 | Implement Message struct | `src/engine_v2/message.rs` |
| 2.3 | Implement ListDelta, ObjectDelta | `src/engine_v2/message.rs` |
| 2.4 | Create RoutingTable | `src/engine_v2/routing.rs` |
| 2.5 | Implement add_route, remove_route | `src/engine_v2/routing.rs` |
| 2.6 | Implement deliver_message in EventLoop | `src/engine_v2/event_loop.rs` |

**Validation:** Unit test: message flows from node A to node B

---

### PHASE 3: Basic Nodes
**Implement fundamental node kinds**

| Step | Action | File |
|------|--------|------|
| 3.1 | Implement Producer (constant) | `src/engine_v2/node.rs` |
| 3.2 | Implement Wire (variable forwarding) | `src/engine_v2/node.rs` |
| 3.3 | Implement Router (object demux) | `src/engine_v2/node.rs` |
| 3.4 | Implement field access resolution | `src/engine_v2/node.rs` |
| 3.5 | Create basic evaluator | `src/evaluator_v2/mod.rs` |

**Validation:** Simple object with fields works
```boon
obj: [a: 1, b: 2]
result: obj.a  -- should be 1
```

---

### PHASE 4: Combinators
**Implement LATEST, HOLD, THEN/WHEN, WHILE**

| Step | Action | File |
|------|--------|------|
| 4.1 | Implement Combiner (LATEST) | `src/engine_v2/node.rs` |
| 4.2 | Implement Register (HOLD) | `src/engine_v2/node.rs` |
| 4.3 | Implement Transformer (THEN) | `src/engine_v2/node.rs` |
| 4.4 | Implement PatternMux (WHEN) | `src/engine_v2/node.rs` |
| 4.5 | Implement SwitchedWire (WHILE) | `src/engine_v2/node.rs` |
| 4.6 | Wire up evaluator for these constructs | `src/evaluator_v2/compile.rs` |

**Validation:** `counter.bn` example works (manually triggered)
```boon
count: 0 |> HOLD state {
    increment |> THEN { state + 1 }
}
```

---

### PHASE 5: Lists
**Implement Bus (dynamic wire collection)**

| Step | Action | File |
|------|--------|------|
| 5.1 | Implement Bus (List container) | `src/engine_v2/node.rs` |
| 5.2 | Implement AllocSite for item identity | `src/engine_v2/arena.rs` |
| 5.3 | Implement List/append | `src/evaluator_v2/api.rs` |
| 5.4 | Implement List/remove | `src/evaluator_v2/api.rs` |
| 5.5 | Implement List/map with external deps | `src/evaluator_v2/api.rs` |
| 5.6 | Diff tracking (ListDelta) | `src/engine_v2/node.rs` |

**Validation:** Simple list append/remove works
```boon
items: LIST {} |> List/append(item: "hello")
```

---

### PHASE 6: Timer & Events
**Implement timer queue and DOM events**

| Step | Action | File |
|------|--------|------|
| 6.1 | Implement timer queue (BinaryHeap) | `src/engine_v2/event_loop.rs` |
| 6.2 | Implement Timer/interval API | `src/evaluator_v2/api.rs` |
| 6.3 | Implement IOPad (LINK node) | `src/engine_v2/node.rs` |
| 6.4 | Create platform trait for timer | `src/platform/traits.rs` |
| 6.5 | Browser adapter with microtasks | `src/platform/browser/engine_v2_adapter.rs` |

**Validation:** `interval.bn` example works
```boon
ticks: 0 |> HOLD state {
    Timer/interval(1000) |> THEN { state + 1 }
}
```

---

### PHASE 7: Bridge & Playground Integration
**Connect new engine to Zoon UI**

| Step | Action | File |
|------|--------|------|
| 7.1 | Create bridge_v2.rs | `src/platform/browser/bridge_v2.rs` |
| 7.2 | Implement arena → Zoon element conversion | `src/platform/browser/bridge_v2.rs` |
| 7.3 | Add feature flag for engine selection | `Cargo.toml`, `interpreter.rs` |
| 7.4 | Wire up Document/new | `src/evaluator_v2/api.rs` |
| 7.5 | Wire up Element/* functions | `src/evaluator_v2/api.rs` |

**Validation:** `todo_mvc.bn` works in playground with new engine

---

### PHASE 8: Snapshot System
**Implement serialization for persistence**

| Step | Action | File |
|------|--------|------|
| 8.1 | Implement GraphSnapshot | `src/engine_v2/snapshot.rs` |
| 8.2 | Serialize arena to JSON | `src/engine_v2/snapshot.rs` |
| 8.3 | Restore arena from snapshot | `src/engine_v2/snapshot.rs` |
| 8.4 | Integrate with localStorage | `src/platform/browser/engine_v2_adapter.rs` |

**Validation:** Reload page, state preserved

---

### PHASE 9: CLI Platform
**Create CLI runtime using new engine**

| Step | Action | File |
|------|--------|------|
| 9.1 | Create CLI platform module | `src/platform/cli/mod.rs` |
| 9.2 | Implement Tokio-based EventLoop driver | `src/platform/cli/runtime.rs` |
| 9.3 | Implement file-based storage | `src/platform/cli/storage.rs` |
| 9.4 | Implement console logger | `src/platform/cli/mod.rs` |

**Validation:** `cargo check --features cli`

---

### PHASE 10: CLI Binary
**Create boon command-line tool**

| Step | Action | File |
|------|--------|------|
| 10.1 | Create boon-cli crate | `crates/boon-cli/Cargo.toml` |
| 10.2 | Implement `boon run` | `crates/boon-cli/src/main.rs` |
| 10.3 | Implement `boon eval` | `crates/boon-cli/src/main.rs` |
| 10.4 | Implement `boon test` | `crates/boon-cli/src/main.rs` |
| 10.5 | JSON output formatting | `crates/boon-cli/src/main.rs` |

**CLI Commands:**
```rust
#[derive(Parser)]
enum Commands {
    /// Execute a Boon file and print result as JSON
    Run { file: PathBuf },
    /// Evaluate inline Boon code
    Eval { code: String },
    /// Run Boon test files
    Test { files: Vec<PathBuf> },
}
```

**Validation:** `cargo run -p boon-cli -- run examples/counter.bn`

---

## Critical Files Summary

| New File | Purpose | Lines Est. |
|----------|---------|------------|
| `engine_v2/arena.rs` | SlotId, Arena, ReactiveNode | ~300 |
| `engine_v2/node.rs` | NodeKind variants, processing | ~1,500 |
| `engine_v2/message.rs` | Message, Payload, deltas | ~200 |
| `engine_v2/event_loop.rs` | EventLoop, run_tick, timers | ~500 |
| `engine_v2/routing.rs` | RoutingTable | ~200 |
| `engine_v2/address.rs` | SourceId, ScopeId, NodeAddress | ~150 |
| `engine_v2/snapshot.rs` | Serialization | ~300 |
| `evaluator_v2/mod.rs` | AST → arena compilation | ~2,000 |
| `evaluator_v2/api.rs` | API function bindings | ~1,500 |
| `platform/browser/bridge_v2.rs` | Arena → Zoon | ~1,000 |
| `platform/cli/runtime.rs` | Tokio EventLoop driver | ~200 |
| `boon-cli/src/main.rs` | CLI binary | ~200 |

**Total new code:** ~8,000 lines (vs ~20,000 in old engine)

---

## Success Criteria

### M1 Complete When:
- [ ] `engine_v2/` compiles without browser deps
- [ ] `counter.bn` works with new engine
- [ ] `interval.bn` works with new engine
- [ ] `todo_mvc.bn` works with new engine
- [ ] State persists across page reload
- [ ] Feature flag switches between old/new engine
- [ ] No performance regression

### M0 Complete When:
- [ ] `boon run file.bn` executes and outputs JSON
- [ ] `boon eval "code"` works
- [ ] `boon test` runs test files
- [ ] Timer/interval works in CLI
- [ ] File-based persistence works

---

## Documentation References

| Section | Topic |
|---------|-------|
| §2.1 | Node identification (SourceId, ScopeId, NodeAddress) |
| §2.2 | Arena memory model |
| §2.3 | Message passing, node kinds |
| §2.4 | EventLoop, timers, microtasks |
| §2.5 | Graph snapshot |
| §2.6 | FLUSH error handling |
| §4.9 | Implementation phases |

---

## Order of Implementation

```
Phase 1: Arena + SlotId          ─┐
Phase 2: Message + Routing        │ Core Engine
Phase 3: Basic Nodes              │ (platform-agnostic)
Phase 4: Combinators              │
Phase 5: Lists                   ─┘
Phase 6: Timer + Events          ─┐
Phase 7: Bridge + Playground      │ Browser Integration
Phase 8: Snapshot                ─┘
Phase 9: CLI Platform            ─┐
Phase 10: CLI Binary              │ CLI
                                 ─┘
```

Each phase should be a separate commit with passing tests.
