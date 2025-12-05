# DATAFLOW: Internal Data Flow Mechanics

**Date**: 2025-12-05
**Status**: Technical Reference

---

## Overview

This document describes how data flows through Boon's reactive combinators at the runtime level. Understanding these mechanics is essential for:
- Debugging unexpected behavior
- Avoiding memory issues
- Writing efficient reactive code

---

## Core Architecture

### ValueActor

The fundamental unit of reactive computation. Every expression evaluates to a `ValueActor`.

```
┌─────────────────────────────────────────────────────────────┐
│                       ValueActor                            │
├─────────────────────────────────────────────────────────────┤
│ input_stream ─────► internal loop ─────► notify_senders[]   │
│                          │                    │             │
│                          ▼                    ▼             │
│                   current_value          try_send(())       │
│                    (ArcSwap)           (bounded channels)   │
│                                                             │
│ notify_sender_sender ◄─── Register subscriber channels      │
│ current_version (AtomicU64) ← Increments on each change     │
│                                                             │
│ inputs: Vec<Arc<ValueActor>> (keeps upstream alive)         │
└─────────────────────────────────────────────────────────────┘
```

**Key characteristics:**
- **Push-pull model**: Notify via bounded(1) channels, pull value on demand
- **Bounded channels**: O(1) memory per subscriber regardless of speed
- **No RefCell**: Subscriber senders stored in loop locals (pure dataflow)
- **Keep-alive semantics**: `inputs` Vec prevents upstream drops
- **Current value cache**: `ArcSwap` allows lock-free reads

### Subscription Model

```rust
pub struct Subscription {
    actor: Arc<ValueActor>,
    last_seen_version: u64,
    notify_receiver: mpsc::Receiver<()>,  // bounded(1)
}
```

When you subscribe:
1. Create bounded(1) channel
2. Send sender to actor via `notify_sender_sender`
3. Actor loop stores sender in local `Vec<mpsc::Sender<()>>`
4. Returns receiver as part of Subscription

On value change:
1. Actor loop calls `try_send(())` on all senders
2. If buffer full, skip (subscriber already has pending notification)
3. If disconnected, remove sender from list via `retain_mut`

When subscriber polls:
1. Wait for notification on bounded receiver (or check version)
2. Pull current value from `ArcSwap` on demand

---

## Combinator Data Flow

### THEN

**Purpose**: Transform values when events occur.

```
input |> THEN { body }
```

**Data flow:**

```
┌──────────┐     ┌─────────────────────────────────────────┐
│  input   │────►│              THEN                       │
│  stream  │     │  ┌─────────────────────────────────┐    │
└──────────┘     │  │ for each input value:           │    │
                 │  │   1. Create temp ValueActor     │    │
                 │  │   2. Evaluate body with value   │    │
                 │  │   3. Get first result           │    │
                 │  │   4. Set new idempotency key    │    │
                 │  │   5. Emit result                │    │
                 │  └─────────────────────────────────┘    │
                 └─────────────────────────────────────────┘
```

**Processing modes:**

| Context | Mode | Implementation |
|---------|------|----------------|
| Outside HOLD | Parallel | `filter_map(async \|v\| body(v))` |
| Inside HOLD (no permit) | Sequential | `.then(async \|v\| body(v))` |
| Inside HOLD (with permit) | Backpressure | `permit.acquire().await; body(v)` |

**Backpressure synchronization:**
```
THEN                                    HOLD
────                                    ────
acquire permit ◄─────────── permit = 1 (initial)
evaluate body
emit result ────────────────► receive result
                              update state
                              release permit
acquire permit ◄─────────── permit = 1 (released)
evaluate body
...
```

---

### WHEN

**Purpose**: Pattern match and transform.

```
input |> WHEN {
    Pattern1 => body1
    Pattern2 => body2
    __ => default_body
}
```

**Data flow:**

```
┌──────────┐     ┌─────────────────────────────────────────┐
│  input   │────►│              WHEN                       │
│  stream  │     │  for each value:                        │
└──────────┘     │    1. Try Pattern1 → body1              │
                 │    2. Try Pattern2 → body2              │
                 │    3. ...                               │
                 │    4. Try __ (wildcard) → default       │
                 │    5. First match wins                  │
                 │    6. Emit result                       │
                 └─────────────────────────────────────────┘
```

**Pattern binding:**
When a pattern matches, bound variables become `ValueActor`s containing the matched value. These are passed to the body via `parameters` in `ActorContext`.

**Same backpressure modes as THEN.**

---

### WHILE

**Purpose**: Continuous streaming while pattern matches.

```
input |> WHILE {
    Pattern => body  -- body streams while pattern matches
}
```

**Data flow:**

```
┌──────────┐     ┌─────────────────────────────────────────┐
│  input   │────►│              WHILE                      │
│  stream  │     │  for each value:                        │
└──────────┘     │    if pattern matches:                  │
                 │      subscribe to body                  │
                 │      forward all body emissions         │
                 │    else:                                │
                 │      stop forwarding from body          │
                 └─────────────────────────────────────────┘
```

**Difference from WHEN:**
- WHEN: Takes first value from body, then moves to next input
- WHILE: Streams all values from body until pattern stops matching

---

### HOLD

**Purpose**: Stateful accumulation with self-reference.

```
initial |> HOLD state_param { body }
```

**Data flow:**

```
┌──────────┐     ┌───────────────────────────────────────────────────┐
│ initial  │────►│                    HOLD                          │
│  stream  │     │  ┌───────────────────────────────────────────┐   │
└──────────┘     │  │ State Management:                         │   │
                 │  │   current_state: Rc<RefCell<Option<Value>>>│   │
                 │  │   state_sender: mpsc::UnboundedSender      │   │
                 │  │   state_actor: ValueActor (for body ref)  │   │
                 │  └───────────────────────────────────────────┘   │
                 │                                                   │
                 │  ┌───────────────────────────────────────────┐   │
                 │  │ Body Actor Context:                       │   │
                 │  │   parameters[state_param] = state_actor   │   │
                 │  │   sequential_processing = true            │   │
                 │  │   backpressure_permit = Some(permit)      │   │
                 │  └───────────────────────────────────────────┘   │
                 │                                                   │
                 │  Output: stream::select(initial_stream,          │
                 │                         body_update_stream)       │
                 └───────────────────────────────────────────────────┘
```

**Synchronization protocol:**
1. HOLD creates `BackpressurePermit::new(1)` - initial permit available
2. THEN/WHEN in body must `acquire()` permit before evaluation
3. When body emits, HOLD:
   - Updates `current_state`
   - Sends to `state_sender` (updates state_actor)
   - Calls `permit.release()`
4. Next THEN/WHEN can now acquire and evaluate

**Why sequential processing matters:**
```boon
counter: 0 |> HOLD counter {
    PULSES { 3 } |> THEN { counter + 1 }
}
```

Without backpressure (parallel):
- Pulse 0 arrives, reads counter=0, computes 1
- Pulse 1 arrives, reads counter=0, computes 1  (stale!)
- Pulse 2 arrives, reads counter=0, computes 1  (stale!)
- Result: 1 (all read same stale value)

With backpressure (sequential):
- Pulse 0: acquire, read counter=0, compute 1, emit
- HOLD: update counter=1, release
- Pulse 1: acquire, read counter=1, compute 2, emit
- HOLD: update counter=2, release
- Pulse 2: acquire, read counter=2, compute 3, emit
- Result: 3 (each reads fresh state)

---

### LATEST

**Purpose**: Merge multiple event sources.

```
LATEST { source1, source2, source3 }
```

**Data flow:**

```
┌──────────┐
│ source1  │───┐
└──────────┘   │     ┌─────────────────────────────────────┐
               │     │            LATEST                   │
┌──────────┐   ├────►│  stream::select_all(sources)        │
│ source2  │───┤     │                                     │
└──────────┘   │     │  Deduplication:                     │
               │     │    - Track idempotency keys         │
┌──────────┐   │     │    - Skip if key == previous key    │
│ source3  │───┘     │    - Save state to persistence      │
└──────────┘         └─────────────────────────────────────┘
```

**Idempotency handling:**
Each input is tracked by index. When a value arrives:
1. Compare its `idempotency_key` with stored key for that index
2. If same, skip (duplicate)
3. If different, update stored key, emit value

---

### FLUSH

**Purpose**: Fail-fast error handling.

```
FLUSH { error_value }
```

**Data flow:**

```
┌──────────┐     ┌─────────────────────────────────────────┐
│  value   │────►│              FLUSH                      │
│  stream  │     │  map(v => Value::Flushed(Box::new(v)))  │
└──────────┘     └─────────────────────────────────────────┘
```

**FLUSHED propagation:**
- `Value::Flushed(inner)` wraps the value
- Function calls check `is_flushed()` on arguments
- If any argument is FLUSHED, bypass function, emit FLUSHED
- Unwraps at boundaries (variable binding, function return)

---

## Complex Values

### LIST

Lists have their own subscription model separate from ValueActor.

```
┌─────────────────────────────────────────────────────────────┐
│                         List                                │
├─────────────────────────────────────────────────────────────┤
│ change_stream ─────► loop ─────► change_senders[]           │
│                        │                                    │
│                        ▼                                    │
│                 list: Option<Vec<Arc<ValueActor>>>          │
└─────────────────────────────────────────────────────────────┘
```

**ListChange types:**
```rust
pub enum ListChange {
    Replace { items: Vec<Arc<ValueActor>> },
    Push { item: Arc<ValueActor> },
    Insert { index: usize, item: Arc<ValueActor> },
    Remove { index: usize },
    // ... more operations
}
```

**When LIST is input to THEN:**

The LIST is passed as a single `Value::List(Arc<List>, metadata)`:
```boon
my_list |> THEN { body }
```

THEN receives the entire list value, NOT individual items.
To iterate over items, use:
- `List/each()` - applies function to each item
- `PULSES { List/length(list) }` - iterate by index
- Spread operator in appropriate contexts

**LIST inside HOLD (NOT recommended):**
```boon
-- BAD: Full list copy on every update
items: LIST {} |> HOLD items {
    event |> THEN { items |> List/push(new_item) }
}
```

Why it's problematic:
1. HOLD stores `current_state` as full Value
2. Each update clones entire list
3. O(n) per update instead of O(1)

**Better: Use reactive list operations:**
```boon
items: LIST {}
    |> List/push(item: new_item_event)
    |> List/remove(index: remove_event)
```

---

## Known Issues and Considerations

### 1. ~~Unbounded Channels - Memory Risk~~ (SOLVED)

**Previous problem:** All subscription channels were `mpsc::unbounded()`.

**Solution:** Now using bounded(1) channels with push-pull architecture:

```rust
// Actor loop stores bounded senders
let mut notify_senders: Vec<mpsc::Sender<()>> = Vec::new();

// On value change - try_send, skip if full
notify_senders.retain_mut(|sender| {
    match sender.try_send(()) {
        Ok(()) => true,
        Err(e) => !e.is_disconnected(),
    }
});
```

**Result:**
- O(1) memory per subscriber regardless of speed
- Slow consumers automatically skip to latest value
- No memory exhaustion from queue buildup

### 2. LIST Subscription - Legacy vs Diff

**Legacy model:** `ListSubscription` still uses unbounded channels for `ListChange` broadcast.

**New model:** `ListDiffSubscription` uses bounded(1) channels with pull-based diffs.

```rust
// Use subscribe_diffs() for memory-efficient subscription
let subscription = list.subscribe_diffs();  // Returns ListDiffSubscription
```

**Recommendation:** Prefer `subscribe_diffs()` for memory-efficient list handling.

### 3. State Channel in HOLD

**Analysis:**
With `BackpressurePermit`, the body can only emit one value before HOLD processes it. This effectively bounds the channel at 1 message.

However, the initial value stream can still flood:
```boon
LATEST { fast_stream1, fast_stream2, ... } |> HOLD state { ... }
```
Each emission from LATEST sets state, no permit control.

**Mitigation:** LATEST already deduplicates by idempotency key.

### 4. ~~Subscriber Leak Risk~~ (SOLVED)

**Previous problem:** Subscribers stored in HashMap, only cleaned when send fails.

**Solution:** Subscriber senders now stored in loop-local Vec, cleaned via `retain_mut`:

```rust
// In actor loop - stored in local variable, not shared state
let mut notify_senders: Vec<mpsc::Sender<()>> = Vec::new();

// Automatic cleanup when sender disconnects
notify_senders.retain_mut(|sender| !sender.try_send(()).is_disconnected());
```

**Result:** No HashMap, no shared state, pure dataflow cleanup.

### 5. Backpressure Blocking Normal Functionality

**Scenario:**
```boon
value: initial |> HOLD value {
    LATEST {
        event1 |> THEN { compute1() }  -- slow
        event2 |> THEN { compute2() }  -- fast
    }
}
```

**Problem:** Both THEN share the same permit. If `compute1()` is slow, `event2` is also blocked even though they're independent streams inside LATEST.

**Analysis:**
This is correct behavior for state consistency - we WANT both to wait because they both read `value` state. If they ran concurrently, they'd both see the same stale state.

**For truly independent operations:** Don't put them in the same HOLD.

### 6. Backpressure Deadlock Risk

**Scenario:**
```boon
a: 0 |> HOLD a {
    b |> THEN { ... }  -- acquires permit, waits for b
}

b: 0 |> HOLD b {
    a |> THEN { ... }  -- acquires permit, waits for a
}
```

**Analysis:**
Unlikely to deadlock because:
- Each HOLD has its own permit
- THEN acquires permit for its enclosing HOLD only
- Cross-references are via subscription, not permit acquisition

But complex cyclic dependencies could theoretically cause issues.

---

## Dataflow Summary Table

| Combinator | Input Processing | Output Timing | State | Backpressure |
|------------|------------------|---------------|-------|--------------|
| THEN | Per value | After body eval | None | In HOLD only |
| WHEN | Per value | After match + eval | None | In HOLD only |
| WHILE | Per value | Streams while match | None | In HOLD only |
| HOLD | Any emission | After body/input | Yes | Permit-based |
| LATEST | Merge all | On any input | Idempotency | None |
| FLUSH | Per value | Immediate wrap | None | None |
| LIST | Changes | Broadcast to subs | Full list | None |

---

## Best Practices

1. **Avoid LIST in HOLD** - Use reactive list operations instead
2. **Keep HOLD bodies simple** - Complex nested THEN/WHEN slow permit release
3. **Mind subscription lifetime** - Drop subscriptions when done
4. **Use LATEST for event merge** - Not for high-frequency streams
5. **Profile memory** - Watch for unbounded growth in long-running apps

---

## Version + Push-Pull Architecture (IMPLEMENTED)

This section documents the version-based push-pull architecture that replaces unbounded channels.

---

### 1. Problem Statement (SOLVED)

**Previous architecture:**
```rust
struct Subscription {
    receiver: mpsc::UnboundedReceiver<Value>,  // Full values queue up!
}
```

**Failure scenario:** 10MB Text changes 100 times, slow consumer:
- Queue: 100 × 10MB = **1GB memory**

**Root cause:** Notification and data delivery were coupled.

---

### 2. Solution Overview (IMPLEMENTED)

**Core insight:** Separate notification (tiny) from data delivery (on-demand).

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     ValueActor (Current Implementation)                  │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  current_value: Arc<ArcSwap<Value>>     ← Lock-free current state       │
│  current_version: Arc<AtomicU64>        ← Increments on every change    │
│  notify_sender_sender: mpsc::Unbounded  ← Register subscriber channels  │
│                                                                         │
│  Actor loop stores locally:                                             │
│    notify_senders: Vec<mpsc::Sender<()>> ← Bounded(1) per subscriber    │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                    Subscribers receive: () notification (0 bytes payload)
                    Subscribers pull: current value when ready
```

**Key difference from original plan:** Uses bounded mpsc channels stored in actor loop locals instead of `watch::Sender`. This avoids RefCell/Mutex entirely - pure dataflow.

---

### 3. Complete Type Definitions (IMPLEMENTED)

#### 3.1 Why ItemId Instead of Index-Based Diffs

**signals-futures approach (index-based):**
```rust
enum VecDiff<T> {
    Replace { values: Vec<T> },
    InsertAt { index: usize, value: T },
    UpdateAt { index: usize, value: T },
    RemoveAt { index: usize },
    Move { old_index: usize, new_index: usize },
    // ...
}
```

signals-futures DOES work with transformations. Each combinator receives upstream diffs,
translates them to its own index space, and emits its own diffs. The issue is **complexity**.

**Index-based filter - O(n) per diff:**

```
Source: [A, B, C, D, E, F, G, H, ...]  (1000 items)
Filter state: Vec<bool> tracking which source indices pass

When RemoveAt { index: 500 } arrives:
1. Was source index 500 included? → lookup[500]: O(1)
2. What OUTPUT index does it map to? → count TRUE bits before 500: O(n)
3. Update tracking state → shift all indices after 500: O(n)
4. Emit RemoveAt { output_index } if included

Total: O(n) per diff operation
```

This is documented in signals-futures: "The performance is linear with the number of
values in self. For example, if self has 1,000 values and a new value is inserted,
filter will require (on average) 1,000 operations to update its internal state."
(https://docs.rs/futures-signals/latest/futures_signals/signal_vec/trait.SignalVecExt.html)

**ID-based filter - O(1) per diff:**

```
Source: [(id:1,A), (id:2,B), (id:3,C), ...]
Filter state: HashSet<ItemId> of included IDs

When Remove { id: 500 } arrives:
1. Is id:500 in included_ids? → HashSet lookup: O(1)
2. If yes: emit Remove { id: 500 }, remove from set: O(1)
3. Done

Total: O(1) per diff operation
```

**Transformation chains multiply the cost:**

```boon
items                              -- 10,000 items
    |> List/filter(predicate1)     -- ~5,000 pass
    |> List/filter(predicate2)     -- ~2,500 pass
    |> List/filter(predicate3)     -- ~1,250 pass
    |> List/map(transform)
```

Per diff operation:
- Index-based: O(10000) + O(5000) + O(2500) = O(17500)
- ID-based: O(1) + O(1) + O(1) = O(3)

At 1000 updates/sec with 10K items and 3 filters:
- Index-based: ~17.5M index operations/sec
- ID-based: ~3K hash lookups/sec

**Insert position needs translation too:**

Index-based `InsertAt { index: 500 }`:
- Filter must: find how many items before 500 pass predicate → O(n)
- Then emit `InsertAt { output_index }`

ID-based `Insert { id: new, after: id:499 }`:
- Filter checks: is id:499 in my included set? → O(1)
- If yes: emit same diff (or translate `after` to last included before it)
- If no: find nearest included ID before it → O(log n) with sorted structure or O(1) with linked structure

**When indices are fine:**

signals-futures' O(n) filter is practical when:
- Lists are small (< 1000 items)
- Transformations are shallow (1-2 levels)
- Update frequency is low (< 100/sec)

**When IDs are needed:**

Boon targets scenarios where indices become bottleneck:
- Large lists (10K+ items)
- Deep transformation chains (5+ levels)
- High update frequency (1000+/sec)
- Network sync (stable identifiers across reconnects)
- Future: collaborative editing (conflict-free merging by ID)

---

#### 3.2 Core Types (IMPLEMENTED)

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::collections::VecDeque;
use std::cell::RefCell;
use arc_swap::ArcSwap;
use futures::channel::mpsc;

/// Unique identifier for list items. Stable across transformations.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ItemId(pub Ulid);

impl ItemId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

/// Diff operations for LIST. ID-based, not index-based.
#[derive(Clone, Debug)]
pub enum ListDiff {
    /// Insert item after another (None = prepend)
    Insert {
        id: ItemId,
        after: Option<ItemId>,
        value: Arc<ValueActor>,
    },
    /// Remove item by ID
    Remove { id: ItemId },
    /// Update item's value
    Update { id: ItemId, value: Arc<ValueActor> },
    /// Replace entire list (checkpoint/reset)
    Replace { items: Vec<(ItemId, Arc<ValueActor>)> },
}
```

**Note:** The actual implementation stores `Arc<ValueActor>` in diffs, not `Arc<Value>`. ValueActors are the reactive units.

#### 3.3 DiffHistory (IMPLEMENTED)

```rust
/// Maintains diff history for LIST. Stored in List via Arc<RefCell<DiffHistory>>.
pub struct DiffHistory {
    /// Ring buffer: (version, diff)
    diffs: VecDeque<(u64, Arc<ListDiff>)>,
    /// Oldest version we can serve diffs from
    oldest_version: u64,
    /// Maximum diffs to retain (default: 1500)
    max_entries: usize,
}

impl DiffHistory {
    pub fn new() -> Self {
        Self {
            diffs: VecDeque::new(),
            oldest_version: 0,
            max_entries: 1500,
        }
    }

    /// Add a new diff
    pub fn add(&mut self, version: u64, diff: Arc<ListDiff>) {
        // If Replace, it supersedes everything
        if matches!(diff.as_ref(), ListDiff::Replace { .. }) {
            self.diffs.clear();
            self.oldest_version = version;
        }

        self.diffs.push_back((version, diff));

        // Trim old entries
        while self.diffs.len() > self.max_entries {
            if let Some((old_version, _)) = self.diffs.pop_front() {
                self.oldest_version = old_version + 1;
            }
        }
    }

    /// Get diffs since a version
    pub fn get_diffs_since(&self, subscriber_version: u64) -> Option<Vec<Arc<ListDiff>>> {
        if subscriber_version < self.oldest_version {
            // Too far behind - subscriber needs full snapshot
            return None;
        }

        let diffs: Vec<Arc<ListDiff>> = self.diffs.iter()
            .filter(|(v, _)| *v > subscriber_version)
            .map(|(_, d)| d.clone())
            .collect();

        Some(diffs)
    }
}
```

**Note:** Actual implementation stores `Arc<RefCell<DiffHistory>>` in List, not `Mutex`. This is safe because the List's actor loop is the only writer, and subscribers only read via bounded channel notifications.

#### 3.4 ValueActor (IMPLEMENTED)

```rust
pub struct ValueActor {
    construct_info: Arc<ConstructInfoComplete>,
    persistence_id: Option<parser::PersistenceId>,
    message_sender: mpsc::UnboundedSender<ActorMessage>,
    inputs: Vec<Arc<ValueActor>>,

    // Lock-free current value
    current_value: Arc<ArcSwap<Option<Value>>>,
    current_version: Arc<AtomicU64>,

    // Channel for registering new subscriber notification senders
    notify_sender_sender: mpsc::UnboundedSender<mpsc::Sender<()>>,

    loop_task: TaskHandle,
}
```

**Key: Loop-local subscriber storage (no RefCell/Mutex)**

```rust
// Inside ValueActor::new_with_inputs()
let (notify_sender_sender, notify_sender_receiver) = mpsc::unbounded::<mpsc::Sender<()>>();

async move {
    let mut notify_sender_receiver = pin!(notify_sender_receiver.fuse());

    // Subscriber senders stored LOCALLY in this loop (no shared state!)
    let mut notify_senders: Vec<mpsc::Sender<()>> = Vec::new();

    loop {
        select! {
            // Handle new subscriber registrations
            sender = notify_sender_receiver.next() => {
                if let Some(sender) = sender {
                    notify_senders.push(sender);
                }
            }

            // Handle value stream updates
            new_value = value_stream.next() => {
                if let Some(value) = new_value {
                    // Store value
                    current_value.store(Arc::new(Some(value)));
                    // Increment version
                    let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;

                    // Notify all subscribers via bounded channels
                    notify_senders.retain_mut(|sender| {
                        match sender.try_send(()) {
                            Ok(()) => true,  // Keep sender
                            Err(e) => !e.is_disconnected(),  // Keep if just full
                        }
                    });
                }
            }
        }
    }
}
```

**Subscribe creates bounded(1) channel:**

```rust
pub fn subscribe(self: Arc<Self>) -> Subscription {
    // Create bounded(1) channel - at most 1 pending notification
    let (sender, receiver) = mpsc::channel::<()>(1);

    // Register sender with actor loop (unbounded send - just registration)
    if let Err(e) = self.notify_sender_sender.unbounded_send(sender) {
        eprintln!("Failed to register subscriber: {e:#}");
    }

    Subscription {
        last_seen_version: 0,
        notify_receiver: receiver,  // bounded(1)
        actor: self,
    }
}
```

**Why this design:**
- No RefCell/Mutex for subscriber storage
- Loop-local Vec cleaned automatically via `retain_mut`
- `try_send(())` skips if buffer full (subscriber already notified)
- Disconnected senders removed on next notify attempt
- Pure dataflow - waker management delegated to channel internals

#### 3.5 Subscription (IMPLEMENTED)

```rust
pub struct Subscription {
    actor: Arc<ValueActor>,
    last_seen_version: u64,
    notify_receiver: mpsc::Receiver<()>,  // bounded(1)
}

impl Stream for Subscription {
    type Item = Value;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check if we have a pending update (version changed)
        let current = self.actor.version();
        if current > self.last_seen_version {
            self.last_seen_version = current;
            if let Some(value) = self.actor.current_value() {
                return Poll::Ready(Some(value));
            }
        }

        // Wait for notification via bounded channel
        match Pin::new(&mut self.notify_receiver).poll_next(cx) {
            Poll::Ready(Some(())) => {
                // Notification received - pull current value
                let current = self.actor.version();
                self.last_seen_version = current;
                if let Some(value) = self.actor.current_value() {
                    Poll::Ready(Some(value))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(None) => Poll::Ready(None),  // Actor dropped
            Poll::Pending => Poll::Pending,
        }
    }
}
```

**Key differences from watch-based design:**
- Uses `mpsc::Receiver<()>` instead of `watch::Receiver<u64>`
- Returns `Value` directly instead of `ValueUpdate` enum
- Waker registration handled by mpsc channel internals
- No `.borrow()` or `.changed()` - just `poll_next()`

---

### 4. Combinator Integration (IMPLEMENTED)

The combinators (THEN, WHEN, WHILE, HOLD) work unchanged with the new subscription model.

#### 4.1 THEN/WHEN Behavior

THEN and WHEN use `constant(value)` for their piped input, creating a stream that emits once then hangs forever. Dependencies in the body use snapshot reads via `current_value()`.

#### 4.2 WHILE Behavior

WHILE uses `subscribe()` to stream all body emissions while pattern matches. This is correct - WHILE needs continuous streaming, not snapshots.

#### 4.3 HOLD Integration

HOLD's `BackpressurePermit` continues to work as before - it controls evaluation pacing for state consistency. The bounded channel notification just replaces how subscribers are notified:

```rust
// In actor loop after state update:
current_value.store(Arc::new(Some(value)));
let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;

// Notify subscribers via bounded channels (replaces unbounded send)
notify_senders.retain_mut(|sender| {
    match sender.try_send(()) {
        Ok(()) => true,
        Err(e) => !e.is_disconnected(),
    }
});

permit.release();  // Allow next body evaluation
```

---

## Future Work

The following sections describe planned features that are NOT yet implemented.

---

### 5. Transformation Chain Implementation (FUTURE)

#### 5.1 FilteredList

```rust
pub struct FilteredList {
    source: Arc<ValueActor>,
    predicate: Box<dyn Fn(&Value) -> bool + Send + Sync>,

    // Track which source IDs pass filter
    included_ids: HashSet<ItemId>,

    // Our own actor for subscribers
    inner_actor: Arc<ValueActor>,
}

impl FilteredList {
    pub fn new(
        source: Arc<ValueActor>,
        predicate: impl Fn(&Value) -> bool + Send + Sync + 'static,
    ) -> Arc<Self> {
        // Initial filter
        let source_snapshot = source.current_value();
        let (included_ids, initial_items) = Self::filter_snapshot(&source_snapshot, &predicate);

        let inner_actor = ValueActor::new_list(
            ConstructInfo::new("FilteredList", None, "filtered"),
            initial_items,
            stream::pending(), // We'll drive updates manually
            vec![source.clone()],
        );

        let filtered = Arc::new(Self {
            source: source.clone(),
            predicate: Box::new(predicate),
            included_ids,
            inner_actor,
        });

        // Start update loop
        let filtered_weak = Arc::downgrade(&filtered);
        Task::start_droppable(async move {
            let mut subscription = source.subscribe();
            loop {
                let update = subscription.next().await;
                if let Some(filtered) = filtered_weak.upgrade() {
                    filtered.handle_source_update(update);
                } else {
                    break;
                }
            }
        });

        filtered
    }

    fn handle_source_update(&self, update: ValueUpdate) {
        match update {
            ValueUpdate::Current => {}
            ValueUpdate::Snapshot(value) => {
                // Full re-filter
                let (new_included, new_items) = Self::filter_snapshot(&value, &self.predicate);
                // Update our state and emit Replace diff
                // ...
            }
            ValueUpdate::Diffs(diffs) => {
                // Translate each diff
                for diff in diffs {
                    if let Some(translated) = self.translate_diff(&diff) {
                        // Apply to our inner_actor
                        // ...
                    }
                }
            }
        }
    }

    fn translate_diff(&self, diff: &ListDiff) -> Option<ListDiff> {
        match diff {
            ListDiff::Insert { id, after, value } => {
                if (self.predicate)(value) {
                    // Find translated 'after' position
                    let translated_after = after.and_then(|a| {
                        if self.included_ids.contains(&a) { Some(a) } else { None }
                    });
                    Some(ListDiff::Insert {
                        id: *id,
                        after: translated_after,
                        value: value.clone(),
                    })
                } else {
                    None
                }
            }
            ListDiff::Remove { id } => {
                if self.included_ids.contains(id) {
                    Some(ListDiff::Remove { id: *id })
                } else {
                    None
                }
            }
            ListDiff::Update { id, value } => {
                let was_included = self.included_ids.contains(id);
                let now_included = (self.predicate)(value);

                match (was_included, now_included) {
                    (false, false) => None,
                    (true, true) => Some(ListDiff::Update { id: *id, value: value.clone() }),
                    (false, true) => Some(ListDiff::Insert {
                        id: *id,
                        after: self.find_insert_position(*id),
                        value: value.clone(),
                    }),
                    (true, false) => Some(ListDiff::Remove { id: *id }),
                }
            }
            ListDiff::Replace { items } => {
                let (_, filtered) = Self::filter_snapshot_items(items, &self.predicate);
                Some(ListDiff::Replace { items: Arc::new(filtered) })
            }
        }
    }
}
```

---

### 6. Network Sync Protocol (FUTURE)

#### 6.1 Message Types

```rust
/// Messages over WebSocket
#[derive(Serialize, Deserialize)]
enum SyncMessage {
    /// Server → Client: Version changed
    VersionNotify {
        variable_id: String,
        version: u64,
    },

    /// Client → Server: Request update
    RequestUpdate {
        variable_id: String,
        from_version: u64,
    },

    /// Server → Client: Update response
    UpdateResponse {
        variable_id: String,
        update: SerializedUpdate,
    },
}

#[derive(Serialize, Deserialize)]
enum SerializedUpdate {
    Snapshot(Vec<u8>),  // Serialized Value
    Diffs(Vec<Vec<u8>>),  // Serialized diffs
}
```

#### 6.2 LocalReplica

```rust
/// Frontend replica of backend value
pub struct LocalReplica {
    variable_id: String,
    inner_actor: Arc<ValueActor>,
    websocket: WebSocketConnection,
}

impl LocalReplica {
    pub fn subscribe(&self) -> Subscription {
        // Subscribe to LOCAL actor, not network
        self.inner_actor.subscribe()
    }

    async fn sync_loop(&mut self) {
        loop {
            match self.websocket.recv().await {
                SyncMessage::VersionNotify { variable_id, version } => {
                    if variable_id == self.variable_id {
                        let my_version = self.inner_actor.version();
                        if version > my_version {
                            // Request update
                            self.websocket.send(SyncMessage::RequestUpdate {
                                variable_id: self.variable_id.clone(),
                                from_version: my_version,
                            }).await;
                        }
                    }
                }
                SyncMessage::UpdateResponse { variable_id, update } => {
                    if variable_id == self.variable_id {
                        self.apply_update(update);
                    }
                }
                _ => {}
            }
        }
    }
}
```

---

### 7. Test Cases (FUTURE)

#### 7.1 Basic Version + Pull

```rust
#[test]
async fn test_scalar_version_pull() {
    let actor = ValueActor::new_scalar(
        info("test"),
        Value::Number(0.0),
        stream::iter(vec![Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)]),
        vec![],
    );

    let mut sub = actor.subscribe();

    // Fast consumer - gets each value
    assert_eq!(sub.next().await, ValueUpdate::Snapshot(Value::Number(1.0)));
    assert_eq!(sub.next().await, ValueUpdate::Snapshot(Value::Number(2.0)));
    assert_eq!(sub.next().await, ValueUpdate::Snapshot(Value::Number(3.0)));
}

#[test]
async fn test_scalar_slow_consumer() {
    let (tx, rx) = mpsc::channel(100);
    let actor = ValueActor::new_scalar(info("test"), Value::Number(0.0), rx, vec![]);

    let mut sub = actor.subscribe();

    // Produce 100 values
    for i in 1..=100 {
        tx.send(Value::Number(i as f64)).await.unwrap();
    }

    // Slow consumer finally pulls - gets LATEST only
    tokio::time::sleep(Duration::from_millis(10)).await;
    let update = sub.next().await;
    assert_eq!(update, ValueUpdate::Snapshot(Value::Number(100.0)));

    // Memory check: should NOT have 100 values queued
    // (This is the key improvement)
}
```

#### 7.2 LIST Diff vs Snapshot

```rust
#[test]
async fn test_list_small_gap_gets_diffs() {
    let actor = create_list_actor(vec![item(1), item(2), item(3)]);

    let mut sub = actor.subscribe();

    // Apply 2 diffs
    actor.apply_diff(ListDiff::Insert { id: id(4), after: Some(id(3)), value: item(4) });
    actor.apply_diff(ListDiff::Remove { id: id(2) });

    let update = sub.next().await;
    match update {
        ValueUpdate::Diffs(diffs) => {
            assert_eq!(diffs.len(), 2);
        }
        _ => panic!("Expected diffs for small gap"),
    }
}

#[test]
async fn test_list_large_gap_gets_snapshot() {
    let actor = create_list_actor(vec![item(1), item(2), item(3)]);

    let mut sub = actor.subscribe();

    // Apply 200 diffs (exceeds threshold)
    for i in 4..204 {
        actor.apply_diff(ListDiff::Insert { id: id(i), after: None, value: item(i) });
    }

    let update = sub.next().await;
    match update {
        ValueUpdate::Snapshot(_) => { /* Expected */ }
        ValueUpdate::Diffs(_) => panic!("Expected snapshot for large gap"),
        _ => {}
    }
}
```

#### 7.3 Memory Bound Test

```rust
#[test]
async fn test_no_unbounded_memory_growth() {
    let actor = ValueActor::new_scalar(info("test"), Value::Text("".into()), stream::pending(), vec![]);

    let sub = actor.subscribe();
    // Don't poll the subscription - simulate slow consumer

    // Rapidly update with large values
    for i in 0..1000 {
        let large_text = "x".repeat(1_000_000);  // 1MB
        actor.update(Value::Text(large_text.into()));
    }

    // Memory should NOT be 1000 * 1MB = 1GB
    // Should be approximately: 1MB (current) + small overhead

    // This test verifies the architecture, not exact memory
    // In practice, use a memory profiler
}
```

#### 7.4 Filter Chain Test

```rust
#[test]
async fn test_filter_chain_diff_translation() {
    let source = create_list_actor(vec![
        (id(1), Value::Number(1.0)),
        (id(2), Value::Number(2.0)),
        (id(3), Value::Number(3.0)),
        (id(4), Value::Number(4.0)),
    ]);

    let filtered = FilteredList::new(source.clone(), |v| {
        v.as_number() % 2.0 == 0.0  // Keep evens: [2, 4]
    });

    let mut sub = filtered.subscribe();

    // Remove odd number from source - should be no-op in filtered
    source.apply_diff(ListDiff::Remove { id: id(1) });
    // No update should be emitted

    // Remove even number from source - should translate
    source.apply_diff(ListDiff::Remove { id: id(2) });
    let update = sub.next().await;
    match update {
        ValueUpdate::Diffs(diffs) => {
            assert_eq!(diffs.len(), 1);
            assert!(matches!(diffs[0].as_ref(), ListDiff::Remove { id } if *id == id(2)));
        }
        _ => panic!("Expected diff"),
    }
}
```

---

### 8. Configuration Constants (FUTURE)

```rust
/// Default configuration values
pub mod config {
    /// Maximum diffs to keep in history
    pub const MAX_DIFF_HISTORY: usize = 1000;

    /// If subscriber is behind by more than this, send snapshot
    pub const SNAPSHOT_THRESHOLD: usize = 100;

    /// Use snapshot if diff cost > snapshot cost * this factor
    pub const COST_FACTOR: f64 = 0.8;

    /// Estimated overhead per diff entry (for cost calculation)
    pub const DIFF_OVERHEAD_BYTES: usize = 24;

    /// Estimated overhead per snapshot item
    pub const SNAPSHOT_ITEM_OVERHEAD_BYTES: usize = 16;
}
```

---

### 9. Migration Strategy (COMPLETED)

The migration was completed using bounded mpsc channels instead of watch channels.

#### What was implemented:

1. **Version tracking** - `current_version: Arc<AtomicU64>` added to ValueActor and List
2. **Bounded notifications** - `notify_sender_sender` channel for subscriber registration
3. **Loop-local storage** - Subscriber senders stored in actor loop local variables (no RefCell/Mutex)
4. **DiffHistory** - `Arc<RefCell<DiffHistory>>` for LIST diff tracking
5. **ListDiff types** - ItemId-based diff operations for O(1) filter translation

#### Key design change from original plan:

- **Original**: `watch::Sender<u64>` for notifications (requires RefCell for wakers)
- **Actual**: Bounded(1) mpsc channels per subscriber (pure dataflow, no RefCell)

This change was made to avoid RefCell/Mutex in the runtime, as they caused debugging issues.

---

### 10. Edge Cases (REFERENCE)

| Scenario | Behavior |
|----------|----------|
| Subscriber never polls | Bounded(1) channel full, `try_send` skips notification (O(1) memory) |
| Actor dropped while subscriber waiting | `poll_next` returns `Poll::Ready(None)`, subscription ends |
| Concurrent updates during pull | Subscriber gets version at time of poll, may see newer value |
| DiffHistory full during burst | Old diffs dropped, slow subscriber may need full list |
| Replace diff received | Clears history, becomes new checkpoint |
| Network disconnect | Local replica keeps working, re-syncs on reconnect |
| Empty list | Works normally, snapshot is empty vec |

---

## See Also

- **HOLD.md** - Stateful accumulator semantics
- **LATEST.md** - Event merging semantics
- **WHEN_VS_WHILE.md** - Pattern matching differences
- **PULSES.md** - Iteration patterns
- **LIST.md** - List operations
