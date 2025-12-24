# M0 + M1 Implementation Plan: New Arena-Based Engine + CLI

## Goal

Build a **completely new engine** based on the design in `docs/new_boon/`. This is NOT extraction of existing code - it's a clean rewrite with arena-based architecture that enables snapshotting, hardware synthesis, and multi-threading.

---

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| CLI output format | **JSON serialization** of root binding | See §CLI Semantics |
| `boon serve` command | **Defer to M4** (Super-Counter) | Server needs WebSocket/HTTP |
| Platform abstraction | **Sync callbacks** (`schedule_tick`, `schedule_microtask`) | Matches sync EventLoop |
| Engine approach | **Clean rewrite** in `engine_v2/` alongside existing | No legacy compromise |
| Timer API naming | **Timer/interval** (not Stream/interval) | Consistent with examples |
| Engine location | **`crates/boon/src/engine_v2/`** (not platform/browser/) | Platform-agnostic core |

---

## CLI Semantics (Critical for M0)

### What value to print?

The CLI outputs the **root binding**, determined by precedence:

1. **`--root name`** flag if provided
2. **`document`** binding if exists (UI programs)
3. **`result`** binding if exists (computation programs)
4. **Last top-level binding** otherwise

```boon
-- Example 1: document binding takes precedence
counter: 0 |> HOLD state { ... }
helpers: [...]
document: counter |> Document/new()
-- Output: document's value (not helpers)

-- Example 2: result binding
x: 1 + 2
y: x * 2
result: y
-- Output: 6 (result binding, not y)

-- Example 3: last binding (no document/result)
x: 1 + 2
y: x * 2
-- Output: 6 (y is last binding)
```

For Document/new, output is the **text content** of the root element:
```boon
document: "Hello" |> Document/new()
-- Output: "Hello"
```

For non-text UI trees, output is a JSON representation of the element structure.

### When does `boon run` exit?

| Program Type | Exit Behavior |
|--------------|---------------|
| Pure computation | Immediately after evaluation |
| Timer-based | After `--ticks N` or `--ms N` (default: 1 tick) |
| With `--until-idle` | When no pending timers (may never exit!) |
| With `--repl` | Interactive mode, manual exit |

### Value Materialization

`Payload::ListHandle(SlotId)` and `ObjectHandle(SlotId)` are expanded to full JSON:

```rust
fn materialize(payload: &Payload, arena: &Arena) -> serde_json::Value {
    match payload {
        Payload::Number(n) => json!(n),
        Payload::Text(s) => json!(s),
        Payload::Bool(b) => json!(b),
        Payload::Tag(t) => json!(arena.tag_name(*t)),
        Payload::ListHandle(slot) => {
            let bus = arena.get(*slot);
            json!(bus.items().map(|item| materialize(item, arena)).collect::<Vec<_>>())
        }
        Payload::ObjectHandle(slot) => {
            let router = arena.get(*slot);
            json!(router.fields().map(|(k, v)| (k, materialize(v, arena))).collect::<Map<_>>())
        }
        Payload::Flushed(inner) => json!({"error": materialize(inner, arena)}),
        _ => json!(null), // Deltas not materialized
    }
}
```

### boon test (M0 minimal)

M0 provides golden-file testing:
```bash
boon test tests/*.bn              # Run all .bn files in tests/
boon test --update tests/foo.bn   # Update expected output
```

Test file format:
```boon
-- test: addition
result: 1 + 2
-- expect: 3

-- test: list
items: LIST { 1, 2, 3 }
-- expect: [1, 2, 3]
```

Note: Boon uses `LIST { ... }` syntax for lists, not `[...]`. The expect output is JSON.

Full Boon-native `TEST {}` syntax is M9.

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
    Unit,                  // Signal with no data (DOM events)
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

## Platform Traits (Canonical Design)

**This is the authoritative platform abstraction.** Other docs (§3.3, M0) should reference this.

The sync EventLoop needs platform support for:
1. **Timer scheduling** - wake up after delay
2. **Microtask scheduling** - run before next paint (browser) or immediately (CLI)
3. **Storage** - persist snapshots
4. **Logging** - console output

```rust
// platform/traits.rs

/// Schedule future work. Sync core, platform handles async wake-up.
pub trait PlatformTimer {
    /// Schedule next EventLoop tick after delay_ms
    fn schedule_tick(&self, delay_ms: u32, callback: Box<dyn FnOnce() + 'static>);

    /// Schedule work before next paint (browser) or immediately (CLI)
    fn schedule_microtask(&self, callback: Box<dyn FnOnce() + 'static>);

    /// Current wall-clock time in milliseconds
    fn now_ms(&self) -> f64;
}

/// Persist and restore snapshots
pub trait PlatformStorage {
    fn load(&self, key: &str) -> Option<String>;
    fn save(&self, key: &str, value: &str);
    fn remove(&self, key: &str);
}

/// Platform-specific logging
pub trait PlatformLog {
    fn info(&self, msg: &str);
    fn warn(&self, msg: &str);
    fn error(&self, msg: &str);
}

/// Combined platform interface (passed to EventLoop)
pub struct Platform {
    pub timer: Box<dyn PlatformTimer>,
    pub storage: Box<dyn PlatformStorage>,
    pub log: Box<dyn PlatformLog>,
}
```

**Platform Implementations:**

| Platform | Timer | Storage | Log |
|----------|-------|---------|-----|
| Browser | `queueMicrotask` / `setTimeout` | `localStorage` | `console.log` |
| CLI | tokio oneshot + sleep | File-based | `println!` |
| Server | tokio timers | Redis/file | `tracing` |

**NOT in platform traits (handled by bridge):**
- DOM rendering (browser-only)
- Element creation (browser-only)
- Event listeners (browser-only)

---

## Required Features (from §6.2 Examples Matrix)

These features are **required for M1 todo_mvc validation**. The plan must explicitly include them:

| Feature | Node/Mechanism | Required By | Issue |
|---------|----------------|-------------|-------|
| PASS/PASSED | Context threading | shopping_list, pages, todo_mvc | - |
| FUNCTION definitions | Compile-time | fibonacci, layers, shopping_list, etc. | - |
| BLOCK local bindings | Lexical scope | fibonacci, shopping_list, pages, etc. | Issue 19 |
| TEXT interpolation (reactive) | TextTemplate node | counter, fibonacci, etc. | Issue 4 |
| Effects (Log/info, Router/go_to) | EffectNode | fibonacci, pages | Issue 18 |
| Element state (hovered, focused) | ElementState streams | button_hover_test | Issue 7 |
| LINK bind/unbind protocol | Scope finalization | switch_hold_test, filter_checkbox_bug | Issue 15 |
| List/map external deps | Dependency tracking | list_map_external_dep, filter_checkbox_bug | Issue 16 |
| Chained List/remove | Removed-set composition | chained_list_remove_bug | Issue 17 |

**Phase mapping:**
- Phases 1-4: Core engine (no features above needed)
- Phase 4.5 (new): FUNCTION + BLOCK compilation
- Phase 5: Lists (includes List/map external deps, chained remove)
- Phase 6: Timer + Effects (Log/info)
- Phase 7: Bridge (PASS/PASSED, TextTemplate, Element states, LINK protocol)

---

## Implementation Phases (Reordered for M0/M1)

**Key change:** CLI test harness added early (Phase 3.5) so M0 enables fast iteration for M1.

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

### PHASE 3.5: CLI Test Harness (M0 Foundation)
**Create minimal CLI for fast native testing - enables M1 iteration**

| Step | Action | File |
|------|--------|------|
| 3.5.1 | Create CLI platform module | `src/platform/cli/mod.rs` |
| 3.5.2 | Implement CLI timer (tokio oneshot) | `src/platform/cli/timer.rs` |
| 3.5.3 | Implement CLI storage (file-based) | `src/platform/cli/storage.rs` |
| 3.5.4 | Implement CLI logger (println) | `src/platform/cli/mod.rs` |
| 3.5.5 | Create boon-cli crate skeleton | `crates/boon-cli/Cargo.toml` |
| 3.5.6 | Implement `boon eval "expr"` | `crates/boon-cli/src/main.rs` |
| 3.5.7 | Implement value materialization | `src/engine_v2/materialize.rs` |

**Validation:** `boon eval "[a: 1, b: 2].a"` outputs `1`

**Why now:** From this point, engine development uses fast native tests instead of browser. All subsequent phases benefit from CLI iteration speed.

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

### PHASE 4.5: FUNCTION + BLOCK Compilation
**Required for fibonacci, shopping_list, pages, todo_mvc**

| Step | Action | File |
|------|--------|------|
| 4.5.1 | Implement FUNCTION definition compilation | `src/evaluator_v2/compile.rs` |
| 4.5.2 | Implement FUNCTION call compilation | `src/evaluator_v2/compile.rs` |
| 4.5.3 | Implement BLOCK local bindings (Issue 19) | `src/evaluator_v2/compile.rs` |
| 4.5.4 | Implement closure capture for List/map predicates | `src/evaluator_v2/compile.rs` |
| 4.5.5 | Test FUNCTION inside WHILE arm | Test: `while_function_call.bn` |

**BLOCK Compilation (from Issue 19):**
```rust
// BLOCK { x: 1, y: x + 1, x * y }
// Compiles to:
//   slot_x = Producer(1)
//   slot_y = Transformer(slot_x, |x| x + 1)
//   slot_output = Transformer([slot_x, slot_y], |x, y| x * y)
//   return slot_output
```

**Validation:**
```boon
add: FUNCTION (a, b) { a + b }
result: add(1, 2)
-- Output: 3
```

---

### PHASE 5: Lists
**Implement Bus (dynamic wire collection) with known bug fixes**

| Step | Action | File |
|------|--------|------|
| 5.1 | Implement Bus (List container) | `src/engine_v2/node.rs` |
| 5.2 | Implement AllocSite for item identity | `src/engine_v2/arena.rs` |
| 5.3 | Implement List/append | `src/evaluator_v2/api.rs` |
| 5.4 | Implement List/remove with removed-set (Issue 17) | `src/evaluator_v2/api.rs` |
| 5.5 | Implement List/map with external deps (Issue 16) | `src/evaluator_v2/api.rs` |
| 5.6 | Implement List/retain, List/count, List/is_empty | `src/evaluator_v2/api.rs` |
| 5.7 | Diff tracking (ListDelta) | `src/engine_v2/node.rs` |
| 5.8 | **Use item_key (not index) for scope** (Issue 12) | `src/evaluator_v2/compile.rs` |

**Issue 16 - External dependency tracking:**
```rust
pub struct ItemSubgraph {
    pub root_slot: SlotId,
    pub external_deps: Vec<SlotId>,  // Deps outside item scope
}
// When external dep changes, re-evaluate affected items
```

**Issue 17 - Chained List/remove:**
```rust
pub struct ListRemoveNode {
    pub removed_keys: HashSet<ItemKey>,  // Keys removed BY THIS SITE
}
// On Replace delta, filter out our removed_keys
```

**Validation:**
```boon
items: LIST {} |> List/append(item: "hello") |> List/remove(if: item.done)
```

Test: `chained_list_remove_bug.bn`, `list_map_external_dep.bn`

---

### PHASE 6: Timer, Events & Effects
**Implement timer queue, DOM events, and side effects (Issue 18)**

| Step | Action | File |
|------|--------|------|
| 6.1 | Implement timer queue (BinaryHeap) | `src/engine_v2/event_loop.rs` |
| 6.2 | Implement Duration type and Timer/interval | `src/evaluator_v2/api.rs` |
| 6.3 | Implement IOPad (LINK node) skeleton | `src/engine_v2/node.rs` |
| 6.4 | Create platform trait for timer | `src/platform/traits.rs` |
| 6.5 | **Implement Stream/pulses (Issue 27)** | `src/evaluator_v2/api.rs` |
| 6.6 | **Implement Stream/skip** | `src/evaluator_v2/api.rs` |
| 6.7 | Implement EffectNode (Issue 18) | `src/engine_v2/node.rs` |
| 6.8 | Implement Log/info effect | `src/evaluator_v2/api.rs` |
| 6.9 | CLI: Execute effects at tick end | `src/platform/cli/runtime.rs` |
| 6.10 | Browser adapter with microtasks | `src/platform/browser/engine_v2_adapter.rs` |

**EffectNode (Issue 18):**
```rust
pub struct EffectNode {
    pub effect_kind: EffectKind,
    pub trigger_input: SlotId,
    pub last_execution_tick: u64,  // Prevent double-run on restore
}

pub enum NodeEffect {
    ConsoleLog { level: LogLevel, message: String },
    RouterNavigate { url: String },
    // ... other effects
}
```

**Timer/interval API (matches existing examples):**
```boon
-- Canonical form: Duration piped to Timer/interval
ticks: Duration[seconds: 1] |> Timer/interval()

-- Duration can also use milliseconds
ticks: Duration[milliseconds: 500] |> Timer/interval()
```

**Stream/pulses (Issue 27 - Sequential like FPGA clock):**
```rust
// 5 |> Stream/pulses() emits 0, 1, 2, 3, 4 over 5 ticks
// Sequential emission allows HOLD body to see updated state between each pulse
pub struct PulsesNode {
    remaining: u32,
    total: u32,
}
// Each pulse schedules next tick, not batched
```

**Stream/skip:**
```rust
// stream |> Stream/skip(count: 3) drops first 3 values
pub struct SkipNode {
    remaining_to_skip: u32,
}
```

**Validation:** `interval.bn`, `interval_hold.bn`, and `fibonacci.bn` work
```boon
-- interval_hold uses Stream/skip
ticks: Duration[seconds: 1] |> Timer/interval() |> Stream/skip(count: 1)
    |> HOLD state { ... }
```

Test: `fibonacci.bn` (uses Stream/pulses, Log/info)

---

### PHASE 7: Bridge & Playground Integration
**Connect new engine to Zoon UI with full feature set**

| Step | Action | File |
|------|--------|------|
| 7.1 | Create bridge_v2.rs | `src/platform/browser/bridge_v2.rs` |
| 7.2 | Implement arena → Zoon element conversion | `src/platform/browser/bridge_v2.rs` |
| 7.3 | Add feature flag for engine selection | `Cargo.toml`, `interpreter.rs` |
| 7.4 | Wire up Document/new | `src/evaluator_v2/api.rs` |
| 7.5 | Wire up Element/* functions | `src/evaluator_v2/api.rs` |
| 7.6 | **Implement PASS/PASSED context threading** | `src/evaluator_v2/compile.rs` |
| 7.7 | **Implement TextTemplate node (Issue 4)** | `src/engine_v2/node.rs` |
| 7.8 | **Implement Element states (Issue 7)** | `src/engine_v2/node.rs` |
| 7.9 | **Implement LINK bind/unbind protocol (Issue 15)** | `src/engine_v2/node.rs` |
| 7.10 | **Implement scope finalization at tick end** | `src/engine_v2/event_loop.rs` |
| 7.11 | Implement Router/go_to, Router/route effects | `src/evaluator_v2/api.rs` |

**PASS/PASSED Context Threading:**
```rust
// During function call compilation:
// 1. Caller sets PASS: value → stored in context
// 2. Inside function, PASSED resolves to context value
// 3. Context cleared after function returns
```

**TextTemplate (Issue 4):**
```rust
pub struct TextTemplate {
    template: String,           // "Value: {0}"
    dependencies: Vec<SlotId>,  // Slots to watch
    cached_output: Option<String>,
}
// On any dependency change → re-render template
```

**Element States (Issue 7):**
```rust
pub struct ElementState {
    pub hovered: SlotId,   // Bool producer
    pub focused: SlotId,   // Bool producer
}
// Automatically updated by LINK bindings
```

**LINK Unbinding (Issue 15):**
```rust
// When WHILE switches arms or List/remove:
// 1. finalize_pending_scopes() at tick end
// 2. Old LINK bindings unbound (event listeners removed)
// 3. No event leakage between arms
```

**Validation:** Full M1 example suite:
- `shopping_list.bn` - PASS/PASSED, List ops
- `pages.bn` - Router/go_to, Router/route
- `button_hover_test.bn` - Element hovered state
- `switch_hold_test.bn` - LINK rebinding
- `todo_mvc.bn` - **FULL VALIDATION TARGET**

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

### PHASE 9: CLI Completion (M0 Finish)
**Complete CLI with `boon run` and `boon test`**

Note: CLI foundation was built in Phase 3.5. This phase completes it.

| Step | Action | File |
|------|--------|------|
| 9.1 | Implement `boon run file.bn` | `crates/boon-cli/src/main.rs` |
| 9.2 | Implement `--ticks N` / `--ms N` flags | `crates/boon-cli/src/main.rs` |
| 9.3 | Implement `boon test` with golden-file | `crates/boon-cli/src/main.rs` |
| 9.4 | Implement `boon test --update` | `crates/boon-cli/src/main.rs` |
| 9.5 | Add helpful error messages | `crates/boon-cli/src/main.rs` |

**CLI Commands:**
```rust
#[derive(Parser)]
enum Commands {
    /// Execute a Boon file and print result as JSON
    Run {
        file: PathBuf,
        #[arg(long, default_value = "1")]
        ticks: u32,
        #[arg(long)]
        ms: Option<u32>,
    },
    /// Evaluate inline Boon code
    Eval { code: String },
    /// Run Boon test files (golden-file comparison)
    Test {
        files: Vec<PathBuf>,
        #[arg(long)]
        update: bool,  // Update expected outputs
    },
}
```

**Validation:**
- `boon run examples/counter.bn` outputs JSON
- `boon run examples/interval.bn --ticks 5` outputs after 5 ticks
- `boon test tests/*.bn` compares outputs

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
Phase 1:   Arena + SlotId            ─┐
Phase 2:   Message + Routing          │ Core Engine
Phase 3:   Basic Nodes                │ (unit tests only)
                                     ─┘
Phase 3.5: CLI Test Harness          ─── M0 FOUNDATION (enables fast iteration)

Phase 4:   Combinators               ─┐
Phase 4.5: FUNCTION + BLOCK           │ Language Features
Phase 5:   Lists + Bug Fixes          │ (CLI tests)
Phase 6:   Timer + Effects           ─┘

Phase 7:   Bridge + Full UI          ─┐ Browser Integration
Phase 8:   Snapshot                  ─┘ (playground tests)

Phase 9:   CLI Completion            ─── M0 COMPLETE
```

**Key insight:** CLI in Phase 3.5 means phases 4-8 can use fast native tests instead of browser.

**Parallel work possible:**
- After Phase 3.5: CLI team can work on Phase 9 while browser team works on Phases 4-8
- Phase 8 (Snapshot) can overlap with Phase 7 (Bridge)

Each phase should be a separate commit with passing tests.

---

## Appendix: Minimum Stdlib for 23 Validation Examples

Union of built-ins actually used in the 23 `.bn` files in `playground/frontend/src/examples/`.

### API Functions Required

| Namespace | Functions |
|-----------|-----------|
| **Document** | `new` |
| **Element** | `button`, `checkbox`, `container`, `label`, `link`, `paragraph`, `stack`, `stripe`, `text_input` |
| **List** | `append`, `clear`, `count`, `is_empty`, `map`, `remove`, `retain` |
| **Log** | `info` |
| **Math** | `sum` |
| **Router** | `go_to`, `route` |
| **Stream** | `pulses`, `skip` |
| **Text** | `empty`, `is_not_empty`, `space`, `trim` |
| **Timer** | `interval` |
| **Bool** | `not`, `or` |

**Total: 30 API functions**

### Language Features Required

| Category | Features |
|----------|----------|
| **Constructs** | `FUNCTION`, `LIST {}`, `HOLD`, `LATEST`, `THEN`, `WHEN`, `WHILE`, `LINK`, `PASS:`/`PASSED` |
| **Types** | Tagged objects (e.g., `Duration[seconds: 1]`), Objects `[a: 1, b: 2]` |
| **Operators** | `+`, `-`, `==`, `>`, field-path access (`a.b.c`), pipe (`\|>`) |

### Implementation Priority

1. **Phase 4-5**: List/*, HOLD, LATEST, THEN, WHEN, WHILE
2. **Phase 6**: Timer/interval, Stream/pulses, Stream/skip
3. **Phase 7**: Element/*, Document/new, Router/*, Text/*, Bool/*, Log/info, Math/sum, LINK, PASS/PASSED

This checklist ensures all 23 validation examples can run on the new engine.
