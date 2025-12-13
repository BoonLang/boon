# Snapshot vs Stream Semantics

## Overview

Boon has two modes of data access:
- **Streaming**: Continuous updates (default behavior)
- **Snapshot**: Single point-in-time value

The difference determines whether your code receives ongoing updates or a one-time snapshot.

## The Context Flag

`is_snapshot_context` propagates through the call stack:
- THEN/WHEN bodies → snapshot context
- WHILE bodies → streaming context (default)
- User functions → inherit caller's context

## How Constructs Set Context

### THEN - Copy on Event

```boon
event |> THEN { body }  -- body evaluated in snapshot context
```

- Evaluates `body` once per event
- All variable references in `body` use `.snapshot()`
- Result: single value per event trigger

### WHEN - Pattern Match on Event

```boon
input |> WHEN { pattern => body }  -- body evaluated in snapshot context
```

- Same as THEN, but with pattern matching
- Body only evaluated when pattern matches
- One value produced per matching event

### WHILE - Continuous While Pattern Matches

```boon
input |> WHILE { pattern => body }  -- body evaluated in streaming context
```

- Body produces continuous stream while pattern matches
- Variable references in `body` use `.stream()`
- Continuous updates until pattern no longer matches

## How It Interacts with Lists

### DiffCollection: Two Ways to Access

Lists implement `DiffCollection` with dual APIs:

| Method | Context | Returns |
|--------|---------|---------|
| `snapshot_count()` | Snapshot | `Future<usize>` - count at this moment |
| `stream_count()` | Streaming | `Stream<usize>` - count updates continuously |

### Example: List/count

```boon
-- Streaming: count updates when list changes
count: my_list |> List/count()

-- Snapshot: count at the moment of event
count: event |> THEN { my_list |> List/count() }
```

## Remote Lists: Performance Considerations

When a List lives on a server:

### Snapshot Context (THEN/WHEN)

```boon
button_click |> THEN { server_list |> List/count() }
```

- Calls `snapshot_count()` → single RPC request
- Efficient even when called rarely (once per day) or frequently (every second)
- No persistent connection needed

### Streaming Context (Default)

```boon
count: server_list |> List/count()
```

- Calls `stream_count()` → persistent subscription
- Server pushes updates when list changes
- Efficient for frequently-changing lists displayed in UI
- Uses WebSocket/SSE for push updates

### Choosing the Right Pattern

| Use Case | Pattern | Why |
|----------|---------|-----|
| Show live count in UI | Streaming | Auto-updates when list changes |
| Log count on button click | Snapshot (THEN) | One-time query, no subscription |
| Periodic report (daily) | Snapshot (Timer + THEN) | No need for live updates |
| Dashboard with many lists | Streaming | All update automatically |

## Version-Based Recovery for Diffs

Streaming uses diff-based updates (efficient, incremental). But diffs cannot be lossy:

```rust
pub enum ValueUpdate {
    Current(Vec<Value>),     // Full snapshot (initial or resync)
    Diffs(Vec<ListDiff>),    // Incremental updates
}
```

- Each diff has a version number
- Subscriber tracks `last_seen_version`
- On version gap → automatic full snapshot resync

## Nested Functions

Context propagates through user-defined functions:

```boon
FUNCTION get_summary() {
    count: my_list |> List/count()  -- Inherits caller's context
    TEXT { Total: {count} }
}

-- Streaming: count updates continuously
summary: get_summary()

-- Snapshot: count at event time
summary: event |> THEN { get_summary() }
```

The same function behaves differently based on how it's called.

## Type-Safe API (Internal Implementation)

The engine provides type-safe methods:

```rust
impl ValueActor {
    /// Exactly ONE value - returns Future, not Stream
    pub fn snapshot(self: Arc<Self>) -> impl Future<Output = Option<Value>>

    /// Continuous stream of all values
    pub fn stream(self: Arc<Self>) -> impl Stream<Item = Value>
}
```

The different return types make it impossible to accidentally mix up the two modes.

## Summary

| Construct | Context | Subscription | Result |
|-----------|---------|--------------|--------|
| Default | Streaming | `.stream()` | Continuous updates |
| THEN body | Snapshot | `.snapshot()` | One value per event |
| WHEN body | Snapshot | `.snapshot()` | One value per match |
| WHILE body | Streaming | `.stream()` | Continuous while matched |
| Function body | Inherited | Depends on caller | Same as caller |
