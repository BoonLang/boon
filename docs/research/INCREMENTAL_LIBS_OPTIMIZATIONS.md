# Incremental Computation Libraries: Research & Optimization Recommendations for Boon

**Status**: Deep Research Complete
**Date**: January 2026
**Scope**: Noria, Jane Street Incremental, DBSP/Feldera, RisingWave, and applicability to Boon's browser WASM + WebWorkers architecture

---

## Executive Summary

After researching five major incremental computation systems, the key findings are:

1. **Boon's actor model is validated** - RisingWave uses nearly identical architecture (actors + tokio async)
2. **Z-sets are the right abstraction** - DBSP, RisingWave, and Differential Dataflow all converge on weight-based change tracking
3. **Barrier-based epochs** could simplify HOLD synchronization vs current backpressure permits
4. **No system fits Boon perfectly** - all are server/database focused, but patterns translate to browser

**Recommendation**: Don't adopt any library wholesale. Instead, cherry-pick:
- Z-set algebra from DBSP for O(delta) collection operations
- Barrier/epoch model from RisingWave for consistency
- Cutoff optimization from Incremental to stop unnecessary propagation

---

## Table of Contents

1. [Systems Overview](#1-systems-overview)
2. [Theoretical Foundations](#2-theoretical-foundations)
3. [Detailed System Analysis](#3-detailed-system-analysis)
4. [Compatibility with Boon](#4-compatibility-with-boon)
5. [Optimization Recommendations](#5-optimization-recommendations)
6. [Browser WASM + WebWorkers Architecture](#6-browser-wasm--webworkers-architecture)
7. [Implementation Priorities](#7-implementation-priorities)
8. [References](#8-references)
9. [Nested Collections & Hybrid Solution](#9-nested-collections--hybrid-solution)

---

## 1. Systems Overview

| System | Focus | License | WASM Support | Boon Fit |
|--------|-------|---------|--------------|----------|
| **Noria** | Database view maintenance | MIT | No | Poor |
| **Incremental** | DAG recomputation | MIT | Yes (Rust port) | Theory only |
| **DBSP/Feldera** | Z-set algebra, SQL | MIT / Apache-2.0 | Possible | Best theory |
| **RisingWave** | Streaming SQL database | Apache-2.0 | No | Best architecture |
| **Differential Dataflow** | Incremental collections | MIT | Yes | Good alternative |

### Quick Comparison

```
                    Noria              Incremental           DBSP              RisingWave
                      │                    │                  │                    │
                      ▼                    ▼                  ▼                    ▼
              ┌───────────────┐    ┌───────────────┐   ┌───────────────┐   ┌───────────────┐
Paradigm:     │   Database    │    │  Computation  │   │  Mathematical │   │   Streaming   │
              │    Views      │    │     DAG       │   │   Algebra     │   │   Database    │
              └───────────────┘    └───────────────┘   └───────────────┘   └───────────────┘
                      │                    │                  │                    │
              ┌───────────────┐    ┌───────────────┐   ┌───────────────┐   ┌───────────────┐
Core:         │   Upqueries   │    │  Stabilize()  │   │    Z-sets     │   │ Actor + Epoch │
              │   (on-demand) │    │   (dirty →    │   │  (weights)    │   │  (barriers)   │
              │               │    │    clean)     │   │               │   │               │
              └───────────────┘    └───────────────┘   └───────────────┘   └───────────────┘
                      │                    │                  │                    │
              ┌───────────────┐    ┌───────────────┐   ┌───────────────┐   ┌───────────────┐
Time Model:   │  Eventually   │    │   Explicit    │   │    Logical    │   │  Epoch-based  │
              │  Consistent   │    │  stabilize()  │   │  Timestamps   │   │   Barriers    │
              └───────────────┘    └───────────────┘   └───────────────┘   └───────────────┘
```

---

## 2. Theoretical Foundations

### 2.1 Z-Sets: The Universal Change Representation

All modern incremental systems converge on **weight-based change tracking**:

```
Z-set: Element → Integer (weight)

Insert:  {item: +1}
Delete:  {item: -1}
Update:  {old_item: -1, new_item: +1}
No-op:   {item: 0}  (can be removed)
```

**Why Z-sets work:**
- Form a **commutative group**: addition is associative, commutative, has identity (0) and inverse (-x)
- Enable **automatic incrementalization**: derivative of any operator can be computed
- **Batch-friendly**: multiple changes combine naturally

**Mapping to existing systems:**

| System | Representation | Notes |
|--------|---------------|-------|
| DBSP | `ZSet<T>` with integer weights | Canonical |
| RisingWave | `StreamChunk` with `ops` column | Insert/Delete/UpdateDelete/UpdateInsert |
| Differential Dataflow | `(data, time, diff)` triples | Time-indexed Z-sets |
| Boon ListDiff | `Insert/Remove/Update` enum | Already compatible! |

### 2.2 Time Models

#### Total Order (Simpler)
```
t=0 → t=1 → t=2 → t=3 → ...

Every event has a single timestamp.
All workers must agree on ordering.
```

**Used by:** DBSP, RisingWave (epochs)

**Coordination:** Requires barrier synchronization or central coordinator

#### Partial Order (More Parallel)
```
       (v2, i0)
      ╱       ╲
(v1, i0)       (v2, i1)    ← Some pairs are incomparable
      ╲       ╱
       (v1, i1)
```

**Used by:** Differential Dataflow (lattice timestamps)

**Coordination:** None needed for independent dimensions

#### For Boon

Current approach (Lamport clocks) gives total order without central generator. For multi-worker browser, consider:
- **Batched epochs** (simpler, like RisingWave)
- **Per-worker sequences** with barrier sync points

### 2.3 Lattices Explained Simply

A **lattice** is a set where any two elements can be "merged":

```
Example: Max lattice (numbers with max operation)

    5
   /|\
  3 4     max(3, 4) = 4
   \|
    1

Example: Set lattice (sets with union)

    {a,b,c}
    /     \
 {a,b}   {b,c}     {a,b} ∪ {b,c} = {a,b,c}
    \     /
      {b}
```

**Why lattices matter:**
- Enable **conflict-free merging** in distributed systems
- Differential Dataflow uses lattice timestamps for multi-dimensional time
- Hydroflow uses lattices for correctness proofs

**For Boon:** Not strictly necessary if using simpler epoch-based model, but useful for future distributed scenarios.

---

## 3. Detailed System Analysis

### 3.1 Noria (MIT PDOS)

**Repository:** https://github.com/mit-pdos/noria

**What it is:** Streaming database that maintains materialized views incrementally with partial state.

**Core Innovation - Upqueries:**
```
Query arrives for missing data
         │
         ▼
┌─────────────────┐
│ Downstream Node │ ── "I need data for key X" ──▶ Upstream
│ (partial state) │ ◀── Response with data ───────
└─────────────────┘
         │
         ▼
State populated on-demand
```

**Strengths:**
- Memory-efficient (only stores hot data)
- Elegant lazy evaluation
- 5x faster than MySQL on benchmarks

**Why NOT for Boon:**
- Database paradigm (tables, SQL) doesn't fit reactive UI
- Upquery protocol is imperative, breaks pure stream model
- No HOLD equivalent - views are queries, not state machines
- Requires ZooKeeper for distributed mode

**Verdict:** Poor fit. Paradigm mismatch too large.

---

### 3.2 Jane Street Incremental (incremental-rs)

**Repository:** https://github.com/cormacrelf/incremental-rs (Rust port)
**Original:** https://github.com/janestreet/incremental (OCaml)

**What it is:** Self-adjusting computation library that efficiently propagates changes through a DAG.

**Core Algorithm:**
```
1. Build DAG:  Var → map → map → Observer

2. On change:  Mark dirty nodes
               ┌─────┐
           ┌──▶│dirty│──┐
           │   └─────┘  │
       ┌───┴───┐    ┌───▼───┐
       │ clean │    │ dirty │
       └───────┘    └───────┘

3. Stabilize: Recompute in topological order

4. Cutoff:    If result unchanged, stop propagation
              ┌─────┐         ┌─────┐
              │  5  │ ──?──▶  │  5  │  STOP! Same value.
              └─────┘         └─────┘
```

**Seven Implementations Evolution:**
1. Two-pass with GC finalizers
2. Topological sort with timestamps
3. Explicit observers for GC
4. Academic insights + dynamic topo sort
5. Heap elimination with pseudo-heights
6. Finalizer reduction using observability
7. GADTs for 3x speedup

**Strengths:**
- Well-proven theory (Umut Acar's PhD thesis)
- Cutoff optimization is powerful
- Jane Street uses it for complex trading UIs

**Limitations:**
- **No incremental collections** - filter/map are O(n), not O(delta)
- **Uses RefCell internally** - violates Boon's no-shared-mutable-state rule
- **Explicit stabilize()** doesn't fit continuous reactive streams

**What to Adopt:**
- Cutoff optimization concept (stop propagation when value unchanged)
- DAG-based dependency tracking ideas

**Verdict:** Valuable theory, but implementation doesn't fit Boon constraints.

---

### 3.3 DBSP / Feldera

**Repository:** https://github.com/feldera/feldera
**Paper:** VLDB 2023 Best Paper - "DBSP: Automatic Incremental View Maintenance"
**License:** MIT / Apache-2.0

**What it is:** Mathematical framework that can automatically incrementalize any query using Z-set algebra.

**The Four Operators:**

| Operator | Symbol | Description |
|----------|--------|-------------|
| **Delay** | `z⁻¹` | Access previous timestep's value |
| **Integration** | `I` | Sum all inputs: `I(s)[t] = Σ s[i] for i≤t` |
| **Differentiation** | `D` | Emit only changes: `D(s)[t] = s[t] - s[t-1]` |
| **Fixpoint** | `δ` | Iterate until convergence |

**Incrementalization Formula:**
```
Q^incremental = D ∘ Q ∘ I

       Input changes (delta)
              │
              ▼
        ┌─────────┐
        │ Integrate│  (rebuild full input)
        └────┬────┘
             │
        ┌────▼────┐
        │  Query  │   (run original query)
        └────┬────┘
             │
        ┌────▼────────┐
        │Differentiate│ (extract only changes)
        └─────────────┘
              │
              ▼
       Output changes (delta)
```

**Automatic Derivative Rules:**
```
D[filter(S, p)] = filter(D[S], p)
D[map(S, f)] = map(D[S], f)
D[join(A, B)] = join(D[A], B) + join(A, D[B]) + join(D[A], D[B])
```

**Indexed Z-Sets (for efficient joins):**
```rust
// Map<Key, ZSet<Value>>
{
    user_123: { Order{id:1}: +1, Order{id:2}: +1 },
    user_456: { Order{id:3}: +1 }
}
```

**Strengths:**
- Pure algebraic model - maps to hardware
- Automatic incrementalization
- O(delta) for all standard operators
- Proven at scale (Feldera processes millions/sec)

**Limitations:**
- Batch-oriented (processes batches, not individual events)
- SQL-focused API
- No built-in UI primitives

**What to Adopt:**
- Z-set algebra for List operations
- Derivative rules for automatic incrementalization
- Indexed Z-sets for efficient grouping

**Verdict:** Best theoretical fit. Adopt the math, not the SQL layer.

---

### 3.4 RisingWave

**Repository:** https://github.com/risingwavelabs/risingwave
**License:** Apache-2.0

**What it is:** Cloud-native streaming SQL database built on actor model with S3 storage.

**Architecture:**
```
┌─────────────────────────────────────────────────────────┐
│                     META SERVICE                         │
│  • Epoch generation                                      │
│  • Barrier injection                                     │
│  • Checkpoint coordination                               │
└────────────────────────────┬────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────┐
│                    COMPUTE NODES                         │
│  ┌────────────────────────────────────────────────────┐ │
│  │                     ACTORS                          │ │
│  │  ┌─────────┐    ┌─────────┐    ┌─────────┐        │ │
│  │  │ Merger  │───▶│Executor │───▶│Dispatcher│        │ │
│  │  │(barrier │    │ (op)    │    │(routing) │        │ │
│  │  │ align)  │    │         │    │          │        │ │
│  │  └─────────┘    └─────────┘    └─────────┘        │ │
│  └────────────────────────────────────────────────────┘ │
│                          │                               │
│                  ┌───────▼───────┐                       │
│                  │ Shared Buffer │                       │
│                  └───────┬───────┘                       │
└──────────────────────────┼──────────────────────────────┘
                           │ async flush
┌──────────────────────────▼──────────────────────────────┐
│                   HUMMOCK (S3)                           │
│  LSM-tree storage with remote compaction                 │
└─────────────────────────────────────────────────────────┘
```

**Stream Chunks (Z-sets in disguise):**
```rust
struct StreamChunk {
    columns: Vec<Column>,     // Columnar (vectorized)
    visibility: Bitmap,        // Valid rows
    ops: Vec<Op>,             // Operation type per row
}

enum Op {
    Insert,       // +1
    Delete,       // -1
    UpdateDelete, // -1 (old)
    UpdateInsert, // +1 (new)
}
```

**Barrier-Based Consistency:**
```
Source A: ─data─data─│B1│─data─data─│B2│─data─
                     ↓              ↓
Source B: ─data─data─data─│B1│─data─│B2│─data─
                          ↓        ↓
                     ┌────┴────────┴────┐
                     │   JOIN ACTOR      │
                     │ (aligns barriers) │
                     └────────┬──────────┘
                              │
                     ─output──│B1│─output─│B2│─
                              ↓           ↓
                         checkpoint   checkpoint
```

**Key properties:**
- Barriers don't overtake data
- All data within epoch has same logical timestamp
- State flushed asynchronously (non-blocking)

**Why NOT RocksDB:**
- Single-node design doesn't scale
- Compaction affects query performance
- Too many features RisingWave doesn't need

**Hummock Storage:**
- LSM-tree with MVCC (epoch as version)
- Remote compaction workers
- Hybrid state: local cache + remote persistence

**Strengths:**
- Actor model identical to Boon's!
- Proven at production scale
- Async checkpointing pattern
- Clear separation of concerns

**Limitations:**
- No WASM support
- No UI integration
- Server-focused architecture

**What to Adopt:**
- Actor-per-operator pattern (validates Boon's approach)
- Stream Chunk batching
- Barrier-based epochs for consistency
- Async state persistence pattern
- Shared buffer concept

**Verdict:** Best architectural validation. Many patterns directly applicable.

---

### 3.5 Differential Dataflow

**Repository:** https://github.com/TimelyDataflow/differential-dataflow
**License:** MIT

**What it is:** Incremental computation on collections, built on Timely Dataflow.

**Core concept: Time-indexed differences**
```rust
// Collection = stream of (data, time, diff) triples
(User{name: "Alice"}, time=5, diff=+1)  // Insert at time 5
(User{name: "Alice"}, time=7, diff=-1)  // Delete at time 7
```

**Partial Order Timestamps:**
```rust
// Product lattice for iteration
type Time = (Version, Iteration);

// (v1, i2) and (v2, i1) are INCOMPARABLE
// Neither is "before" the other → can process in parallel
```

**Arrangements (Indexed State):**
```rust
let arranged = collection.arrange_by_key();

// Efficient lookups and joins
other.join_core(&arranged, |k, v1, v2| ...);
```

**Trace Compaction:**
```rust
// "I no longer need times before T"
arranged.trace.set_logical_compaction(&[new_frontier]);
// Old data merged, memory freed
```

**Strengths:**
- Proven at massive scale (Materialize)
- Native incremental collections
- Sophisticated time model
- WASM possible (with work)

**Limitations:**
- Complex to understand (frontiers, capabilities)
- No SQL layer (manual operator composition)
- Batch-oriented API

**What to Adopt:**
- Arrangement pattern for indexed state
- Trace compaction for memory management
- Progress tracking concepts

**Verdict:** Good alternative to DBSP. More complex but more flexible.

---

## 4. Compatibility with Boon

### 4.1 Boon's Requirements

| Requirement | Source |
|-------------|--------|
| Pure actor model | Only actors + pure streams |
| No shared mutable state | No RefCell/Mutex in engine |
| Hardware compilable | Must map to FSMs/wires |
| HVM compilable | Must work with interaction nets |
| WASM + WebWorkers | Browser deployment |
| Infinite streams | ValueActors need non-terminating streams |
| Backpressure (HOLD) | Sequential state updates |
| ID-based list diffs | O(1) operations |

### 4.2 Compatibility Matrix

| Requirement | Noria | Incremental | DBSP | RisingWave | DD |
|-------------|-------|-------------|------|------------|-----|
| Pure actor model | ❌ | ⚠️ | ⚠️ | ✅ | ⚠️ |
| No shared state | ❌ | ❌ | ✅ | ✅ | ✅ |
| Hardware target | ❌ | ⚠️ | ✅ | ❌ | ⚠️ |
| HVM target | ❌ | ⚠️ | ✅ | ❌ | ⚠️ |
| WASM support | ❌ | ✅ | ⚠️ | ❌ | ⚠️ |
| Infinite streams | ⚠️ | ⚠️ | ✅ | ✅ | ✅ |
| Backpressure | ✅ | ❌ | ⚠️ | ✅ | ⚠️ |
| ID-based diffs | ✅ | ❌ | ✅ | ✅ | ✅ |

### 4.3 Mapping Boon Constructs

**LATEST:**
```boon
result: LATEST { a, b, c }
```

| System | Mapping |
|--------|---------|
| RisingWave | Union of streams |
| DBSP | Z-set concatenation |
| Incremental | `map_n` combinator |

**HOLD:**
```boon
[count: 0] |> HOLD state {
    clicks |> THEN { [count: state.count + 1] }
}
```

| System | Mapping |
|--------|---------|
| RisingWave | Stateful operator with epoch-based updates |
| DBSP | `delay(z⁻¹)` + `integrate` |
| Incremental | `Var` + `bind` |

**List/filter:**
```boon
completed: todos |> List/retain(item, if: item.completed)
```

| System | Complexity | Notes |
|--------|------------|-------|
| RisingWave | O(delta) | StreamChunk filter |
| DBSP | O(delta) | Z-set filter |
| Incremental | O(n) | Full recompute |

---

## 5. Optimization Recommendations

### 5.1 Adopt Z-Set Semantics for Collections

**Current Boon ListDiff:**
```rust
pub enum ListChange<T> {
    Insert { id: ItemId, value: T },
    Remove { id: ItemId },
    Update { id: ItemId, value: T },
}
```

**Enhanced with weights:**
```rust
pub struct ZSetChange<T> {
    pub id: ItemId,
    pub value: T,
    pub weight: i64,  // +1 insert, -1 delete
}

impl<T> ZSetChange<T> {
    pub fn insert(id: ItemId, value: T) -> Self {
        Self { id, value, weight: 1 }
    }

    pub fn delete(id: ItemId, value: T) -> Self {
        Self { id, value, weight: -1 }
    }
}

// Consolidation: remove zero-weight items
pub fn consolidate<T: Eq + Hash>(changes: &mut Vec<ZSetChange<T>>) {
    let mut weights: HashMap<ItemId, i64> = HashMap::new();
    for change in changes.iter() {
        *weights.entry(change.id).or_default() += change.weight;
    }
    changes.retain(|c| weights.get(&c.id).map(|w| *w != 0).unwrap_or(false));
}
```

**Benefits:**
- Insert + Delete = no change (cancels out)
- Natural batching semantics
- Matches DBSP/RisingWave/DD models

### 5.2 Implement Cutoff Optimization

From Jane Street Incremental: stop propagation when value unchanged.

```rust
impl ValueActor {
    async fn emit(&mut self, new_value: Value) {
        // Cutoff: don't propagate if unchanged
        if Some(&new_value) == self.last_emitted.as_ref() {
            return;  // Skip!
        }

        self.last_emitted = Some(new_value.clone());

        for subscriber in &self.subscribers {
            subscriber.send(new_value.clone()).await;
        }
    }
}
```

**Impact:** Prevents unnecessary downstream recomputation when values stabilize.

### 5.3 Consider Barrier-Based Epochs

Replace LazyValueActor's backpressure permits with barrier-based consistency:

```rust
enum Message {
    Data(StreamChunk),
    Barrier { epoch: u64 },
}

async fn hold_actor_loop(
    mut input: Receiver<Message>,
    mut output: Sender<Message>,
    mut state: Value,
) {
    loop {
        match input.recv().await {
            Message::Data(chunk) => {
                // Process chunk, update state
                for change in chunk.changes {
                    state = apply_hold_body(state, change);
                }
                // Emit state update
                output.send(Message::Data(state.clone().into())).await;
            }
            Message::Barrier { epoch } => {
                // Checkpoint state (async, non-blocking)
                spawn_local(persist_state(epoch, state.clone()));

                // Forward barrier
                output.send(Message::Barrier { epoch }).await;
            }
        }
    }
}
```

**Benefits:**
- Simpler than permit-based backpressure
- Natural checkpointing integration
- Matches RisingWave's proven model

### 5.4 Batch Changes into Chunks

Instead of individual events, batch into chunks:

```rust
struct StreamChunk {
    changes: Vec<ZSetChange<Value>>,
    epoch: u64,
}

// Accumulate changes until:
// 1. Chunk size threshold (e.g., 1000 changes)
// 2. Time threshold (e.g., 16ms for 60fps UI)
// 3. Barrier arrives

impl ChunkAccumulator {
    fn maybe_flush(&mut self) -> Option<StreamChunk> {
        let should_flush =
            self.changes.len() >= 1000 ||
            self.last_flush.elapsed() > Duration::from_millis(16);

        if should_flush {
            Some(self.take_chunk())
        } else {
            None
        }
    }
}
```

**Benefits:**
- Amortizes per-message overhead
- Better cache locality
- Enables vectorized operations

### 5.5 Async State Persistence

Following RisingWave's pattern:

```rust
// Don't block on IndexedDB writes
async fn checkpoint_state(epoch: u64, dirty_keys: &HashSet<Key>, cache: &StateCache) {
    let changes: Vec<_> = dirty_keys.iter()
        .filter_map(|k| cache.get(k).map(|v| (k.clone(), v.clone())))
        .collect();

    // Fire-and-forget to storage worker
    storage_worker.send(StorageCommand::Write { epoch, changes });
}

// Storage worker handles actual IndexedDB operations
async fn storage_worker_loop(mut rx: Receiver<StorageCommand>) {
    loop {
        match rx.recv().await {
            StorageCommand::Write { epoch, changes } => {
                // Batch writes to IndexedDB
                indexed_db_batch_put(&changes).await;

                // Optionally: compact old epochs
                if should_compact(epoch) {
                    compact_old_epochs(epoch - RETENTION).await;
                }
            }
        }
    }
}
```

---

## 6. Browser WASM + WebWorkers Architecture

### 6.1 Proposed Architecture

```
┌────────────────────────────────────────────────────────────────┐
│                        MAIN THREAD                              │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                      UI ACTORS                            │  │
│  │   • Element rendering                                     │  │
│  │   • LINK event handlers                                   │  │
│  │   • DOM updates (requestAnimationFrame batched)           │  │
│  └──────────────────────┬───────────────────────────────────┘  │
│                         │ postMessage / SharedArrayBuffer       │
└─────────────────────────┼──────────────────────────────────────┘
                          │
┌─────────────────────────▼──────────────────────────────────────┐
│                    COMPUTE WORKER(S)                            │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                   EPOCH COORDINATOR                       │  │
│  │   • Generates epochs (every 100ms or configurable)        │  │
│  │   • Injects barriers into all source actors               │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                     BOON ACTORS                           │  │
│  │  ┌─────────┐   ┌─────────┐   ┌─────────┐                 │  │
│  │  │ Source  │──▶│  HOLD   │──▶│  List/  │──▶ UI Bridge    │  │
│  │  │  Actor  │   │  Actor  │   │ filter  │                 │  │
│  │  └─────────┘   └─────────┘   └─────────┘                 │  │
│  │                      │                                    │  │
│  │              ┌───────▼───────┐                            │  │
│  │              │ State Cache   │  (LRU HashMap)             │  │
│  │              └───────┬───────┘                            │  │
│  └──────────────────────┼────────────────────────────────────┘  │
│                         │ checkpoint commands                   │
└─────────────────────────┼──────────────────────────────────────┘
                          │
┌─────────────────────────▼──────────────────────────────────────┐
│                    STORAGE WORKER                               │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                  INDEXEDDB BACKEND                        │  │
│  │   • Epoch-based versioning (MVCC)                         │  │
│  │   • Batch writes                                          │  │
│  │   • Background compaction                                 │  │
│  └──────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────┘
```

### 6.2 SharedArrayBuffer for Hot Path

For high-frequency communication between workers:

```rust
// Ring buffer in SharedArrayBuffer
pub struct SharedChannel<T> {
    buffer: SharedArrayBuffer,
    write_idx: AtomicU32,
    read_idx: AtomicU32,
    _marker: PhantomData<T>,
}

impl<T: Serialize + DeserializeOwned> SharedChannel<T> {
    pub fn send(&self, value: &T) {
        let serialized = bincode::serialize(value).unwrap();

        loop {
            let write = self.write_idx.load(Ordering::Acquire);
            let read = self.read_idx.load(Ordering::Acquire);

            if write - read < CAPACITY {
                self.write_to_buffer(write as usize, &serialized);
                self.write_idx.store(write + 1, Ordering::Release);

                // Wake reader
                unsafe { Atomics::notify(&self.buffer, 0, 1); }
                return;
            }

            // Buffer full - wait
            unsafe { Atomics::wait(&self.buffer, 0, write as i32, None); }
        }
    }

    pub async fn recv(&self) -> T {
        loop {
            let write = self.write_idx.load(Ordering::Acquire);
            let read = self.read_idx.load(Ordering::Acquire);

            if write > read {
                let data = self.read_from_buffer(read as usize);
                self.read_idx.store(read + 1, Ordering::Release);
                return bincode::deserialize(&data).unwrap();
            }

            // Wait for data
            let promise = unsafe { Atomics::wait_async(&self.buffer, 0, write as i32) };
            JsFuture::from(promise).await.ok();
        }
    }
}
```

### 6.3 Epoch Coordinator

```rust
pub struct EpochCoordinator {
    current_epoch: AtomicU64,
    source_actors: Vec<Sender<Message>>,
    interval_ms: u64,
}

impl EpochCoordinator {
    pub fn start(&self) {
        let interval = Interval::new(self.interval_ms as u32);

        spawn_local(async move {
            loop {
                interval.next().await;

                let epoch = self.current_epoch.fetch_add(1, Ordering::SeqCst);

                // Inject barrier into all sources
                for source in &self.source_actors {
                    source.send(Message::Barrier { epoch }).await.ok();
                }
            }
        });
    }
}
```

---

## 7. Implementation Priorities

### Phase 1: Foundation (Immediate)

| Task | Effort | Impact |
|------|--------|--------|
| Add cutoff optimization to ValueActor | Low | High |
| Enhance ListDiff with weight semantics | Low | Medium |
| Implement consolidation for Z-set changes | Low | Medium |

### Phase 2: Batching (Short-term)

| Task | Effort | Impact |
|------|--------|--------|
| Create StreamChunk type | Medium | High |
| Add chunk accumulator with thresholds | Medium | High |
| Batch DOM updates per animation frame | Low | High |

### Phase 3: Epochs (Medium-term)

| Task | Effort | Impact |
|------|--------|--------|
| Design epoch coordinator | Medium | High |
| Implement barrier-based HOLD | High | High |
| Add async checkpointing | Medium | Medium |

### Phase 4: Multi-Worker (Long-term)

| Task | Effort | Impact |
|------|--------|--------|
| SharedArrayBuffer channel | High | High |
| Worker topology management | High | Medium |
| Distributed epoch coordination | High | Medium |

---

## 8. References

### Papers & Academic

- **DBSP Paper**: Budiu, M., et al. "DBSP: Automatic Incremental View Maintenance" (VLDB 2023 Best Paper)
- **Self-Adjusting Computation**: Acar, U. PhD Thesis, Carnegie Mellon University
- **Naiad Paper**: Murray, D., et al. "Naiad: A Timely Dataflow System" (SOSP 2013)
- **Differential Dataflow**: McSherry, F., et al. "Differential Dataflow" (CIDR 2013)

### Documentation & Blogs

- [Jane Street: Introducing Incremental](https://blog.janestreet.com/introducing-incremental/)
- [Jane Street: Seven Implementations of Incremental](https://www.janestreet.com/tech-talks/seven-implementations-of-incremental/)
- [Jane Street: Self-Adjusting DOM](https://blog.janestreet.com/self-adjusting-dom/)
- [Feldera: Indexed Z-Sets Explained](https://www.feldera.com/blog/Indexed-Zsets)
- [RisingWave: Stream Processing Engine Deep Dive](https://risingwave.com/blog/deep-dive-into-the-risingwave-stream-processing-engine-part-1-overview/)
- [RisingWave: Hummock Storage Engine](https://risingwave.com/blog/hummock-a-storage-engine-designed-for-stream-processing/)
- [Materialize: Building Differential Dataflow from Scratch](https://materialize.com/blog/differential-from-scratch/)
- [The Morning Paper: Differential Dataflow](https://blog.acolyer.org/2015/06/17/differential-dataflow/)

### Repositories

| System | Repository | License |
|--------|-----------|---------|
| Noria | https://github.com/mit-pdos/noria | MIT |
| incremental-rs | https://github.com/cormacrelf/incremental-rs | MIT |
| Feldera/DBSP | https://github.com/feldera/feldera | MIT/Apache-2.0 |
| RisingWave | https://github.com/risingwavelabs/risingwave | Apache-2.0 |
| Differential Dataflow | https://github.com/TimelyDataflow/differential-dataflow | MIT |
| Timely Dataflow | https://github.com/TimelyDataflow/timely-dataflow | MIT |
| Hydroflow | https://github.com/hydro-project/hydro | MIT |
| futures_signals | https://github.com/Pauan/rust-signals | MIT |

### Related Boon Documentation

- `docs/research/TIMELY_DIFFERENTIAL_DATAFLOW.md` - Previous DD research
- `docs/research/FLO_CHORUS_SUKI.md` - Flo/Suki/ChoRus analysis
- `docs/ANOTHER_STREAMING_RESEARCH.md` - Streaming library comparison
- `docs/language/storage/DATALOG_INCREMENTAL_RESEARCH.md` - Datalog systems
- `docs/language/storage/RUST_STREAMING_ECOSYSTEM.md` - Ecosystem overview
- `docs/engine/ACTOR_MODEL.md` - Boon's actor architecture

---

## 9. Nested Collections & Hybrid Solution

### 9.1 The Nested Reactivity Problem

Boon's actor model handles **nested reactive structures** (lists within lists, objects with reactive fields) correctly by design—each actor owns its subscriptions and propagates changes through the hierarchy. However, most incremental computation libraries were designed for **flat collections**:

| Library | Nesting Model | Limitation |
|---------|---------------|------------|
| **Differential Dataflow** | Flat collections | Must flatten to `(parent_id, child_id, value)` |
| **DBSP** | Flat Z-sets | Same flattening required |
| **Jane Street Incremental** | DAG of scalars | Collections aren't native |
| **RisingWave** | Relational tables | Foreign key joins for hierarchy |

**Example: Todo with Subtasks**

```boon
todos: [
    [
        id: "todo-1",
        title: "Main Task",
        subtasks: [
            [id: "sub-1", done: false],
            [id: "sub-2", done: true]
        ]
    ]
]
```

In DD/DBSP, this becomes three flat collections:
```
todos:    {("todo-1", "Main Task"): +1}
subtasks: {("sub-1", "todo-1", false): +1, ("sub-2", "todo-1", true): +1}
-- parent-child relationship via foreign key
```

**Boon's actor model preserves structure naturally** through nested actors.

### 9.2 Libraries That Handle Nesting Well

| Library | Language | Nesting | Patches | Incremental | Snapshots |
|---------|----------|---------|---------|-------------|-----------|
| **MobX-State-Tree** | JS/TS | ✅ Native tree | ✅ JSON-Patch | ⚠️ Manual | ✅ Built-in |
| **Signia** | JS/TS | ⚠️ Manual | ✅ Diff history | ✅ Built-in | ✅ Epochs |
| **Adapton** | Rust/OCaml | ⚠️ Via IDs | ❌ | ✅ Memoization | ❌ |
| **Salsa** | Rust | ⚠️ Via IDs | ❌ | ✅ Queries | ❌ |

#### MobX-State-Tree (MST)

**Core Innovation:** Tree of typed nodes where identity is determined by position.

```typescript
const TodoModel = types.model({
  id: types.identifier,
  title: types.string,
  subtasks: types.array(SubtaskModel)  // Nested!
})

// Every mutation produces JSON-Patch
[
  { op: "replace", path: "/todos/0/subtasks/1/done", value: true }
]
```

**Strengths:**
- Full path in every patch (which subtask changed?)
- Snapshots are immutable (time-travel, undo)
- Middleware can intercept all changes

**Weakness:** No automatic incremental derivations—`computed` values recompute fully.

#### Signia

**Core Innovation:** Epoch-based diff history with incremental computed values.

```typescript
const atom = atom("items", []);
const computed = computed("filtered", () => {
  return atom.value.filter(x => x.active);
}, {
  computeDiff: (prev, next) => {
    // Incremental: only diff the changed items
    return computeArrayDiff(prev, next);
  }
});
```

**Strengths:**
- `getDiffSince(epoch)` returns only changes
- Derived values can update incrementally
- Explicit epoch tracking

**Weakness:** Nesting requires manual tree construction.

### 9.3 Boon's Current Limitations (with Code Evidence)

| Limitation | Impact | Location |
|------------|--------|----------|
| **No unified diff/patch format** | Can't serialize/replay changes | Actor emits `Value`, not "what changed" |
| **No path information** | Can't pinpoint nested changes | Subscribers see whole object, not field path |
| **Full recomputation** | O(n) work for O(1) change | `engine.rs:7576-7591` |
| **No snapshot/time-travel** | No undo, no debugging history | No epoch tracking in actors |

#### Evidence: Full Recomputation in List/retain

In `crates/boon/src/platform/browser/engine.rs:7576-7591`:

```rust
// Emit updated filtered result
let filtered: Vec<_> = items.iter()                    // O(n) - iterate ALL items
    .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
    .cloned()                                           // O(n) - clone ALL matching
    .collect();

// ... deduplication check ...

return Some((
    Some(ListChange::Replace { items: filtered }),     // Full replacement!
    // ...
));
```

**The problem:** When item #42 in a 1000-item list changes its `done` field:

1. Predicate for item #42 updates → triggers merged predicate stream
2. Code updates `predicate_results` for that one item ✓ (O(1))
3. But then it **iterates ALL 1000 items** to rebuild `filtered` ✗ (O(n))
4. Emits `Replace { items: ... }` with ALL matching items ✗ (O(n))

**Downstream cascade:** `List/count` in `api.rs:1650-1676` receives the full list and must recount entirely:

```rust
list.stream().scan((None, 0usize), |(last_count, current_count), change| {
    match change {
        ListChange::Replace { items } => {
            *current_count = items.len();  // Must count ALL items
        }
        // ...
    }
})
```

**What incremental would look like:**

```rust
// Instead of Replace, emit granular change:
ListChange::Insert { index: 42, item: item_42 }  // O(1) to emit

// Then List/count can do:
match change {
    ListChange::Insert { .. } => *current_count += 1,  // O(1)!
    ListChange::Remove { .. } => *current_count -= 1,
}
```

### 9.4 Proposed Hybrid Solution

**Keep Boon's actor hierarchy** (like MST's tree) but add:
1. **Path-aware patches** (from MST)
2. **Diff history with epochs** (from Signia)
3. **Incremental derivations** (from Signia)

#### Core Types

```rust
/// Epoch counter for change tracking
pub type Epoch = u64;

/// Path segment for nested access
pub enum PathSegment {
    Field(String),      // .field_name
    Index(usize),       // [0]
    Key(String),        // map key
}

/// A patch describes a single change with full path
pub struct Patch {
    pub epoch: Epoch,
    pub path: Vec<PathSegment>,  // e.g., [Index(0), Field("subtasks"), Index(1), Field("done")]
    pub op: PatchOp,
}

pub enum PatchOp {
    Replace { old: Value, new: Value },
    Insert { value: Value },
    Remove { value: Value },
}
```

#### ReactiveNode Trait

```rust
pub trait ReactiveNode: Send + Sync {
    type Snapshot: Clone + Send;

    /// Current epoch (increments on any change)
    fn last_modified(&self) -> Epoch;

    /// Immutable snapshot for time-travel
    fn snapshot(&self) -> Self::Snapshot;

    /// Get patches since given epoch
    fn patches_since(&self, since: Epoch) -> PatchResult;

    /// Subscribe to patch stream
    fn subscribe_patches(&self) -> Receiver<Patch>;

    /// Apply external patch (for undo, sync)
    fn apply_patch(&mut self, patch: &Patch) -> Result<(), PatchError>;
}

pub enum PatchResult {
    Patches(Vec<Patch>),           // Incremental update possible
    TooOld,                        // Epoch predates history
    Snapshot(Value),               // Fall back to full value
}
```

#### Automatic Path Building

When a nested actor changes, paths build automatically as the change bubbles up:

```
subtask.done changes (false → true)
    │
    ├── subtask emits: Patch { path: [Field("done")], op: Replace }
    │
    ▼
parent actor receives, prepends index:
    │
    ├── Patch { path: [Index(1), Field("done")], op: Replace }
    │
    ▼
grandparent prepends field:
    │
    └── Patch { path: [Field("subtasks"), Index(1), Field("done")], op: Replace }
```

#### Incremental Computed Values

```rust
pub struct IncrementalComputed<T, Input: ReactiveNode> {
    input: Arc<Input>,
    cached_value: Option<T>,
    last_computed_epoch: Epoch,

    /// Full recompute function
    compute_full: Box<dyn Fn(&Input::Snapshot) -> T>,

    /// Optional incremental update (if None, falls back to full)
    compute_incremental: Option<Box<dyn Fn(&T, &[Patch]) -> Option<T>>>,
}

impl<T, Input: ReactiveNode> IncrementalComputed<T, Input> {
    pub fn get(&mut self) -> &T {
        let current_epoch = self.input.last_modified();

        if self.last_computed_epoch == current_epoch {
            return self.cached_value.as_ref().unwrap();
        }

        // Try incremental update first
        if let Some(ref incremental_fn) = self.compute_incremental {
            if let Some(cached) = &self.cached_value {
                match self.input.patches_since(self.last_computed_epoch) {
                    PatchResult::Patches(patches) => {
                        if let Some(updated) = incremental_fn(cached, &patches) {
                            self.cached_value = Some(updated);
                            self.last_computed_epoch = current_epoch;
                            return self.cached_value.as_ref().unwrap();
                        }
                    }
                    _ => {}
                }
            }
        }

        // Fall back to full recompute
        let snapshot = self.input.snapshot();
        self.cached_value = Some((self.compute_full)(&snapshot));
        self.last_computed_epoch = current_epoch;
        self.cached_value.as_ref().unwrap()
    }
}
```

#### Diff History Buffer

```rust
pub struct DiffHistory {
    patches: VecDeque<Patch>,
    capacity: usize,
    oldest_epoch: Epoch,
}

impl DiffHistory {
    pub fn push(&mut self, patch: Patch) {
        self.patches.push_back(patch);

        // Evict old entries if over capacity
        while self.patches.len() > self.capacity {
            if let Some(old) = self.patches.pop_front() {
                self.oldest_epoch = old.epoch + 1;
            }
        }
    }

    pub fn since(&self, epoch: Epoch) -> PatchResult {
        if epoch < self.oldest_epoch {
            return PatchResult::TooOld;
        }

        let patches: Vec<_> = self.patches
            .iter()
            .filter(|p| p.epoch > epoch)
            .cloned()
            .collect();

        PatchResult::Patches(patches)
    }
}
```

### 9.5 Architecture Synthesis

```
┌─────────────────────────────────────────────────────────────────────┐
│                        BOON ACTOR HIERARCHY                          │
│                     (MST-style tree semantics)                       │
│                                                                      │
│   ┌─────────────┐     ┌─────────────┐     ┌─────────────┐          │
│   │   Parent    │────▶│    Child    │────▶│  Grandchild │          │
│   │   Actor     │     │   Actor     │     │   Actor     │          │
│   │             │     │             │     │             │          │
│   │ DiffHistory │     │ DiffHistory │     │ DiffHistory │          │
│   └──────┬──────┘     └──────┬──────┘     └──────┬──────┘          │
│          │                   │                   │                  │
│          │                   │                   │                  │
│          ▼                   ▼                   ▼                  │
│   ┌──────────────────────────────────────────────────────────┐     │
│   │              PATCH STREAM (path-aware)                    │     │
│   │   { epoch: 42, path: [Index(0), Field("done")],          │     │
│   │     op: Replace { old: false, new: true } }              │     │
│   └──────────────────────────────────────────────────────────┘     │
│                              │                                      │
│                              ▼                                      │
│   ┌──────────────────────────────────────────────────────────┐     │
│   │            INCREMENTAL COMPUTED                           │     │
│   │                                                           │     │
│   │   completed_count: IncrementalComputed {                 │     │
│   │       compute_full: |todos| todos.iter()                 │     │
│   │                          .filter(|t| t.done).count(),    │     │
│   │       compute_incremental: |&old, patches| {             │     │
│   │           // Only adjust count based on done changes     │     │
│   │           patches.iter().fold(old, |acc, p| ...)         │     │
│   │       }                                                   │     │
│   │   }                                                       │     │
│   └──────────────────────────────────────────────────────────┘     │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

### 9.6 What This Enables

| Capability | Benefit | Example |
|------------|---------|---------|
| **Path-aware patches** | Precise change tracking | "Which todo's subtask changed?" |
| **Diff history** | Time-travel debugging | "What changed in last 5 updates?" |
| **Incremental derivations** | O(delta) for derived values | `completed_count` adjusts by ±1 |
| **Snapshots** | Undo/redo, persistence replay | Serialize/restore exact state |
| **External sync** | Collaborative editing | Merge patches from other clients |

### 9.7 Implementation Priority

| Phase | Task | Complexity | Impact |
|-------|------|------------|--------|
| **1** | Add `Epoch` to actors | Low | Foundation |
| **1** | Define `Patch` and `PathSegment` | Low | Foundation |
| **2** | Implement `DiffHistory` | Medium | Time-travel |
| **2** | Auto path building in subscriptions | Medium | Observability |
| **3** | `IncrementalComputed` wrapper | High | Performance |
| **3** | `apply_patch` for external changes | High | Undo/sync |

---

## Appendix A: Quick Reference - When to Use What

| Need | Best Source | Pattern |
|------|-------------|---------|
| O(delta) filter/map | DBSP | Z-set with weight propagation |
| Stop unnecessary updates | Incremental | Cutoff optimization |
| Consistent snapshots | RisingWave | Barrier-based epochs |
| State persistence | RisingWave | Async checkpoint to storage worker |
| Multi-worker comms | Timely | SharedArrayBuffer allocator |
| Complex joins | DD | Arrangements (indexed state) |
| Memory management | DD | Trace compaction |

## Appendix B: Glossary

| Term | Definition |
|------|------------|
| **Z-set** | Multiset with integer weights; element → ℤ |
| **Epoch** | Logical timestamp; all data in epoch is "simultaneous" |
| **Barrier** | Marker in stream that separates epochs |
| **Cutoff** | Optimization: stop propagation when value unchanged |
| **Arrangement** | Indexed, persistent collection for efficient lookups |
| **Trace** | History of changes over time (for time-travel queries) |
| **Compaction** | Merging old data to reduce storage/memory |
| **Lattice** | Set with merge operation (join/meet) |
| **Frontier** | Set of timestamps that might still arrive |
