## Part 4: Event Loop (No Rust Async)

### 4.1 Central Scheduler

Replace `Task::start_droppable` with explicit event loop. **Canonical EventLoop definition** (combines all fields):

```rust
pub struct EventLoop {
    // Core state
    arena: Arena,
    timer_queue: BinaryHeap<TimerEvent>,
    dirty_nodes: Vec<SlotId>,
    dom_events: VecDeque<DomEvent>,

    // Tick tracking
    current_tick: u64,
    tick_seq: u64,           // Sequence within tick for ordering

    // Wall-clock integration (see K6)
    tick_start_ms: f64,      // Performance.now() when tick started
    ms_per_tick: f64,        // Default: 16.67 (60 fps)

    // Scope cleanup (see Section 4.4)
    pending_finalizations: Vec<ScopeId>,

    // Effects (see K18)
    pending_effects: Vec<NodeEffect>,
}
```

**NOTE:** No `run_until_idle()` - browser kills page if main thread blocks (see K28). Use `run_tick()` instead.

### 4.2 Logical Ticks (Deterministic Ordering)

See **K20** for full tick processing algorithm with quiescence loop.

```rust
impl EventLoop {
    pub fn run_tick(&mut self) {
        self.current_tick += 1;
        self.tick_seq = 0;

        // 1. Collect external inputs
        self.process_timers();
        self.process_dom_events();

        // 2. Propagate until quiescence (see K20 for full algorithm)
        while !self.dirty_nodes.is_empty() {
            // Sort for determinism, process, may add new dirty nodes
            // ...
        }

        // 3. Finalize scopes at tick end
        self.finalize_pending_scopes();

        // 4. Execute effects (after all nodes settled)
        self.execute_pending_effects();
    }
}
```

**Determinism guarantees:**
- Same input events → same output sequence
- Tie-breaking by `{source_id, scope_id, port, tick_seq}`
- Glitch-free: within tick, use last-value semantics

### 4.3 Backpressure (Deadlock Prevention)

**Rule:** No node ever blocks. If output queue full, return "not ready".

```rust
pub enum ProcessResult {
    Emitted(Message),
    NotReady,  // Output queue full, retry later
    NoOutput,  // Node consumed input but produced nothing
}

impl EventLoop {
    fn process_node(&mut self, node: SlotId) -> ProcessResult {
        let node = self.arena.get_mut(node);

        // Check if output channel has capacity
        if !node.output_has_capacity() {
            return ProcessResult::NotReady;  // Will retry next tick
        }

        // Process normally
        node.process()
    }
}
```

**Backpressure policies by source type:**
| Source | Policy |
|--------|--------|
| Timer | Drop if full (timer keeps ticking) |
| Mouse move | Coalesce (keep latest position) |
| Click/keypress | Buffer (don't drop user input) |
| Network | Buffer with timeout |

### 4.4 Explicit Scope Finalization

**Problem:** Implicit drops cause "receiver is gone" errors.

**Solution:** Scopes are explicitly finalized, not implicitly dropped.

```rust
pub enum ScopeState {
    Active,
    Finalizing,  // Cleanup in progress
    Finalized,   // Ready for deallocation
}

impl Bus {
    fn remove_item(&mut self, key: ItemKey) {
        let scope = self.item_scopes.get_mut(key);

        // 1. Mark as finalizing (no new messages accepted)
        scope.state = ScopeState::Finalizing;

        // 2. Emit cleanup event (subscribers can react)
        self.emit(ListDelta::Remove { key });

        // 3. Schedule finalization for next epoch
        self.event_loop.schedule_finalization(scope.id);
    }
}
```

**Epoch-based deallocation:**
```rust
impl EventLoop {
    fn end_tick(&mut self) {
        // After tick completes, finalize scheduled scopes
        for scope_id in self.pending_finalizations.drain(..) {
            self.finalize_scope(scope_id);
        }
    }

    fn finalize_scope(&mut self, scope_id: ScopeId) {
        // All nodes in scope can now be safely freed
        for slot in self.arena.nodes_in_scope(scope_id) {
            self.arena.free(slot);
        }
    }
}
```

**Benefits:**
- No "receiver is gone" errors mid-processing
- Cleanup is deterministic and debuggable
- Items can emit final events before deallocation
- Safe for multi-threaded execution

### 4.5 Timer Implementation

No `async Timer::sleep()`. See **K6** for wall-clock integration and **K30** for backgrounded tab handling.

```rust
// Canonical TimerEvent (from K6) - uses BOTH tick and wall-clock
struct TimerEvent {
    deadline_tick: u64,    // For deterministic ordering
    deadline_ms: f64,      // For precise wall-clock timing
    node_id: SlotId,
}
```

**WASM integration:** Use `requestAnimationFrame` for visual updates, `setTimeout` for timers (see K30).

**Files to modify:**
- `crates/boon/src/platform/browser/engine.rs` - EventLoop replacing ActorLoop
- `crates/boon/src/platform/browser/api.rs` - Timer/interval using EventLoop

---

