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
│ input_stream ─────► internal loop ─────► subscribers[]      │
│                          │                                  │
│                          ▼                                  │
│                   current_value (ArcSwap)                   │
│                                                             │
│ message_sender ◄─── Subscribe/Unsubscribe messages          │
│                                                             │
│ inputs: Vec<Arc<ValueActor>> (keeps upstream alive)         │
└─────────────────────────────────────────────────────────────┘
```

**Key characteristics:**
- **Broadcast model**: All subscribers receive all values
- **Unbounded channels**: No automatic backpressure
- **Keep-alive semantics**: `inputs` Vec prevents upstream drops
- **Current value cache**: `ArcSwap` allows lock-free reads

### Subscription Model

```rust
pub struct Subscription {
    subscriber_id: SubscriberId,
    receiver: mpsc::UnboundedReceiver<Value>,
    actor: Arc<ValueActor>,  // Keeps actor alive
}
```

When you subscribe:
1. `Subscribe` message sent to actor
2. Actor stores sender in `subscribers` HashMap
3. Current value (if any) sent immediately
4. All future values broadcast to subscriber

When subscription dropped:
1. `Unsubscribe` message sent to actor
2. Actor removes entry from `subscribers`

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

### 1. Unbounded Channels - Memory Risk

**Problem:** All subscription channels are `mpsc::unbounded()`.

```rust
let (value_sender, value_receiver) = mpsc::unbounded();
```

**Impact:**
- Fast producer + slow consumer = memory growth
- No automatic backpressure
- Can cause memory exhaustion

**Where it matters:**
- High-frequency event sources (timers, sensor data)
- Complex pipelines with slow downstream processing
- Long-lived subscriptions that process slowly

**Mitigation (current):**
- `BackpressurePermit` in HOLD synchronizes THEN/WHEN
- But only for HOLD bodies, not general subscriptions

**Potential fix:**
- Bounded channels with configurable capacity
- Drop oldest policy for real-time scenarios
- Block producer for guaranteed delivery

### 2. LIST Subscription Broadcast

**Problem:** All subscribers receive cloned `ListChange`.

```rust
change_senders.retain(|change_sender| {
    change_sender.unbounded_send(change.clone())  // Clone for each!
});
```

**Impact:**
- N subscribers = N clones of `ListChange::Replace { items }`
- For large lists, significant memory

**Potential fix:**
- Structural sharing (immutable data structures)
- Reference counting for shared list snapshots

### 3. State Channel in HOLD

**Problem:** `state_sender` is unbounded.

```rust
let (state_sender, state_receiver) = mpsc::unbounded::<Value>();
```

**Analysis:**
With `BackpressurePermit`, the body can only emit one value before HOLD processes it. This effectively bounds the channel at 1 message.

However, the initial value stream can still flood:
```boon
LATEST { fast_stream1, fast_stream2, ... } |> HOLD state { ... }
```
Each emission from LATEST sets state, no permit control.

### 4. Subscriber Leak Risk

**Problem:** Subscribers stored in HashMap, only cleaned when send fails.

```rust
subscribers.retain(|_, sender| {
    sender.unbounded_send(value.clone()).is_ok()
});
```

**Scenario:**
If a subscriber task panics or is cancelled without dropping the `Subscription`, the sender entry might remain until next emission reveals it's disconnected.

**Impact:** Minor - entries cleaned on next value.

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

## Future: Universal Version + Push-Pull Architecture

The current push-based model with unbounded channels has fundamental issues. This section describes a unified architecture that solves them.

---

### 1. Problem Statement

**Current architecture:**
```rust
struct Subscription {
    receiver: mpsc::UnboundedReceiver<Value>,  // Full values queue up!
}
```

**Failure scenario:** 10MB Text changes 100 times, slow consumer:
- Queue: 100 × 10MB = **1GB memory**

**Root cause:** Notification and data delivery are coupled.

---

### 2. Solution Overview

**Core insight:** Separate notification (tiny) from data delivery (on-demand).

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     ValueActor (New Architecture)                       │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  current_value: Arc<ArcSwap<Value>>     ← Lock-free current state       │
│  current_version: AtomicU64             ← Increments on every change    │
│  version_sender: watch::Sender<u64>     ← Notifies subscribers          │
│  diff_history: Option<DiffHistory>      ← For collections only          │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                    Subscribers receive: just version number (8 bytes)
                    Subscribers pull: optimal update when ready
```

---

### 3. Complete Type Definitions

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

#### 3.2 Core Types

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::collections::VecDeque;
use arc_swap::ArcSwap;
use tokio::sync::watch;

/// Unique identifier for list items. Stable across transformations.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ItemId(pub u64);

impl ItemId {
    pub fn new() -> Self {
        Self(ulid::Ulid::new().0)
    }
}

/// Diff operations for LIST. ID-based, not index-based.
#[derive(Clone, Debug)]
pub enum ListDiff {
    /// Insert item after another (None = prepend)
    Insert {
        id: ItemId,
        after: Option<ItemId>,
        value: Arc<Value>,
    },
    /// Remove item by ID
    Remove { id: ItemId },
    /// Update item's value
    Update { id: ItemId, value: Arc<Value> },
    /// Replace entire list (checkpoint/reset)
    Replace { items: Arc<Vec<(ItemId, Arc<Value>)>> },
}

impl ListDiff {
    pub fn is_replace(&self) -> bool {
        matches!(self, Self::Replace { .. })
    }

    /// Estimate serialization cost (for diff vs snapshot decision)
    pub fn cost(&self) -> usize {
        match self {
            Self::Insert { value, .. } => 24 + value.estimated_size(),
            Self::Remove { .. } => 8,
            Self::Update { value, .. } => 16 + value.estimated_size(),
            Self::Replace { items } => items.iter().map(|(_, v)| 8 + v.estimated_size()).sum(),
        }
    }
}

/// What subscriber receives when pulling updates
#[derive(Clone, Debug)]
pub enum ValueUpdate {
    /// No changes since last pull
    Current,
    /// Full current value (scalars always, collections when gap too large)
    Snapshot(Arc<Value>),
    /// Incremental diffs (collections only, when gap is small)
    Diffs(Vec<Arc<ListDiff>>),
}
```

#### 3.3 DiffHistory

```rust
/// Configuration for diff history
pub struct DiffHistoryConfig {
    /// Maximum diffs to retain
    pub max_entries: usize,
    /// If diff count exceeds this, prefer snapshot
    pub snapshot_threshold: usize,
    /// If total diff cost exceeds snapshot cost * this factor, use snapshot
    pub cost_factor: f64,
}

impl Default for DiffHistoryConfig {
    fn default() -> Self {
        Self {
            max_entries: 1000,
            snapshot_threshold: 100,
            cost_factor: 0.8,  // Use snapshot if diffs cost > 80% of snapshot
        }
    }
}

/// Maintains diff history for collections
pub struct DiffHistory {
    /// Ring buffer: (version, diff)
    diffs: VecDeque<(u64, Arc<ListDiff>)>,
    /// Current snapshot (always available)
    current_snapshot: Arc<Vec<(ItemId, Arc<Value>)>>,
    /// Oldest version we can serve diffs from
    oldest_version: u64,
    /// Configuration
    config: DiffHistoryConfig,
}

impl DiffHistory {
    pub fn new(initial_items: Vec<(ItemId, Arc<Value>)>) -> Self {
        Self {
            diffs: VecDeque::new(),
            current_snapshot: Arc::new(initial_items),
            oldest_version: 0,
            config: DiffHistoryConfig::default(),
        }
    }

    /// Add a new diff and update snapshot
    pub fn add(&mut self, version: u64, diff: ListDiff) {
        let diff = Arc::new(diff);

        // Apply diff to snapshot
        self.apply_to_snapshot(&diff);

        // If Replace, it supersedes everything
        if diff.is_replace() {
            self.diffs.clear();
            self.oldest_version = version;
        }

        self.diffs.push_back((version, diff));

        // Trim old entries
        while self.diffs.len() > self.config.max_entries {
            if let Some((old_version, _)) = self.diffs.pop_front() {
                self.oldest_version = old_version + 1;
            }
        }
    }

    /// Apply diff to current snapshot
    fn apply_to_snapshot(&mut self, diff: &ListDiff) {
        let snapshot = Arc::make_mut(&mut self.current_snapshot);
        match diff {
            ListDiff::Insert { id, after, value } => {
                let pos = match after {
                    None => 0,
                    Some(after_id) => {
                        snapshot.iter().position(|(id, _)| id == after_id)
                            .map(|p| p + 1)
                            .unwrap_or(snapshot.len())
                    }
                };
                snapshot.insert(pos, (*id, value.clone()));
            }
            ListDiff::Remove { id } => {
                snapshot.retain(|(item_id, _)| item_id != id);
            }
            ListDiff::Update { id, value } => {
                if let Some((_, v)) = snapshot.iter_mut().find(|(item_id, _)| item_id == id) {
                    *v = value.clone();
                }
            }
            ListDiff::Replace { items } => {
                *snapshot = items.as_ref().clone();
            }
        }
    }

    /// Get optimal update for subscriber at given version
    pub fn get_update_since(&self, subscriber_version: u64) -> ValueUpdate {
        // Already up to date?
        let current_version = self.oldest_version + self.diffs.len() as u64;
        if subscriber_version >= current_version {
            return ValueUpdate::Current;
        }

        // Can we serve diffs?
        if subscriber_version >= self.oldest_version {
            let diffs: Vec<Arc<ListDiff>> = self.diffs.iter()
                .filter(|(v, _)| *v > subscriber_version)
                .map(|(_, d)| d.clone())
                .collect();

            // Should we use diffs or snapshot?
            if self.should_use_diffs(&diffs) {
                return ValueUpdate::Diffs(diffs);
            }
        }

        // Fall back to snapshot
        ValueUpdate::Snapshot(Arc::new(Value::List(
            self.current_snapshot.clone(),
            ValueMetadata::new(),
        )))
    }

    /// Decide: diffs or snapshot?
    fn should_use_diffs(&self, diffs: &[Arc<ListDiff>]) -> bool {
        // Too many diffs?
        if diffs.len() > self.config.snapshot_threshold {
            return false;
        }

        // Cost comparison
        let diff_cost: usize = diffs.iter().map(|d| d.cost()).sum();
        let snapshot_cost = self.current_snapshot.iter()
            .map(|(_, v)| 8 + v.estimated_size())
            .sum::<usize>();

        (diff_cost as f64) < (snapshot_cost as f64 * self.config.cost_factor)
    }

    pub fn current_version(&self) -> u64 {
        self.oldest_version + self.diffs.len() as u64
    }

    pub fn snapshot(&self) -> Arc<Vec<(ItemId, Arc<Value>)>> {
        self.current_snapshot.clone()
    }
}
```

#### 3.4 ValueActor (New)

```rust
pub struct ValueActor {
    construct_info: Arc<ConstructInfoComplete>,

    // Core state
    current_value: Arc<ArcSwap<Value>>,
    current_version: AtomicU64,

    // Notification channel (O(1) memory, just latest version)
    version_sender: watch::Sender<u64>,

    // For collections only
    diff_history: Option<Mutex<DiffHistory>>,

    // Keep inputs alive
    inputs: Vec<Arc<ValueActor>>,

    // Actor task
    loop_task: TaskHandle,
}

impl ValueActor {
    /// Create for scalar types (Number, Text, Tag, Object)
    pub fn new_scalar(
        construct_info: ConstructInfoComplete,
        initial_value: Value,
        input_stream: impl Stream<Item = Value> + 'static,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Arc<Self> {
        let (version_sender, _) = watch::channel(0u64);
        let current_value = Arc::new(ArcSwap::from_pointee(initial_value));
        let current_version = AtomicU64::new(0);

        let actor = Arc::new(Self {
            construct_info: Arc::new(construct_info),
            current_value: current_value.clone(),
            current_version,
            version_sender: version_sender.clone(),
            diff_history: None,
            inputs,
            loop_task: TaskHandle::empty(),
        });

        // Start processing loop
        let actor_weak = Arc::downgrade(&actor);
        let loop_task = Task::start_droppable(async move {
            let mut stream = pin!(input_stream);
            while let Some(value) = stream.next().await {
                if let Some(actor) = actor_weak.upgrade() {
                    actor.current_value.store(Arc::new(value));
                    let new_version = actor.current_version.fetch_add(1, Ordering::SeqCst) + 1;
                    let _ = actor.version_sender.send(new_version);
                } else {
                    break;
                }
            }
        });

        // Set loop_task (requires interior mutability pattern)
        actor
    }

    /// Create for LIST type
    pub fn new_list(
        construct_info: ConstructInfoComplete,
        initial_items: Vec<(ItemId, Arc<Value>)>,
        diff_stream: impl Stream<Item = ListDiff> + 'static,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Arc<Self> {
        let (version_sender, _) = watch::channel(0u64);
        let diff_history = DiffHistory::new(initial_items.clone());
        let initial_value = Value::List(Arc::new(initial_items), ValueMetadata::new());
        let current_value = Arc::new(ArcSwap::from_pointee(initial_value));
        let current_version = AtomicU64::new(0);

        let actor = Arc::new(Self {
            construct_info: Arc::new(construct_info),
            current_value: current_value.clone(),
            current_version,
            version_sender: version_sender.clone(),
            diff_history: Some(Mutex::new(diff_history)),
            inputs,
            loop_task: TaskHandle::empty(),
        });

        // Start processing loop
        let actor_weak = Arc::downgrade(&actor);
        let loop_task = Task::start_droppable(async move {
            let mut stream = pin!(diff_stream);
            while let Some(diff) = stream.next().await {
                if let Some(actor) = actor_weak.upgrade() {
                    if let Some(history) = &actor.diff_history {
                        let mut history = history.lock().unwrap();
                        let new_version = actor.current_version.fetch_add(1, Ordering::SeqCst) + 1;
                        history.add(new_version, diff);
                        // Update current_value from history snapshot
                        let snapshot = history.snapshot();
                        actor.current_value.store(Arc::new(Value::List(snapshot, ValueMetadata::new())));
                        let _ = actor.version_sender.send(new_version);
                    }
                } else {
                    break;
                }
            }
        });

        actor
    }

    /// Subscribe to this actor
    pub fn subscribe(self: &Arc<Self>) -> Subscription {
        Subscription {
            actor: self.clone(),
            last_seen_version: 0,
            version_receiver: self.version_sender.subscribe(),
        }
    }

    /// Get current value without subscribing (one-shot read)
    pub fn current_value(&self) -> Arc<Value> {
        self.current_value.load_full()
    }

    /// Get current version
    pub fn version(&self) -> u64 {
        self.current_version.load(Ordering::SeqCst)
    }

    /// Get optimal update for subscriber
    pub fn get_update_since(&self, subscriber_version: u64) -> ValueUpdate {
        let current = self.version();
        if subscriber_version >= current {
            return ValueUpdate::Current;
        }

        // For collections with diff history
        if let Some(history) = &self.diff_history {
            return history.lock().unwrap().get_update_since(subscriber_version);
        }

        // For scalars: always snapshot
        ValueUpdate::Snapshot(self.current_value.load_full())
    }
}
```

#### 3.5 Subscription

```rust
pub struct Subscription {
    actor: Arc<ValueActor>,
    last_seen_version: u64,
    version_receiver: watch::Receiver<u64>,
}

impl Subscription {
    /// Wait for next update and return optimal data
    pub async fn next(&mut self) -> ValueUpdate {
        // Wait for version to change
        loop {
            let current = *self.version_receiver.borrow();
            if current > self.last_seen_version {
                break;
            }
            // Wait for change notification
            if self.version_receiver.changed().await.is_err() {
                // Sender dropped - actor gone
                return ValueUpdate::Current;
            }
        }

        // Pull optimal update
        let update = self.actor.get_update_since(self.last_seen_version);
        self.last_seen_version = self.actor.version();
        update
    }

    /// Get current value immediately (no wait)
    pub fn current(&self) -> Arc<Value> {
        self.actor.current_value()
    }

    /// Check if there are pending updates
    pub fn has_pending(&self) -> bool {
        *self.version_receiver.borrow() > self.last_seen_version
    }
}

/// Make Subscription a Stream for compatibility
impl Stream for Subscription {
    type Item = ValueUpdate;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check if already have pending update
        let current = *self.version_receiver.borrow();
        if current > self.last_seen_version {
            let update = self.actor.get_update_since(self.last_seen_version);
            self.last_seen_version = self.actor.version();
            return Poll::Ready(Some(update));
        }

        // Wait for change
        match Pin::new(&mut self.version_receiver).poll_changed(cx) {
            Poll::Ready(Ok(())) => {
                let update = self.actor.get_update_since(self.last_seen_version);
                self.last_seen_version = self.actor.version();
                Poll::Ready(Some(update))
            }
            Poll::Ready(Err(_)) => Poll::Ready(None), // Actor dropped
            Poll::Pending => Poll::Pending,
        }
    }
}
```

---

### 4. Combinator Integration

#### 4.1 THEN with New Architecture

```rust
// THEN: Evaluate body on each trigger, reading dependencies as snapshots

async fn evaluate_then(
    trigger_subscription: &mut Subscription,
    body: &Expression,
    dependencies: &[Arc<ValueActor>],
) {
    loop {
        // Wait for trigger
        let trigger_update = trigger_subscription.next().await;
        if matches!(trigger_update, ValueUpdate::Current) {
            continue;
        }

        // Read dependencies as SNAPSHOTS (not subscribing!)
        // This is key: no queue buildup for slow THEN
        let dep_values: Vec<Arc<Value>> = dependencies
            .iter()
            .map(|dep| dep.current_value())  // One-shot read
            .collect();

        // Evaluate body with snapshot values
        let result = evaluate_body(body, &dep_values).await;

        // Emit result...
    }
}
```

#### 4.2 WHILE with New Architecture

```rust
// WHILE: Subscribe to body, forward all updates

async fn evaluate_while(
    condition_subscription: &mut Subscription,
    body_actor: &Arc<ValueActor>,
    output_sender: &watch::Sender<u64>,
) {
    let mut body_subscription: Option<Subscription> = None;

    loop {
        if let Some(body_sub) = &mut body_subscription {
            // Forward body updates
            tokio::select! {
                update = body_sub.next() => {
                    // Forward update to our subscribers
                    // ...
                }
                condition_update = condition_subscription.next() => {
                    // Re-evaluate condition
                    if !condition_matches(&condition_update) {
                        body_subscription = None;
                    }
                }
            }
        } else {
            // Wait for condition to become true
            let update = condition_subscription.next().await;
            if condition_matches(&update) {
                body_subscription = Some(body_actor.subscribe());
            }
        }
    }
}
```

#### 4.3 HOLD Integration

HOLD's `BackpressurePermit` still works - it controls evaluation pacing.
The new architecture just changes how values are delivered.

```rust
// HOLD body still uses permit for state consistency
// But now subscribers get version notifications instead of queued values

// In HOLD's state update:
fn update_state(&mut self, new_value: Value) {
    self.current_value.store(Arc::new(new_value));
    let new_version = self.current_version.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = self.version_sender.send(new_version);
    self.permit.release();  // Allow next body evaluation
}
```

---

### 5. Transformation Chain Implementation

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

### 6. Network Sync Protocol

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

### 7. Test Cases

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

### 8. Configuration Constants

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

### 9. Migration Strategy

#### Phase 1: Add versioning to ValueActor (non-breaking)
- Add `current_version: AtomicU64`
- Add `version_sender: watch::Sender<u64>`
- Keep existing subscription mechanism working

#### Phase 2: New Subscription type (parallel)
- Create `VersionedSubscription` alongside existing `Subscription`
- Test with specific use cases

#### Phase 3: Migrate combinators
- Update THEN to use snapshot reads for dependencies
- Update WHILE to use versioned subscription
- Update HOLD state management

#### Phase 4: Add DiffHistory to LIST
- Implement `DiffHistory`
- Add `ItemId` to list items
- Convert existing `ListChange` to `ListDiff`

#### Phase 5: Remove old subscription (breaking)
- Remove `mpsc::unbounded` subscription channels
- All subscriptions use version + pull

---

### 10. Edge Cases

| Scenario | Behavior |
|----------|----------|
| Subscriber never polls | Version notifications overwrite (O(1) memory) |
| Actor dropped while subscriber waiting | `watch::Receiver::changed()` returns `Err`, subscription ends |
| Concurrent updates during pull | Subscriber gets version at time of pull, may need another pull |
| DiffHistory full during burst | Old diffs dropped, slow subscriber gets snapshot |
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
