# Plan: Actors Engine Performance and Allocation Reduction

## 0. Purpose

This document is the single implementation plan for improving the **Actors engine** only.

Primary goals:
- Reduce allocations and clone pressure.
- Reduce Arc-driven ownership complexity and lifetime bugs.
- Improve interactive latency in real UI workloads (especially `todo_mvc.bn`).
- Keep architecture compatible with future multithreading/distribution work.

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
- Text input event channels and buffering: `bridge.rs:2609`, `bridge.rs:2727`
- Per-event object/value construction for `change` and `key_down`: `bridge.rs:2656`, `bridge.rs:2686`
- Hover streams/events in multiple element builders: e.g. `bridge.rs:697`, `bridge.rs:2003`

Symptoms:
- Frequent small allocations
- Event burst amplification under fast typing/hover
- Pending vectors may grow when sender not ready

### 2.2 List fanout clone cost

List subscribers get cloned `Replace` payloads repeatedly:
- Initial send on subscribe: `engine.rs:6133`
- Output valve impulse send: `engine.rs:6155`
- Replace-heavy combinators (`retain`, `sort_by`) still trigger full payload traffic in many cases

### 2.3 List combinator incremental gaps

Already optimized (`coalesce`, dedup, transform cache), but gaps remain:
- `retain` rebuilds merged predicate stream on `Push`: `engine.rs:7575`, `engine.rs:7909`
- Smart diffing focuses on single-item visibility changes: `engine.rs:7750`
- `every/any` and `sort_by` still clone state vectors and do broad recomputation: `engine.rs:8453`, `engine.rs:8698`

### 2.4 Remove pipeline task explosion

`List/remove` spawns one async task per item to wait for trigger stream:
- `engine.rs:8158`, `engine.rs:8219`

Symptoms:
- Large task count with bigger lists
- More channels/handles and lifetime complexity

### 2.5 Evaluator data structure overhead

- Dense slot IDs stored in `HashMap` instead of indexed vec storage: `evaluator.rs:524`
- Repeated function registry merge/clone for control-flow actors: `evaluator.rs:820`, `evaluator.rs:842`, `evaluator.rs:863`

### 2.6 Ownership complexity around Arc

- Core actor APIs and list internals are Arc-heavy (`Arc<ValueActor>` almost everywhere)
- Input liveness currently managed via `inputs: Vec<Arc<ValueActor>>` keepalive capture: `engine.rs:3725`, `engine.rs:3752`
- Easy to accidentally keep too much alive or drop too early when composing flows

---

## 3. Performance strategy

Use four staged tracks:
1. **Track A (P0):** low-complexity wins and instrumentation.
2. **Track B (P1):** list-path algorithmic improvements (largest runtime wins for TodoMVC).
3. **Track C (P1/P2):** evaluator and value-path allocation reduction.
4. **Track D (P2):** ownership simplification and Arc reduction while keeping async/await.

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
1. For small batch changes (e.g. <= 8 or <= 25% list), emit ordered sequence of `InsertAt`/`Remove` operations.
2. Keep Replace fallback for large/complex changes.
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
1. Maintain indexed key table and item order.
2. On key change, reposition only affected item when possible.
3. Keep full stable sort fallback for complex multi-change batches.

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

## Track D - ownership simplification and Arc reduction (still async)

This track is optional until A/B/C gains are captured, but should be planned now.

### D1. Ownership taxonomy

Define explicit categories:
1. **Owned runtime state** (single owner: actor loop/list loop/evaluator state)
2. **Shared immutable data** (Arc acceptable)
3. **Cross-task references** (prefer IDs + channel handles over raw Arc graph)

### D2. Introduce typed handles for actor references

Implementation:
1. Add `ActorId` and registry mapping in runtime context.
2. Convert selected internal maps/vectors from `Arc<ValueActor>` to `ActorId`.
3. Keep public API unchanged initially.

### D3. Replace broad keepalive Arc vectors with keepalive table

Current:
- `inputs: Vec<Arc<ValueActor>>` captured to keep dependencies alive.

Implementation:
1. Replace with scoped keepalive registrations keyed by actor IDs.
2. Actor loop owns keepalive tokens and releases deterministically.

### D4. Scope-based lifecycle cleanup

Implementation:
1. Build explicit scope teardown hooks for dynamic list/map instances.
2. Ensure subscriptions/tasks/channels are removed on scope destruction.

### D5. Optional arena-backed actor state (advanced)

If needed after prior phases:
1. Runtime-owned arena for actor/list state objects.
2. Async tasks operate on IDs/handles; no custom event loop required.
3. Keep channel-driven async model intact.

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

Deliverables:
- metrics counters feature flag
- baseline report for TodoMVC interactions
- memory/task/subscription leak sanity checks

Checklist:
- [ ] Add feature-gated counters and logging
- [ ] Define benchmark scenarios and scripts
- [ ] Capture baseline JSON/markdown report in docs

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

## Phase 4 - ownership simplification (optional but recommended)

Checklist:
- [ ] D1 ownership taxonomy in code comments/types
- [ ] D2 ActorId handles for selected internals
- [ ] D3 keepalive table replacing broad Arc vectors
- [ ] D4 scope lifecycle teardown hooks
- [ ] D5 arena-backed runtime state prototype (if needed)

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
3. Preserve fallback paths where necessary (Replace fallback, full sort fallback), but do not add identity fallbacks.
4. If a phase regresses correctness, revert that phase only, keep prior gains.

---

## 9. Definition of done

This plan is complete when:

1. TodoMVC interactions are stable and significantly faster in Actors mode.
2. Allocation hot spots in bridge/list/evaluator are measurably reduced.
3. No obvious unbounded buffers or task growth patterns remain.
4. Ownership/lifetime behavior is simpler and less Arc-fragile.
5. Documentation and metrics reports are updated for future contributors.

---

## 10. Suggested implementation order for another AI

If implemented by a separate AI, follow this exact order:
1. Add instrumentation and baseline (Phase 0).
2. Implement A1, A2, A5 first (fastest wins with low risk).
3. Implement B1, B2, B3 next (largest list-path wins).
4. Implement B6 before deep ownership refactors (reduces task complexity early).
5. Implement C3 after list-path stabilization (to avoid persistence noise while tuning list logic).
6. Do D-track only after measurable gains are captured and correctness is stable.

This sequencing minimizes risk and keeps each step measurable.
