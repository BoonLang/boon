# Pure Actor Model Architecture

**Date**: 2025-12-14
**Status**: Implemented

---

## Overview

Boon's runtime uses a **pure actor model** where only two constructs are allowed for async processing:

1. **Pure Streams** (combinators) - Wires in hardware
2. **Actors** (explicit types) - Finite State Machines in hardware

This architecture ensures Boon code can be:
- Compiled to HVM (interaction nets)
- Synthesized to hardware (FPGAs)
- Run on different runtimes (WebWorkers, clusters, etc.)

---

## Core Principle

**Only Actors can be async processing units.** No raw `Task::start_droppable` calls scattered through code.

```
Correct Model:
┌─────────┐     wire      ┌─────────┐
│ Actor A │──────────────→│ Actor B │
└─────────┘  (combinator) └─────────┘
     FSM      pure logic       FSM

Wrong Model (avoided):
┌─────────┐    ┌────────┐    ┌─────────┐
│ Actor A │───→│ Task   │───→│ Actor B │
└─────────┘    └────────┘    └─────────┘
     FSM       raw Task        FSM
```

Raw Tasks scattered through code:
- Not mappable to hardware (no corresponding FSM type)
- Not portable to HVM (spawn is runtime-specific)
- Hidden complexity (is it an actor? a wire? neither?)

---

## The Two Constructs

### 1. Pure Streams (Wires)

Stateless or stateful-but-demand-driven transformations using stream combinators:

```rust
// Pure stream combinator - no Task spawn
pub fn switch_map<S, F, U>(outer: S, f: F) -> impl Stream<Item = U::Item>
where
    S: Stream + 'static,
    F: Fn(S::Item) -> U + 'static,
    U: Stream + 'static,
{
    stream::unfold(initial_state, |mut state| async move {
        // State machine logic with select!
        // Only executes when downstream polls
        loop {
            select! {
                outer_item = state.outer.next() => { /* handle */ }
                inner_item = state.inner.next() => { /* handle */ }
            }
        }
    })
}
```

**Characteristics:**
- Use `stream::unfold()`, `map()`, `filter()`, `scan()`, etc.
- Only execute when downstream polls (demand-driven)
- No independent async loop
- **Hardware mapping**: combinational logic, muxes, wires

### 2. Actors (FSMs)

Explicit actor types with well-defined interfaces:

```rust
/// ValueActor - the fundamental reactive unit
pub struct ValueActor {
    // ... fields
    actor_loop: ActorLoop,  // Encapsulates the async loop
}

/// LazyValueActor - demand-driven evaluation
pub struct LazyValueActor {
    // ... fields
    actor_loop: ActorLoop,
}

/// List - reactive list with diff tracking
pub struct List {
    // ... fields
    actor_loop: ActorLoop,
}
```

**Characteristics:**
- Independent async processing loop
- Explicit communication channels (mpsc)
- Well-defined lifecycle
- **Hardware mapping**: registers, state machines

---

## ActorLoop Abstraction

`ActorLoop` is the ONE place where `Task::start_droppable` is allowed:

```rust
/// Encapsulates the async loop that makes an Actor an Actor.
///
/// This abstraction keeps `Task::start_droppable` in ONE place,
/// making the codebase portable to different runtimes (HVM, hardware, etc.).
pub struct ActorLoop {
    handle: TaskHandle,
}

impl ActorLoop {
    pub fn new(future: impl Future<Output = ()> + 'static) -> Self {
        Self {
            handle: Task::start_droppable(future),
        }
    }
}
```

**Why this matters:**

1. **Single point of change**: If we need to change how actors are spawned (different runtime), we only change `ActorLoop::new()`.

2. **Clear semantics**: `ActorLoop` explicitly marks "this is an actor's processing loop" vs a random background task.

3. **Lifetime management**: The `ActorLoop` owns the task handle. When the actor is dropped, the loop is cancelled.

4. **HVM compilation**: `ActorLoop` can be compiled to an HVM interaction net node.

---

## Actor Types in Boon

### ValueActor

The fundamental reactive unit. Every expression evaluates to a `ValueActor`.

```rust
pub struct ValueActor {
    construct_info: Arc<ConstructInfoComplete>,
    current_value: Arc<ArcSwap<Option<Value>>>,
    current_version: Arc<AtomicU64>,
    notify_sender_sender: mpsc::UnboundedSender<mpsc::Sender<()>>,
    notify_senders: Arc<std::sync::Mutex<Vec<mpsc::Sender<()>>>>,
    actor_loop: ActorLoop,  // The actor's processing loop
    // ...
}
```

### LazyValueActor

Demand-driven evaluation for sequential state updates in HOLD.

```rust
pub struct LazyValueActor {
    construct_info: Arc<ConstructInfoComplete>,
    request_tx: mpsc::UnboundedSender<LazyValueRequest>,
    actor_loop: ActorLoop,  // Handles demand-driven value delivery
    // ...
}
```

### List

Reactive list with diff-based updates.

```rust
pub struct List {
    construct_info: Arc<ConstructInfoComplete>,
    actor_loop: ActorLoop,  // Handles list changes and subscriptions
    diff_history: Arc<RefCell<DiffHistory>>,
    // ...
}
```

### Infrastructure Actors

Internal actors that provide runtime services:

```rust
/// Storage for persistent state
pub struct ConstructStorage {
    actor_loop: ActorLoop,
    // ...
}

/// Resolves forward references in objects
pub struct ReferenceConnector {
    actor_loop: ActorLoop,
    // ...
}

/// Resolves LINK connections
pub struct LinkConnector {
    actor_loop: ActorLoop,
    // ...
}

/// Broadcasts output valve signals to subscribers
pub struct ActorOutputValveSignal {
    actor_loop: ActorLoop,
    // ...
}
```

---

## Pure Stream Functions

Stream functions in `api.rs` use `stream::unfold()` instead of spawning tasks:

### Stream/skip

```rust
pub fn function_stream_skip(
    stream_actor: Arc<ValueActor>,
    count_actor: Arc<ValueActor>,
    // ...
) -> impl Stream<Item = Value> {
    struct SkipState {
        stream: Fuse<LocalBoxStream<'static, Value>>,
        count: Fuse<LocalBoxStream<'static, Value>>,
        skip_count: usize,
        skipped: usize,
        // ...
    }

    stream::unfold(initial_state, |mut state| async move {
        loop {
            select! {
                count_value = state.count.next() => { /* update skip count */ }
                stream_value = state.stream.next() => { /* skip or emit */ }
            }
        }
    })
}
```

### Stream/take

Same pattern as skip - pure stream combinator.

### Stream/debounce

```rust
pub fn function_stream_debounce(
    input_actor: Arc<ValueActor>,
    duration_actor: Arc<ValueActor>,
    // ...
) -> impl Stream<Item = Value> {
    stream::unfold(initial_state, |mut state| async move {
        loop {
            match state.pending.take() {
                Some(pending) => {
                    let timer = Timer::sleep(state.duration_ms);
                    select! {
                        new_value = state.input.next() => { state.pending = new_value; }
                        _ = timer => { return Some((pending, state)); }
                    }
                }
                None => { /* wait for input */ }
            }
        }
    })
}
```

### switch_map

Internal stream combinator used by WHILE and other constructs:

```rust
pub fn switch_map<S, F, U>(outer: S, f: F) -> LocalBoxStream<'static, U::Item>
where
    S: Stream + 'static,
    F: Fn(S::Item) -> U + 'static,
    U: Stream + 'static,
{
    stream::unfold(
        (outer.boxed_local().fuse(), None, f),
        |state| async move {
            // State machine that switches inner streams
        },
    )
    .boxed_local()
}
```

---

## Forwarding Pattern

When an actor needs to forward values from a source (e.g., for forward references):

```rust
impl ValueActor {
    /// Connect a forwarding actor to its source actor.
    ///
    /// Returns an ActorLoop that must be kept alive for forwarding to continue.
    pub fn connect_forwarding(
        forwarding_sender: mpsc::UnboundedSender<Value>,
        source_actor: Arc<ValueActor>,
        initial_value: Option<Value>,
    ) -> ActorLoop {
        // Send initial value synchronously if provided
        if let Some(value) = initial_value {
            let _ = forwarding_sender.unbounded_send(value);
        }

        ActorLoop::new(async move {
            let mut subscription = source_actor.subscribe();
            while let Some(value) = subscription.next().await {
                if forwarding_sender.unbounded_send(value).is_err() {
                    break;
                }
            }
        })
    }
}
```

Usage in evaluator for object field forward references:

```rust
let forwarding_loop = ValueActor::connect_forwarding(
    sender.clone(),
    source_actor.clone(),
    initial_value,
);
Variable::new_arc_with_forwarding_loop(
    construct_info,
    construct_context,
    name,
    forwarding_actor,
    persistence_id,
    forwarding_loop,
)
```

---

## Why No Mutexes/Locks

The actor model avoids Mutex/RwLock for several reasons:

1. **Hardware mapping**: Blocking primitives don't map to hardware.

2. **HVM compilation**: HVM uses interaction nets which don't have locks.

3. **Deadlock freedom**: Actor message passing can't deadlock (unlike nested locks).

4. **Performance**: Lock-free data structures (ArcSwap, AtomicU64) for shared state.

Instead of locks, Boon uses:
- **Channels** (mpsc) for actor communication
- **ArcSwap** for lock-free current value access
- **AtomicU64** for version tracking
- **Actor-local state** owned by the actor loop (no shared mutable state)

---

## Hardware/HVM Mapping

| Boon Construct | Hardware Equivalent | HVM Equivalent |
|----------------|---------------------|----------------|
| Pure Stream | Combinational logic, wires | Lambda composition |
| ActorLoop | FSM, registers | Interaction net node |
| mpsc channel | FIFO buffer | Port connection |
| ArcSwap | Register with read port | Reference cell |
| AtomicU64 | Counter register | Numeric node |

---

## Implementation Checklist

After the refactoring, these properties hold:

- [x] `Task::start_droppable` only appears inside `ActorLoop::new()`
- [x] No `Task::start_droppable` in api.rs
- [x] No `Task::start_droppable` in evaluator.rs
- [x] No `Task::start_droppable` in bridge.rs
- [x] `switch_map` uses `stream::unfold()`
- [x] `Stream/skip` uses `stream::unfold()`
- [x] `Stream/take` uses `stream::unfold()`
- [x] `Stream/debounce` uses `stream::unfold()`
- [x] All infrastructure actors use `ActorLoop`
- [x] `ValueActor` uses `actor_loop: ActorLoop`
- [x] `LazyValueActor` uses `actor_loop: ActorLoop`
- [x] `List` uses `actor_loop: ActorLoop`
- [x] Forwarding integrated via `ValueActor::connect_forwarding()`

---

## See Also

- **DATAFLOW.md** - Internal data flow mechanics
- **gpu/HVM_ACTORS_RESEARCH.md** - HVM actor research
- **gpu/HVM_WEBGPU_ACTORS.md** - WebGPU/HVM integration
- **CLAUDE.md** - Engine architecture rules
