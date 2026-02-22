# Actors Engine Optimizations for Single-Threaded Browser WASM

This document describes potential optimizations to make the Actors engine competitive with the WASM engine for browser UI workloads, without abandoning the actor abstraction.

## Current Bottlenecks

The Actors engine treats every expression as an independent async task with channel-based communication:

- **Per-actor overhead**: ~300+ bytes (struct) + 5 MPSC channels (capacity 16-64 each) + 1 async task
- **Per-event propagation**: hundreds of `Future::poll()` cycles cascading through `select!` loops
- **TodoMVC with 20 todos**: ~100 spawned tasks, ~500 channels, hundreds of KB for channel ring buffers

The WASM engine does the same logical work with flat `f64` cells and a single synchronous `emit_downstream_updates` loop.

---

## Optimization 1: Arena-Allocated Nodes (replace heap-scattered actors)

**Problem**: Each `ValueActor` is a separate heap allocation with `Arc` reference counting. Traversing the graph means chasing pointers across scattered memory, destroying cache locality.

**Solution**: Store all actor state in a contiguous `Vec<Node>` (generational arena / slotmap). Actor references become `(index: u32, generation: u32)` pairs instead of `Arc<ValueActor>`.

```rust
struct NodeArena {
    nodes: Vec<Node>,
    generations: Vec<u32>,
}

struct NodeId {
    index: u32,
    generation: u32,
}

struct Node {
    value: Value,
    height: u16,               // for propagation ordering
    flags: NodeFlags,           // dirty, maybe_dirty, clean
    subscribers: SmallVec<[NodeId; 4]>,
    sources: SmallVec<[NodeId; 2]>,
    compute: ComputeFn,         // how to recompute this node
}
```

**Impact**:
- Per-node memory: ~64-128 bytes (vs ~300+ bytes + channel allocations)
- Cache-friendly linear traversal during propagation
- O(1) node access by index (vs Arc deref + potential cache miss)
- No atomic reference counting overhead

**Prior art**: Leptos `reactive_graph` uses slotmap internally. Jane Street Incremental uses flat arrays. Rust crates: `slotmap`, `generational-arena`.

---

## Optimization 2: Synchronous Propagation Loop (replace async task cascade)

**Problem**: Each event triggers a cascade of async wakes: task A sends on channel -> wakes task B -> B polls `select!` with 5 branches -> sends to C -> wakes C -> etc. Hundreds of poll cycles per click.

**Solution**: Replace with a single synchronous `propagate()` function that walks dirty nodes in height order.

```rust
fn propagate(arena: &mut NodeArena, dirty_queue: &mut HeightQueue) {
    while let Some(node_id) = dirty_queue.pop_lowest() {
        let node = &arena[node_id];
        let old_value = node.value.clone();
        let new_value = (node.compute)(arena, node_id);

        // Cutoff: stop propagation if value unchanged
        if new_value == old_value {
            continue;
        }

        arena[node_id].value = new_value;

        // Mark subscribers dirty, add to queue
        for &sub_id in &arena[node_id].subscribers {
            arena[sub_id].flags.set_dirty();
            dirty_queue.push(sub_id, arena[sub_id].height);
        }
    }
}
```

**Height queue**: Array-of-lists indexed by node height (typically ~20-50 distinct heights for UI graphs). O(1) push and pop-lowest, no heap needed.

```rust
struct HeightQueue {
    buckets: Vec<Vec<NodeId>>,  // buckets[height] = list of dirty nodes at that height
    min_height: usize,
}
```

**Impact**:
- One function call per event instead of hundreds of poll cycles
- No `Future::poll()`, no `select!`, no waker registration
- Deterministic execution order (height-based = glitch-free by construction)
- All work happens in a single call stack (excellent instruction cache locality)

**Prior art**: Jane Street Incremental's height-indexed recompute heap (~30ns per node). SolidJS/Preact/Angular all use single-pass synchronous propagation.

---

## Optimization 3: Eliminate Channels (direct subscriber notification)

**Problem**: 5 MPSC channels per actor (message, subscription, direct store, query, ready signal), each with a ring buffer of capacity 16-64. For 100 actors: 500 channels with hundreds of KB of buffer memory. Every send involves atomic operations; every receive involves polling.

**Solution**: Replace channels with direct function calls on arena nodes. A "message" becomes a write to the node's value slot + marking subscribers dirty.

```rust
// Before (channel-based):
actor.message_sender.send(ActorMessage::StoreValue(value)).await;
// ... async wake ... poll select! ... find ready branch ... process ...

// After (direct):
arena[node_id].value = value;
arena[node_id].flags.set_dirty();
propagation_queue.push(node_id, arena[node_id].height);
```

For the cases where channels currently serve specific roles:

| Current Channel | Replacement |
|----------------|-------------|
| `message_sender` | Direct write to `arena[id].value` |
| `subscription_sender` | Append to `arena[id].subscribers` list |
| `direct_store_sender` | Direct write to `arena[id].value` |
| `stored_value_query_sender` | Direct read from `arena[id].value` |
| `ready_signal` | Node flags (initialized/ready bit) |

**Impact**:
- Zero channel allocations
- Zero atomic operations for local communication
- Zero poll overhead (no wakers, no ready queue)
- Memory savings: hundreds of KB of channel buffers eliminated

**Prior art**: Stakker (Rust) replaces channels with `FnOnce` closures on a flat buffer, which the compiler inlines to direct function calls.

---

## Optimization 4: Push-Pull Hybrid (lazy recomputation with cutoff)

**Problem**: The eager `ValueActor` continuously polls its source stream regardless of whether anyone is reading the value. `LazyValueActor` adds demand-driven evaluation but through request/response channels, adding round-trip overhead.

**Solution**: Two-phase push-pull propagation (used by Preact Signals, Angular Signals, Leptos):

**Phase 1 — Push invalidation (cheap):**
When a source changes, walk the subscriber graph and flip dirty/maybe-dirty flags. No recomputation, no side effects. O(edges).

```rust
fn mark_dirty(arena: &mut NodeArena, node_id: NodeId) {
    if arena[node_id].flags.is_dirty() { return; }  // already marked
    arena[node_id].flags.set_maybe_dirty();
    for &sub_id in &arena[node_id].subscribers {
        mark_dirty(arena, sub_id);
    }
}
```

**Phase 2 — Pull recomputation (lazy):**
When the renderer (bridge) reads a node's value, walk UP the source chain to find the first actually-dirty ancestor, recompute top-down, cache results.

```rust
fn get_value(arena: &mut NodeArena, node_id: NodeId) -> &Value {
    if arena[node_id].flags.is_maybe_dirty() {
        // Walk up to sources, ensure they're clean
        for &src_id in &arena[node_id].sources {
            get_value(arena, src_id);  // recursive pull
        }
        // Now recompute if any source actually changed
        if arena[node_id].flags.is_dirty() {
            let new_value = (arena[node_id].compute)(arena, node_id);
            let changed = new_value != arena[node_id].value;
            arena[node_id].value = new_value;
            if !changed {
                // Cutoff: value unchanged, mark clean, stop propagation
                arena[node_id].flags.set_clean();
            }
        }
        arena[node_id].flags.set_clean();
    }
    &arena[node_id].value
}
```

**Impact**:
- Unobserved subgraphs (e.g., hidden UI sections, off-screen list items) never recompute
- Cutoff eliminates redundant propagation (if `count + 0` still equals the old value, its subscribers don't fire)
- No wasted work on intermediate nodes that don't affect the final output
- Replaces both eager `ValueActor` and lazy `LazyValueActor` with a single unified mechanism

**Prior art**: Reactively algorithm (used by Leptos), Preact Signals (version counting + 4-step cache validation), Angular Signals (producer/consumer versioning).

---

## Optimization 5: Batched Event Processing

**Problem**: Each DOM event currently triggers a full propagation cascade independently. If a single user action generates multiple events (e.g., blur + change + focus), each creates a separate wave of task wakes.

**Solution**: Collect events in a microtask-aligned batch, then run a single propagation pass.

```rust
fn handle_events(arena: &mut NodeArena, events: &[Event]) {
    // Phase 1: Apply all source changes
    for event in events {
        let node_id = event.target_node;
        arena[node_id].value = event.value.clone();
        mark_dirty(arena, node_id);
    }

    // Phase 2: Single propagation pass for ALL changes
    propagate(arena, &mut dirty_queue);

    // Phase 3: Single DOM update pass
    update_dom(arena);
}
```

**Impact**:
- Multiple source changes -> single propagation pass
- Each node fires at most once per batch (not once per event)
- DOM updates are batched (fewer reflows/repaints)
- Solves the diamond problem by construction (both inputs updated before dependents fire)

**Prior art**: SolidJS `batch()`, Preact `batch()`, MobX `runInAction()`, Timely Dataflow epoch-based batching.

---

## Optimization 6: Compile-Time Graph Optimization (actor fusion)

**Problem**: A chain like `Variable -> Add(1) -> Multiply(2) -> Display` creates 4 separate actors with 4 tasks and 12+ channels. Each step requires scheduling, polling, and channel communication.

**Solution**: The Boon compiler (which already has the AST) can fuse straight-line chains into single compute functions at lower-time.

```rust
// Before: 4 nodes, each scheduled independently
// Node 1: read variable
// Node 2: add 1
// Node 3: multiply by 2
// Node 4: display

// After fusion: 1 node with a fused compute function
// Node 1 (fused): |arena, id| {
//     let x = arena[source_id].value;
//     (x + 1) * 2
// }
```

**Fusible patterns**:
- Chains of pure transformations (map/add/multiply/compare)
- Constant folding (expressions with no reactive inputs)
- Static WHEN/WHILE branches with known patterns

**Non-fusible** (must remain separate nodes):
- Nodes with multiple subscribers (fork points in the graph)
- HOLD state (needs to observe intermediate values)
- Dynamic graph restructuring (conditional branches that change topology)
- Side-effecting nodes (DOM updates, network calls)

**Impact**:
- Fewer nodes = fewer propagation steps
- Eliminated intermediate allocations and value copies
- Better instruction cache utilization (one function vs many small poll functions)
- 30% wall time improvement, 53% memory reduction (per MLIR actor fusion paper, IEEE 2022)

**Prior art**: MLIR co-optimization of dataflow graphs and actors. SDF static scheduling. XLA/TVM operator fusion in ML compilers.

---

## Optimization 7: Pre-Computed Heights and Static Scheduling

**Problem**: In a dynamic graph, heights must be recomputed when the topology changes (e.g., WHEN/WHILE branches). Currently the Actors engine has no height concept at all — propagation order is determined by the async runtime's ready queue, which is essentially random.

**Solution**: Assign heights at lower-time for the static portion of the graph. Only recompute heights for dynamically added subgraphs.

```rust
// During IR lowering / program init:
fn assign_heights(arena: &mut NodeArena) {
    // Source nodes (inputs, constants) get height 0
    for id in arena.source_nodes() {
        arena[id].height = 0;
    }
    // BFS: each node's height = max(source heights) + 1
    let mut queue: VecDeque<NodeId> = arena.source_nodes().collect();
    while let Some(id) = queue.pop_front() {
        for &sub_id in &arena[id].subscribers {
            let new_height = arena[id].height + 1;
            if new_height > arena[sub_id].height {
                arena[sub_id].height = new_height;
            }
            queue.push_back(sub_id);
        }
    }
}
```

For fully static graphs (no WHEN/WHILE), the propagation order can be pre-computed as a flat array of node IDs — turning graph traversal into a linear scan.

**Impact**:
- Glitch-free propagation guaranteed by construction
- O(1) scheduling via height-indexed queue
- Static graphs: zero runtime scheduling overhead (just iterate the pre-computed order)
- Eliminates the need for `select!` branch ordering to determine who fires first

**Prior art**: Jane Street Incremental (height-indexed array-of-lists, ~70 distinct heights in practice). Lustre/Esterel (fully static schedule compiled to a single loop).

---

## Optimization 8: Efficient List Operations

**Problem**: TodoMVC list operations (add/remove/filter) create per-item actors with full overhead. 20 todos = 100+ actors for item data alone, plus subscription chains for each property access in the bridge.

**Solution**: Lists become a specialized node type with per-item storage in a secondary arena.

```rust
struct ListNode {
    items: Vec<ItemSlot>,       // contiguous per-item storage
    item_template: Vec<NodeId>, // template nodes for per-item cells
    diffs: Vec<ListDiff>,       // pending add/remove/move diffs
}

struct ItemSlot {
    cells: SmallVec<[f64; 8]>,  // per-item cell values (inline, no heap alloc for small items)
    flags: ItemFlags,            // dirty, visible, etc.
}
```

**Per-item operations become array operations:**
- Add: push to `items` vec, clone template node values
- Remove: swap-remove from `items` vec
- Filter: iterate `items`, set visibility flag (no new allocations)
- Map: iterate `items`, apply transform in-place

**Impact**:
- Per-item overhead: ~64 bytes (inline cells) vs ~1500+ bytes (5 actors + 25 channels)
- Adding a todo: one vec push vs 5 task spawns + channel setup
- Filtering: linear scan with flag flip vs per-item actor message cascade
- Memory for 20 todos: ~1.3 KB vs ~30+ KB

**Prior art**: ECS archetype storage (Flecs). The WASM engine's `ItemCellStore` already uses this pattern (flat arrays indexed by item).

---

## Optimization 9: Efficient Bridge Subscription

**Problem**: The Actors bridge accesses each element property through `switch_map` + `stream` + `signal::from_stream` chains. Every property read is an async subscription creating new channels.

**Solution**: The bridge reads node values directly from the arena and subscribes to change notifications via lightweight callbacks.

```rust
// Before (Actors bridge):
let color_signal = switch_map(style_actor.stream(), |style| {
    style.get_field("color").value_actor().stream()
}).map(|v| value_to_css_color(v));
// ... multiple async stream layers ...

// After (arena bridge):
fn build_element_style(arena: &NodeArena, style_node: NodeId) -> impl Signal<Item = String> {
    // Direct read from arena - O(1), no channels
    let color_node = arena[style_node].field("color");
    arena.signal(color_node).map(|v| value_to_css_color(v))
}
```

The `arena.signal(node_id)` method returns a `futures_signals::Signal` backed by a `Mutable` that the propagation loop updates directly — one signal per observed node, no intermediate stream layers.

**Impact**:
- Property access: O(1) arena index lookup vs O(n) stream combinator chain
- Per-property subscription: one `Mutable` signal vs multi-layer stream + channel
- Bridge setup for a todo item: ~5 signal subscriptions vs ~15+ stream combinator chains

---

## Optimization 10: Actors Only at Async Boundaries

**Problem**: Everything is an actor, even pure synchronous computations like `count + 1`.

**Solution**: Reserve actual async actors (with tasks and channels) for genuinely asynchronous operations. Everything else is a synchronous node in the arena.

| Operation | Current | Optimized |
|-----------|---------|-----------|
| `count + 1` | ValueActor + task + 5 channels | Arena node (synchronous) |
| `HOLD state { ... }` | ValueActor + LazyValueActor + channels | Arena node with state slot |
| `LATEST { a, b }` | ValueActor + subscription channels | Arena node with multiple sources |
| `Timer/interval(1000)` | ValueActor + task | **Real async actor** (timer callback) |
| `Http/fetch(url)` | ValueActor + task | **Real async actor** (network I/O) |
| DOM event handlers | PassThroughConnector + channels | Event callback writes to arena node |

**Impact**:
- TodoMVC: ~2-3 actual async actors (timer if any, persistence save debounce) vs ~100+
- The synchronous graph handles 95%+ of the reactive logic
- Async actors inject events into the synchronous graph via `arena[node].set_value(v); propagate();`

---

## Combined Architecture

```
                    ┌─────────────────────────────────────────┐
  DOM Events ──────>│                                         │
                    │   Synchronous Reactive Graph (Arena)    │
  Timer Actor ─────>│                                         │──────> DOM Updates
                    │   - Flat Vec<Node> storage              │   (via Mutable signals)
  Network Actor ───>│   - Height-ordered propagation          │
                    │   - Push-pull with cutoff               │
  Storage Actor ───>│   - Batched event processing            │
                    │   - Fused compute chains                │
                    │                                         │
                    └─────────────────────────────────────────┘
                           ▲                      │
                           │                      ▼
                    2-3 real async actors    Single propagate() call
                    (timer, network, etc)    per event batch
```

## Expected Performance Improvement

| Metric | Current Actors | Optimized | WASM Engine |
|--------|---------------|-----------|-------------|
| Per-node memory | ~300+ bytes + channels | ~64-128 bytes | ~8 bytes (f64) |
| Per-event propagation | ~100+ poll cycles | ~20 node visits | ~10 cell writes |
| Channel allocations per todo | ~25 | 0 | 0 |
| Task spawns per todo add | ~15 | 0 | 0 |
| TodoMVC total memory (20 items) | ~30+ KB actors + channels | ~5-10 KB | ~2 KB |

The optimized Actors engine would be within ~2-5x of the WASM engine (vs current ~50-100x gap), with the remaining difference due to:
- Arena node overhead vs raw f64 (richer `Value` type, subscriber lists)
- `Mutable` signal bridge vs direct cell read
- Rust-level interpretation vs compiled WASM instructions

---

## Implementation Priority

1. **Arena + synchronous propagation** (Optimizations 1, 2, 3) — highest impact, eliminates the core bottleneck
2. **Push-pull with cutoff** (Optimization 4) — eliminates redundant work
3. **Batched events** (Optimization 5) — quick win, small change
4. **Efficient lists** (Optimization 8) — critical for TodoMVC-class apps
5. **Bridge optimization** (Optimization 9) — removes the second layer of overhead
6. **Actors at boundaries only** (Optimization 10) — cleanup / architectural clarity
7. **Compile-time fusion** (Optimization 6) — bonus optimization, lower priority
8. **Static scheduling** (Optimization 7) — bonus for fully static graphs

## References

- [Stakker: single-threaded actor runtime for Rust](https://github.com/uazu/stakker) — inlined FnOnce message dispatch
- [Jane Street: Seven Implementations of Incremental](https://www.janestreet.com/tech-talks/seven-implementations-of-incremental/) — height-ordered propagation, ~30ns/node
- [Preact Signal Boosting](https://preactjs.com/blog/signal-boosting/) — push-pull hybrid, doubly-linked deps, version counting
- [Leptos Reactive Graph](https://book.leptos.dev/appendix_reactive_graph.html) — Reactively algorithm in Rust, slotmap arena
- [Angular Signals Push-Pull](https://angularexperts.io/blog/angular-signals-push-pull/) — producer/consumer versioning
- [SolidJS Fine-Grained Reactivity](https://docs.solidjs.com/advanced-concepts/fine-grained-reactivity) — synchronous propagation, direct DOM updates
- [Reactively Algorithm](https://github.com/modderme123/reactively/blob/main/Reactive-algorithms.md) — graph coloring (dirty/check/clean)
- [TC39 Signals Proposal](https://github.com/tc39/proposal-signals) — standardizing push-pull signals
- [Co-optimizing Dataflow Graphs and Actors with MLIR](https://hal.science/hal-03845902v1/document) — actor fusion, 30% speedup
- [Pony Language](https://tutorial.ponylang.io/) — 240 bytes/actor, zero-copy messaging via reference capabilities
- [CAF (C++ Actor Framework)](https://www.actor-framework.org/) — ~10 bytes/actor, copy-on-write messages
- [Timely/Differential Dataflow](https://github.com/TimelyDataflow/timely-dataflow) — epoch-based batching, progress tracking
- [Naiad: A Timely Dataflow System (SOSP 2013)](https://sigops.org/s/conferences/sosp/2013/papers/p439-murray.pdf)
- [Alien-Signals](https://github.com/stackblitz/alien-signals) — lightest signal library, doubly-linked lists, bitwise flags
- [Skip (SkipLabs)](https://skiplabs.io/blog/why-skip) — language-level immutability for zero-copy reactive
- [Lustre Synchronous Dataflow](https://www.researchgate.net/publication/2632196_The_synchronous_dataflow_programming_language_LUSTRE) — compile-time static scheduling
