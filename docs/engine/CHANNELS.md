# Channel Architecture

All inter-actor communication uses bounded, named channels with explicit backpressure handling.

## Overview

The Boon engine uses an actor model where components communicate via message-passing channels. This architecture enables:
- **Distributed readiness**: No shared mutable state, all coordination via channels
- **Bounded buffers**: Required for network flow control and memory safety
- **Explicit backpressure**: Slow consumers don't overwhelm producers
- **Drop semantics**: Graceful degradation under load

## NamedChannel<T>

Wrapper around `mpsc::Sender<T>` providing:
- **Named identification** for debugging
- **Backpressure logging** when events are dropped
- **Debug timeouts** via feature flag for detecting deadlocks

```rust
pub struct NamedChannel<T> {
    inner: mpsc::Sender<T>,
    name: &'static str,
    capacity: usize,
}
```

### Methods

| Method | Description | Use Case |
|--------|-------------|----------|
| `send().await` | Async send with debug timeout | Must-deliver messages in async context |
| `try_send()` | Sync send, returns error if full | Fire-and-forget from sync callbacks |
| `send_or_drop()` | Send or silently drop, logs in debug | DOM events where dropping is OK |

## Capacity Guidelines

| Channel Type | Capacity | Rationale |
|-------------|----------|-----------|
| Unit signals `()` | 1 | Single pending signal sufficient |
| Control messages | 8-16 | Low rate, must complete |
| Subscriptions | 32 | May have burst of new subscribers |
| Value data | 32-64 | Buffer for bursty updates |
| LINK/DOM events | 64-128 | User input can be bursty |
| DOM hover/blur | 2-8 | Simple state, drop OK |

## Backpressure Strategies

### Block (must-deliver)
- **Used for**: Actor control messages, subscriptions, state updates
- **Behavior**: `.send().await` blocks until space available
- **When full**: Sender waits (with debug timeout warning)

### Drop-newest (responsive UI)
- **Used for**: DOM events (clicks, keypresses, hover)
- **Behavior**: `.send_or_drop()` drops new event if full, logs in debug
- **When full**: New events discarded to maintain responsiveness

### Try-send (sync contexts)
- **Used for**: Synchronous callbacks, fire-and-forget
- **Behavior**: `.try_send()` returns immediately with result
- **When full**: Caller decides (usually ignore or log)

## Debug Mode

Enable with: `cargo build --features debug-channels` (enabled by default)

Provides:
- 5-second timeout on all channel sends
- Detailed logging of dropped events
- Channel name in all log messages

### Log Prefixes

| Prefix | Meaning |
|--------|---------|
| `[CHANNEL TIMEOUT]` | Send blocked > 5s (possible deadlock) |
| `[BACKPRESSURE DROP]` | Event dropped due to full channel |
| `[BACKPRESSURE]` | Channel full (`try_send` failed) |

To disable for production: `cargo build --no-default-features`

## Channel Registry

### engine.rs

| Component | Channel Name | Capacity | Strategy |
|-----------|--------------|----------|----------|
| ValueActor | `value_actor.messages` | 16 | block |
| ValueActor | `value_actor.subscriptions` | 32 | block |
| ValueActor | `value_actor.direct_store` | 64 | block |
| ValueActor | `value_actor.queries` | 8 | block |
| LazyValueActor | `lazy_value_actor.requests` | 16 | block |
| ConstructStorage | `construct_storage.inserter` | 32 | block |
| ConstructStorage | `construct_storage.getter` | 32 | block |
| VirtualFilesystem | `virtual_fs.requests` | 32 | block |
| ReferenceConnector | `reference_connector.inserter` | 64 | try_send |
| ReferenceConnector | `reference_connector.getter` | 64 | try_send |
| LinkConnector | `link_connector.inserter` | 64 | try_send |
| LinkConnector | `link_connector.getter` | 64 | try_send |
| PassThroughConnector | `pass_through.ops` | 32 | try_send |
| PassThroughConnector | `pass_through.getter` | 32 | try_send |
| PassThroughConnector | `pass_through.sender_getter` | 32 | try_send |
| ActorOutputValveSignal | `output_valve.subscriptions` | 32 | try_send |
| List | `list.change_subscribers` | 32 | try_send |
| List | `list.diff_subscribers` | 32 | try_send |
| List | `list.diff_queries` | 16 | try_send |
| BackpressuredStream | (demand signal) | 1 | try_send |

### evaluator.rs

| Component | Channel Name | Capacity | Strategy |
|-----------|--------------|----------|----------|
| ModuleLoader | `module_loader.requests` | 16 | try_send |

### api.rs

| Component | Channel Name | Capacity | Strategy |
|-----------|--------------|----------|----------|
| Router | (route changes) | 8 | try_send |

## BackpressureCoordinator

Replaces the old `BackpressurePermit` which used shared atomics and Mutex.

The new coordinator uses pure message-passing:
- `acquire()` sends request, waits for grant
- `release()` sends completion signal
- Sequential ordering enforced by bounded(1) request channel

This enables distributed/cluster deployment where shared memory isn't available.

## Migration Notes

When adding new channels:

1. **Choose appropriate capacity** based on expected throughput
2. **Use NamedChannel** for debugging visibility
3. **Pick backpressure strategy**:
   - `send().await` for must-deliver in async
   - `try_send()` for sync contexts
   - `send_or_drop()` for droppable events
4. **Add to this registry** for documentation

### Converting unbounded to bounded

```rust
// Before (unbounded)
let (tx, rx) = mpsc::unbounded::<T>();
tx.unbounded_send(value).ok();

// After (bounded with NamedChannel)
let (tx, rx) = NamedChannel::new("component.channel", 32);
tx.try_send(value).ok();  // sync context
// or
tx.send(value).await.ok();  // async context
```
