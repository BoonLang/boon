# Plan: Actors Engine Performance and Allocation Reduction

## 0. Purpose

This document is the single implementation plan for improving the **Actors engine** only.

Primary goals:
- Reduce allocations and clone pressure.
- Reduce Arc-driven ownership complexity and lifetime bugs.
- Improve interactive latency in real UI workloads (especially `todo_mvc.bn`).
- Don't preclude future multithreading, but don't over-engineer for it — browser WASM is single-threaded and that's the current target.

This plan is designed for another AI/engineer to execute incrementally.

---

## 1. Scope and constraints

In scope:
- `crates/boon/src/platform/browser/engine_actors/engine.rs`
- `crates/boon/src/platform/browser/engine_actors/bridge.rs`
- `crates/boon/src/platform/browser/engine_actors/evaluator.rs`
- Supporting docs/tests/bench harnesses for Actors mode

Out of scope:
- Differential Dataflow engine changes
- Replacing async/await with a custom sync event loop
- Language semantic changes (HOLD/THEN/WHEN/WHILE/LATEST/LINK meaning)

Hard constraints:
1. Keep async/await and actor-loop style (`Task::start_droppable`, `select!`, channels).
2. Preserve correctness and deterministic behavior.
3. No fallback hacks in key/identity logic.
4. No hidden event loss for semantic events (`press`, `click`, `key_down`, `blur`).
5. Any lossy optimization must be explicit and limited to high-frequency UI signals (`hover`, text `change`).

---

## 2. Current bottleneck map (actors engine)

### 2.1 Event-path allocation pressure

The bridge allocates heavily for frequent DOM events:
- Text input event channels and buffering: `NamedChannel::new("change_event_sender")` in `build_text_input_event_handling()` (`bridge.rs:2609`), `Vec<TimestampedEvent<String>>` pending buffers (`bridge.rs:2727`)
- Per-event object/value construction for `change` and `key_down`: `create_change_event_value()` builds Object/Variable/ValueActor tree per event (`bridge.rs:2656`), `create_key_down_event_value()` same pattern (`bridge.rs:2686`)
- Hover streams/events in multiple element builders: `NamedChannel::new("element.hovered")` in `element_stripe()` (`bridge.rs:697`), `NamedChannel::new("button.hovered")` in `element_button()` (`bridge.rs:2003`)

Symptoms:
- Frequent small allocations
- Event burst amplification under fast typing/hover
- Pending vectors may grow when sender not ready

### 2.2 List fanout clone cost

List subscribers get cloned `Replace` payloads repeatedly:
- Initial send on subscribe: `ListChange::Replace { items: list.clone() }` in new-subscriber handler (`engine.rs:6133`)
- Output valve impulse send: `ListChange::Replace` clone in output_valve impulse `.retain()` broadcast loop (`engine.rs:6155`)
- Replace-heavy combinators (`retain`, `sort_by`) still trigger full payload traffic in many cases

### 2.3 List combinator incremental gaps

Already optimized (`coalesce`, dedup, transform cache), but gaps remain:
- `retain` rebuilds merged predicate stream on `Push`: `predicates.iter().map().collect()` in retain Push handler (`engine.rs:7575`), same full rebuild in retain Replace handler (`engine.rs:7909`)
- Smart diffing focuses on single-item visibility changes: `engine.rs:7750`
- `every/any` and `sort_by` still clone state vectors and do broad recomputation: `future::ready(Some(item_predicates.clone()))` in every/any after ListChange match (`engine.rs:8453`), `future::ready(Some(item_keys.clone()))` in sort_by (`engine.rs:8698`)

### 2.4 Remove pipeline task explosion

`List/remove` spawns one async task per item to wait for trigger stream:
- `Task::start_droppable(async { when.stream_from_now()... })` per item in remove Replace handler (`engine.rs:8158`), same per-item task spawn in remove Push handler (`engine.rs:8219`)

Symptoms:
- Large task count with bigger lists
- More channels/handles and lifetime complexity

### 2.5 Evaluator data structure overhead

- Dense slot IDs stored in `HashMap` instead of indexed vec storage: `results: HashMap<SlotId, Arc<ValueActor>>` in `EvaluationState` struct (`evaluator.rs:524`)
- Repeated function registry merge/clone for control-flow actors: `ctx.function_registry_snapshot` deep clone + insert merge in `build_then_actor()` (`evaluator.rs:820`) / `build_when_actor()` (`evaluator.rs:842`) / `build_while_actor()` (`evaluator.rs:863`)

### 2.6 Ownership complexity around Arc

- Core actor APIs and list internals are Arc-heavy (`Arc<ValueActor>` ~145 occurrences, 248 total `Arc<` uses in engine.rs)
- Input liveness managed via `inputs: Vec<Arc<ValueActor>>` parameter in `ValueActor::new_with_inputs()` (`engine.rs:3725`), `let _inputs = inputs.clone()` keepalive capture in `ActorLoop::new()` closure (`engine.rs:3752`)
- Subscription takes `self: Arc<Self>` and wraps in `PushSubscription { _actor: Arc<ValueActor> }` (`engine.rs:4594`, `engine.rs:1244`) — actor lifetime depends on subscriber lifetime (backwards)
- Easy to accidentally keep too much alive or drop too early when composing flows
- Entire debugging infrastructure (`LOG_DROPS_AND_LOOP_ENDS`, `NamedChannel::send_or_drop` logging) exists solely for Arc ownership issues

---

## 3. Performance strategy

Use four staged tracks:
1. **Track A (P0):** low-complexity wins and instrumentation.
2. **Track B (P1):** list-path algorithmic improvements (largest runtime wins for TodoMVC).
3. **Track C (P1/P2):** evaluator and value-path allocation reduction.
4. **Track D (P2):** scope-based generational arena replacing `Arc<ValueActor>` entirely.

Execution rule:
- No large structural changes before baseline metrics and P0 wins are landed.

---

## 4. Implementation tracks and tasks

## Track A - P0 quick wins (low complexity, high ROI)

### A1. Event dedup and latest-wins coalescing for noisy UI signals

Scope:
- `hovered` and text `change` events only.

Do not apply to:
- `press`, `click`, `key_down`, `blur`.

Implementation:
1. Add tiny per-handler last-value cache (bool for hover, string hash/eq for change).
2. Skip emission when value unchanged.
3. If sender not ready, keep only latest pending change/hover event (replace previous pending entry).

Expected result:
- Large reduction in event-object allocations during hover/typing.

### A2. Bound pending event buffers in bridge

Implementation:
1. Replace unbounded `Vec<TimestampedEvent<_>>` pending queues with bounded ring/VecDeque.
2. Define overflow policy per event type:
   - hover/change: keep latest
   - key_down/blur: preserve lossless semantics
3. Add debug counter for dropped/coalesced events.

### A3. Centralize channel capacities and tune defaults

Implementation:
1. Replace scattered numeric capacities with named constants.
2. Group by subsystem (`VALUE_ACTOR_*`, `LIST_*`, `BRIDGE_TEXT_INPUT_*`).
3. Adjust capacities based on measured backlog distributions.

### A4. SmallVec for hot tiny vectors

Candidates:
- Subscriber vectors (`subscribers`, `notify_senders`, `change_senders`)
- Temporary per-batch state where size is usually very small

Implementation:
1. Introduce `smallvec` dependency.
2. Migrate selected vectors to `SmallVec<[T; N]>` with conservative inline capacity.

### A5. Evaluator slot storage optimization

Implementation:
1. Replace `HashMap<SlotId, Arc<ValueActor>>` results store with `Vec<Option<Arc<ValueActor>>>`.
2. Keep SKIP semantics via `None`.
3. Preserve behavior of `alloc_slot`, `store`, `get`.

### A6. Function registry overlay (avoid full clone/merge)

Implementation:
1. Introduce immutable registry snapshot + local overlay map.
2. In `Then/When/While` builders, pass overlay structure or Arc chain instead of cloning merged map each time.

### A7. Add baseline instrumentation hooks (feature-gated)

Add counters for:
- event payloads constructed
- ValueActor created/dropped
- ListChange variants emitted (especially Replace)
- per-item remove tasks spawned
- channel full/disconnected rates
- persistence writes count/bytes

Use a feature flag (e.g. `actors-metrics`) to avoid production overhead.

---

## Track B - P1 list pipeline optimizations

### B1. Shared Replace payload representation

Problem:
- `ListChange::Replace { items: Vec<Arc<ValueActor>> }` causes repeated Vec clones in fanout.

Implementation options:
1. Change internal representation to `Arc<[Arc<ValueActor>]>`.
2. Keep public API compatibility wrappers where needed.

Expected impact:
- Significant reduction in large-list clone allocation.

### B2. Extend `retain` smart diffing beyond single-item changes

Current:
- Smart diffing is strongest for exactly one visibility change.

Implementation:
1. The diffing algorithm must produce correct InsertAt/Remove sequences for ANY number of visibility changes.
2. No Replace fallback — eliminate Replace from the retain path entirely.
3. Preserve deterministic order.

### B3. Incremental predicate stream registry for `retain`

Current:
- `Push` path rebuilds merged stream from all predicates.

Implementation:
1. Keep per-item predicate subscription state keyed by `PersistenceId`.
2. Add/remove single predicate streams incrementally.
3. Eliminate O(N) merged-stream rebuild on each push.

### B4. Similar incrementalization for `every/any`

Implementation:
1. Avoid cloning full `item_predicates` state each event (`engine.rs:8453`).
2. Track predicate state in mutable indexed table keyed by item identity.
3. Update aggregate result incrementally.

### B5. Incremental updates for `sort_by`

Implementation:
1. Maintain a sorted index structure (e.g. `BTreeMap<SortKey, ItemId>`).
2. On key change, remove old position and insert at new position.
3. For multi-key batch changes, apply all removals then all insertions. Always incremental — no full re-sort threshold.

### B6. Remove actor: replace per-item task spawning

Current:
- One task per item to await `when` stream emission.

Implementation:
1. Move to centralized stream fan-in for remove triggers.
2. Keep per-item identity and persistence semantics unchanged.
3. Ensure task count no longer scales linearly with item count.

### B7. Diff history sizing and retention policy

Implementation:
1. Make `DiffHistoryConfig` tunable from constants/config.
2. Consider adaptive cap based on active subscriber lag.
3. Keep snapshot threshold semantics deterministic.

---

## Track C - allocation reduction in values and persistence paths

### C1. Scalar fast paths

Implementation:
1. Identify hot scalar creation points (Tag/Number/Text in event and combinator paths).
2. Reduce repeated metadata/object scaffolding when semantically equivalent.
3. Keep lamport/idempotency semantics intact.

### C2. Intern frequently repeated text/tag values

Candidates:
- common tags (`True`, `False`, `All`, `Active`, `Completed`, etc.)
- repeated event field names where helpful

Implementation:
1. Introduce lightweight interning only where profiling proves wins.
2. Avoid broad invasive replacement.

### C3. Persistence write coalescing

Current:
- frequent save on each list change in persistence wrappers.

Implementation:
1. Coalesce writes per micro-batch/tick.
2. Ensure flush on shutdown/critical boundaries.
3. Preserve crash-safety expectations as much as existing behavior allows.

### C4. Avoid broad clone in persistence restore paths

Implementation:
1. Audit `Replace` and list restore flows for unnecessary intermediate Vec clones.
2. Use shared payload form from B1 where applicable.

---

## Track D - Scope-based generational arena (replace Arc<ValueActor>)

This track replaces `Arc<ValueActor>` (248 occurrences in engine.rs) with a scope-based
generational arena. This is a single, complete architectural change — not incremental.

### The Root Problem

`Arc<ValueActor>` serves two purposes and both are wrong:
1. **Ownership/keepalive**: `inputs: Vec<Arc<ValueActor>>` captured in async closures prevents premature drop. Creates an invisible ownership graph that's impossible to debug.
2. **Subscription/sharing**: `stream(self: Arc<Self>)` wraps subscriptions in `PushSubscription { _actor: Arc<ValueActor> }` to keep actors alive. Makes actor lifetime depend on subscriber lifetime — backwards.

The DD engine already proves the alternative works: `CellId` (lightweight enum) + worker-owned state + channel-based observation. 111 `Arc<` total in DD core (mostly `Arc<str>` for strings), vs 248 in actor engine.rs.

### The Solution: OwnedActor + ActorHandle + Scope Registry

Split `ValueActor` into two types:

**OwnedActor** — heavy, lives in registry, never cloned:
```rust
struct OwnedActor {
    actor_loop: ActorLoop,          // Owns the async task (TaskHandle)
    extra_loops: Vec<ActorLoop>,    // Additional task handles
    construct_info: Arc<ConstructInfoComplete>,
    scope_id: ScopeId,
    list_item_origin: Option<Arc<ListItemOrigin>>,
}
```

**ActorHandle** — lightweight channel senders, freely cloned, passed around everywhere:
```rust
#[derive(Clone)]
pub struct ActorHandle {
    actor_id: ActorId,
    subscription_sender: NamedChannel<SubscriptionSetup>,
    direct_store_sender: NamedChannel<Value>,
    stored_value_query_sender: NamedChannel<StoredValueQuery>,
    message_sender: NamedChannel<ActorMessage>,
    ready_signal: Shared<oneshot::Receiver<()>>,
    current_version: Arc<AtomicU64>,
    persistence_id: parser::PersistenceId,
    lazy_delegate: Option<Box<LazyActorHandle>>,
}
```

Cloning an ActorHandle does NOT keep the actor alive — the registry does.
If the OwnedActor is destroyed (scope cleanup), channels close and subscribers see `None`.

**ActorId** — generational index, Copy, 8 bytes:
```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorId { index: u32, generation: u32 }
```

**ScopeId** — same pattern:
```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId { index: u32, generation: u32 }
```

**ActorRegistry** — central ownership, thread_local in WASM:
```rust
pub struct ActorRegistry {
    actors: Vec<Option<(u32, OwnedActor)>>,  // SlotMap-style
    free_list: Vec<u32>,
    next_generation: u32,
    scopes: Vec<Option<(u32, Scope)>>,
    scope_free_list: Vec<u32>,
}

struct Scope {
    parent: Option<ScopeId>,
    actors: Vec<ActorId>,
    children: Vec<ScopeId>,
}

thread_local! {
    static REGISTRY: RefCell<ActorRegistry> = RefCell::new(ActorRegistry::new());
}
```

### Key API Changes

**Actor creation** (`Arc::new(ValueActor::new(...))` → `create_actor(scope_id, ...)`):
- Build channels, spawn actor loop, construct OwnedActor + ActorHandle
- Insert OwnedActor into registry under scope_id, return ActorHandle

**Subscription** (`actor.clone().stream()` → `handle.stream()`):
```rust
impl ActorHandle {
    pub fn stream(&self) -> LocalBoxStream<'static, Value> {
        let (tx, rx) = mpsc::channel(32);
        self.subscription_sender.send_or_drop(SubscriptionSetup {
            sender: tx, starting_version: 0,
        });
        rx.boxed_local()  // No PushSubscription wrapper needed
    }
}
```

**Scope destruction** (WHILE branch switch, list item removal, program teardown):
```rust
impl ActorRegistry {
    pub fn destroy_scope(&mut self, scope_id: ScopeId) {
        if let Some((_, scope)) = self.scopes[scope_id.index as usize].take() {
            for child_id in scope.children { self.destroy_scope(child_id); }
            for actor_id in scope.actors {
                self.remove_actor(actor_id);
                // OwnedActor drops → ActorLoop drops → task cancelled → channels close
            }
        }
    }
}
```

**Evaluator results** (`HashMap<SlotId, Arc<ValueActor>>` → `Vec<Option<ActorHandle>>`).

### Scope Tree Mapping

| Boon Construct | Scope Action |
|---|---|
| Program load | Create root scope |
| Top-level variable | Create actor in root scope |
| Function call | Create child scope of caller's scope |
| THEN/WHEN/WHILE body | Create child scope |
| WHILE branch switch | Destroy old branch scope, create new |
| List/map item creation | Create child scope per item |
| List/remove item | Destroy that item's scope |
| Navigation / program reload | Destroy root scope (cascade) |

### What Gets Eliminated

- `PushSubscription { _actor: Arc<ValueActor> }` wrapper (engine.rs:1244-1262)
- `inputs: Vec<Arc<ValueActor>>` on ValueActor and all `let _inputs = inputs.clone()` captures
- `new_with_inputs()`, `new_arc_with_inputs()` constructors
- `LOG_DROPS_AND_LOOP_ENDS` debugging infrastructure (no longer needed — scope tree is deterministic)
- All 145 `Arc<ValueActor>` occurrences

### Migration Order

1. Add ActorId, ScopeId, ActorRegistry types to engine.rs. Add thread_local REGISTRY.
2. Split ValueActor into OwnedActor + ActorHandle. Create compatibility `create_actor() -> ActorHandle`.
3. Change evaluator.rs: `Arc<ValueActor>` → `ActorHandle` in results and all functions.
4. Change bridge.rs: `Arc<ValueActor>` → `ActorHandle`, subscription calls `handle.stream()`.
5. Remove PushSubscription wrapper.
6. Remove all keepalive patterns (inputs, _inputs, new_with_inputs, new_arc_with_inputs).
7. Add scope destruction to WHILE switching, List/remove, program teardown.

Each step compiles and examples keep working.

---

## 5. TodoMVC impact matrix

Reference example:
- `playground/frontend/src/examples/todo_mvc/todo_mvc.bn`

Operation profile:
- `HOLD`: 3
- `LATEST`: 6
- `WHEN`: 13
- `WHILE`: 10
- `THEN`: 11
- `LINK`: 36
- list operations: append/remove/retain/map/count/is_empty

Expected wins by user action:

1. Typing new todo title
- Wins: A1, A2, A3, C1
- Why: high-frequency `change` events and event payload construction

2. Hovering todo rows
- Wins: A1, A2
- Why: frequent bool hover signals

3. Add todo on Enter
- Wins: B3, B1, A5
- Why: append + map/retain update path

4. Toggle single todo
- Wins: B2, B1
- Why: avoid full replace for small visibility/state changes

5. Switch filter (All/Active/Completed)
- Wins: B2, B3, B1
- Why: mass predicate updates with incremental list output

6. Toggle all
- Wins: B1, B2, C3
- Why: multi-item updates and persistence pressure

7. Clear completed
- Wins: B6, B1, C3
- Why: remove trigger fan-in and write coalescing

8. Edit todo (double click, type, blur/enter)
- Wins: A1, A2, C1

---

## 6. Rollout roadmap

## Phase 0 - baseline and guardrails

### Instrumentation insertion points

Add feature-gated counters (`#[cfg(feature = "actors-metrics")]`) at these specific locations:

**bridge.rs (event path):**
- `create_change_event_value()` → CHANGE_EVENTS_CONSTRUCTED
- `create_key_down_event_value()` → KEYDOWN_EVENTS_CONSTRUCTED
- hover handler in `element_stripe()` / `element_button()` → HOVER_EVENTS_EMITTED
- pending buffer pushes in `build_text_input_event_handling()` → PENDING_QUEUE_HIGH_WATER

**engine.rs (list path):**
- `ListChange::Replace` send in subscribe handler → REPLACE_PAYLOADS_SENT + items.len()
- `ListChange::Replace` in output_valve impulse → REPLACE_FANOUT_SENDS
- `predicates.iter().map().collect()` in retain → RETAIN_PREDICATE_REBUILDS + predicates.len()
- `Task::start_droppable` in remove → REMOVE_TASKS_ACTIVE (gauge: +1 spawn, -1 complete)
- `item_predicates.clone()` in every/any → EVERY_ANY_STATE_CLONES + vec.len()

**engine.rs (actor lifecycle):**
- `ValueActor::new*` constructors → ACTORS_CREATED
- ValueActor Drop → ACTORS_DROPPED (leak detection: created - dropped must converge)
- NamedChannel send_or_drop failure → CHANNEL_DROPS_BY_NAME[name]

**evaluator.rs:**
- `alloc_slot()` → SLOTS_ALLOCATED
- registry clone in `build_then/when/while_actor()` → REGISTRY_CLONES + entries.len()

### WASM measurement approach

Browser WASM has no `perf` or `valgrind`. Use:
1. `web_sys::Performance::now()` for wall-clock timing around key operations
2. AtomicU64 counters (no contention overhead in single-threaded WASM)
3. Console dump function: `[actors-metrics] REPLACE_PAYLOADS_SENT=47, ACTORS_CREATED=312, ...`
4. Browser DevTools Performance tab for visual flame traces (manual, supplements counters)

### Baseline scenarios (TodoMVC with 20 pre-existing items)

Measure counter values for each interaction:
1. Type character → CHANGE_EVENTS_CONSTRUCTED, KEYDOWN_EVENTS_CONSTRUCTED
2. Hover across 5 rows → HOVER_EVENTS_EMITTED
3. Enter to add todo → ACTORS_CREATED delta, REPLACE_PAYLOADS_SENT, RETAIN_PREDICATE_REBUILDS
4. Toggle single todo → REPLACE_PAYLOADS_SENT, REPLACE_FANOUT_SENDS
5. Switch filter All→Active→Completed → RETAIN_PREDICATE_REBUILDS, REPLACE_PAYLOADS_SENT
6. Toggle all → REPLACE_PAYLOADS_SENT
7. Clear completed (10 items) → REMOVE_TASKS_ACTIVE peak

### Automation

Use MCP tools: `boon_inject` → `boon_run` → `boon_type_text`/`boon_press_key` → `boon_console(pattern="actors-metrics")`.
Compare counter dumps before/after each optimization.

Checklist:
- [ ] Add feature-gated counters at all listed insertion points
- [ ] Add console dump function triggered by debug key or MCP command
- [ ] Capture baseline report for all 7 scenarios
- [ ] Verify actor created/dropped counts converge after repeated add/remove cycles (leak check)

## Phase 1 - P0 quick wins

Checklist:
- [ ] A1 event dedup/coalescing for hover/change
- [ ] A2 bounded pending queues
- [ ] A3 channel capacity constants and initial tuning
- [ ] A4 selective SmallVec migration
- [ ] A5 evaluator Vec slot storage
- [ ] A6 function registry overlay

Exit criteria:
- measurable allocation and CPU reduction in typing/hover/filter scenarios
- no behavior regressions in examples

## Phase 2 - list-path upgrades (P1)

Checklist:
- [ ] B1 shared Replace payload
- [ ] B2 retain smart diff for small multi-change batches
- [ ] B3 retain incremental predicate registry
- [ ] B4 every/any incremental state updates
- [ ] B5 sort_by incremental path
- [ ] B6 remove trigger fan-in (no per-item task explosion)
- [ ] B7 adaptive/tunable diff history

Exit criteria:
- substantial drop in Replace count and clone traffic during filter/toggle-all/clear-completed

## Phase 3 - value/persistence allocation tuning

Checklist:
- [ ] C1 scalar fast paths
- [ ] C2 selective interning
- [ ] C3 persistence write coalescing
- [ ] C4 restore-path clone reductions

## Phase 4 - scope-based generational arena (replace Arc<ValueActor>)

Checklist:
- [ ] Add ActorId, ScopeId, ActorRegistry types + thread_local REGISTRY
- [ ] Split ValueActor into OwnedActor + ActorHandle
- [ ] Migrate evaluator.rs from Arc<ValueActor> to ActorHandle
- [ ] Migrate bridge.rs from Arc<ValueActor> to ActorHandle
- [ ] Remove PushSubscription wrapper
- [ ] Remove all keepalive patterns (inputs, _inputs, new_with_inputs, new_arc_with_inputs)
- [ ] Add scope destruction to WHILE switching, List/remove, program teardown

Exit criteria:
- zero `Arc<ValueActor>` remaining in codebase
- scope tree deterministically cleans up all actors
- no "receiver is gone" errors — replaced by clean channel closure on scope destroy

---

## 7. Verification plan

## 7.1 Correctness

Must pass:
- Existing actor-engine tests
- TodoMVC manual/automation scenarios
- Persistence restoration scenarios
- HOLD sequential update behavior

Must not regress:
- LINK event ordering/visibility semantics
- WHILE arm switch behavior
- list identity and persistence semantics

## 7.2 Performance

Measure before/after for:
- allocations per second
- peak RSS / retained memory
- task count and channel count in steady state
- Replace vs incremental list change ratio
- frame-time spikes on filter switch and toggle-all

Recommended benchmark scenarios:
1. Add 200 todos by simulated typing + Enter
2. Toggle all on/off 20 times
3. Switch filter All/Active/Completed repeatedly
4. Edit 50 random items (double click, type, Enter/blur)
5. Clear completed repeatedly after random toggles

## 7.3 Leak/lifetime checks

Use drop/end logs and counters to ensure:
- created/dropped actor counts converge after churn
- no monotonic growth in subscriptions/channels/tasks after repeated cycles

---

## 8. Risk management and rollout safety

1. Land P0 in small PRs with metrics delta in each PR.
2. Keep feature flags for risky algorithmic changes (B2/B3/B5/B6) during stabilization.
3. No fallback paths — each optimization is complete or not done. No identity fallbacks.
4. If a phase regresses correctness, revert that phase only, keep prior gains.
5. **Baseline gate:** If Phase 0 measurements show that a specific interaction already meets latency targets, skip the corresponding optimization tasks.

---

## 9. Definition of done

This plan is complete when:

1. TodoMVC interactions are stable and significantly faster in Actors mode.
2. Allocation hot spots in bridge/list/evaluator are measurably reduced.
3. No obvious unbounded buffers or task growth patterns remain.
4. Zero `Arc<ValueActor>` — ownership is scope-based via ActorRegistry with deterministic cleanup.
5. Documentation and metrics reports are updated for future contributors.

---

## 10. Suggested implementation order for another AI

If implemented by a separate AI, follow this exact order:
1. Add instrumentation and baseline (Phase 0).
2. Implement A1, A2, A5 first (fastest wins with low risk).
3. Implement B1, B2, B3 next (largest list-path wins).
4. Implement B6 before arena migration (reduces task complexity early).
5. Implement C3 after list-path stabilization (to avoid persistence noise while tuning list logic).
6. Do D-track (arena migration) after measurable gains are captured and correctness is stable. Follow the 7-step migration order in Track D exactly.

This sequencing minimizes risk and keeps each step measurable.

---

## 11. Future: Multi-Threading and Distribution

This section documents what changes are needed when actors move beyond single-threaded
browser WASM. The scope-based arena design (Track D) is intentionally compatible with
these future requirements.

### Phase 1: Web Workers (same machine, separate WASM modules)

Workers have their own WASM heap — memory cannot be shared. Communication is via
`postMessage()` with structured clone serialization.

What the arena design already provides:
- ActorId is an opaque Copy handle — naturally serializable across worker boundaries
- ActorHandle communicates via channels — channels can be abstracted over transport
- Scope tree is a declarative structure — can be replicated to a coordinator

What needs to change:
1. **ActorId gains a location field**: `ActorLocation { Local, Worker(WorkerId) }`
2. **ActorHandle channels become trait-based**: `trait ActorChannel<T> { fn send(&self, T); }`
   with `LocalChannel` (current mpsc) and `WorkerChannel` (MessagePort + serialization)
3. **Registry becomes a router**: local lookup for local actors, message forwarding for
   remote actors via a coordinator
4. **Scope destruction becomes a protocol**: coordinator sends destroy command to worker,
   worker destroys local scope, acknowledges back
5. **Value serialization**: Values crossing worker boundaries need serde support.
   Most Value variants are already serialization-friendly (Number, Text, Bool, Tagged).
   ActorId references serialize trivially (Copy, 8 bytes).

### Phase 2: Backend / distributed (cross-network)

Actors on a different machine. Communication over WebSocket, HTTP, or custom protocol.

Additional changes beyond Phase 1:
1. **ActorId gains network location**: `ActorLocation { Local, Worker(WorkerId), Remote(ServerId) }`
2. **Network transport for channels**: WorkerChannel generalizes to NetworkChannel with
   reconnection, buffering, and ordering guarantees
3. **Subscription becomes a streaming protocol**: Server-Sent Events or WebSocket streams
   for continuous value updates
4. **Scope tree partitioning**: Some scopes live on backend, others on frontend. Scope
   destruction crosses network boundary — needs timeout and failure handling
5. **Persistence migration**: Currently localStorage-based. Backend actors need server-side
   persistence (database, file system)
6. **Latency tolerance**: Subscription streams must handle network delay. The current
   fire-and-forget pattern (send_or_drop) needs at-least-once delivery for remote actors

### Design invariants to preserve now

These properties of the arena design must not be violated by any optimization work,
as they're prerequisites for distribution:

1. **All actor interaction goes through ActorHandle, never direct object access.**
   (Enables transparent remoting — swap channel implementation, same API.)
2. **ActorId is the only way to reference an actor externally.**
   (Enables location-transparent addressing.)
3. **Scope tree is explicit and queryable.**
   (Enables distributed scope management — coordinator knows which scopes are where.)
4. **No shared mutable state between actors.**
   (Already enforced by CLAUDE.md. Enables moving actors to workers without breaking invariants.)
5. **Channel-based subscription is the only observation mechanism.**
   (Enables network subscription — replace local channel with network stream.)
