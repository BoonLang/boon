# Boon v3 Engine Prototype Comparison

## Goal

Build two prototype engines in `experiments/` to empirically compare:
- **Path A**: Dirty propagation + explicit captures (incremental improvement to v2)
- **Path B**: Full re-evaluation + no cloning (radical simplification from Idea 2)

Both must fix the **toggle-all bug**: newly added TodoMVC items don't respond to the toggle-all checkbox.

---

## Background: The Core Problem

**Current v2 bug**: When `List/map` or `List/append` creates new items, the subgraph cloning misses external dependencies. In TodoMVC:

```boon
// todo_mvc.bn - each todo item's completed status
completed: False |> HOLD state {
    LATEST {
        todo_checkbox.event.click |> THEN { state |> Bool/not() }
        store.elements.toggle_all_checkbox.event.click  // EXTERNAL DEPENDENCY
            |> THEN { store.all_completed |> Bool/not() }
    }
}
```

New items don't subscribe to `toggle_all_checkbox.event.click` because the runtime cloning doesn't properly capture this external reference.

---

## Two Architectural Solutions

### Path A: Dirty Propagation + Explicit Captures

Keep arena-based nodes, but make template dependencies explicit at compile time.

**Key changes**:
- Templates store a `captures: Vec<CaptureSpec>` listing all external dependencies
- At instantiation, bind each capture to its external slot
- Add SKIP as first-class value (unbound LINK returns SKIP)
- Add hierarchical ScopeId for stable identity

**Performance model**: O(dirty nodes) per tick - unchanged nodes never visited

### Path B: Re-evaluation + No Cloning (Idea 3 "Instance VM")

Eliminate subgraph cloning entirely. Evaluate same code in different scopes.
Based on detailed specification in `../docs/boon_3_idea_3.md`.

**Key changes**:
- State lives in cells keyed by `(ScopeId, ExprId)` = `SlotKey`
- Each tick re-evaluates expressions with caching
- Cache check: if no inputs changed, return cached value
- No cloning means no "missed wires" bug by design
- `TickSeq = (tick, seq)` for intra-tick ordering
- Cache entries track `deps` for "why did X change?" diagnostics
- Commit order: HOLD → LINK → LIST → effects

**Performance model**: O(all expressions) per tick with cache hits

---

## Performance Comparison

| Scenario | Path A | Path B |
|----------|--------|--------|
| 100 items, 1 changes | ~10 nodes processed | ~1000 cache checks, 10 cache misses |
| 100 items, toggle-all | 100 subgraphs processed | 100 cache misses |
| Add new item | Clone template (~10 nodes) | O(1) add, next tick evaluates |
| Large static UI, no changes | O(1) | O(n) cache checks |
| Memory (100 items) | ~1000 arena slots | ~300 cells |

**Unknown until benchmarked**: Actual cache check overhead in WASM, real-world cache hit rates.

---

## Experiments Structure

```
experiments/
├── path_a/                    # Dirty propagation + explicit captures
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── arena.rs           # Slot allocator
│   │   ├── node.rs            # NodeKind with captures
│   │   ├── template.rs        # Template + CaptureSpec
│   │   ├── evaluator.rs       # Compile AST → nodes
│   │   ├── engine.rs          # Dirty propagation tick loop
│   │   ├── value.rs           # Payload types + SKIP
│   │   └── ledger.rs          # Delta ledger
│   └── tests/
│       ├── counter.rs
│       ├── list_append.rs
│       ├── toggle_all.rs      # Critical test
│       └── todo_mvc.rs
│
├── path_b/                    # Re-eval + no cloning (Idea 3 "Instance VM")
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── tick.rs            # TickSeq for intra-tick ordering
│   │   ├── slot.rs            # SlotKey = (ScopeId, ExprId)
│   │   ├── scope.rs           # Hierarchical ScopeId
│   │   ├── cell.rs            # HoldCell, LinkCell, ListCell
│   │   ├── cache.rs           # CacheEntry with deps tracking
│   │   ├── runtime.rs         # Runtime struct (cells + cache + tick state)
│   │   ├── evaluator.rs       # eval(expr, scope, ctx) -> (Value, TickSeq)
│   │   ├── value.rs           # Payload types + SKIP
│   │   └── diagnostics.rs     # "Why did X change?" queries
│   └── tests/
│       ├── counter.rs
│       ├── list_append.rs
│       ├── toggle_all.rs      # Critical test
│       ├── todo_mvc.rs
│       └── diagnostics.rs     # Test "why did X change?"
│
├── shared/                    # Shared infrastructure
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── test_harness.rs    # Event simulation, assertions
│   │   ├── ast.rs             # Shared AST types (or use parser)
│   │   └── examples.rs        # Load .bn files
│   └── boon_examples/
│       ├── counter.bn
│       ├── interval.bn
│       ├── shopping_list.bn
│       └── todo_mvc.bn
│
└── bench/
    ├── Cargo.toml
    └── src/
        └── main.rs            # Criterion benchmarks
```

---

## Test Harness (No Browser)

```rust
// shared/src/test_harness.rs
pub trait Engine {
    fn new(ast: &Ast) -> Self;
    fn inject(&mut self, path: &str, payload: Value);
    fn tick(&mut self);
    fn read(&self, path: &str) -> Value;
}

pub struct TestEngine<E: Engine> {
    engine: E,
}

impl<E: Engine> TestEngine<E> {
    pub fn inject_event(&mut self, path: &str, payload: Value) {
        self.engine.inject(path, payload);
        self.engine.tick();
    }

    pub fn assert_eq(&self, path: &str, expected: Value) {
        assert_eq!(self.engine.read(path), expected);
    }
}
```

---

## Critical Test: Toggle-All Bug

```rust
#[test]
fn toggle_all_affects_new_items() {
    let mut engine = TestEngine::new(parse_file("todo_mvc.bn"));

    // Add 3 items
    engine.inject_event("new_todo_input.submit", text("Buy milk"));
    engine.inject_event("new_todo_input.submit", text("Walk dog"));
    engine.inject_event("new_todo_input.submit", text("Code boon"));

    // All uncompleted
    engine.assert_eq("todos[0].completed", Bool(false));
    engine.assert_eq("todos[1].completed", Bool(false));
    engine.assert_eq("todos[2].completed", Bool(false));

    // Click toggle-all
    engine.inject_event("toggle_all.click", Unit);

    // All completed
    engine.assert_eq("todos[0].completed", Bool(true));
    engine.assert_eq("todos[1].completed", Bool(true));
    engine.assert_eq("todos[2].completed", Bool(true));

    // Add NEW item after toggle-all
    engine.inject_event("new_todo_input.submit", text("New item"));

    // THE CRITICAL TEST: New item should also be completed
    engine.assert_eq("todos[3].completed", Bool(true));
}
```

---

## Benchmarks

```rust
use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};

fn bench_toggle_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("toggle_all");

    for n in [10, 100, 1000] {
        group.bench_with_input(BenchmarkId::new("path_a", n), &n, |b, &n| {
            b.iter(|| {
                let mut e = path_a::Engine::new(todo_ast());
                for i in 0..n { e.inject("new_todo", text(format!("Item {i}"))); }
                e.inject("toggle_all.click", Unit);
                e.tick();
            });
        });

        group.bench_with_input(BenchmarkId::new("path_b", n), &n, |b, &n| {
            b.iter(|| {
                let mut e = path_b::Engine::new(todo_ast());
                for i in 0..n { e.inject("new_todo", text(format!("Item {i}"))); }
                e.inject("toggle_all.click", Unit);
                e.tick();
            });
        });
    }
}

fn bench_steady_state(c: &mut Criterion) {
    // Bench ticking with no changes (tests cache efficiency)
}

fn bench_add_item(c: &mut Criterion) {
    // Bench instantiation cost
}

criterion_group!(benches, bench_toggle_all, bench_steady_state, bench_add_item);
criterion_main!(benches);
```

---

## Implementation Phases

### Phase 1: Foundation (~1 day) ✅ COMPLETE
- [x] Create `experiments/` directory structure
- [x] Set up Cargo workspace
- [x] Create `shared/` crate with test harness
- [x] Copy .bn examples from playground

### Phase 2: Path A Prototype (~2 days) ✅ COMPLETE
- [x] `value.rs` - Payload types + SKIP
- [x] `arena.rs` - Slot allocator (simplified from v2)
- [x] `node.rs` - NodeKind enum
- [x] `template.rs` - Template + CaptureSpec
- [x] `evaluator.rs` - AST → nodes
- [x] `engine.rs` - Dirty propagation tick loop
- [x] `ledger.rs` - Delta logging
- [x] Tests passing (including toggle_all)

### Phase 3: Path B Prototype (~2-3 days, more complete per Idea 3) ✅ COMPLETE
- [x] `value.rs` - Payload types + SKIP
- [x] `tick.rs` - TickSeq for intra-tick ordering
- [x] `slot.rs` - SlotKey = (ScopeId, ExprId)
- [x] `scope.rs` - Hierarchical ScopeId
- [x] `cell.rs` - HoldCell, LinkCell, ListCell
- [x] `cache.rs` - CacheEntry with deps tracking
- [x] `runtime.rs` - Runtime struct combining cells + cache
- [x] `evaluator.rs` - eval(expr, scope, ctx) -> (Value, TickSeq)
- [x] `diagnostics.rs` - "Why did X change?" queries
- [x] Tests passing including diagnostics test (and toggle_all)

### Phase 4: Comparison (~1 day) ✅ COMPLETE
- [x] Run all tests on both
- [x] Run benchmarks
- [x] Measure memory usage
- [x] Count lines of code (Path A: 1,747, Path B: 1,622)
- [x] Document findings (see FINDINGS.md)

### Phase 5: Decision ✅ COMPLETE
- [x] Choose winner based on data → **Path B wins**
- [x] Plan port to main engine (see FINDINGS.md recommendations)

---

## Success Criteria

| Criterion | Path A | Path B | Status |
|-----------|--------|--------|--------|
| All tests pass | Required | Required | ✅ Both pass |
| toggle_all_affects_new_items | Required | Required | ✅ Both pass |
| 1000-item toggle-all < 100ms | Required | Required | ✅ Path B: 3.5ms, Path A: 14.4s (fails) |
| Memory for 1000 items < 10MB | Preferred | Preferred | ✅ Both use minimal memory |

---

## Features Both Prototypes Include

| Feature | Purpose |
|---------|---------|
| **SKIP value** | Unbound LINK returns SKIP, propagates through THEN/WHEN |
| **Hierarchical ScopeId** | Stable identity: `child(parent, discriminator)` |
| **Delta Ledger** | Debug log: Set, ListInsert, ListRemove, Event, LinkBind |
| **No browser** | Pure Rust tests, event simulation |

---

## Key Files to Reference

| File | What to learn |
|------|---------------|
| `../crates/boon/src/parser/` | AST types to reuse or simplify |
| `../crates/boon/src/engine_v2/node.rs` | Current NodeKind enum |
| `../crates/boon/src/engine_v2/event_loop.rs` | Current tick processing |
| `../crates/boon/src/evaluator_v2/mod.rs` | Current AST → nodes compilation |
| `../playground/frontend/src/examples/todo_mvc/` | TodoMVC example |
| `../docs/boon_3_idea_1.md` | Chronicle Reactor design (Path A inspiration) |
| `../docs/boon_3_idea_2.md` | Instance VM overview (Path B concept) |
| `../docs/boon_3_idea_3.md` | **Instance VM detailed spec (Path B implementation guide)** |
| `../docs/boon_3_idea_4.md` | Graph IR + Host Adapters (future layering improvement) |

---

## Data Structures

### Path A: Template with Captures

```rust
struct Template {
    id: TemplateId,
    input_slots: Vec<SlotId>,       // e.g., `item` parameter
    output_slot: SlotId,
    captures: Vec<CaptureSpec>,     // External dependencies
    internal_nodes: Vec<SlotId>,
}

struct CaptureSpec {
    external_slot: SlotId,          // The external slot being captured
    placeholder_slot: SlotId,       // Wire in template to rebind
}
```

### Path B: Cell Store (from Idea 3)

```rust
// Ordering within a tick
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TickSeq {
    pub tick: u64,
    pub seq: u32,
}

// Universal address for values/cells
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotKey {
    pub scope: ScopeId,
    pub expr: ExprId,
}

// Cache entry with dependency tracking
pub struct CacheEntry {
    pub value: Value,
    pub computed_at: u64,           // Tick when computed
    pub last_changed: TickSeq,      // When value actually changed
    pub deps: SmallVec<[SlotKey; 8]>, // Dependencies for "why did X change?"
}

// Runtime state
struct Runtime {
    tick: u64,
    seq: u32,                               // Intra-tick counter
    cache: HashMap<SlotKey, CacheEntry>,
    holds: HashMap<SlotKey, HoldCell>,
    links: HashMap<SlotKey, LinkCell>,
    lists: HashMap<SlotKey, ListCell>,
}

struct HoldCell { value: Value }
struct LinkCell { bound: Option<SlotKey> }  // Bound to another slot
struct ListCell {
    keys: Vec<ItemKey>,
    alloc: AllocSite,
}

struct AllocSite { site: SourceId, next: ItemKey }
struct ScopeId(Vec<u64>);   // Path from root
```

---

## Expected Outcome

After benchmarks, we'll know:
1. Which architecture is **correct** (both must pass toggle_all)
2. Which is **faster** (toggle-all with 10/100/1000 items)
3. Which uses **less memory** (1000 items)
4. Which is **simpler** (lines of code)

**Winner gets ported to replace engine_v2.**

---

## Future Improvement: Graph IR + Host Adapters (Idea 4)

After the prototype comparison, consider adding Idea 4's layering to the winning architecture.

**What Idea 4 adds** (orthogonal to A vs B):
- **Explicit IR layer**: Parser → Graph IR → Runtime (instead of AST → Runtime directly)
- **Host abstraction**: Platform adapters for browser/CLI/FPGA with clean contracts
- **Effects as data**: `EffectRequest` enum instead of inline side effects
- **NodeSpec vs NodeState split**: Only state persisted, spec derived from code

**Benefits**:
- Same semantic graph for all platforms
- Easier hot reload (reattach state by stable id)
- Replayable debugging (effects are deterministic)
- Future FPGA/RISC-V targets

**Implementation**: Add after prototype comparison, to whichever path wins.

See `../docs/boon_3_idea_4.md` for full specification.
