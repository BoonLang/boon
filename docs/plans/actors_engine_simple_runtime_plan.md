# Actors Engine Simple Runtime Plan

## Summary

- Keep the classic `boon-engine-actors` crate, `interpreter::run(...)`, parser pipeline, and playground `Actors` engine label.
- Replace the current per-actor channel bundle and task cascade with one mailbox per actor and one runtime-owned ready queue.
- Keep async only for real external sources such as DOM events, timers, router changes, and module loading.
- Strip overlapping identity and ordering layers where they are not required for user-visible semantics.

## Target Runtime Shape

- `RuntimeCore` owns actor slots, ready queue, async source task registry, and scope teardown state.
- `ActorHandle` becomes a lightweight `(RuntimeRef, ActorId)` handle plus minimal immutable metadata.
- `ValueActor` and `LazyValueActor` converge on one slot model with:
  - current value
  - mailbox
  - subscriber list
  - optional recompute-on-read path
- Pure internal propagation runs by mailbox dispatch, not by channel wakeups or spawned tasks.

## Simplification Rules

- Keep `PersistenceId` only where durable identity actually matters:
  - persistence
  - list item identity
  - reload-sensitive state
- Keep `ScopeId` only as a runtime-local live instance identifier for ownership and teardown.
- Do not introduce new lowering-specific stable ids such as `ExprId` or `SourceId` into this engine.
- Remove the separate `ValueIdempotencyKey` concept from the final runtime direction.
- Remove Lamport-specific reasoning from the final runtime direction.
- Replace broad versioning and stale-event filtering layers with one runtime-local monotonic emission sequence owned by `RuntimeCore`.

## Behavioral Constraints

- `todo_mvc`, older classic playground examples, `cells`, and `cells_dynamic` must keep working.
- `cells` interaction latency must be unnoticeable during normal editing.
- No spreadsheet-specific evaluator or Cells-only fast path is allowed.
- Repeated equal semantic events must still be observable where semantics depend on discrete pulses.
- Lossless event behavior stays required for `press`, `click`, `key_down`, `blur`, and commit-like flows.
- Coalescing is allowed only for noisy UI state such as `hovered` and text `change`.

## Implementation Slices

### Slice 1: Control Plane and Guidance

- Add this plan document.
- Retarget `prompter.json` to this workspace and this plan.
- Stop directing prompter automation toward the ActorsLite plan.

### Slice 2: Ordering and Identity Cleanup

- Introduce one explicit emission-identity seam in the runtime and evaluator.
- Collapse repeated `(idempotency_key, lamport_time)` plumbing behind a single abstraction.
- Move toward a runtime-local sequence model and delete duplicated ordering layers as behavior is preserved.

### Slice 3: Actor Runtime Rewrite

- Replace actor handle channel bundles with mailbox-driven actor slots.
- Replace current-value query channels and ready signals with direct runtime slot access plus ready-queue state.
- Preserve scope teardown and async-source integration.

### Slice 4: Hot Path Simplification

- Rework `THEN`, `WHEN`, `WHILE`, `LATEST`, `HOLD`, `LINK`, and list operators to run on mailbox dispatch.
- Remove per-item async task spawning in list flows.
- Keep list identity incremental and stable.

### Slice 5: Verification

- Keep focused evaluator and engine tests green.
- Add runtime tests for mailbox order, ready-queue dedup, late-subscription behavior, and teardown.
- Add Actors-side latency capture for:
  - `todo_mvc` add/toggle/filter/edit
  - `cells`
  - `cells_dynamic`
- Treat browser feel as the real acceptance bar even when synthetic budgets pass.

## Initial Acceptance Budgets

- `todo_mvc` add/toggle/filter/edit p50/p95 within `25/50 ms`
- `cells` edit p50/p95 within `50/100 ms`
- `cells_dynamic` edit p50/p95 within `50/100 ms`

## Non-Goals

- No migration to ActorsLite, FactoryFabric, DD, or Wasm internals.
- No new lowering pipeline for this engine.
- No global runtime state.
- No partial solution that keeps the current channel topology and only renames it.
