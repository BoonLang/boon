# Channel Architecture

All inter-actor communication uses bounded, named channels with explicit backpressure handling.

## Migration Progress

| Category | Done | TODO | Notes |
|----------|------|------|-------|
| engine.rs (core actors) | 20 | 0 | ✅ Complete |
| engine.rs (additional) | 3 | 0 | ✅ Complete (LINK, forwarding, List changes) |
| evaluator.rs | 4 | 0 | ✅ Complete (WHILE, object fields, HOLD state) |
| bridge.rs (DOM events) | 10 | 0 | ✅ Complete - all use `send_or_drop()` |
| api.rs | 1 | 0 | ✅ Complete |
| **Total** | **38** | **0** | ✅ All channels converted to bounded |

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

## Request-Response vs Fire-and-Forget

### When Request-Response (oneshot) is REQUIRED

Use oneshot channels when:
- **READ operations** - Caller needs data back (can't proceed without it)
- **Sequential ordering** - Next operation depends on previous result
- **Demand-driven polling** - Each value is explicitly requested (e.g., LazyValueActor)

Examples:
- `StoredValueQuery` - needs current value from actor
- `VirtualFilesystem.read_text()` - needs file content
- `ConstructStorage.load_state()` - needs stored state
- `BackpressureCoordinator.acquire()` - must wait for grant

### When Fire-and-Forget Works

Use fire-and-forget when:
- **WRITE operations** - Eventual consistency is acceptable
- **Subscriptions** - Caller creates channel, sends sender, uses receiver immediately
- **Event notifications** - Dropping under load is acceptable

Examples:
- `ConstructStorage.save_state()` - persist asynchronously
- `VirtualFilesystem.write_text()` - write asynchronously
- Subscription setup - caller creates (tx, rx), sends tx, returns rx

### Fire-and-Forget Pattern (Subscriptions)

```rust
// 1. Caller creates channel
let (tx, rx) = mpsc::channel(32);

// 2. Send sender to actor (best-effort with logging)
actor_channel.send_or_drop(Setup { sender: tx });

// 3. Return receiver immediately - no await needed
rx

// 4. If caller dropped → actor sees tx disconnect → cleanup
```

### Error Handling Rules

- **NEVER** use `let _ =` to swallow channel errors
- Use `send_or_drop()` for acceptable drops (logs in debug mode)
- Use `try_send()` when caller needs to handle failure explicitly
- Use `send().await` for must-deliver messages

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

### engine.rs (core actors)

| Component | Channel Name | Capacity | Strategy | Status |
|-----------|--------------|----------|----------|--------|
| ValueActor | `value_actor.messages` | 16 | block | ✅ |
| ValueActor | `value_actor.subscriptions` | 32 | block | ✅ |
| ValueActor | `value_actor.direct_store` | 64 | block | ✅ |
| ValueActor | `value_actor.queries` | 8 | block | ✅ |
| LazyValueActor | `lazy_value_actor.requests` | 16 | block | ✅ |
| ConstructStorage | `construct_storage.inserter` | 32 | send_or_drop | ✅ |
| ConstructStorage | `construct_storage.getter` | 32 | block | ✅ |
| VirtualFilesystem | `virtual_fs.requests` | 32 | block | ✅ |
| ReferenceConnector | `reference_connector.inserter` | 64 | try_send | ✅ |
| ReferenceConnector | `reference_connector.getter` | 64 | try_send | ✅ |
| LinkConnector | `link_connector.inserter` | 64 | try_send | ✅ |
| LinkConnector | `link_connector.getter` | 64 | try_send | ✅ |
| PassThroughConnector | `pass_through.ops` | 32 | try_send | ✅ |
| PassThroughConnector | `pass_through.getter` | 32 | try_send | ✅ |
| PassThroughConnector | `pass_through.sender_getter` | 32 | try_send | ✅ |
| ActorOutputValveSignal | `output_valve.subscriptions` | 32 | try_send | ✅ |
| List | `list.change_subscribers` | 32 | try_send | ✅ |
| List | `list.diff_subscribers` | 32 | try_send | ✅ |
| List | `list.diff_queries` | 16 | try_send | ✅ |
| BackpressuredStream | (demand signal) | 1 | try_send | ✅ |

### evaluator.rs

| Component | Channel Name | Capacity | Strategy | Status |
|-----------|--------------|----------|----------|--------|
| ModuleLoader | `module_loader.requests` | 16 | try_send | ✅ |
| WHILE | `while.pass_through` | 64 | block | ✅ |
| Object fields | `object.field_stream` | 32 | block | ✅ |
| HOLD state | `hold.state_updates` | 16 | block | ✅ |

### bridge.rs (DOM Events)

All DOM event channels use `send_or_drop()` - dropping events is acceptable for UI responsiveness.

| Component | Channel Name | Capacity | Strategy | Status |
|-----------|--------------|----------|----------|--------|
| Element | `element.hovered` | 2 | send_or_drop | ✅ |
| Button | `button.press_event` | 8 | send_or_drop | ✅ |
| Button | `button.hovered` | 2 | send_or_drop | ✅ |
| TextInput | `text_input.change` | 16 | send_or_drop | ✅ |
| TextInput | `text_input.key_down` | 32 | send_or_drop | ✅ |
| TextInput | `text_input.blur` | 8 | send_or_drop | ✅ |
| Checkbox | `checkbox.click` | 8 | send_or_drop | ✅ |
| DoubleClick | `double_click.event` | 8 | send_or_drop | ✅ |
| DoubleClick | `double_click.hovered` | 2 | send_or_drop | ✅ |
| Link | `link.hovered` | 2 | send_or_drop | ✅ |

### engine.rs (additional)

| Component | Channel Name | Capacity | Strategy | Status |
|-----------|--------------|----------|----------|--------|
| LINK | `link.values` | 128 | send_or_drop | ✅ |
| Forwarding | `forwarding.values` | 64 | block | ✅ |
| List | `list.changes` | 64 | try_send (keep if full) | ✅ |

### api.rs

| Component | Channel Name | Capacity | Strategy | Status |
|-----------|--------------|----------|----------|--------|
| Router | (route changes) | 8 | try_send | ✅ |

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
