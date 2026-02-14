# DD v2 Engine Architecture & Implementation Plan

## Context

The DD v1 engine (~17K lines) failed because it wrapped DD around imperative code instead of using DD's native capabilities. Lists were `Arc<Vec<Value>>` with `.to_vec()` calls (14 occurrences). No joins, no arrangements, no actual incremental computation. DD v2 is a clean rewrite that models Boon's computation natively as DD collections and operators.

**Decisions:**
- Clean rewrite, replacing `engine_dd/` (DD v1 deleted, kept as reference in git history)
- Full DD purity: everything is a DD collection (scalars = single-element collections)
- Anti-cheat from day 1: no sync state reads, type-level enforcement
- Timely 0.26 + differential-dataflow 0.19 (latest stable)
- Persistence deferred (focus on correct DD computation first)
- Full architecture design before implementation

---

## Core Principle: Everything Is a DD Collection

Every Boon value exists as a `Collection<G, Value, isize>`:

- **Scalar** `x: 42` → collection `{(42, t, +1)}`. Change to 43 → `{(42, -1), (43, +1)}`
- **List** `LIST { a, b }` → collection `{((key0, a), +1), ((key1, b), +1)}`. Append c → `{((key2, c), +1)}`
- **Event** (LINK press) → transient diff: insert at time t, retracted at t+1

---

## Boon Construct → DD Operator Mapping

| Boon | DD Operator | Key Insight |
|------|------------|-------------|
| `x: 42` | `input.insert(42)` | Scalar = single-element collection |
| `LATEST { a, b }` | `concat` + custom `hold_latest` unary | Merges sources, tracks latest value |
| `initial \|> HOLD state { body }` | Custom `hold_state` unary operator | State in closure, retract-old/insert-new |
| `event \|> THEN { body }` | `flat_map` (insertions only) | Filter diff>0, apply body |
| `input \|> WHEN { arms }` | `flat_map` with pattern matching | One-shot evaluation per change |
| `input \|> WHILE { arms }` | **DD `join`** with dependency collections | Reactive re-evaluation when deps change |
| `BLOCK { vars, output }` | Scoped collection aliases | Compile-time scoping, no runtime cost |
| `TEXT { {a} and {b} }` | **DD `join`** on referenced vars | Recompute when any dep changes |

---

## Custom DD Operators (core/operators.rs)

Only 6 custom operators — everything else is standard DD or standard library.

1. **`hold_latest`** - Single latest value from merged sources (LATEST)
2. **`hold_state`** - Stateful accumulator (HOLD)
3. **`then`** - Event-triggered map (THEN)
4. **`when`** - Frozen pattern match (WHEN)
5. **`while_reactive`** - Reactive pattern match via join (WHILE)
6. **`latest`** - For List/latest()

---

## CollectionSpec Enum

```rust
pub enum CollectionSpec {
    Literal(Value),
    Input(InputId),
    Concat(Vec<VarId>),
    HoldLatest(VarId),
    HoldState { initial: VarId, events: VarId, transform: TransformFn },
    Map { source: VarId, transform: TransformFn },
    Filter { source: VarId, predicate: PredicateFn },
    FlatMap { source: VarId, f: FlatMapFn },
    Join { left: VarId, right: VarId, combine: CombineFn },
    Count(VarId),
    Antijoin { source: VarId, remove_keys: VarId },
    Latest(VarId),
    SideEffect { source: VarId, effect: EffectFn },
}
```

---

## Module Structure

```
engine_dd/
├── mod.rs
├── core/
│   ├── mod.rs
│   ├── value.rs          (~150 lines)  Simplified Value enum
│   ├── types.rs          (~200 lines)  VarId, LinkId, ListKey, etc.
│   ├── operators.rs      (~500 lines)  Custom DD operators
│   ├── compile.rs        (~1500 lines) AST → DataflowGraph compiler
│   └── runtime.rs        (~300 lines)  Timely worker lifecycle
├── io/
│   ├── mod.rs
│   ├── events.rs         (~200 lines)  Browser events → DD input
│   ├── outputs.rs        (~300 lines)  DD diffs → Mutable/VecDiff signals
│   └── timers.rs         (~100 lines)  Timer/interval implementation
└── render/
    ├── mod.rs
    ├── bridge.rs          (~800 lines)  Value descriptors → Zoon elements
    ├── diff_adapter.rs    (~200 lines)  DD diffs → VecDiff conversion
    └── reactive.rs        (~200 lines)  Signal wrappers for scalar DD outputs
```

Estimated total: ~4,500 lines (vs 17K for v1)

---

## Incremental Implementation Phases

### Phase 1: Minimal Counter (`counter_hold.bn`)
Validates: hold_state, THEN, LINK event injection, scalar rendering

### Phase 2: Counter with LATEST (`counter.bn`)
Validates: LATEST (concat + hold_latest), Math/sum

### Phase 3: WHEN / WHILE + TEXT
Validates: TEXT interpolation (join), pattern matching, reactive switching

### Phase 4: Simple List Operations
Validates: Multi-element DD collections, list append, count, VecDiff rendering

### Phase 5: Full TodoMVC (`todo_mvc.bn`)
Validates: All constructs needed for production UI

---

## Anti-Cheat Architecture

### Module Boundary
```
core/   → NO zoon, web_sys, thread_local, RefCell, Mutex, Mutable<T>
io/     → Bridges DD ↔ browser. Only place with Mutable<T> (for Signal output)
render/ → Converts Value descriptors to Zoon elements. Reads from io/ Signals only
```

### Compile-Time Verification
`tools/verify_dd_v2.sh` checks core/ for forbidden patterns.

---

## Execution Model

```
Browser Events (clicks, keys, timers)
  → Event Queue (channel)
  → Timely Worker Loop:
      1. yield_to_browser().await
      2. drain_pending_events()
      3. inject into DD InputSession, advance epoch
      4. worker.step() until caught up
      5. extract output diffs via capture()
      6. apply diffs to DOM
```

Single-threaded via `timely::execute_directly()`. One epoch per event batch.
