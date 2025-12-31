# Plan: Replace Hardcoded Event LINK Detection

## Overview

The Toggle All bug requires filtering "stale" events from being replayed to late subscribers. Two approaches are being considered:

**Current fragile pattern:**
```rust
let is_event_link = is_link && matches!(
    alias_part.as_str(),
    "click" | "press" | "key_down" | "change" | "blur" | "double_click" | "hovered"
);
```

---

## Approach Comparison

| Aspect | LinkType Enum | Causal/Version Approach |
|--------|--------------|------------------------|
| Where filtering happens | Subscription setup (`stream_from_now()`) | Value processing (at WHEN/THEN entrance) |
| What's filtered | By source type (Event/State) | By temporal ordering (version comparison) |
| Extensibility | Add to enum per new LINK type | Automatic for ALL values |
| Complexity | Simple, compile-time | More complex, runtime |
| Memory overhead | 1 byte per LINK Variable | 8 bytes per Value |
| Generality | Only for LINK variables | Works for ANY value source |
| Existing infrastructure | `starting_version` in SubscriptionSetup | `ValueMetadata`, `current_version` in ValueActor |

---

## Option A: LinkType Enum (Simple, LINK-specific)

See "Solution: LinkType Enum" section below.

## Option B: Causal/Version Approach (General, Value-level)

### Core Idea

Instead of marking LINKs at creation, use version/timestamp on values to automatically filter stale data at the WHEN/THEN entrance.

**Inspired by:**
- [Lamport clocks](https://martinfowler.com/articles/patterns-of-distributed-systems/lamport-clock.html) - logical timestamps for event ordering
- [SkipLabs dependency tracking](https://skiplabs.io/blog/cache_invalidation) - reactive dependency graphs with version invalidation
- FRP glitch freedom research - "a glitch is when a value is propagated but it is not a fresh value"

### How it would work:

1. **Extend `ValueMetadata` with origin version:**
```rust
pub struct ValueMetadata {
    pub idempotency_key: ValueIdempotencyKey,
    pub origin_version: u64,  // NEW: when was this value created?
}
```

2. **Stamp values when emitted from ValueActor:**
```rust
// In ValueActor's internal loop, when emitting:
let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
let value = value.with_origin_version(new_version);  // stamp it
```

3. **Record "subscription epoch" in ActorContext:**
```rust
pub struct ActorContext {
    // ... existing fields ...
    /// Epoch for filtering stale values in THEN/WHEN bodies
    pub subscription_epoch: Option<u64>,
}
```

4. **Set epoch when THEN/WHEN is created:**
```rust
// In build_then_actor():
let subscription_epoch = get_current_input_version(&piped);  // snapshot the current version
let new_actor_context = ActorContext {
    subscription_epoch: Some(subscription_epoch),
    // ...
};
```

5. **Filter at value processing (in VariableOrArgumentReference):**
```rust
// Instead of checking LINK type:
if let Some(epoch) = actor_context.subscription_epoch {
    if value.origin_version() <= epoch {
        continue; // Skip stale value
    }
}
```

### Challenge: Cross-Actor Version Comparison

Each ValueActor has its own version counter. For cross-actor comparison, we'd need:

**Option B1: Global Monotonic Counter**
- Single `AtomicU64` shared across all actors
- Simple but potential contention in high-throughput scenarios

**Option B2: Lamport Clock**
- Each actor has local counter
- On message receive: `local = max(local, received) + 1`
- Provides happened-before ordering without global state

**Option B3: Source-Scoped Versions (hybrid)**
- Only compare versions within same source actor
- Falls back to LinkType for cross-actor cases
- Combines benefits of both approaches

### Benefits of Causal Approach

1. **Truly general** - Works for ANY value, not just LINKs
2. **No manual marking** - Automatic based on temporal ordering
3. **Future-proof** - New event sources work automatically
4. **Aligned with FRP theory** - Glitch freedom through temporal ordering

### Drawbacks of Causal Approach

1. **Memory overhead** - 8 bytes per Value
2. **Runtime cost** - Version comparison on each value
3. **Complexity** - Cross-actor ordering is non-trivial
4. **Existing infrastructure** - `stream_from_now()` already works well

---

## Selected Approach: Option B (Causal/Version)

**Chosen for**:
- Generality - Works for ANY value, not just LINKs
- Aligned with FRP glitch-freedom theory
- Future-proof for Boon's distributed/hardware targets
- No manual marking of event types required

---

## Implementation: Lamport Clock-based Filtering

### Step 1: Add Lamport Clock Infrastructure (engine.rs)

Lamport clocks provide happened-before ordering without global state:

```rust
// At module level in engine.rs
use std::sync::atomic::{AtomicU64, Ordering};
use std::cell::Cell;

// Thread-local Lamport clock for the current execution context
thread_local! {
    static LOCAL_CLOCK: Cell<u64> = const { Cell::new(0) };
}

/// Advance local clock and return new timestamp (for value creation).
pub fn lamport_tick() -> u64 {
    LOCAL_CLOCK.with(|c| {
        let new_time = c.get() + 1;
        c.set(new_time);
        new_time
    })
}

/// Update local clock on receiving a value (Lamport receive rule).
/// local = max(local, received) + 1
pub fn lamport_receive(received_time: u64) -> u64 {
    LOCAL_CLOCK.with(|c| {
        let new_time = c.get().max(received_time) + 1;
        c.set(new_time);
        new_time
    })
}

/// Get current local clock value (for recording subscription epoch).
pub fn lamport_now() -> u64 {
    LOCAL_CLOCK.with(|c| c.get())
}
```

### Step 2: Extend ValueMetadata with lamport_time (engine.rs ~line 4539)

```rust
#[derive(Debug, Clone, Copy)]
pub struct ValueMetadata {
    pub idempotency_key: ValueIdempotencyKey,
    pub lamport_time: u64,  // NEW: Lamport timestamp when value was created
}

impl ValueMetadata {
    pub fn new(idempotency_key: ValueIdempotencyKey) -> Self {
        Self {
            idempotency_key,
            lamport_time: lamport_tick(),  // Stamp with current Lamport time
        }
    }
}
```

### Step 3: Add Value helper methods (engine.rs)

```rust
impl Value {
    pub fn lamport_time(&self) -> u64 {
        self.metadata().lamport_time
    }

    /// Returns true if this value happened-before the given timestamp.
    /// Such values are considered "stale" and should be filtered.
    pub fn happened_before(&self, timestamp: u64) -> bool {
        self.lamport_time() <= timestamp
    }
}
```

### Step 4: Add subscription_time to ActorContext (engine.rs ~line 1700)

```rust
pub struct ActorContext {
    // ... existing fields ...

    /// Lamport timestamp at which this context was created.
    /// Used by THEN/WHEN to filter values that happened-before subscription.
    /// None = no filtering (streaming context, accept all values)
    /// Some(time) = filter values with lamport_time <= time
    pub subscription_time: Option<u64>,
}
```

### Step 5: Set subscription_time in THEN/WHEN (evaluator.rs)

**In build_then_actor (~line 2783):**
```rust
let new_actor_context = ActorContext {
    // ... existing fields ...
    // Record current Lamport time - values from before this are stale
    subscription_time: Some(lamport_now()),
    // ...
};
```

**In build_when_actor (~similar):**
```rust
let new_actor_context = ActorContext {
    // ...
    subscription_time: Some(lamport_now()),
    // ...
};
```

### Step 6: Apply Lamport receive rule and filter (engine.rs ~line 2316)

Replace the hardcoded event LINK check with Lamport-based filtering:

```rust
// BEFORE (fragile hardcoded names):
let is_link = variable.link_value_sender().is_some();
let is_event_link = is_link && matches!(
    alias_part.as_str(),
    "click" | "press" | "key_down" | "change" | "blur" | "double_click" | "hovered"
);
if is_event_link {
    actor.stream_from_now()
} else {
    actor.stream()
}

// AFTER (Lamport clock filtering):
// Always use stream() - filtering happens at value processing time
let mut subscription = actor.stream();

// In the stream processing:
subscription
    .filter_map(move |value| {
        // Apply Lamport receive rule: update local clock
        lamport_receive(value.lamport_time());

        // If we have a subscription time, filter stale values
        if let Some(sub_time) = subscription_time {
            if value.happened_before(sub_time) {
                return future::ready(None); // Skip stale value
            }
        }
        future::ready(Some(value))
    })
```

### Step 7: Update all Value constructors to use new ValueMetadata

All places that create `ValueMetadata` need to use the new constructor:

```rust
// BEFORE:
ValueMetadata { idempotency_key }

// AFTER:
ValueMetadata::new(idempotency_key)
```

This automatically stamps each value with a Lamport timestamp.

### Step 8: Remove old hardcoded is_event_link pattern

Remove the following from `VariableOrArgumentReference::new_arc_value_actor` (engine.rs ~line 2294-2298):

```rust
// REMOVE THIS:
let is_link = variable.link_value_sender().is_some();
let is_event_link = is_link && matches!(
    alias_part.as_str(),
    "click" | "press" | "key_down" | "change" | "blur" | "double_click" | "hovered"
);

// REMOVE the is_event_link conditional:
if is_event_link {
    actor.stream_from_now()  // DELETE
} else {
    actor.stream()           // KEEP (always use stream())
}
```

Replace with simple `actor.stream()` call - filtering now happens at value processing time via Lamport timestamps.

---

## Files to Modify

| File | Change |
|------|--------|
| `crates/boon/src/platform/browser/engine.rs` | Add `LOCAL_CLOCK`, `lamport_tick()`, `lamport_receive()`, `lamport_now()` |
| `crates/boon/src/platform/browser/engine.rs` | Extend `ValueMetadata` with `lamport_time` |
| `crates/boon/src/platform/browser/engine.rs` | Add `Value::lamport_time()`, `happened_before()` |
| `crates/boon/src/platform/browser/engine.rs` | Add `subscription_time` to `ActorContext` |
| `crates/boon/src/platform/browser/engine.rs` | Update `VariableOrArgumentReference` to apply Lamport receive + filter |
| `crates/boon/src/platform/browser/engine.rs` | **REMOVE** `is_event_link` hardcoded check and `stream_from_now()` calls |
| `crates/boon/src/platform/browser/evaluator.rs` | Set `subscription_time` in `build_then_actor` |
| `crates/boon/src/platform/browser/evaluator.rs` | Set `subscription_time` in `build_when_actor` |
| All files creating `ValueMetadata` | Use `ValueMetadata::new()` instead of struct literal |

---

## Testing

After implementation:
1. All 11 `boon-tools exec test-examples` tests pass
2. Toggle All bug test (Section 6b) continues to pass
3. New test: verify stale event filtering works for dynamically created subscriptions

---

## Why Lamport Clocks

**The Lamport Clock Algorithm:**
1. Each process maintains a local counter (not shared)
2. On send: attach current counter to message, increment counter
3. On receive: `local = max(local, received) + 1`

**Happened-Before Ordering:**
- If event A causes event B, then `timestamp(A) < timestamp(B)`
- Provides causal ordering without global synchronization
- No contention on shared state (each actor has its own counter)

**Why thread_local! is used:**
- Browser WASM is single-threaded
- `thread_local!` gives us zero-overhead access
- If Boon moves to multi-threaded (WebWorkers), each worker gets its own clock
- Lamport semantics still work: clocks sync on message passing

**Future Distributed Boon:**
- When actors run on different machines/workers
- Lamport clocks naturally handle network delays
- No central clock server needed
- Happens-before is preserved across network boundaries

---

## Benefits Over LinkType Approach

1. **No manual marking** - Events are automatically detected as stale
2. **Works for all values** - Not just LINKs, but any value source
3. **Theoretically sound** - Based on FRP glitch-freedom principles
4. **Future-proof** - Infrastructure for distributed Boon

---

## References

**FRP Glitch Freedom Theory:**
- [Glitch Avoidance in Distributed Reactive Apps using Timestamps](https://www.academia.edu/34767761/Glitch_Avoidance_in_a_Distributed_Reactive_Web_Application_using_timestamps)
- [Asynchronous FRP for GUIs (Elm paper, PLDI 2013)](https://people.seas.harvard.edu/~chong/pubs/pldi13-elm.pdf)
- [Functional Reactive Programming - Wikipedia](https://en.wikipedia.org/wiki/Functional_Reactive_Programming)

**Related Distributed Systems Patterns:**
- [Lamport Clock (Martin Fowler)](https://martinfowler.com/articles/patterns-of-distributed-systems/lamport-clock.html)
- [SkipLabs: Cache Invalidation and Reactive Systems](https://skiplabs.io/blog/cache_invalidation)
