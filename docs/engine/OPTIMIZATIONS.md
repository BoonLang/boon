# Reactive Engine Performance Optimizations

This document describes the performance optimizations implemented in Boon's reactive engine to address O(N^2) performance issues during filter switching and similar operations.

## Problem Statement

When switching filters in TodoMVC with N todos, the naive implementation caused O(N^2) work:

```
Filter button clicked
    ↓
selected_filter changes (1 event)
    ↓
WHILE inside List/retain for each todo evaluates new arm (N WHILE arms)
    ↓
Each predicate emits new boolean (N predicate updates)
    ↓
List/retain emits ListChange::Replace on EACH predicate update (N Replace events)
    ↓
List/map receives N Replace events, calls transform_item for ALL items each time
    ↓
O(N^2) work!
```

## Implemented Optimizations

### Phase 1: Coalescing & Deduplication

#### A3: Coalesce Stream Combinator

**Location:** `engine.rs` (near `switch_map`)

A reusable stream combinator that batches all synchronously-available items before yielding:

```rust
pub fn coalesce<S>(source: S) -> impl Stream<Item = Vec<S::Item>>
```

**How it works:**
1. Wait for at least one item (blocking point)
2. Non-blocking: drain all immediately-ready items using `poll_fn`
3. Emit the batch as a Vec

**Applied to:**
- `List/retain` - merged predicate streams
- `List/sort_by` - sort key streams
- `List/every` and `List/any` - predicate streams

**Effect:** N predicate updates arriving synchronously → 1 batched emission

#### B2: Boolean Predicate Deduplication

**Location:** `engine.rs` (in List/retain, List/every, List/any)

Each predicate stream is wrapped with a deduplication filter that skips emissions when the boolean value hasn't changed:

```rust
.scan(None::<bool>, |last_bool, (pid, is_true)| {
    if Some(is_true) == *last_bool {
        future::ready(Some(None)) // Skip duplicate
    } else {
        *last_bool = Some(is_true);
        future::ready(Some(Some((pid, is_true))))
    }
})
.filter_map(future::ready)
```

**Effect:** When filter switches from "All" to "Active", items that were already showing (predicate True → True) don't emit at all.

#### A2: Output Deduplication

**Location:** `engine.rs` (in List/retain)

Before emitting a Replace, compare the filtered PersistenceIds with the last emitted set. Skip if identical (order-aware comparison using Vec, not HashSet).

```rust
let current_pids: Vec<_> = filtered.iter()
    .map(|item| item.persistence_id())
    .collect();
if current_pids == last_emitted_pids {
    continue; // Skip redundant emission
}
last_emitted_pids = current_pids;
```

**Effect:** Safety net that catches any remaining redundant emissions.

### Phase 2: Caching

#### B1: List/map Transform Cache

**Location:** `engine.rs` (in `transform_list_change_for_map_with_tracking`)

Caches transformed ValueActors by source PersistenceId to avoid re-transforming unchanged items:

```rust
type MapState = (
    usize,
    HashMap<PersistenceId, PersistenceId>,
    HashMap<PersistenceId, Arc<ValueActor>>, // Transform cache
);
```

On Replace:
1. Check cache for each item by source PersistenceId
2. If cache hit, reuse the cached transformed actor
3. If cache miss, call `transform_item` and add to cache
4. Clean up cache entries for items no longer in the list

**Effect:** O(N) transform calls → O(new items only)

**Note:** Cached actors still forward updates from their source - no staleness issue.

### Phase 3: Fine-Grained Updates (Future)

#### C1: Smart List/retain Diffing

**Status:** Not yet implemented

Instead of emitting Replace for every predicate change, emit InsertAt/Remove based on which items' visibility changed:

- Predicate True → False: emit `Remove { id }`
- Predicate False → True: emit `InsertAt { index, item }`

**Effect:** Single predicate change → O(1) work

## Combined Effect

With all Phase 1 and Phase 2 optimizations:

**Before:**
```
Filter switch with 10 todos, 3 completed (All → Active):
- 10 predicate updates
- 10 Replace emissions from List/retain
- 10 × 10 = 100 transform_item calls in List/map
- O(N^2) work
```

**After:**
```
Filter switch with 10 todos, 3 completed (All → Active):
- B2: 7 predicates don't emit (True → True)
- A3: 3 changed predicates batched into 1 emission
- A2: Skip if result unchanged
- B1: Cache hit for all 7 unchanged items
- 1 Replace emission, 0 transform_item calls (all cached)
- O(N) work
```

## Files Modified

| File | Changes |
|------|---------|
| `engine.rs` | Added `coalesce()` combinator, B2 dedup in List/retain, List/every, List/any |
| `engine.rs` | Added A2 output dedup in List/retain |
| `engine.rs` | Added A3 coalesce to List/retain, List/sort_by, List/every, List/any |
| `engine.rs` | Added B1 transform cache to List/map |

## Hardware Mapping

These optimizations align with hardware design principles:

- **Coalesce:** "Drain FIFO until empty" - standard hardware pattern
- **Boolean dedup:** Simple comparator gate
- **Transform cache:** Content-addressable memory (CAM)
- **Actor model:** Each optimization is localized to specific actors, maintaining portability to HVM/hardware

## Known Limitations

1. **List/sort_by:** No boolean dedup (sort keys are arbitrary values, not booleans). Coalescing still applies.

## Future Considerations

- **Virtual List / Windowing:** For very large lists, only render visible items
- **Lazy Transform Evaluation:** Don't transform until item is actually rendered
- **MobX-style Computed Values:** Track dependencies for automatic cache invalidation
