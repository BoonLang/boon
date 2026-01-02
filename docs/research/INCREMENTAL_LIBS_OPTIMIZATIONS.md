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
