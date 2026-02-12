# DD Flag-Day Redesign Plan (Option 1: Item-Local State)

Status: draft (agreed direction)
Owner: DD redesign branch
Date: 2026-02-12

## Why this plan exists

We are repeatedly hitting fragile behavior in `todo_mvc` around dynamic list items (edit mode, clear completed, focus flow). The current DD path still relies on runtime template remapping and dynamic link/cell wiring that can silently drop event mappings.

This document locks a single, coherent redesign direction so we stop tactical patching.

## Chosen direction

Use **Option 1**:

- Per-item UI state (`editing`, `completed`, `title`, etc.) lives **inside each list item**.
- Dynamic interactions are resolved by **stable item key** (`__key`) and action id.
- Runtime emits list diffs (`ListItemUpdate`, `ListRemoveByKey`, `ListRemoveBatch`, ...), not per-item dynamic HOLD graph growth.

## Non-goals

- No temporary app-specific fallback for `todo_mvc`.
- No mixed architecture where some dynamic item flows still depend on dynamic remap IDs.
- No merge of half-complete milestones into main.

## Hard invariants

1. **Plan immutability:** after worker starts, event processing does not mutate runtime schema (`cells`, `link_mappings`, collection op graph).
2. **Identity first:** every list item has a stable `__key`; item updates/removals target key only.
3. **Deterministic routing:** one event route resolves to at most one action target.
4. **Diff-only list updates:** list cells emit list diffs; scalar cells emit scalar updates.
5. **No event-loop restart hacks:** normal UI events never restart/reinitialize worker.
6. **No silent no-op for valid routes:** missing route/key/action is a fail-fast bug.

## Target architecture

### 1) Immutable execution plan

Evaluator produces an explicit plan consumed by worker, including:

- scalar cell definitions
- collection ops
- list action routes (list id + action id + event domain -> diff operation)
- validation metadata

### 2) Item-scoped event model

Add explicit typed item events (conceptually):

- `list_cell_id`
- `item_key`
- `action_id`
- event payload (`unit`/`key_down`/`text`/`bool`)

No encoded string protocol required for key extraction.

### 3) Worker as pure executor

Worker processes events against immutable plan and mutable runtime state only:

- apply keyed list updates
- produce DD outputs
- sync outputs to IO layer

No runtime config growth, no remap-based schema mutation in hot path.

### 4) Bridge emits intent only

Render bridge sends typed events and does not rely on synchronous state reads to decide writes.

## Implementation scope (single branch)

Primary files expected to change:

- `crates/boon/src/platform/browser/engine_dd/core/types.rs`
- `crates/boon/src/platform/browser/engine_dd/core/worker.rs`
- `crates/boon/src/platform/browser/engine_dd/core/dataflow.rs`
- `crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs`
- `crates/boon/src/platform/browser/engine_dd/eval/interpreter.rs`
- `crates/boon/src/platform/browser/engine_dd/render/bridge.rs`
- `crates/boon/src/platform/browser/engine_dd/io/inputs.rs`
- `crates/boon/src/platform/browser/engine_dd/io/outputs.rs` (boundary simplification only)

## Execution strategy

Single long-lived redesign branch; merge only when full architecture is coherent and tests pass.

Recommended internal sequence:

1. Define plan/event contracts and validators (fail fast on missing/duplicate key/route ambiguity).
2. Implement item-scoped event path end-to-end (bridge -> inputs -> worker routing).
3. Replace per-item dynamic remap action flow with key-based list diffs.
4. Remove event-time schema mutation and remove event-triggered reinit paths.
5. Stabilize derived collection incrementality (avoid clear/push churn from scalar-only changes).
6. Remove obsolete remap/debug paths and update docs.

## Acceptance criteria (merge gate)

`todo_mvc` must pass fully, including:

- double-click enters edit mode for the right item
- edit input receives focus and stays stable
- Escape/blur exits edit mode correctly
- clear completed removes the correct items and keeps derived counts/views consistent

And globally:

- no "event fired -> 0 DD outputs" for valid mapped item actions
- no worker restart during normal interactions
- boundary contract tests for evaluator -> worker -> dataflow are green

## Risk notes

- Persistence migration may need translation from dynamic remap-era identity to key-based identity.
- Derived collection operators must be audited for update storms after key-based item updates.
- Existing dirty-tree debug code may mask behavior; stabilize baseline before final verification.
