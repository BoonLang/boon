# Timely and Differential Dataflow for Boon Runtime

**Date:** 2026-01-02
**Status:** Research

This document summarizes research into using Timely Dataflow and Differential Dataflow as potential replacements or enhancements for Boon's reactive runtime.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Current Boon Engine Challenges](#2-current-boon-engine-challenges)
3. [Timely Dataflow Overview](#3-timely-dataflow-overview)
4. [Differential Dataflow Overview](#4-differential-dataflow-overview)
5. [WASM and WebWorker Support](#5-wasm-and-webworker-support)
6. [Mapping Boon Constructs](#6-mapping-boon-constructs)
7. [Memory Optimizations](#7-memory-optimizations)
8. [E-Graph Optimization Techniques](#8-e-graph-optimization-techniques)
9. [Alternative: futures_signals](#9-alternative-futures_signals)
10. [Comparison Matrix](#10-comparison-matrix)
11. [Proposed Architecture](#11-proposed-architecture)
12. [Implementation Roadmap](#12-implementation-roadmap)
13. [References](#13-references)

---

## 1. Executive Summary

### Key Findings

1. **WASM support is now available** - As of July 2025, Timely and Differential Dataflow compile to WASM (PR #663 removed `std::time::Instant` dependency).

2. **WebWorker parallelism is architecturally feasible** - Timely's pluggable `Allocate` trait allows implementing custom communication backends, including SharedArrayBuffer-based WebWorker communication.

3. **Differential Dataflow provides automatic incrementality** - List operations (retain, map, count) become O(delta) instead of O(n) with minimal code changes.

4. **Memory overhead is manageable** - Recent research provides techniques to reduce DD's memory usage by 1.7x through read-friendly indices and bounded diff history.

5. **E-graph optimization can discover incremental patterns automatically** - The Hydroflow research shows how simple rewrite rules compose to find semi-naive evaluation strategies.

### Recommendation

**Start with Timely Dataflow** for parallelism foundation, **add Differential Dataflow** for List/Map operations where incrementality matters, and **apply e-graph optimizations** to minimize recomputation.

---

## 2. Current Boon Engine Challenges

### Pain Points

The current Boon engine (`engine.rs`, ~9000 lines) has complex List management:

```rust
pub struct List {
    construct_info: Arc<ConstructInfoComplete>,
    actor_loop: ActorLoop,
    change_sender_sender: NamedChannel<NamedChannel<ListChange>>,
    current_version: Arc<AtomicU64>,
    notify_sender_sender: NamedChannel<mpsc::Sender<()>>,
    diff_query_sender: NamedChannel<DiffHistoryQuery>,
    persistence_loop: Option<ActorLoop>,
}
```

Plus supporting types: `ListDiff`, `ListChange`, `ListSubscription`, `ListDiffSubscription`, `ItemId`, `ListBindingFunction`, `ListBindingConfig`.

### Specific Issues

| Issue | Description |
|-------|-------------|
| **List/retain complexity** | Re-filters entire list on every change |
| **List/map complexity** | Re-maps entire list on every change |
| **List/count** | Recomputes count on every change (O(n)) |
| **Memory management** | Diff history can grow unbounded |
| **No parallelism** | Single-threaded execution only |
| **Future Map type** | Would require similar complexity |

---

## 3. Timely Dataflow Overview

### What It Is

[Timely Dataflow](https://github.com/TimelyDataflow/timely-dataflow) is a low-latency, data-parallel dataflow computational model. It provides:

- **Data parallelism**: Operations can run on independent parts of data concurrently
- **Streaming**: Process unbounded data streams that arrive progressively
- **Expressivity**: Support for iteration and complex control flow
- **Progress tracking**: Know when all workers have processed up to timestamp T

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Timely Dataflow                                            │
├─────────────────────────────────────────────────────────────┤
│  Worker 0          Worker 1          Worker 2               │
│  ┌──────────┐      ┌──────────┐      ┌──────────┐          │
│  │ Operators│ ←──→ │ Operators│ ←──→ │ Operators│          │
│  └──────────┘      └──────────┘      └──────────┘          │
│       ↑                 ↑                 ↑                 │
│       └─────────────────┼─────────────────┘                 │
│                    Allocator                                │
│              (Thread/Process/Cluster)                       │
└─────────────────────────────────────────────────────────────┘
```

### Key Concepts

#### Workers and Parallelism

Workers operate independently through closures. The `-w N` flag spawns N workers:

```rust
timely::execute(Config::process(4), |worker| {
    worker.dataflow(|scope| {
        // Define dataflow graph
    });
});
```

#### The Allocate Trait

The communication layer is pluggable via the `Allocate` trait:

```rust
pub trait Allocate {
    fn index(&self) -> usize;           // Which worker am I?
    fn peers(&self) -> usize;           // How many workers total?
    fn allocate<T>(&mut self) -> (Vec<Push<T>>, Pull<T>);  // Create channels
}
```

Three built-in configurations:

| Config | Backend | Description |
|--------|---------|-------------|
| `Thread` | Queue | Single worker, channels become simple queues |
| `Process` | Rust channels | Multiple threads, in-process communication |
| `Cluster` | TCP + serialization | Distributed across machines |

#### Operators

Timely provides flexible operator construction:

```rust
// Map operator
let mapped = input.map(|x| x * 2);

// Stateful unary operator (perfect for HOLD)
let held = input.unary(Pipeline, "Hold", |_cap, _info| {
    let mut state = initial_value;
    move |input, output| {
        while let Some((time, data)) = input.next() {
            for item in data.iter() {
                state = transform(state, item);
            }
            output.session(&time).give(state.clone());
        }
    }
});

// Exchange for data partitioning
let partitioned = input.exchange(|item| hash(item.key));
```

### When to Use Timely Alone

| Use Case | Timely Alone |
|----------|--------------|
| Event-driven state machines | ✅ Excellent |
| HOLD semantics | ✅ Direct mapping to `unary` |
| Parallelism | ✅ Built-in |
| Simple transformations | ✅ Sufficient |
| Incremental collections | ❌ Manual implementation |
| Complex joins | ❌ Manual implementation |

---

## 4. Differential Dataflow Overview

### What It Is

[Differential Dataflow](https://github.com/TimelyDataflow/differential-dataflow) is built on Timely and adds **incremental collection semantics**. Instead of recomputing results from scratch, it propagates only deltas.

### Core Concept: Differences

Every value is associated with a (time, diff) pair:

```rust
// Instead of storing: [a, b, c]
// DD stores: [(a, t1, +1), (b, t1, +1), (c, t2, +1), (a, t3, -1)]
// At time t3, the collection is: [b, c]
```

### Key Benefits

1. **Automatic incrementality**: Operators automatically process only changes
2. **Iterative computation**: The `iterate` operator handles recursive queries
3. **Joins and aggregations**: Built-in efficient implementations
4. **Parallelism**: Inherits Timely's data-parallel execution

### Example: Incremental Count

```rust
worker.dataflow(|scope| {
    let (input, todos) = scope.new_collection::<Todo>();

    // This count is INCREMENTAL - O(1) per change, not O(n)
    let count = todos.count();

    // Filter is also incremental
    let completed = todos.filter(|t| t.completed);
    let completed_count = completed.count();

    // Map is incremental
    let titles = todos.map(|t| t.title.clone());
});
```

### Arrangements

Arrangements are DD's indexed data structures for efficient access:

```rust
// Create an arrangement indexed by key
let arranged = collection.arrange_by_key();

// Join uses arrangements efficiently
let joined = arranged.join(&other_arranged);
```

### Memory Model

DD tracks all historical changes to support:
- Retroactive updates (change something in the past)
- Consistent recomputation across time

**Trade-off**: This requires storing difference history, which can grow unbounded.

---

## 5. WASM and WebWorker Support

### WASM Support Status

**✅ Now Supported** (as of July 2025)

From [GitHub Issue #402](https://github.com/TimelyDataflow/timely-dataflow/issues/402):

> "This is now possible! As of #663 `std::time::Instant` is not required, and you can run timely and differential in WASM."

### WebWorker Allocator Design

To run Timely in WebWorkers, implement a custom `Allocate`:

```rust
pub struct WebWorkerAllocator {
    worker_index: usize,
    worker_count: usize,
    // SharedArrayBuffer-backed ring buffers
    send_buffers: Vec<SharedRingBuffer>,
    recv_buffer: SharedRingBuffer,
}

impl Allocate for WebWorkerAllocator {
    fn index(&self) -> usize { self.worker_index }
    fn peers(&self) -> usize { self.worker_count }

    fn allocate<T: Bytesable>(&mut self) -> (Vec<Push<T>>, Pull<T>) {
        // Create push endpoints that write to SharedArrayBuffer
        // Create pull endpoint that reads from SharedArrayBuffer
    }
}
```

### Browser Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  Browser Main Thread                                         │
│  ┌────────────────────────────────────────────────────────┐  │
│  │ Boon UI Coordinator                                    │  │
│  │ - DOM updates only                                     │  │
│  │ - Event dispatch to workers                            │  │
│  └────────────────────────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────────┤
│  WebWorker Pool (SharedArrayBuffer communication)            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                   │
│  │ Timely   │  │ Timely   │  │ Timely   │                   │
│  │ Worker 0 │←→│ Worker 1 │←→│ Worker 2 │                   │
│  │ (WASM)   │  │ (WASM)   │  │ (WASM)   │                   │
│  └──────────┘  └──────────┘  └──────────┘                   │
│       ↑              ↑              ↑                        │
│       └──────────────┼──────────────┘                        │
│            SharedArrayBuffer rings                           │
└──────────────────────────────────────────────────────────────┘
```

### Requirements

1. **Cross-Origin Isolation**: Headers `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp`
2. **SharedArrayBuffer**: For zero-copy inter-worker communication
3. **No main thread blocking**: All Timely work in WebWorkers

### Helpful Crates

- [`wasm_thread`](https://lib.rs/crates/wasm_thread): std::thread replacement for WASM
- [`wasm-mt`](https://docs.rs/wasm-mt): Ergonomic multithreading without SharedArrayBuffer
- [`wasmworker`](https://docs.rs/wasmworker): Worker pool with `par_map` for iterators

---

## 6. Mapping Boon Constructs

### LATEST

```boon
value: LATEST {
    input_a
    input_b
    input_c
}
```

**Timely mapping**: Concatenate streams

```rust
let latest = input_a.concat(&input_b).concat(&input_c);
```

**Differential mapping**: Same, with automatic delta tracking

```rust
let latest = input_a.concat(&input_b).concat(&input_c);
// Changes to any input propagate as deltas
```

### HOLD

```boon
completed: False |> HOLD state {
    checkbox.event.click |> THEN { state |> Bool/not() }
}
```

**Timely mapping**: Stateful unary operator

```rust
let held = events.unary(Pipeline, "Hold", |_| {
    let mut state = false;
    move |input, output| {
        while let Some((time, data)) = input.next() {
            for _click in data.iter() {
                state = !state;
            }
            output.session(&time).give(state);
        }
    }
});
```

**Differential mapping**: Use `iterate` for recursive state

```rust
let held = scope.iterative::<u64, _, _>(|subscope| {
    let events = events_input.enter(subscope);
    let (handle, cycle) = subscope.feedback(1);

    let updated = cycle
        .concat(&initial.enter(subscope))
        .join(&events)
        .map(|(state, _event)| !state);

    updated.connect_loop(handle);
    updated.leave()
});
```

### WHEN / WHILE

```boon
value |> WHEN {
    pattern1 => result1
    pattern2 => result2
    __ => default
}
```

**Timely/DD mapping**: Filter + map chain

```rust
let matched = value
    .filter(|v| matches!(v, pattern1))
    .map(|v| transform_to_result1(v));
```

### List/append

```boon
todos |> List/append(item: new_todo)
```

**Differential mapping**: Insert into collection

```rust
input_handle.insert(new_todo);
input_handle.advance_to(next_time);
input_handle.flush();
```

### List/remove

```boon
todos |> List/remove(item, on: item.remove_button.event.press)
```

**Differential mapping**: Retract from collection

```rust
// On remove event:
input_handle.remove(todo_to_remove);
input_handle.advance_to(next_time);
input_handle.flush();
```

### List/retain

```boon
todos |> List/retain(item, if: item.completed)
```

**Differential mapping**: Filter (incremental!)

```rust
let retained = todos.filter(|item| item.completed);
// Only processes CHANGED items, not entire list
```

### List/map

```boon
todos |> List/map(item, new: render_item(item))
```

**Differential mapping**: Map (incremental!)

```rust
let mapped = todos.map(|item| render_item(item));
// Only processes CHANGED items
```

### List/count

```boon
todos_count: todos |> List/count()
```

**Differential mapping**: Count (incremental!)

```rust
let count = todos.count();
// O(1) per change, not O(n)
```

### Summary Table

| Boon Construct | Timely | Differential | Incrementality |
|----------------|--------|--------------|----------------|
| LATEST | `concat` | `concat` | N/A |
| HOLD | `unary` with state | `iterate` | Automatic |
| WHEN/WHILE | `filter` + `map` | `filter` + `map` | Automatic |
| List literal | Input stream | `new_collection` | N/A |
| List/append | Stream concat | `insert` | Automatic |
| List/remove | Stream filter | `remove` (weight -1) | Automatic |
| List/retain | `filter` (O(n)) | `filter` (O(delta)) | ✅ |
| List/map | `map` (O(n)) | `map` (O(delta)) | ✅ |
| List/count | Manual (O(n)) | `count` (O(1)) | ✅ |

---

## 7. Memory Optimizations

### The Problem

Differential Dataflow keeps full history for time-travel capabilities:

> "Differential dataflow carefully preserves all historical data to support arbitrary input modifications, including deletions."

This can cause:
- Unbounded memory growth
- Latency degradation (47 seconds in one test case)

### Research: Optimizing DD for Large-Scale Graph Processing

From [GRADES-NDA 2024 paper](https://dl.acm.org/doi/10.1145/3661304.3661900):

#### Optimization 1: Read-Friendly Indices

DD's default indices are write-optimized. The paper redesigns them for read-heavy workloads (like UI rendering):

```rust
// Before: Write-optimized (append-only log)
struct WriteOptimizedIndex {
    log: Vec<(Time, Key, Value, Diff)>,
}

// After: Read-optimized (sorted by key)
struct ReadFriendlyIndex {
    by_key: BTreeMap<Key, Vec<(Time, Value, Diff)>>,
}
```

**Result**: Up to 19x runtime improvement for read-heavy workloads.

#### Optimization 2: Fast Empty Difference Verification

Before running DD's full logic, check if there are any changes:

```rust
fn process_tick(&mut self, input: &Input) {
    // Fast path: no changes?
    if input.is_unchanged_since(self.last_processed) {
        return;  // Skip entirely!
    }

    // Slow path: process changes
    self.run_differential_logic(input);
}
```

**Impact**: Most UI ticks have no changes → near-zero work.

#### Optimization 3: Drop Differences + Recompute

Instead of keeping full history, drop old diffs and recompute when needed:

```rust
struct BoundedArrangement<K, V> {
    // Current snapshot (always available)
    current: BTreeMap<K, V>,

    // Recent diffs only (bounded)
    recent_diffs: VecDeque<(Time, K, V, Diff)>,
    max_diffs: usize,  // e.g., 100

    // Checkpoint for recovery
    last_checkpoint: Time,
}

impl BoundedArrangement {
    fn maybe_drop_old_diffs(&mut self) {
        while self.recent_diffs.len() > self.max_diffs {
            self.recent_diffs.pop_front();
        }
    }

    fn get_diffs_since(&self, time: Time) -> Option<Vec<Diff>> {
        // Fast path: diffs available
        if self.has_diffs_since(time) {
            return Some(self.collect_diffs_since(time));
        }
        // Slow path: recompute from snapshot
        None  // Caller must use snapshot instead
    }
}
```

**Result**: 1.7x memory reduction.

### Self-Compacting Dataflows

From [Materialize blog](https://materialize.com/blog/managing-memory-with-differential-dataflow/):

Use feedback loops to automatically retract old data:

```rust
let (handle, cycle) = scope.feedback(delay);

let compacted = input
    .concat(&cycle.negate())  // Retract items from cycle
    .consolidate();           // Merge updates

// Identify items to retract (e.g., not in output)
let to_retract = compute_retractions(&compacted);
to_retract.connect_loop(handle);
```

**Tuning the delay parameter**:
- Tight delays (1ms): Smaller working set, lower latency (~2ms)
- Large delays (1s): Better batching, but more memory

### For Boon: Practical Memory Strategy

```rust
struct BoonList<T> {
    // Current state (always accurate)
    current: Vec<T>,

    // Bounded diff history for subscribers
    recent_diffs: VecDeque<VecDiff<T>>,
    max_diffs: usize,

    // Version for fast empty-diff check
    version: AtomicU64,
}

impl BoonList<T> {
    fn subscribe(&self) -> Subscription<T> {
        Subscription {
            list: self.clone(),
            last_version: self.version.load(Ordering::Relaxed),
        }
    }
}

impl Subscription<T> {
    async fn next(&mut self) -> SubscriptionUpdate<T> {
        let current = self.list.version.load(Ordering::Relaxed);

        // Fast empty-diff check
        if current == self.last_version {
            return SubscriptionUpdate::NoChange;
        }

        // Try to get diffs
        if let Some(diffs) = self.list.get_diffs_since(self.last_version) {
            self.last_version = current;
            return SubscriptionUpdate::Diffs(diffs);
        }

        // Fallback to snapshot
        self.last_version = current;
        SubscriptionUpdate::Snapshot(self.list.current.clone())
    }
}
```

---

## 8. E-Graph Optimization Techniques

### The Research

From [Optimizing Stateful Dataflow with Local Rewrites](https://arxiv.org/html/2306.10585) (EGRAPHS 2023):

Traditional optimizers use specialized passes with complex correctness proofs. E-graphs enable composable local rewrite rules that automatically discover optimizations.

### Key Operators

The paper introduces temporal operators for reasoning about state:

| Operator | Semantics | Description |
|----------|-----------|-------------|
| `persist` | Emit entire history up to current tick | All values ever received |
| `delta` | Emit only new values this tick | Just the changes |
| `prev` | Emit history excluding current tick | Previous state |

### Fundamental Rewrite Rules

```
// Inverse relationships
persist(delta(x)) ≡ x
delta(persist(x)) ≡ x

// Recursive definition
persist(x) ≡ x ∪ prev(persist(x))

// Delta extraction
delta(x ∪ y) ≡ delta(x) ∪ delta(y)
```

### Automatic Semi-Naive Discovery

By composing these simple rules, the optimizer discovers semi-naive evaluation:

```
// Before (naive): Recompute entire join each tick
output = join(persist(a), persist(b))

// After (semi-naive): Only join NEW data
output = prev(output) ∪
         join(delta(a), prev(persist(b))) ∪
         join(persist(a), delta(b))
```

### Application to Boon

#### HOLD Optimization

```boon
-- Original
completed: False |> HOLD state {
    LATEST { checkbox.click, toggle_all.click }
    |> THEN { state |> Bool/not() }
}
```

E-graph rewrites detect:
- Most ticks: no events → skip body entirely
- On event: only process that specific event

```
// Rewrite rule for HOLD
hold(init, body) ≡ latest(prev(hold(init, body)), delta(body))

// When delta(body) is empty (no events):
hold(init, body) ≡ prev(hold(init, body))  // Just return previous state
```

#### List/retain Optimization

```boon
-- Original (re-filters entire list)
todos |> List/retain(item, if: item.completed)
```

E-graph discovers incremental filter:

```
// Rewrite rule
retain(persist(list), pred) ≡
    prev(retain(persist(list), pred)) ∪
    retain(delta(list), pred) -
    removed_items
```

#### Implementation with egg

Using the [egg](https://egraphs-good.github.io/) crate:

```rust
use egg::{rewrite, define_language, EGraph, Runner};

define_language! {
    enum BoonExpr {
        // Temporal operators
        "persist" = Persist([Id; 1]),
        "delta" = Delta([Id; 1]),
        "prev" = Prev([Id; 1]),

        // Boon operators
        "hold" = Hold([Id; 2]),
        "latest" = Latest(Vec<Id>),
        "list-retain" = ListRetain([Id; 2]),
        "list-map" = ListMap([Id; 2]),
        "list-count" = ListCount([Id; 1]),

        // Combinators
        "union" = Union([Id; 2]),
        "diff" = Diff([Id; 2]),
    }
}

let rules: Vec<Rewrite<BoonExpr, ()>> = vec![
    // Fundamental temporal rules
    rewrite!("persist-delta"; "(persist (delta ?x))" => "?x"),
    rewrite!("delta-persist"; "(delta (persist ?x))" => "?x"),

    // HOLD incrementalization
    rewrite!("hold-incremental";
        "(hold ?init ?body)" =>
        "(latest (prev (hold ?init ?body)) (delta ?body))"
    ),

    // Filter incrementalization
    rewrite!("retain-incremental";
        "(list-retain (persist ?list) ?pred)" =>
        "(union (prev (list-retain (persist ?list) ?pred))
                (list-retain (delta ?list) ?pred))"
    ),

    // Count incrementalization
    rewrite!("count-incremental";
        "(list-count (persist ?list))" =>
        "(+ (prev (list-count (persist ?list)))
            (list-count (delta ?list)))"
    ),

    // Fast empty-delta detection
    rewrite!("empty-delta-skip";
        "(list-retain (delta empty) ?pred)" => "empty"
    ),
];

fn optimize(expr: &BoonExpr) -> BoonExpr {
    let runner = Runner::default()
        .with_expr(expr)
        .run(&rules);
    runner.extract_best()
}
```

---

## 9. Alternative: futures_signals

### Overview

[futures_signals](https://github.com/Pauan/rust-signals) provides reactive primitives with built-in collection support:

- `Mutable<T>`: Thread-safe container with change notifications
- `MutableVec<T>`: Reactive vector with `VecDiff` deltas
- `MutableBTreeMap<K, V>`: Reactive map with `MapDiff` deltas

### VecDiff Semantics

```rust
pub enum VecDiff<A> {
    Replace { values: Vec<A> },
    InsertAt { index: usize, value: A },
    UpdateAt { index: usize, value: A },
    RemoveAt { index: usize },
    Move { old_index: usize, new_index: usize },
    Push { value: A },
    Pop {},
    Clear {},
}
```

### Key Characteristics

| Feature | futures_signals | Differential Dataflow |
|---------|-----------------|----------------------|
| WASM support | ✅ Native | ✅ (as of 2025) |
| VecDiff for lists | ✅ Built-in | ✅ Via collections |
| MapDiff for maps | ✅ MutableBTreeMap | ✅ Via arrangements |
| Parallelism | ❌ No | ✅ Built-in |
| Incremental counts | ❌ Manual | ✅ Automatic |
| Memory overhead | 🟢 Low | 🟡 Medium-high |
| Complexity | 🟢 Simple | 🔴 Complex |

### Lossless Guarantees

Unlike `Signal` (which is lossy - skips intermediate values), `SignalVec` guarantees:
- No changes are skipped
- Order is maintained
- Perfect for applying diffs to external state (like DOM)

### Mapping to Boon

| Boon Operation | futures_signals |
|----------------|-----------------|
| `LIST { ... }` | `MutableVec::new_with_values(...)` |
| `List/append(item:)` | `vec.lock_mut().push(value)` |
| `List/remove(...)` | `vec.lock_mut().remove(index)` |
| `List/clear(...)` | `vec.lock_mut().clear()` |
| Subscribe to changes | `vec.signal_vec_cloned()` |

### When to Choose futures_signals

Choose **futures_signals** when:
- Lists are small (< 1000 items)
- No parallelism needed
- Minimal WASM size required
- Simple reactive patterns sufficient

Choose **Differential Dataflow** when:
- Lists are large (1000+ items)
- Incremental aggregations matter (count, reduce)
- Complex joins between collections
- Parallelism desired

---

## 10. Comparison Matrix

### Feature Comparison

| Feature | Timely | Differential | futures_signals |
|---------|--------|--------------|-----------------|
| **Core Model** | Dataflow | Incremental collections | Reactive signals |
| **WASM** | ✅ | ✅ | ✅ |
| **WebWorker parallelism** | 🟡 Implement allocator | 🟡 Via Timely | ❌ |
| **VecDiff** | ❌ Manual | ✅ Automatic | ✅ Built-in |
| **Incremental filter** | ❌ | ✅ | ❌ |
| **Incremental map** | ❌ | ✅ | ❌ |
| **Incremental count** | ❌ | ✅ | ❌ |
| **Joins** | 🟡 Manual | ✅ Built-in | ❌ |
| **Memory model** | Low | High (history) | Low |
| **Complexity** | Medium | High | Low |

### WASM Size Estimates

| Component | Estimated Size |
|-----------|----------------|
| Timely core | ~200-400 KB |
| Differential Dataflow | ~100-200 KB |
| futures_signals | ~50-100 KB |
| Current Boon engine | ~150+ KB |

### Mapping HOLD

| Approach | How | Complexity |
|----------|-----|------------|
| Timely | `unary` operator with state | Low |
| Differential | `iterate` with feedback | High |
| futures_signals | `Mutable<T>` + signals | Low |

### todo_mvc Analysis

Based on `/playground/frontend/src/examples/todo_mvc/todo_mvc.bn`:

| Operation | Count | DD Benefit |
|-----------|-------|------------|
| `List/append` | 1 | Minimal |
| `List/remove` | 2 | Automatic retraction |
| `List/retain` | 2 | Incremental filter |
| `List/map` | 1 | Incremental transform |
| `List/count` | 2 | O(1) per change |
| `List/is_empty` | 2 | O(1) check |
| HOLD states | 3 per todo | Timely better |

**Verdict for todo_mvc**:
- Small lists → futures_signals sufficient
- HOLD semantics → Timely maps well
- Incremental counts → DD would help at scale

---

## 11. Proposed Architecture

### Hybrid Approach

```
┌─────────────────────────────────────────────────────────────────┐
│  Boon Language                                                  │
│  LATEST, HOLD, WHEN, WHILE, LINK, List/*, Map/*                 │
├─────────────────────────────────────────────────────────────────┤
│  E-Graph Optimizer                                              │
│  - Rewrite rules for persist/delta/prev                         │
│  - Automatic semi-naive discovery                               │
│  - Fast empty-diff detection                                    │
├─────────────────────────────────────────────────────────────────┤
│  Optimized Dataflow IR                                          │
│  - Explicit delta operators                                     │
│  - Bounded diff history                                         │
│  - Read-friendly indices                                        │
├─────────────────────────────────────────────────────────────────┤
│  Reactive Core (futures_signals or DD)                          │
│  - MutableVec for small lists                                   │
│  - DD Collection for large lists / complex ops                  │
├─────────────────────────────────────────────────────────────────┤
│  Parallelism Layer (Timely)                                     │
│  - WebWorker allocator                                          │
│  - Data partitioning                                            │
│  - Progress tracking                                            │
├─────────────────────────────────────────────────────────────────┤
│  Platform Backends                                              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │ Browser     │  │ Native      │  │ Hardware    │              │
│  │ (WASM+WW)   │  │ (Threads)   │  │ (FSM gen)   │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
└─────────────────────────────────────────────────────────────────┘
```

### Decision Points

```
┌─────────────────────────────────────────┐
│ Is parallelism needed?                  │
└─────────────────┬───────────────────────┘
                  │
        ┌─────────┴─────────┐
        │ No                │ Yes
        ▼                   ▼
┌───────────────┐   ┌───────────────────┐
│ Small lists?  │   │ Use Timely        │
└───────┬───────┘   │ WebWorker alloc   │
        │           └─────────┬─────────┘
  ┌─────┴─────┐               │
  │Yes    │No │               ▼
  ▼       ▼   │       ┌───────────────────┐
┌─────┐ ┌─────┴─────┐ │ Complex joins/    │
│f_sig│ │    DD     │ │ aggregations?     │
└─────┘ └───────────┘ └─────────┬─────────┘
                          ┌─────┴─────┐
                          │Yes    │No │
                          ▼       ▼   │
                    ┌─────────┐ ┌─────┴─────┐
                    │  Full   │ │  Timely   │
                    │   DD    │ │  + f_sig  │
                    └─────────┘ └───────────┘
```

---

## 12. Implementation Roadmap

### Phase 1: futures_signals Integration (2-4 weeks)

Replace current List implementation with futures_signals:

- [ ] Add futures_signals dependency
- [ ] Create `BoonMutableVec` wrapper
- [ ] Implement List/append, List/remove, List/clear
- [ ] Implement List/retain with filter signal
- [ ] Implement List/map with transform signal
- [ ] Implement List/count (manual, not incremental)
- [ ] Update bridge.rs for VecDiff rendering
- [ ] Test with todo_mvc example

**Expected reduction**: ~1000-2000 lines from engine.rs

### Phase 2: Timely WebWorker Allocator (2-4 weeks)

Enable parallelism for browser:

- [ ] Implement `WebWorkerAllocator` trait
- [ ] SharedArrayBuffer ring buffers
- [ ] Worker discovery protocol
- [ ] Serialization with Abomonation or Serde
- [ ] Test with simple parallel map

### Phase 3: E-Graph Optimizer (4-6 weeks)

Add automatic optimization:

- [ ] Add egg dependency
- [ ] Define BoonExpr language
- [ ] Implement temporal operators (persist, delta, prev)
- [ ] Define core rewrite rules
- [ ] Add fast empty-diff detection
- [ ] Integrate with Boon compiler

### Phase 4: Selective DD Integration (4-6 weeks)

Add Differential Dataflow for complex operations:

- [ ] Add differential-dataflow dependency
- [ ] Create DD-backed BoonCollection
- [ ] Implement incremental List/count
- [ ] Implement incremental List/reduce
- [ ] Apply memory optimizations (bounded history)
- [ ] Benchmark vs futures_signals

### Phase 5: Full DD + Parallelism (6-8 weeks)

Complete integration:

- [ ] DD running on WebWorkers via Timely
- [ ] Partitioned List operations
- [ ] Join operations (if needed for future Map type)
- [ ] Performance tuning

---

## 13. References

### Core Systems

- [Timely Dataflow GitHub](https://github.com/TimelyDataflow/timely-dataflow)
- [Timely Dataflow Book](https://timelydataflow.github.io/timely-dataflow/)
- [Differential Dataflow GitHub](https://github.com/TimelyDataflow/differential-dataflow)
- [Differential Dataflow Book](https://timelydataflow.github.io/differential-dataflow/)
- [futures_signals GitHub](https://github.com/Pauan/rust-signals)
- [futures_signals Tutorial](https://docs.rs/futures-signals/latest/futures_signals/tutorial/index.html)

### Research Papers

- [Optimizing Stateful Dataflow with Local Rewrites](https://arxiv.org/html/2306.10585) - EGRAPHS 2023
- [Optimizing Differential Computation for Large-Scale Graph Processing](https://dl.acm.org/doi/10.1145/3661304.3661900) - GRADES-NDA 2024
- [Naiad: A Timely Dataflow System](https://dl.acm.org/doi/10.1145/2517349.2522738) - SOSP 2013
- [Differential Dataflow](http://www.cidrdb.org/cidr2013/Papers/CIDR13_Paper111.pdf) - CIDR 2013

### Related Systems

- [Hydro Project](https://hydro.run/)
- [Materialize](https://materialize.com/)
- [Feldera](https://www.feldera.com/) (DBSP-based)
- [egg: E-Graphs Good](https://egraphs-good.github.io/)

### Memory Management

- [Managing Memory with Differential Dataflow](https://materialize.com/blog/managing-memory-with-differential-dataflow/)
- [Arrangements in Materialize](https://materialize.com/docs/get-started/arrangements/)

### WASM and Parallelism

- [Timely WASM Support Issue #402](https://github.com/TimelyDataflow/timely-dataflow/issues/402) (Resolved)
- [Timely Communication Crate](https://docs.rs/timely_communication/latest/timely_communication/)
- [WASM Threads and Messages](https://www.tweag.io/blog/2022-11-24-wasm-threads-and-messages/)
- [wasm_thread crate](https://lib.rs/crates/wasm_thread)

---

## Appendix: Code Examples

### A. Timely HOLD Implementation

```rust
use timely::dataflow::operators::*;

fn hold_operator<G, D, F>(
    scope: &G,
    initial: D,
    events: Stream<G, ()>,
    transform: F,
) -> Stream<G, D>
where
    G: Scope,
    D: Data + Clone,
    F: Fn(D) -> D + 'static,
{
    events.unary(Pipeline, "Hold", move |_cap, _info| {
        let mut state = initial;
        move |input, output| {
            while let Some((time, data)) = input.next() {
                for _event in data.iter() {
                    state = transform(state.clone());
                }
                output.session(&time).give(state.clone());
            }
        }
    })
}
```

### B. futures_signals List Wrapper

```rust
use futures_signals::signal_vec::{MutableVec, SignalVec, VecDiff};

pub struct BoonList<T: Clone + 'static> {
    inner: MutableVec<T>,
}

impl<T: Clone + 'static> BoonList<T> {
    pub fn new() -> Self {
        Self { inner: MutableVec::new() }
    }

    pub fn append(&self, item: T) {
        self.inner.lock_mut().push_cloned(item);
    }

    pub fn remove_at(&self, index: usize) {
        self.inner.lock_mut().remove(index);
    }

    pub fn retain<F>(&self, predicate: F) -> impl SignalVec<Item = T>
    where
        F: Fn(&T) -> bool + 'static,
    {
        self.inner.signal_vec_cloned()
            .filter(move |item| predicate(item))
    }

    pub fn map<U, F>(&self, transform: F) -> impl SignalVec<Item = U>
    where
        U: Clone + 'static,
        F: Fn(T) -> U + 'static,
    {
        self.inner.signal_vec_cloned()
            .map(move |item| transform(item))
    }

    pub fn subscribe(&self) -> impl SignalVec<Item = T> {
        self.inner.signal_vec_cloned()
    }
}
```

### C. WebWorker Allocator Sketch

```rust
use timely_communication::{Allocate, Push, Pull};

pub struct WebWorkerAllocator {
    index: usize,
    peers: usize,
    send_buffers: Vec<SharedRingBuffer>,
    recv_buffer: SharedRingBuffer,
}

impl Allocate for WebWorkerAllocator {
    fn index(&self) -> usize { self.index }
    fn peers(&self) -> usize { self.peers }

    fn allocate<T: Bytesable>(&mut self) -> (Vec<Box<dyn Push<T>>>, Box<dyn Pull<T>>) {
        let pushers: Vec<Box<dyn Push<T>>> = (0..self.peers)
            .map(|i| {
                if i == self.index {
                    Box::new(LocalPush::new()) as Box<dyn Push<T>>
                } else {
                    Box::new(SharedBufferPush::new(&self.send_buffers[i])) as Box<dyn Push<T>>
                }
            })
            .collect();

        let puller = Box::new(SharedBufferPull::new(&self.recv_buffer));

        (pushers, puller)
    }
}
```
