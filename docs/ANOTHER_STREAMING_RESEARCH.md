# Streaming/Dataflow Libraries Research for Boon Engine

This document analyzes whether Timely Dataflow, Differential Dataflow, Hydroflow, or other Rust dataflow libraries could replace or augment Boon's actor-based engine.

## Overview

### The Question

Could we replace boon_actors' hand-rolled actor system with an established dataflow library like Timely Dataflow or Hydroflow?

### The Answer

**No for complete replacement, but YES for the transport layer.**

Boon's engine has two distinct layers:

| Layer | What It Does | Can Be Replaced? |
|-------|--------------|------------------|
| **Transport/Scheduling** | Message routing, parallelism, distribution | ✅ Yes - adopt Timely's `Allocate` trait |
| **Semantic** | HOLD, THEN, WHEN, WHILE, LATEST, LINK, UI integration | ❌ No - these are Boon-specific |

### Key Findings

1. **Timely Dataflow** is batch-oriented (`Vec<(data, time, diff)>`) — excellent for distributed data processing, but lacks per-event stateful semantics needed for HOLD
2. **Hydroflow** is simpler and WASM-first, but still has no UI primitives or Boon-specific combinators
3. **Differential Dataflow** excels at incremental collections — useful for List operations, not core reactivity
4. **Salsa** is pull-based only — no push notifications for UI reactivity

### Recommended Architecture

```
┌─────────────────────────────────────────────────────────┐
│              Boon Semantic Layer                        │
│   HOLD, THEN, WHEN, WHILE, LATEST, LINK, Element API    │
├─────────────────────────────────────────────────────────┤
│              Allocate Trait (from Timely)               │
├───────────────┬───────────────┬─────────────────────────┤
│ SharedArray   │  WebSocket    │        TCP              │
│ Buffer        │  (Browser↔    │    (Cluster)            │
│ (WebWorkers)  │   Server)     │                         │
└───────────────┴───────────────┴─────────────────────────┘
```

### Action Items

| Priority | Action |
|----------|--------|
| Short-term | Finish Lamport clock integration, study Timely's Allocate |
| Medium-term | Abstract message passing, implement SharedArrayBuffer allocator |
| Long-term | Multi-worker browser, cluster deployment, optional DD for Lists |

---

## Context

Boon's current engine (`boon_actors`) uses:
- **Lamport clocks** for deterministic ordering across actors
- **ValueActor / LazyValueActor** for reactive values
- **ActorLoop abstraction** for hardware-portable async processing
- **Channel-based communication** (no shared mutable state)
- **HOLD/THEN/WHEN/WHILE/LATEST combinators** for dataflow

Target environments include:
- **Browser** (WASM + WebWorkers + SharedArrayBuffer)
- **Server** (native Rust with multi-threading)
- **Distributed** (frontend-backend, cluster deployments)
- **Hardware** (FPGA, RISC-V - future target)

## Libraries Surveyed

### Tier 1: Production-Ready Distributed Dataflow

| Library | Focus | WASM Support | Distribution |
|---------|-------|--------------|--------------|
| **Timely Dataflow** | Low-level dataflow primitives | Possible (no_std core) | Built-in (Thread/Process/Cluster) |
| **Differential Dataflow** | Incremental computation on collections | Requires Timely | Built-in |
| **Materialize** | Streaming SQL database | Server-only | Kubernetes |
| **RisingWave** | Cloud-native streaming DB | Server-only | Cloud-native |
| **Arroyo** | Event processing with SQL | Server-only | Kubernetes |

### Tier 2: Research/Emerging

| Library | Focus | WASM Support | Distribution |
|---------|-------|--------------|--------------|
| **Hydroflow** | Semilattice-based dataflow (UC Berkeley) | Yes (hydroflow_plus) | Designed for it |
| **DBSP** | Formal Z-Set algebra | Possible | Not built-in |
| **Datafrog** | Datalog engine | Yes | No |

### Tier 3: Specialized

| Library | Focus | WASM Support | Distribution |
|---------|-------|--------------|--------------|
| **Salsa** | Incremental compilation (rust-analyzer) | Yes | No |
| **Adapton** | Demand-driven incremental | Yes | No |
| **crepe** | Datalog macro DSL | Yes | No |
| **ascent** | Datalog with aggregation | Yes | No |

## Deep Dive: Timely Dataflow

### Architecture

Timely Dataflow provides a **progress tracking** system based on **pointstamps** (timestamp, location) and **frontiers** (sets of pointstamps that may still arrive).

```rust
// Core abstraction: Worker with generic Allocator
pub trait Allocate: Clone {
    fn index(&self) -> usize;        // Worker index
    fn peers(&self) -> usize;        // Total workers
    fn allocate<T>(&mut self, ...) -> (Vec<Push<T>>, Pull<T>);
}

// Built-in allocators:
// - Thread (single-threaded, zero-copy)
// - Process (crossbeam channels)
// - Cluster (TCP sockets, 48-byte protocol)
```

### Progress Tracking

```
Pointstamp = (Timestamp, Location)
Frontier = Set<Pointstamp>  // "what might still arrive"

Progress Protocol:
1. Operator produces output → updates capability
2. Capabilities flow through graph
3. Frontier advances when no capabilities remain
4. Downstream operators notified of frontier changes
```

### Cyclic Dataflow

Timely supports cycles via **feedback operators**:

```rust
worker.dataflow(|scope| {
    let (handle, stream) = scope.feedback(1);  // delay = 1

    let result = input
        .concat(&stream)
        .filter(|x| *x > 0)
        .map(|x| x - 1);

    result.connect_loop(handle);
});
```

### Strengths for Boon

1. **Proven at scale** - Powers Materialize (production streaming SQL)
2. **Flexible allocation** - Same code runs single-threaded, multi-process, or distributed
3. **Formal progress tracking** - Guarantees about "no more data at time T"
4. **Cyclic dataflow** - Native support for iterative algorithms
5. **Low overhead** - 48-byte messages, zero-copy in-process

### Challenges for Boon

1. **Batch-oriented API** - Designed for `Vec<(data, time, diff)>` batches, not individual events
2. **No built-in WASM transport** - Would need SharedArrayBuffer allocator
3. **Complex to understand** - Frontiers/capabilities have learning curve
4. **No UI primitives** - Pure dataflow, no DOM/event concepts

## Deep Dive: Differential Dataflow

Built on Timely, adds **incremental collection maintenance**:

```rust
// Z-Set: (data, time, diff)
// diff > 0: insertion
// diff < 0: deletion
// diff = 0: no change (can be compacted away)

collection
    .map(|x| x + 1)
    .filter(|x| *x > 10)
    .reduce(|key, vals, output| {
        output.push((*vals.next().unwrap(), 1));
    })
```

### Arrangements

Indexed state that persists across batches:

```rust
let arranged = collection.arrange_by_key();

// Efficient joins using arrangement
other.join_core(&arranged, |k, v1, v2| Some((k, v1, v2)));
```

### Trace Compaction

Memory management via logical compaction:

```rust
// "I no longer need times before T"
arranged.trace.set_logical_compaction(&[new_frontier]);
// Trace merges older batches, reducing memory
```

## Deep Dive: Hydroflow

UC Berkeley's **Hydro project** - formal semantics based on semilattices:

```rust
// hydroflow! macro DSL
hydroflow! {
    source_iter([1, 2, 3]) -> map(|x| x * 2) -> for_each(|x| println!("{}", x));
}
```

### Key Differentiators

1. **Formal proofs** - POPL papers proving correctness properties
2. **WASM-first** - `hydroflow_plus` designed for browser/distributed
3. **Semilattice types** - Guarantees monotonicity and convergence
4. **Simpler than Timely** - Less powerful but easier to understand

### Lattice-Based Consistency

```rust
// Example: Max lattice for distributed counter
struct MaxLattice<T: Ord>(T);

impl<T: Ord> Merge for MaxLattice<T> {
    fn merge(&mut self, other: Self) {
        if other.0 > self.0 {
            self.0 = other.0;
        }
    }
}
```

## Deep Dive: Salsa

Rust-analyzer's incremental computation framework:

```rust
#[salsa::query_group(DatabaseStorage)]
trait Database {
    #[salsa::input]
    fn source_text(&self, name: String) -> Arc<String>;

    fn parse(&self, name: String) -> Arc<Ast>;  // Derived, memoized
}
```

### Red-Green Algorithm

```
1. Mark all dependent queries RED (potentially stale)
2. Re-execute query
3. If result unchanged, mark GREEN (cutoff - don't propagate)
4. If result changed, propagate to dependents
```

### Durability Levels

```rust
#[salsa::input(durability = "HIGH")]   // Rarely changes (config)
#[salsa::input(durability = "MEDIUM")] // Sometimes changes (files)
#[salsa::input(durability = "LOW")]    // Frequently changes (editor buffer)
```

### Why Not Salsa for Boon

- **Pull-based only** - No push notifications (must poll)
- **Single-threaded** - No parallelism story
- **No distribution** - Local computation only
- **Compile-time focus** - Optimized for expensive pure computations, not UI

## Comparison: boon_actors vs Timely/Hydroflow

| Aspect | boon_actors | Timely Dataflow | Hydroflow |
|--------|-------------|-----------------|-----------|
| **Ordering** | Lamport clocks | Logical timestamps + frontiers | Semilattice merge |
| **Parallelism** | Planned (SharedArrayBuffer) | Built-in (Allocate trait) | Built-in (hydroflow_plus) |
| **Distribution** | Planned (WebSocket) | Built-in (TCP) | Built-in |
| **Cycles** | HOLD combinator | feedback() operator | Lattice fixpoint |
| **UI Integration** | Native (LINK, Element API) | None | None |
| **WASM** | Yes | Possible (needs allocator) | Yes |
| **Complexity** | Medium | High | Low-Medium |
| **Maturity** | Research | Production | Research |

### What boon_actors Has That Others Don't

1. **UI-first design** - LINK pattern, Element API, DOM integration
2. **HOLD semantics** - Stateful accumulator with self-reference
3. **PASS context** - Implicit data flow for configuration
4. **Text interpolation** - `TEXT { Hello {name} }` syntax
5. **Persistence IDs** - Automatic durable state via ULID

### What Timely/Hydroflow Have That boon_actors Doesn't

1. **Proven distribution** - Battle-tested at scale
2. **Formal progress tracking** - Know when "time T is complete"
3. **Arrangement/indexing** - Efficient joins and lookups
4. **Trace compaction** - Automatic memory management
5. **Type-level proofs** - (Hydroflow) Semilattice guarantees

## Browser Parallelism: SharedArrayBuffer

For WASM parallelism, SharedArrayBuffer enables shared memory between WebWorkers:

### Requirements

```
// HTTP headers required:
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

### Rust Integration

```rust
// wasm-bindgen-rayon pattern
use wasm_bindgen_rayon::init_thread_pool;

#[wasm_bindgen]
pub async fn init_parallel(num_threads: usize) {
    init_thread_pool(num_threads).await;
}
```

### Atomics API

```javascript
// Wait for value change (blocking)
Atomics.wait(sharedArray, index, expectedValue);

// Notify waiting threads
Atomics.notify(sharedArray, index, count);

// Non-blocking wait (async)
Atomics.waitAsync(sharedArray, index, expectedValue);
```

### Proposed SharedArrayBuffer Allocator for Timely

```rust
pub struct SharedArrayBufferAllocator {
    worker_index: usize,
    total_workers: usize,
    shared_memory: SharedArrayBuffer,
    // Ring buffers in shared memory for each channel
}

impl Allocate for SharedArrayBufferAllocator {
    fn allocate<T>(&mut self, identifier: usize)
        -> (Vec<Push<T>>, Pull<T>)
    {
        // Create ring buffer views into SharedArrayBuffer
        // Use Atomics for synchronization
    }
}
```

## Distribution: Frontend-Backend-Cluster

### Unified Transport Layer

```
┌─────────────────────────────────────────────────────────┐
│                    Boon Program                          │
├─────────────────────────────────────────────────────────┤
│                 Allocate Trait                           │
├───────────────┬───────────────┬─────────────────────────┤
│ SharedArray   │  WebSocket    │        TCP              │
│ Buffer        │  (Browser↔    │    (Cluster)            │
│ (WebWorkers)  │   Server)     │                         │
└───────────────┴───────────────┴─────────────────────────┘
```

### Electric Clojure Model

Network-transparent reactivity:
```clojure
; Same code runs client, server, or both
(e/def counter (e/server (atom 0)))

(e/defn App []
  (e/client
    (let [c (e/server @counter)]
      (dom/button
        (dom/on "click" (e/fn [_]
          (e/server (swap! counter inc))))
        (dom/text c)))))
```

This is the closest existing model to Boon's vision. Electric achieves this via:
1. **Compiler analysis** - Determines what runs where
2. **Automatic serialization** - Data crosses network boundaries
3. **Reactive consistency** - Server state changes push to clients

## Hybrid Architecture Proposal

Rather than wholesale replacement, adopt Timely's **core abstractions** while keeping boon_actors' **domain-specific features**:

### Phase 1: Adopt Allocate Trait

```rust
// Keep existing ValueActor/LazyValueActor
// But route messages through Allocate

pub struct BoonWorker<A: Allocate> {
    allocator: A,
    actors: Arena<Actor>,
    lamport_clock: u64,
}

impl<A: Allocate> BoonWorker<A> {
    fn send_to_actor(&mut self, actor_id: ActorId, msg: Message) {
        // Use allocator for cross-worker messages
        // Use direct Arena access for local messages
    }
}
```

### Phase 2: Implement SharedArrayBuffer Allocator

```rust
#[cfg(target_arch = "wasm32")]
pub struct WasmAllocator { ... }

#[cfg(target_arch = "wasm32")]
impl Allocate for WasmAllocator {
    // SharedArrayBuffer + Atomics implementation
}
```

### Phase 3: Add WebSocket Allocator

```rust
pub struct WebSocketAllocator {
    connections: Vec<WebSocket>,
    worker_index: usize,
}

impl Allocate for WebSocketAllocator {
    // WebSocket-based message passing
    // Compatible with browser and server
}
```

### Phase 4: Unified Deployment

```rust
fn main() {
    let allocator = match deployment_mode() {
        Single => ThreadAllocator::new(1),
        WebWorkers(n) => WasmAllocator::new(n),
        Server(n) => ProcessAllocator::new(n),
        Cluster(hosts) => TcpAllocator::new(hosts),
        Hybrid { browsers, servers } => CompositeAllocator::new(...),
    };

    BoonRuntime::new(allocator).run(program);
}
```

## Key Questions Answered

### Could Timely Dataflow replace boon_actors entirely?

**Partially.** Timely provides excellent foundations for:
- Message routing and progress tracking
- Multi-worker coordination
- Cyclic dataflow

But it lacks:
- UI primitives (LINK, Element API)
- HOLD semantics (would need custom operator)
- Text interpolation
- Persistence abstraction

**Recommendation**: Use Timely's `Allocate` trait as the transport layer, keep boon_actors' semantic layer.

### Could Hydroflow replace boon_actors entirely?

**More likely than Timely.** Hydroflow is:
- Simpler to understand
- WASM-first design
- Actively developed for distributed browser scenarios

But still lacks:
- UI integration
- Boon-specific combinators

**Recommendation**: Study hydroflow_plus's WASM distribution model, potentially adopt similar patterns.

### Should we use Differential Dataflow?

**For specific use cases.** DD excels at:
- Collection-based queries (lists, tables)
- Incremental joins
- Time-traveling queries

**Recommendation**: Consider DD for Boon's List operations and query capabilities, not for core reactive engine.

---

## Code Sketches: Integration Approaches

This section provides concrete code examples showing how each library could integrate with Boon.

### Sketch 1: Boon Allocator Trait (Inspired by Timely)

First, define Boon's own allocator abstraction based on Timely's design:

```rust
//! crates/boon/src/engine/allocator.rs

use futures::Stream;

/// Message envelope with Lamport timestamp
#[derive(Clone, Debug)]
pub struct Envelope<T> {
    pub payload: T,
    pub lamport_time: u64,
    pub source_worker: usize,
}

/// Push endpoint for sending messages
pub trait Push<T>: Send {
    fn push(&mut self, message: Envelope<T>);
    fn flush(&mut self);
}

/// Pull endpoint for receiving messages
pub trait Pull<T>: Send {
    fn pull(&mut self) -> Option<Envelope<T>>;
    /// Async version for use in actor loops
    fn pull_async(&mut self) -> impl Future<Output = Envelope<T>> + Send;
}

/// Core allocator trait - creates communication channels
pub trait Allocate: Clone + Send + 'static {
    /// This worker's index (0..peers)
    fn index(&self) -> usize;

    /// Total number of workers
    fn peers(&self) -> usize;

    /// Allocate a new channel with given identifier
    /// Returns: (senders to each peer, receiver from all peers)
    fn allocate<T: Send + Clone + 'static>(
        &mut self,
        identifier: usize,
    ) -> (Vec<Box<dyn Push<T>>>, Box<dyn Pull<T>>);

    /// Broadcast to all workers (including self)
    fn broadcast<T: Send + Clone + 'static>(
        &mut self,
        identifier: usize,
    ) -> (Box<dyn Push<T>>, Box<dyn Pull<T>>);
}
```

### Sketch 2: Single-Threaded Allocator (Current Behavior)

```rust
//! Single-threaded allocator - zero overhead, current behavior

use std::collections::VecDeque;
use std::cell::RefCell;
use std::rc::Rc;

pub struct ThreadAllocator {
    // Single worker, no actual message passing needed
}

impl Allocate for ThreadAllocator {
    fn index(&self) -> usize { 0 }
    fn peers(&self) -> usize { 1 }

    fn allocate<T: Send + Clone + 'static>(
        &mut self,
        _identifier: usize,
    ) -> (Vec<Box<dyn Push<T>>>, Box<dyn Pull<T>>) {
        // Direct channel - push goes straight to pull
        let buffer = Rc::new(RefCell::new(VecDeque::new()));

        let pusher = DirectPush { buffer: buffer.clone() };
        let puller = DirectPull { buffer };

        (vec![Box::new(pusher)], Box::new(puller))
    }
}

struct DirectPush<T> {
    buffer: Rc<RefCell<VecDeque<Envelope<T>>>>,
}

impl<T> Push<T> for DirectPush<T> {
    fn push(&mut self, message: Envelope<T>) {
        self.buffer.borrow_mut().push_back(message);
    }
    fn flush(&mut self) {} // No-op for direct
}

struct DirectPull<T> {
    buffer: Rc<RefCell<VecDeque<Envelope<T>>>>,
}

impl<T> Pull<T> for DirectPull<T> {
    fn pull(&mut self) -> Option<Envelope<T>> {
        self.buffer.borrow_mut().pop_front()
    }

    async fn pull_async(&mut self) -> Envelope<T> {
        loop {
            if let Some(msg) = self.pull() {
                return msg;
            }
            // Yield to other tasks
            futures::future::yield_now().await;
        }
    }
}
```

### Sketch 3: SharedArrayBuffer Allocator (WebWorker Parallelism)

```rust
//! WASM allocator using SharedArrayBuffer for WebWorker parallelism

use wasm_bindgen::prelude::*;
use js_sys::{SharedArrayBuffer, Atomics, Int32Array};

pub struct WasmAllocator {
    worker_index: usize,
    total_workers: usize,
    shared_memory: SharedArrayBuffer,
    /// Ring buffer metadata: [write_pos, read_pos, capacity] per channel
    channel_offsets: Vec<usize>,
}

impl WasmAllocator {
    pub fn new(worker_index: usize, total_workers: usize) -> Self {
        // Allocate shared memory (e.g., 16MB)
        let shared_memory = SharedArrayBuffer::new(16 * 1024 * 1024);

        Self {
            worker_index,
            total_workers,
            shared_memory,
            channel_offsets: Vec::new(),
        }
    }
}

impl Allocate for WasmAllocator {
    fn index(&self) -> usize { self.worker_index }
    fn peers(&self) -> usize { self.total_workers }

    fn allocate<T: Send + Clone + 'static>(
        &mut self,
        identifier: usize,
    ) -> (Vec<Box<dyn Push<T>>>, Box<dyn Pull<T>>) {
        // Create ring buffer in shared memory for each peer
        let mut pushers: Vec<Box<dyn Push<T>>> = Vec::new();

        for peer in 0..self.total_workers {
            let ring_buffer = SharedRingBuffer::new(
                self.shared_memory.clone(),
                self.compute_offset(identifier, self.worker_index, peer),
            );
            pushers.push(Box::new(SharedPush { ring_buffer }));
        }

        // Puller reads from all peers
        let puller = SharedPull {
            ring_buffers: (0..self.total_workers)
                .map(|peer| SharedRingBuffer::new(
                    self.shared_memory.clone(),
                    self.compute_offset(identifier, peer, self.worker_index),
                ))
                .collect(),
        };

        (pushers, Box::new(puller))
    }
}

/// Ring buffer backed by SharedArrayBuffer
struct SharedRingBuffer {
    buffer: Int32Array,  // View into SharedArrayBuffer
    write_idx: usize,    // Offset of write position atomic
    read_idx: usize,     // Offset of read position atomic
    data_start: usize,   // Offset where data begins
    capacity: usize,
}

impl SharedRingBuffer {
    fn push(&self, data: &[u8]) {
        // 1. Atomically read write position
        let write_pos = Atomics::load(&self.buffer, self.write_idx as u32);

        // 2. Write data to ring buffer
        // (serialize T to bytes, copy to shared memory)

        // 3. Atomically update write position
        Atomics::store(&self.buffer, self.write_idx as u32, new_pos);

        // 4. Wake up waiting readers
        Atomics::notify(&self.buffer, self.write_idx as u32, 1);
    }

    async fn pull(&self) -> Option<Vec<u8>> {
        loop {
            let write_pos = Atomics::load(&self.buffer, self.write_idx as u32);
            let read_pos = Atomics::load(&self.buffer, self.read_idx as u32);

            if write_pos > read_pos {
                // Data available - read it
                // ...
                return Some(data);
            }

            // Wait for new data (non-blocking in async context)
            let wait_result = Atomics::wait_async(
                &self.buffer,
                self.write_idx as u32,
                write_pos,
            );
            wait_result.await;
        }
    }
}
```

### Sketch 4: WebSocket Allocator (Frontend ↔ Backend)

```rust
//! WebSocket allocator for browser-server communication

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::WebSocketStream;

pub struct WebSocketAllocator {
    worker_index: usize,
    total_workers: usize,
    connections: Vec<WebSocketStream<...>>,
}

impl Allocate for WebSocketAllocator {
    fn index(&self) -> usize { self.worker_index }
    fn peers(&self) -> usize { self.total_workers }

    fn allocate<T: Send + Clone + Serialize + DeserializeOwned + 'static>(
        &mut self,
        identifier: usize,
    ) -> (Vec<Box<dyn Push<T>>>, Box<dyn Pull<T>>) {
        let mut pushers = Vec::new();

        for (peer_idx, ws) in self.connections.iter().enumerate() {
            pushers.push(Box::new(WebSocketPush {
                sink: ws.clone(),
                channel_id: identifier,
            }));
        }

        let puller = WebSocketPull {
            streams: self.connections.iter()
                .map(|ws| ws.clone())
                .collect(),
            channel_id: identifier,
        };

        (pushers, Box::new(puller))
    }
}

struct WebSocketPush<T> {
    sink: SplitSink<WebSocketStream<...>, Message>,
    channel_id: usize,
}

impl<T: Serialize> Push<T> for WebSocketPush<T> {
    fn push(&mut self, message: Envelope<T>) {
        let wire_msg = WireMessage {
            channel_id: self.channel_id,
            lamport_time: message.lamport_time,
            payload: bincode::serialize(&message.payload).unwrap(),
        };

        // Fire-and-forget with backpressure
        let _ = self.sink.send(Message::Binary(
            bincode::serialize(&wire_msg).unwrap()
        ));
    }
}
```

### Sketch 5: Integrating Allocator with ValueActor

```rust
//! How ValueActor uses the Allocator

pub struct BoonRuntime<A: Allocate> {
    allocator: A,
    actors: Arena<ActorNode>,
    lamport_clock: u64,
    next_channel_id: usize,
}

impl<A: Allocate> BoonRuntime<A> {
    /// Create a new ValueActor with cross-worker communication
    pub fn create_actor<S>(&mut self, stream: S) -> ActorId
    where
        S: Stream<Item = Value> + Send + 'static
    {
        // Allocate channels for this actor
        let channel_id = self.next_channel_id;
        self.next_channel_id += 1;

        let (pushers, puller) = self.allocator.allocate::<Value>(channel_id);

        // Create actor with communication endpoints
        let actor = ActorNode {
            pushers,
            puller,
            source_stream: Box::pin(stream),
            subscribers: Vec::new(),
        };

        let id = self.actors.insert(actor);

        // Start actor loop
        self.spawn_actor_loop(id);

        id
    }

    /// Subscribe to an actor (possibly on different worker)
    pub fn subscribe(&mut self, actor_id: ActorId) -> impl Stream<Item = Value> {
        // If actor is local, direct subscription
        if self.is_local(actor_id) {
            return self.local_subscribe(actor_id);
        }

        // If actor is remote, use allocator channels
        let (reply_push, reply_pull) = self.allocator.allocate(self.next_channel_id);
        self.next_channel_id += 1;

        // Send subscription request to remote worker
        self.send_to_worker(
            actor_id.worker_index(),
            SubscribeRequest { actor_id, reply_channel: reply_push }
        );

        // Return stream from reply channel
        stream::unfold(reply_pull, |mut pull| async move {
            let envelope = pull.pull_async().await;
            Some((envelope.payload, pull))
        })
    }
}
```

### Sketch 6: Hydroflow-Style Lattice for HOLD

```rust
//! Using Hydroflow's lattice concepts for HOLD semantics

use std::cmp::Ordering;

/// Lattice trait from Hydroflow
pub trait Lattice: Clone + PartialOrd {
    /// Merge another value into self (must be monotonic)
    fn merge(&mut self, other: Self);

    /// Check if self is "bottom" (identity element)
    fn is_bottom(&self) -> bool;
}

/// HOLD state as a lattice: (lamport_time, value)
/// Ordering: higher lamport_time wins
#[derive(Clone)]
pub struct HoldState<T> {
    pub lamport_time: u64,
    pub value: T,
}

impl<T: Clone> Lattice for HoldState<T> {
    fn merge(&mut self, other: Self) {
        // Later timestamp wins (total order via Lamport clock)
        if other.lamport_time > self.lamport_time {
            *self = other;
        }
    }

    fn is_bottom(&self) -> bool {
        self.lamport_time == 0
    }
}

impl<T: Clone> PartialOrd for HoldState<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.lamport_time.partial_cmp(&other.lamport_time)
    }
}

/// HOLD combinator using lattice semantics
pub fn hold_lattice<T: Clone + Send + 'static>(
    initial: T,
    body_stream: impl Stream<Item = T> + Send + 'static,
) -> impl Stream<Item = T> {
    let initial_state = HoldState {
        lamport_time: lamport_tick(),
        value: initial,
    };

    stream::unfold(
        (initial_state.clone(), Box::pin(body_stream)),
        move |(mut state, mut body)| async move {
            // Emit current state first
            let output = state.value.clone();

            // Wait for next body value
            if let Some(new_value) = body.next().await {
                let new_state = HoldState {
                    lamport_time: lamport_tick(),
                    value: new_value,
                };

                // Lattice merge (newer always wins for HoldState)
                state.merge(new_state);
            }

            Some((output, (state, body)))
        }
    )
}
```

### Sketch 7: Differential Dataflow for Boon Lists

```rust
//! Using DD concepts for incremental List operations

/// Z-Set: (element, time, diff)
/// diff = +1 for insert, -1 for delete
#[derive(Clone, Debug)]
pub struct ZSet<T> {
    changes: Vec<(T, u64, i64)>,  // (value, lamport_time, diff)
}

impl<T: Eq + Hash + Clone> ZSet<T> {
    pub fn insert(&mut self, value: T, time: u64) {
        self.changes.push((value, time, 1));
    }

    pub fn remove(&mut self, value: T, time: u64) {
        self.changes.push((value, time, -1));
    }

    /// Compact changes up to given time
    pub fn compact(&mut self, frontier: u64) {
        // Group by value, sum diffs for times <= frontier
        let mut consolidated: HashMap<T, i64> = HashMap::new();

        self.changes.retain(|(value, time, diff)| {
            if *time <= frontier {
                *consolidated.entry(value.clone()).or_default() += diff;
                false
            } else {
                true
            }
        });

        // Re-add non-zero consolidated values at frontier time
        for (value, total_diff) in consolidated {
            if total_diff != 0 {
                self.changes.push((value, frontier, total_diff));
            }
        }
    }

    /// Materialize to actual collection
    pub fn materialize(&self) -> Vec<T> {
        let mut counts: HashMap<T, i64> = HashMap::new();

        for (value, _time, diff) in &self.changes {
            *counts.entry(value.clone()).or_default() += diff;
        }

        counts.into_iter()
            .filter(|(_, count)| *count > 0)
            .flat_map(|(value, count)| std::iter::repeat(value).take(count as usize))
            .collect()
    }
}

/// Incremental map operation on Z-Set
pub fn zset_map<T, U, F>(input: ZSet<T>, f: F) -> ZSet<U>
where
    T: Clone,
    U: Clone,
    F: Fn(T) -> U,
{
    ZSet {
        changes: input.changes
            .into_iter()
            .map(|(value, time, diff)| (f(value), time, diff))
            .collect()
    }
}

/// Incremental filter
pub fn zset_filter<T, F>(input: ZSet<T>, predicate: F) -> ZSet<T>
where
    T: Clone,
    F: Fn(&T) -> bool,
{
    ZSet {
        changes: input.changes
            .into_iter()
            .filter(|(value, _, _)| predicate(value))
            .collect()
    }
}

/// Boon List using Z-Set internally
pub struct ReactiveList<T> {
    zset: ZSet<(usize, T)>,  // (index, value)
    current_time: u64,
}

impl<T: Clone + Eq + Hash> ReactiveList<T> {
    pub fn push(&mut self, value: T) {
        let index = self.len();
        self.current_time = lamport_tick();
        self.zset.insert((index, value), self.current_time);
    }

    pub fn remove(&mut self, index: usize) {
        // Find value at index and remove
        if let Some(value) = self.get(index) {
            self.current_time = lamport_tick();
            self.zset.remove((index, value), self.current_time);
        }
    }

    /// Get incremental diff since last observation
    pub fn changes_since(&self, since: u64) -> Vec<ListChange<T>> {
        self.zset.changes
            .iter()
            .filter(|(_, time, _)| *time > since)
            .map(|((idx, val), _, diff)| {
                if *diff > 0 {
                    ListChange::Insert { index: *idx, value: val.clone() }
                } else {
                    ListChange::Remove { index: *idx }
                }
            })
            .collect()
    }
}
```

### Sketch 8: Unified Runtime Selection

```rust
//! Select allocator based on deployment environment

pub enum DeploymentMode {
    /// Single-threaded (current behavior, tests)
    SingleThread,
    /// WebWorkers with SharedArrayBuffer
    WebWorkers { count: usize },
    /// Native multi-process
    MultiProcess { count: usize },
    /// TCP cluster
    Cluster { hosts: Vec<String> },
    /// Hybrid: browser + server
    Hybrid { frontend_workers: usize, backend_hosts: Vec<String> },
}

pub fn create_runtime(mode: DeploymentMode) -> Box<dyn BoonRuntimeTrait> {
    match mode {
        DeploymentMode::SingleThread => {
            Box::new(BoonRuntime::new(ThreadAllocator::new()))
        }

        #[cfg(target_arch = "wasm32")]
        DeploymentMode::WebWorkers { count } => {
            Box::new(BoonRuntime::new(WasmAllocator::new(
                current_worker_index(),
                count,
            )))
        }

        #[cfg(not(target_arch = "wasm32"))]
        DeploymentMode::MultiProcess { count } => {
            Box::new(BoonRuntime::new(ProcessAllocator::new(count)))
        }

        DeploymentMode::Cluster { hosts } => {
            Box::new(BoonRuntime::new(TcpAllocator::new(hosts)))
        }

        DeploymentMode::Hybrid { frontend_workers, backend_hosts } => {
            // Composite allocator that routes based on actor location
            Box::new(BoonRuntime::new(HybridAllocator::new(
                frontend_workers,
                backend_hosts,
            )))
        }
    }
}

// Usage in Boon program
fn main() {
    let mode = DeploymentMode::from_env();
    let runtime = create_runtime(mode);

    // Same Boon code runs everywhere!
    runtime.run(|ctx| {
        let counter = ctx.hold(0, |state| {
            ctx.button("Increment")
                .link()
                .then(|| state + 1)
        });

        ctx.text(format!("Count: {}", counter))
    });
}
```

---

## Recommendations

### Short Term (0-3 months)

1. **Finish current architecture** - Complete Lamport clock integration, stabilize API
2. **Study Timely's Allocate trait** - Understand abstraction, plan integration
3. **Prototype SharedArrayBuffer transport** - Prove WASM parallelism works

### Medium Term (3-6 months)

4. **Implement Allocate-compatible layer** - Abstract message passing
5. **Add WebSocket allocator** - Enable frontend-backend reactivity
6. **Consider Hydroflow for formal verification** - Semilattice proofs for HOLD/LATEST

### Long Term (6-12 months)

7. **Multi-worker browser runtime** - Full SharedArrayBuffer support
8. **Cluster deployment** - TCP-based distribution
9. **Differential Dataflow integration** - For collection queries

## Conclusion

The ideal architecture combines:

| Layer | Technology | Purpose |
|-------|------------|---------|
| **Transport** | Timely's Allocate trait | Unified message passing across all deployment modes |
| **Ordering** | Lamport clocks | Deterministic replay, debugging |
| **Semantics** | boon_actors combinators | HOLD, THEN, WHEN, WHILE, LATEST, LINK |
| **Collections** | Differential Dataflow (optional) | Incremental list/table operations |
| **Verification** | Hydroflow-style lattices (optional) | Formal proofs for core combinators |

This gives us:
- **Portability**: Same code runs everywhere
- **Performance**: Parallelism where available
- **Correctness**: Formal foundations where needed
- **Expressiveness**: Boon's unique UI-first semantics

The actor-based model is not obsolete—it provides the semantic layer. What changes is the underlying transport and scheduling, which can adopt proven abstractions from Timely without losing Boon's unique characteristics.
