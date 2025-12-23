## Critical Files Summary

| File | Changes |
|------|---------|
| `crates/boon/src/parser.rs` | Add SourceId to Spanned |
| `crates/boon/src/parser/persistence_resolver.rs` | Integrate SourceId |
| `crates/boon/src/platform/browser/engine_v2/mod.rs` | **NEW:** Arena, SlotId, ReactiveNode |
| `crates/boon/src/platform/browser/engine_v2/event_loop.rs` | **NEW:** EventLoop, timer queue |
| `crates/boon/src/platform/browser/engine_v2/message.rs` | **NEW:** Message, Payload, routing |
| `crates/boon/src/platform/browser/engine_v2/nodes.rs` | **NEW:** Node kinds (Router, Register, etc.) |
| `crates/boon/src/platform/browser/evaluator_v2.rs` | **NEW:** Evaluator using SlotId |
| `crates/boon/src/platform/browser/api_v2.rs` | **NEW:** API functions for new engine |
| `crates/boon/src/platform/browser/bridge.rs` | Adapt to support both engines |

---

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Clean rewrite takes longer than expected | Parallel development doesn't block existing users |
| New engine has different semantics | Test against all existing examples early |
| FPGA constraints too restrictive | Start "hardware-inspired", relax if blocking |
| Multi-threading adds complexity | Phase 9 is optional, single-threaded MVP first |
| Snapshot format changes | Version the format, support migration |

---

## Success Criteria

1. **No Arc<ValueActor> in hot path** - Reactive nodes use SlotId (strings use Arc<str>, which is fine)
2. **Deterministic execution** - Same inputs produce same outputs (formal logical ticks)
3. **Full snapshot/restore** - Can serialize and restore any graph state
4. **Timer/interval works** - `interval.bn` example runs correctly
5. **todo_mvc works** - Full app with Lists, LINK, HOLD functions
6. **No deadlocks** - Backpressure with "not ready" pattern, no blocking ever
7. **Simpler debugging** - Single-threaded, explicit state, traceable by NodeAddress
8. **FLUSH works** - Fail-fast error handling with `flush.bn` example
9. **Delta streams** - List updates are O(1), efficient WebSocket sync
10. **Explicit finalization** - No "receiver is gone" errors, clean scope cleanup
11. **Backend ready** - Architecture scales to 500+ users with nested reactive data

---

## Next Steps (Immediate)

### Create Simple FLUSH Example

Create `playground/frontend/src/examples/flush/flush.bn`:

```boon
-- FLUSH example: Fail-fast error handling in List/map

numbers: LIST { 1, 2, TEXT { invalid }, 4, 5 }

result: numbers
    |> List/map(item, new:
        item |> WHEN {
            Number[n] => n * 2        -- Double valid numbers
            other => FLUSH { other }   -- Stop on first non-number
        }
    )
    |> WHEN {
        Number[n] => TEXT { Result: {n} }
        error => TEXT { Error: stopped at non-number }
    }

document: Document/new(root: result)
```

This example demonstrates:
- `FLUSH` exits List/map early on first error
- Remaining items (4, 5) are NOT processed
- Error propagates through pipeline
- Final result shows the error, not partial results

---

## Hardware Synthesis Notes (Future)

For eventual FPGA compilation:
- Each `ReactiveNode` → one module instance
- `RoutingTable` → static wiring
- `EventLoop.current_tick` → global clock
- `dirty` flag → clock enable
- `Message` → bus signals with valid/ready handshake
- `List` → parameterized array of modules

The arena-based design ensures all state is explicitly tracked and addressable, which is essential for HDL generation.

---

## Future Directions (Post-MVP)

### F1. DomainRuntime Trait

Abstract interface for different execution contexts:

```rust
pub trait DomainRuntime {
    fn enqueue(&mut self, message: Message);
    fn poll(&mut self) -> Option<Message>;
    fn snapshot(&self, node_id: NodeAddress) -> Option<NodeSnapshot>;
    fn apply_snapshot(&mut self, node_id: NodeAddress, snapshot: NodeSnapshot);
}

// Implementations:
// - LocalRuntime: single-threaded event loop (Phase 1-7)
// - WorkerRuntime: runs in WebWorker (Phase 9)
// - ServerRuntime: runs on backend over WebSocket (future)
```

**Benefits:**
- Same interface whether running locally or across network
- Makes testing easier (can mock domains)
- Clean abstraction for multi-threaded and distributed execution

**When to implement:** After Phase 7 (Bridge & UI works), before Phase 9 (Multi-Threading).

### F2. WorkerRouter

Main thread component that routes messages to/from workers:

```rust
pub struct WorkerRouter {
    workers: Vec<WebWorker>,
    outbound_queues: Vec<SABRingBuffer>,   // To workers (fast path)
    inbound_queue: SABRingBuffer,          // From workers (shared)
    pending_slow_path: VecDeque<Message>,  // For postMessage fallback
}

impl WorkerRouter {
    fn route(&mut self, msg: Message) {
        let target_worker = msg.to.domain.worker_index();
        if msg.payload.is_primitive() {
            self.outbound_queues[target_worker].push(msg);
        } else {
            // Complex payload: use postMessage
            self.workers[target_worker].post_message(msg.serialize());
        }
    }
}
```

**Partitioning strategy:**
- Main/UI domain: DOM LINK nodes, rendering bridge, UI-facing combinators
- Worker domains: CPU-heavy pure subgraphs (search, indexing, heavy transforms)

### F3. Container Handles Mental Model

Conceptually, containers are **routers** not **storage**:

```
Object { name: "Alice", age: 30 }
  │
  ├─► Not stored as: { "name": "Alice", "age": 30 }
  │
  └─► Stored as routing surface:
        ObjectHandle {
            fields: {
                "name" -> SlotId(42),  // Points to value node
                "age"  -> SlotId(43),
            }
        }
```

**Implications:**
- Field access `.name` compiles to a Router projection node
- Values inside containers flow as messages routed by handles
- Nested structures don't require deep copying
- Already aligns with delta streams approach

### F4. MoonZoon WebWorker Integration

Reference files for SAB threading setup:

| Purpose | MoonZoon File |
|---------|---------------|
| COOP/COEP headers | `MoonZoon/crates/moon/src/lib.rs:651` |
| Worker bootstrap | `MoonZoon/crates/zoon/src/task.rs:157` |
| Worker script | `MoonZoon/crates/zoon/src/task/worker_script.js:1` |
| Build flags | `MoonZoon/crates/mzoon/src/build_frontend.rs:183` |

**Key build flags for wasm threads:**
```
-C target-feature=+atomics,+bulk-memory,+mutable-globals
-Z build-std=panic_abort,std
```

### F5. Structured ScopeId Debugging

While runtime uses hash-based ScopeId for efficiency, add debug mode for diagnostics:

```rust
#[cfg(debug_assertions)]
pub struct ScopeDebugger {
    scope_paths: HashMap<ScopeId, Vec<ScopeSegment>>,
}

#[derive(Clone, Debug)]
pub enum ScopeSegment {
    Root,
    CallSite(SourceId),
    WhileArm(SourceId, u8),
    ListItem(SourceId, ItemKey),
    // NOTE: No HoldIteration - HOLD doesn't create new scopes per iteration (see K1)
}

impl ScopeDebugger {
    pub fn describe(&self, scope_id: ScopeId) -> String {
        // "Root > CallSite(fn:45) > ListItem(todos, 3) > WhileArm(filter, 0)"
    }
}
```

### F6. CRDTs for Collaborative Editing (Optional)

For concurrent/offline edits of shared structures:

```rust
pub enum ContainerMode {
    LocalOnly,                    // Default: deterministic, no conflicts
    Collaborative(CRDTConfig),    // Opt-in: eventual consistency
}

// Only specific containers marked as collaborative
// Core runtime stays deterministic
```

**Use cases:** Real-time collaborative apps, offline-first mobile apps.

---

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Clean rewrite takes longer than expected | Parallel development doesn't block existing users |
| New engine has different semantics | Test against all existing examples early |
| FPGA constraints too restrictive | Start "hardware-inspired", relax if blocking |
| Multi-threading adds complexity | Phase 9 is optional, single-threaded MVP first |
| Snapshot format changes | Version the format, support migration |

---

## Success Criteria

1. **No Arc<ValueActor> in hot path** - Reactive nodes use SlotId (strings use Arc<str>, which is fine)
2. **Deterministic execution** - Same inputs produce same outputs (formal logical ticks)
3. **Full snapshot/restore** - Can serialize and restore any graph state
4. **Timer/interval works** - `interval.bn` example runs correctly
5. **todo_mvc works** - Full app with Lists, LINK, HOLD functions
6. **No deadlocks** - Backpressure with "not ready" pattern, no blocking ever
7. **Simpler debugging** - Single-threaded, explicit state, traceable by NodeAddress
8. **FLUSH works** - Fail-fast error handling with `flush.bn` example
9. **Delta streams** - List updates are O(1), efficient WebSocket sync
10. **Explicit finalization** - No "receiver is gone" errors, clean scope cleanup
11. **Backend ready** - Architecture scales to 500+ users with nested reactive data

---

## Hardware Synthesis Notes (Future)

For eventual FPGA compilation:
- Each `ReactiveNode` → one module instance
- `RoutingTable` → static wiring
- `EventLoop.current_tick` → global clock
- `dirty` flag → clock enable
- `Message` → bus signals with valid/ready handshake
- `List` → parameterized array of modules

The arena-based design ensures all state is explicitly tracked and addressable, which is essential for HDL generation.

---

