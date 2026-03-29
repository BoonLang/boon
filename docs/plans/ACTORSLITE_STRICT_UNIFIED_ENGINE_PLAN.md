# ActorsLite strict unified-engine implementation plan

**Suggested repo destination:** `docs/plans/actors_lite_strict_unified_engine.md`

**Intent:** This is the implementation playbook for turning `ActorsLite` into the one engine to move forward with. It must absorb the good runtime and host-diff ideas proven by Fabric, keep the semantic discipline already defined for ActorsLite, and ship as a generic Boon engine with first-class persistence, no fallbacks, no example-specific hardcoding, and no temporary acceptance shortcuts.

---

## 1. Mission

Implement **one canonical engine** by evolving `crates/boon-engine-actors-lite` into a generic, fast, persistent, deterministic runtime and retiring the need for a separate “future main engine”.

The final engine must:

1. preserve Boon semantics;
2. stay generic and engine-pure;
3. support persistence as a real feature, not a gated omission;
4. make `counter`, `todo_mvc`, `cells`, and `cells_dynamic` pass unchanged in style;
5. make `cells` fast through generic runtime design, not through spreadsheet-specific code;
6. become the default engine to move forward with;
7. avoid all runtime fallback to old engines.

---

## 2. Non-negotiable rules

Codex must treat the following as hard constraints, not suggestions.

### 2.1 Generic-engine rules

- Do **not** add spreadsheet-specific builtins, evaluator branches, or runtime types.
- Do **not** add `Sheet/*` builtins or `if example == cells` fast paths.
- Do **not** add fixed runtime assumptions such as 26 columns, 100 rows, or hardcoded A1/B1/C1 knowledge.
- Do **not** add generated Boon source or source rewriting to make a specific example pass.
- Do **not** key list identity by render order or value equality when a stable item identity exists.
- Do **not** keep example-name or source-marker checks in the final lowerer.

### 2.2 No-fallback rules

- No runtime fallback to `Actors`, `DD`, `Wasm`, or `FactoryFabric`.
- No delegation from ActorsLite into Fabric.
- No “if unsupported then run other engine” logic anywhere in playground, CLI, WS, MCP, or tests.
- No acceptance mode that excludes persistence in the final implementation.
- No `--skip-persistence` in the final verification path for ActorsLite.

### 2.3 No-temporary-green rules

Remove or replace all fake-green mechanisms before declaring completion.

Specifically, the final state must **not** depend on:

- static baked acceptance JSON files that merely say ActorsLite is green;
- capability-gated “persistence unsupported in v1” behavior;
- source-subset detectors for shipped examples;
- example-specific `try_lower_*` entry points as the main lowering pipeline;
- public exposure gates that are disconnected from real functionality.

### 2.4 Complexity discipline

- Single-threaded v1 is correct and sufficient.
- Optimize the hot path with arenas, stable handles, dirty propagation, and retained diffing.
- Do not introduce async/channel-heavy scheduling into the hot path.
- Do not introduce a shared production runtime with other engines.
- Keep the reference kernel as an oracle only.

---

## 3. What this plan replaces

This plan is an explicit successor to the current milestone-style ActorsLite direction that still allows no persistence in v1 and capability-gated milestone green. This plan also supersedes any path where Fabric remains a parallel future-main-engine candidate.

The target end state is:

- `ActorsLite` is the single engine to move forward with;
- Fabric ideas are absorbed into ActorsLite internals;
- old engines may remain for comparison or archival purposes only, never as hidden execution fallbacks;
- persistence is built into ActorsLite itself.

---

## 4. Source-of-truth principles

Use these as design anchors throughout implementation:

1. **Reference kernel remains the semantic oracle.** It defines semantics and conformance expectations, but it is not a production runtime.
2. **ActorsLite remains the production runtime.** Its physical internals may differ from the kernel as long as externally visible semantics match.
3. **Fabric is an implementation donor, not a dependency.** Import good ideas, not a second runtime.
4. **Parser persistence identity is foundational.** Reuse the repository’s persistence-id approach rather than inventing example-specific keys.
5. **Shipped examples prove genericity.** `cells` and `cells_dynamic` must both pass to prove the engine was fixed generally rather than for one exact file shape.

---

## 5. Architecture target

## 5.1 High-level pipeline

Final canonical pipeline:

```text
Source code
  -> parser + reference resolution + persistence resolution
  -> generic lowered semantic IR
  -> generic lowered HostViewIR
  -> ActorsLite runtime plan
  -> RuntimeCore drains to quiescence
  -> retained host diff
  -> renderer / browser host
  -> persistence batch commit
```

Keep semantic runtime and UI bridge separate:

```text
Boon AST -> ActorsLite IR -> RuntimeCore
Boon UI AST -> HostViewIR -> retained bridge -> renderer
```

## 5.2 What to take from current ActorsLite

Keep and strengthen:

- generational ids;
- stale-event rejection via `emission_seq`, `cutoff_seq`, and `edge_epoch`;
- deterministic single-threaded execution;
- quiescence-before-render;
- function-instance reuse keyed by function id, call site, parent scope, and mapped-item identity;
- list identity and mapped-scope reuse semantics;
- `cells_dynamic` as a peer proof target;
- existing performance budgets as baseline targets.

## 5.3 What to absorb from Fabric

Absorb the design ideas, not the crate dependency:

- slot-oriented runtime storage;
- dirty-bit propagation;
- ready-region scheduling;
- retained host tree diffing with metrics;
- explicit host batch / flush model;
- stable render-node identity and diff statistics.

## 5.4 What must disappear from the final implementation

The final ActorsLite implementation must not rely on:

- actor-per-node mailbox execution for pure reactive hot paths;
- example-specific lowerers as the production compiler path;
- static acceptance files marking the engine green;
- persistence-disabled UI or test paths;
- any direct use of another engine to render or evaluate ActorsLite programs.

---

## 6. Required module shape

A clean final module layout is required. Exact filenames may vary, but the conceptual split must exist.

```text
crates/boon-engine-actors-lite/src/
  ids.rs
  seq.rs
  value.rs
  builtin_registry.rs
  program.rs

  ir/
    mod.rs
    semantic.rs
    view.rs
    persistence.rs
    functions.rs

  lower/
    mod.rs
    lower_program.rs
    lower_expr.rs
    lower_view.rs
    lower_builtin.rs
    lower_persistence.rs

  runtime/
    mod.rs
    core.rs
    scheduler.rs
    slots.rs
    machines.rs
    eval.rs
    lists.rs
    links.rs
    functions.rs
    persist.rs
    diagnostics.rs

  bridge/
    mod.rs
    host_batch.rs
    host_state.rs
    retained_view.rs
    diff.rs
    browser.rs

  metrics/
    mod.rs
    report.rs

  tests/
    kernel_conformance.rs
    stale_event.rs
    list_identity.rs
    function_reuse.rs
    persistence.rs
    counter.rs
    todo_mvc.rs
    cells.rs
    cells_dynamic.rs
```

Codex may stage file moves incrementally, but the final state must converge to this separation of concerns.

---

## 7. Generic lowering requirements

## 7.1 Replace example-specific lowering with one real lowerer

The current pattern of many `try_lower_*` functions is not the final architecture.

Final rule:

- There is exactly one general lowering entry point for semantic lowering.
- There is exactly one general lowering entry point for view lowering.
- Example names may be used by the test harness only.
- Example names may **not** influence the lowering pipeline.

### Mandatory final behavior

- `counter.bn`, `todo_mvc.bn`, `cells.bn`, and `cells_dynamic.bn` all lower through the same generic pipeline.
- Builtins are selected by a builtin registry keyed by function path, not by example name.
- Unsupported constructs fail with explicit lower-time diagnostics naming the missing generic feature.
- The final lowerer must not search the source for magic marker strings like “top-level store”, “cells”, or other subset tags.

## 7.2 Required semantic IR families

The final semantic IR must explicitly represent at least:

- literals / constants;
- parameters;
- object construction;
- field reads;
- `BLOCK`;
- `HOLD`;
- `THEN`;
- `WHEN`;
- `WHILE`;
- `LATEST`;
- `SKIP`;
- `LINK` placeholder, bind, and read;
- builtin arithmetic and comparison nodes used by shipped examples;
- function definition templates;
- function call instantiation points;
- list literals and list operators;
- host-boundary nodes:
  - `SourcePort`
  - `MirrorCell`
  - `SinkPort`
- persistence annotations for durable state.

## 7.3 Required HostViewIR families

The final HostViewIR must be generic and passive. It must explicitly represent at least:

- `Document/new`;
- `Element/button`;
- `Element/checkbox`;
- `Element/container`;
- `Element/label`;
- `Element/link`;
- `Element/paragraph`;
- `Element/stripe`;
- `Element/text_input`;
- `NoElement`;
- `Reference[element: ...]`;
- style trees / classes / attributes used by shipped examples;
- event-port attachment metadata;
- stable view-site ids.

## 7.4 Persistence metadata in lowering

Every durable node must carry explicit persistence metadata in IR.

Required shape:

```text
PersistPolicy
  None
  Durable {
    root_key: PersistenceId,
    local_slot: u32,
    persist_kind: Hold | ListStore | FutureDurableCell
  }
```

Rules:

- Persistence metadata comes from parser-resolved persistence ids and stable lowering-site ids.
- The lowerer decides durability structurally, not by example name.
- Durable children derive keys from parent durable scope + site identity + item identity.
- Host mirrors, source ports, sink ports, and element references are never durable runtime state.

---

## 8. RuntimeCore target design

## 8.1 Runtime storage model

Replace mailbox-heavy pure-node execution with a slot-and-machine runtime.

Required concepts:

- **Value slots** for scalar/object/list-handle outputs;
- **Pulse slots** for eventful outputs that must not cut off repeated equal values;
- **Hold cells** for durable or ephemeral stateful memory;
- **Link cells** for `LINK` binding state;
- **List stores** for stable item identity and mapped-scope management;
- **Machine states** for compiled evaluators;
- **Region states** for scheduling batches of machines;
- **Scope states** for ownership and cleanup;
- **dirty bitsets** or dirty queues;
- **dependents adjacency** for transitive closure propagation;
- **dirty sink collection** for host flush;
- **dirty persistence collection** for persistence flush.

A good physical target shape is:

```text
RuntimeCore {
  seq_counter,
  scopes,
  regions,
  machines,
  value_slots,
  hold_cells,
  link_cells,
  list_stores,
  ready_regions,
  dependents,
  dirty_sinks,
  dirty_persistence,
  diagnostics,
}
```

## 8.2 Scheduling model

Required scheduling model:

1. host event batch enters bridge;
2. bridge injects all source pulses and mirror writes for that batch;
3. runtime marks affected regions dirty;
4. runtime drains all ready regions until quiescent;
5. bridge reads dirty sinks and computes retained diff;
6. renderer flushes patches;
7. persistence adapter commits the coalesced durability batch;
8. host batch ends.

No intermediate half-render is allowed.

## 8.3 Determinism rules

Mandatory deterministic rules:

- single-threaded v1;
- stable lowering order where ties are possible;
- one host batch produces one deterministic quiescence cycle;
- no behavior may depend on async wake order;
- all equal-timestamp conflicts must break by stable source order.

## 8.4 Sequence and stale-event model

Keep the semantic contract, but allow a simple physical implementation.

Required fields:

- `Seq { turn, order }` or equivalent opaque monotonic causal sequence;
- `edge_epoch` on dynamic edges;
- `cutoff_seq` on subscribed edges;
- `last_changed` on outputs and durable cells.

Delivery acceptance rule:

- edge must still exist;
- epoch must match;
- `source_seq > cutoff_seq`.

This contract must cover:

- `THEN`;
- `WHEN`;
- `WHILE`;
- `LATEST`;
- dynamic list rewiring;
- scope drop;
- function-instance replacement;
- `LINK` rebinding.

## 8.5 Cutoff rules

Required hot-path cutoff behavior:

- scalars compare by value;
- pulses never cut off repeated equal payloads;
- objects and lists do **not** use recursive deep equality in hot paths;
- objects and lists cut off by stable versioned handles or representation ids;
- list diffs preserve item ids whenever possible.

## 8.6 Dirty-closure rule

After initial mount, a change may only schedule the changed node set’s transitive dependent closure.

Forbidden fallback behaviors:

- full-scope recompute;
- full-document recompute;
- full-list recompute when delta is smaller;
- full-grid recompute for `cells` steady-state edits.

---

## 9. Function-instance and list-identity rules

## 9.1 Function-instance reuse

Required final rule:

```text
FunctionInstanceKey =
  function_def_id
  + call_site_id
  + parent_scope_id
  + mapped_item_identity(optional)
```

Behavior:

- same key => reuse function instance;
- arguments change under same key => update parameter cells in place;
- key change => destroy old instance and build new one;
- stale work for destroyed instance must be rejected via generation / epoch checks.

Do not rebuild full helper subgraphs on every input change.

## 9.2 List identity

Required list identity rules:

- `ListLiteral` creates fresh stable item ids;
- `ListRange` keys by produced value, not ordinal position;
- `ListAppend` creates fresh item ids for appended items;
- `ListRemove` removes ids and preserves surviving ids;
- `ListRetain` preserves surviving ids;
- `ListMap` keys child scopes by upstream item id + mapper-site id;
- nested `ListMap` keys descendants by full parent identity chain.

## 9.3 Mapped-item local state

Required invariant:

- sibling add/remove/filter/reorder must not destroy unaffected item-local `HOLD` state;
- sibling changes must not destroy unaffected element focus or element-reference identity;
- external dependency changes must update mapped outputs without unnecessary scope recreation.

---

## 10. Persistence architecture

Persistence is part of v1 in this plan. It is not optional.

## 10.1 Persistence scope

Persist only semantic durable runtime state.

Persist:

- durable `HOLD` cells;
- persistent list-store membership and item ids;
- durable child scope state inside persisted function/list scopes;
- future durable runtime cells only if they represent semantic program state.

Do **not** persist:

- source-port queues;
- mirror cells such as hover/focus/current input DOM state;
- sink caches;
- retained view trees;
- DOM handles or element references;
- ephemeral route snapshots that are already represented by browser location.

## 10.2 Persistence adapter

Add a real adapter interface:

```text
trait PersistenceAdapter {
  load_namespace(...)
  load_records(...)
  apply_batch(...)
}
```

Required concrete implementation in v1:

- browser localStorage adapter.

Design for future:

- file/database adapter must be possible without changing runtime semantics.

## 10.3 Namespace and key scheme

Use a namespaced key scheme, not one giant blob rewrite per event.

Required namespace components:

- engine name (`actorslite`);
- project or file identity;
- schema version.

Required record key basis:

- parser `PersistenceId` for root durable nodes;
- derived child keys from:
  - parent durable key;
  - call-site id / local persistence slot;
  - stable list item identity when mapped.

Rules:

- same logical durable node across reruns => same persistence key;
- sibling order changes alone must not change persistence keys;
- removed durable nodes must become unreachable and eligible for GC after a successful run.

## 10.4 Persistence record types

At minimum, add explicit persisted record types:

```text
PersistManifest {
  schema_version,
  live_root_keys,
  generation,
}

PersistedHold {
  value,
}

PersistedListStore {
  next_item_id,
  items: [
    { item_id, child_key, value? }
  ],
}
```

Notes:

- Derived lists should generally not be persisted; only source/stateful list stores are.
- Derived caches, dependency closures, and host view state must be recomputed on load.
- `KernelValue` serialization must remain generic.

## 10.5 Restore protocol

Required restore order:

1. parse + resolve persistence ids;
2. lower IR with durability metadata;
3. load persistence manifest and records for the current namespace;
4. construct runtime skeleton;
5. restore list stores first so stable item identities exist;
6. instantiate mapped scopes using restored item identities;
7. restore durable holds inside those scopes;
8. recompute derived state to quiescence before first render;
9. mark live durable key set for later GC.

If a record is corrupt:

- discard only that record;
- surface a clear diagnostic;
- continue with seed/default state for that node;
- do not crash the engine and do not fall back to another engine.

## 10.6 Commit protocol

Required commit protocol:

1. during runtime evaluation, mark durable nodes dirty when committed state changes;
2. after quiescence, build a coalesced persistence batch containing only changed records and required deletes;
3. write the batch atomically from the adapter’s perspective;
4. update manifest and live-key set;
5. clear dirty durability markers only after successful adapter commit.

Required behavior:

- multiple updates to the same durable cell within one host batch commit as one final write;
- writes are delta-based, not whole-program blob rewrites;
- persistence write cost must not scale with full `cells` document size when one cell changes.

## 10.7 Garbage collection

Add persistent record GC.

Required rule:

- after a successful run, any durable key in the namespace that is not live in the new program may be deleted.

This is required for source-driven migrations such as:

1. add new state while still referencing old state;
2. deploy and observe both live;
3. later remove old state;
4. old persistence disappears automatically because it is no longer live.

## 10.8 Persistence and code changes

Code upgrades must stay source-driven and generic.

Required rule:

- The engine does not invent migration logic for specific examples.
- The parser/lowering persistence identity system provides stable keys.
- User-authored Boon source expresses migration when needed.

---

## 11. Host bridge design

## 11.1 Bridge responsibilities

The bridge owns:

- host event intake;
- mirrored host state;
- retained node identity;
- retained tree diffing;
- host commands;
- renderer patch emission.

The runtime core does **not** own DOM or renderer types.

## 11.2 Required retained identity

Retained node identity key:

```text
RetainedNodeKey =
  view_site_id
  + function_instance_id(optional)
  + mapped_item_identity(optional)
```

This is required for:

- stable focus;
- stable element references;
- low node churn;
- efficient `todo_mvc` and `cells` updates.

## 11.3 Required host event surface

Support at least:

- `press`
- `click`
- `change`
- `key_down`
- `blur`
- `focus`
- `double_click`

Mirrored host state must support at least:

- text input current value;
- focus state;
- hover state;
- route state.

## 11.4 Retained diffing

Required behavior:

- diff only after runtime quiescence;
- preserve stable node ids where identity key is unchanged;
- collect diff stats for retained creations/deletions and patch count;
- treat host commands as explicit diff outputs, not side effects hidden in the runtime.

---

## 12. Cells-specific direction

`cells` is the main heavy performance target, but the engine must remain generic.

### Hard rules

- no spreadsheet-specific runtime code;
- no fixed grid dimensions;
- no formula engine inside ActorsLite;
- no special AST markers for `cells`;
- no source generation;
- no specialized “A1 path” or “row template path” in the engine.

### Allowed generic optimizations

- sparse runtime state;
- explicit dependency invalidation;
- stable function-instance reuse;
- stable mapped-item scopes;
- generic retained node patching;
- coalesced persistence writes;
- handle/version-based cutoff for objects and lists;
- list/item scope reuse;
- generic topological propagation of affected closures.

### Canonical proof targets

Both are mandatory:

- current `cells.bn` style remains accepted unchanged in spirit;
- `cells_dynamic.bn` uses both axes as normal Boon values via nested `List/range |> List/map`.

Canonical dynamic shape requirement:

```bn
row_count: 100
col_count: 26

all_row_cells:
  List/range(from: 1, to: row_count)
    |> List/map(row_number,
      new: [
        row: row_number
        cells:
          List/range(from: 1, to: col_count)
            |> List/map(column,
              new: make_cell(column: column, row: row_number)
            )
      ]
    )
```

Phase completion is impossible unless both `cells` and `cells_dynamic` pass.

---

## 13. Performance target

The performance target remains strict and measured, not assumed.

## 13.1 Required runtime strategies

Implement these before calling the engine “fast”:

- arena-backed runtime storage;
- dirty-region scheduling;
- stable function reuse;
- list diff propagation with stable ids;
- handle/version cutoff instead of deep equality;
- retained diff stats;
- persistence delta writes;
- no repeated whole-subtree rebuilds on single-cell edit.

## 13.2 Required metrics

Measure at least:

- actor/machine creation cost;
- send/schedule latency;
- messages or region evaluations per second;
- peak live scope/function instance counts;
- peak queue depth / ready-region depth;
- cold mount latency for `counter`, `todo_mvc`, `cells`, `cells_dynamic`;
- steady-state single-edit latency for `cells` and `cells_dynamic`;
- retained node creations and deletions per edit;
- dirty sink/export count per edit;
- function-instance reuse hit rate;
- recreated mapped-scope count per edit;
- persistence batch writes per edit;
- persistence bytes written per edit.

## 13.3 Minimum budgets

Use these as the minimum acceptance budgets on the pinned benchmark setup already used by repo plans.

- `counter` press-to-paint: `p50 <= 8 ms`, `p95 <= 16 ms`
- `todo_mvc` add/toggle/filter/edit-to-paint: `p50 <= 25 ms`, `p95 <= 50 ms`
- `cells` cold mount to stable first paint: `p50 <= 1200 ms`, `p95 <= 2000 ms`
- `cells` steady-state single-cell edit-to-paint: `p50 <= 50 ms`, `p95 <= 100 ms`
- `cells_dynamic` steady-state single-cell edit-to-paint: same as `cells`
- retained node creations per steady-state `cells` edit: `<= 6`
- retained node deletions per steady-state `cells` edit: `<= 6`
- dirty sink/export count per steady-state `cells` edit: `<= 32`
- function-instance reuse hit rate after warm mount: `>= 95%`
- recreated mapped-scope count per steady-state `cells` edit: `0`
- persistence writes per steady-state single-cell edit: proportional to changed durable closure only, never whole-program state rewrite
- no browser freeze on repeated `cells` edit/reopen/commit/cancel traces

---

## 14. Strict execution sequence

Do not start later phases while earlier phase exit criteria are red.

## Phase 0 — Freeze direction and remove temptation

### Goals

- establish this plan as the implementation source of truth;
- stop extending temporary patterns;
- prepare for one real engine path.

### Required actions

1. Add this plan to the repo docs.
2. Mark older milestone-only ActorsLite/Fabric docs as superseded for implementation.
3. Add a tracker issue/checklist that mirrors the phases below.
4. Freeze new example-specific lowerer additions immediately.
5. Freeze any new acceptance shortcuts (`skip-persistence`, static-green JSON, subset markers).

### Exit criteria

- this plan is committed in repo;
- team work is pointed at one engine path only;
- no new temporary admissions are added after this point.

---

## Phase 1 — Build the generic lowering spine

### Goals

- create the real generic compiler path;
- decouple the engine from example-name lowering.

### Required actions

1. Create the new IR and lower module split.
2. Add one generic `lower_program(...)` entry point.
3. Add one generic `lower_view(...)` entry point.
4. Implement builtin registry lookup by path.
5. Carry persistence metadata through lowering.
6. Add lower-time diagnostics for unsupported constructs.
7. Add lower tests using small purpose-built Boon snippets, not example-name detection.

### Mandatory rule

Do **not** delete the old example-specific lowerers until the generic lowerer covers the same surface. But do **not** add any new example-specific lowerer logic from this point forward.

### Exit criteria

- `counter`, `todo_mvc`, and minimal list/control examples lower through the generic pipeline;
- no new `try_lower_*` functions are added;
- unsupported features fail explicitly in the generic lowerer.

---

## Phase 2 — Replace pure hot-path execution with slot/region runtime

### Goals

- move the runtime off mailbox-heavy pure-node execution;
- implement dirty-closure scheduling.

### Required actions

1. Add slot arenas for values, holds, links, and list stores.
2. Add machine and region state.
3. Add ready-region queue.
4. Add dependents adjacency and dirty propagation.
5. Add causal sequence handling and edge epochs.
6. Add generational scope / region / machine ids.
7. Implement scalar, object, field, `BLOCK`, `HOLD`, `THEN`, `WHEN`, `WHILE`, `LATEST`, `SKIP`, and `LINK` execution.
8. Add unit tests for stale-event rejection, deterministic order, and quiescence.

### Exit criteria

- semantic microtests are green;
- runtime no longer needs actor-per-pure-node hot-path mailboxes;
- dirty-closure rule holds under dedicated tests.

---

## Phase 3 — Implement real persistence core

### Goals

- persistence is first-class from now on;
- no more milestone logic that excludes it.

### Required actions

1. Add `PersistenceAdapter` and browser localStorage implementation.
2. Carry parser `PersistenceId` into lowered durability metadata.
3. Implement restore protocol for durable holds and list stores.
4. Implement delta-based batch commit.
5. Implement namespace manifest and dead-record GC.
6. Add corruption handling diagnostics.
7. Add tests for:
   - rerun restore;
   - remove-after-migration GC;
   - durable list item identity restore;
   - unaffected sibling persistent state survival;
   - no persistence of host-only mirrors.

### Mandatory repository changes

- change playground logic so `ActorsLite` reports persistence support as true;
- remove “ActorsLite persistence is not supported in v1” UI behavior;
- stop using `skip_persistence` in ActorsLite-specific tooling once this phase is complete.

### Exit criteria

- persistence microtests are green;
- counter-style rerun state survives;
- list/mapped-scope persistent state survives sibling changes;
- no capability-gated persistence branches remain for ActorsLite.

---

## Phase 4 — Finish the host bridge and retained diff path

### Goals

- make UI structure generic and stable;
- support host state and focus-sensitive interactions correctly.

### Required actions

1. Implement HostViewIR lowering generically.
2. Add retained node identity keyed by view site, function instance, and mapped item.
3. Add source port intake and mirror-cell write batching.
4. Add sink collection and retained diffing after quiescence.
5. Add explicit host commands for route change, focus, and related actions.
6. Add tests for focus stability, element-reference stability, and no half-render.

### Exit criteria

- retained diffing works for shipped example surfaces;
- focus and element references stay stable where identity is preserved;
- no bridge-time full-tree rebuild is needed for single-item changes.

---

## Phase 5 — Counter end-to-end, with persistence

### Goals

- fully validate the smallest persistent interactive example.

### Required actions

1. Run `counter` through the generic lowerer and runtime only.
2. Add burst-click reliability trace if missing.
3. Make persistence rerun section pass with ActorsLite enabled.
4. Ensure stable button retained identity under repeated presses.
5. Verify metrics budget.

### Exit criteria

- `counter.expected` is green **with persistence enabled**;
- burst-click trace is green;
- metrics budget is green.

---

## Phase 6 — TodoMVC end-to-end, with persistence

### Goals

- validate list identity, function reuse, retained identity, and route-sensitive UI.

### Required actions

1. Implement remaining list operators and mapped-scope mechanics.
2. Make unaffected item state survive sibling add/remove/filter changes.
3. Make unaffected focus / reference identity survive sibling changes.
4. Enable edit-save trace and make it pass.
5. Make rerun persistence pass for todo state.
6. Verify metrics budget.

### Exit criteria

- `todo_mvc.expected` is green **with persistence enabled**;
- edit-save trace is green and not skipped;
- no unnecessary scope recreation for unaffected items.

---

## Phase 7 — Cells and cells_dynamic, with persistence and speed

### Goals

- finish the hardest proof target without hardcoding business logic.

### Required actions

1. Add `cells_dynamic.bn` and `cells_dynamic.expected` if absent.
2. Ensure both `cells` and `cells_dynamic` lower through the same generic pipeline.
3. Finish nested list identity and helper-function reuse behavior.
4. Ensure repeated edit/open/commit/cancel/blur traces behave correctly.
5. Ensure dependent recomputation only touches the affected closure.
6. Persist committed cell state across rerun.
7. Ensure persistence writes are delta-based.
8. Tighten harness timing or add readiness-based assertions; do not depend on long fixed sleeps.
9. Verify metrics budgets for both `cells` and `cells_dynamic`.

### Exit criteria

- `cells.expected` green **with persistence enabled**;
- `cells_dynamic.expected` green **with persistence enabled**;
- no browser freeze on repeated edit flows;
- performance budgets green;
- no full-grid recompute on steady-state edit.

---

## Phase 8 — Remove temporary mechanisms and cut over to the one engine

### Goals

- eliminate old temporary logic;
- make ActorsLite the real engine to move forward with.

### Required actions

1. Delete or retire example-specific lowering paths from ActorsLite.
2. Delete or retire static-green acceptance gating.
3. Delete or retire ActorsLite-specific `skip_persistence` usage.
4. Remove persistence-disabled UI branches for ActorsLite.
5. Expose ActorsLite directly in picker/CLI/WS/MCP without fake gates.
6. Make ActorsLite the default forward engine.
7. Remove any runtime execution fallback to older engines.
8. Keep old engines only under explicit dev/archive status if the repo still wants them for comparison.
9. Stop presenting Fabric as a competing future-main-engine choice.

### Exit criteria

- ActorsLite is exposed because it really works, not because a JSON file says green;
- no shipped example goes through subset-marker admission checks;
- no ActorsLite execution path can silently route into another engine.

---

## Phase 9 — Broader catalog parity and cleanup

### Goals

- ensure the one-engine path is sustainable beyond milestone examples.

### Required actions

1. Expand generic lowering/runtime support so exposed playground examples actually run.
2. Hide unimplemented examples at the UI/catalog layer until implemented.
3. Preserve meaningful styling and layout in the bridge.
4. Update docs, README, and developer workflows to reflect one forward engine.
5. Keep comparison backends optional and clearly non-default.

### Exit criteria

- all examples shown for ActorsLite really run;
- unsupported examples are not falsely exposed;
- styling/layout is not degraded to bare text-only output.

---

## 15. Required tests

## 15.1 Kernel and semantic tests

Add and keep green:

- stale-event cutoff on dynamic resubscribe;
- `edge_epoch` rejection of old deliveries;
- generational id rejection of stale queued work;
- branch switch cannot leak old arm events;
- `LINK` rebind cannot leak pre-rebind events;
- deterministic tie-breaking for `LATEST`;
- quiescence-before-render;
- dirty-closure only execution;
- function-instance reuse under stable keys;
- mapped-scope preservation under sibling changes;
- persistence restore and GC.

## 15.2 Example tests

The following are mandatory and must run under ActorsLite with persistence enabled:

- `counter`
- `todo_mvc`
- `cells`
- `cells_dynamic`

Add or strengthen traces for:

- burst-click counter;
- todo edit-save;
- repeated cells edit/reopen/commit/cancel;
- dependent recomputation traces;
- rerun persistence for all durable examples.

## 15.3 Performance tests

Add or keep a dedicated ActorsLite metrics capture command. It must report the metrics listed in section 13.

## 15.4 Final verification command set

The exact CLI subcommand names may differ slightly by repo state, but the final repo must expose a clear command set equivalent to:

```bash
makers build-tools
makers verify-integrity
./target/release/boon-tools exec test-examples --engine ActorsLite --filter counter
./target/release/boon-tools exec test-examples --engine ActorsLite --filter todo_mvc
./target/release/boon-tools exec test-examples --engine ActorsLite --filter cells
./target/release/boon-tools exec test-examples --engine ActorsLite --filter cells_dynamic
./target/release/boon-tools exec verify-actors-lite --check
./target/release/boon-tools exec actors-lite-metrics --check
```

Final rule:

- the final verification path must not pass `--skip-persistence` for ActorsLite.

---

## 16. File-by-file mandatory repository cleanup

Before declaring completion, Codex must explicitly check and fix these repository-level issues.

### `crates/boon-engine-actors-lite`

- replace example-specific lowering as the production path;
- keep only generic lowering/runtime/bridge code;
- remove static acceptance-green logic;
- keep metrics and verification grounded in real execution.

### `crates/boon-engine-factory-fabric`

- do not make ActorsLite depend on this crate at runtime;
- if kept, mark it as experimental/comparison only;
- do not expose it as the future main engine once ActorsLite cutover is complete.

### playground frontend / app wiring

- make ActorsLite persistence-supported;
- remove persistence-disabled messages for ActorsLite;
- expose ActorsLite directly when real support is present;
- remove any automatic fallback logic.

### tools / verification

- remove ActorsLite-specific `skip_persistence` use from final verification;
- wire real metrics and real browser verification;
- keep readiness-based checks where long fixed waits used to exist.

### docs

- mark milestone-only docs as superseded;
- document ActorsLite as the forward engine;
- document persistence and one-engine cutover.

---

## 17. Definition of done

ActorsLite is done only when **all** of the following are true.

1. `counter`, `todo_mvc`, `cells`, and `cells_dynamic` pass under ActorsLite.
2. They pass with persistence enabled where the example semantics require persistence after rerun.
3. The engine uses one generic lowering pipeline, not example-name lowering.
4. The runtime uses dirty-closure propagation and retained diffing, not whole-program recompute.
5. `cells` performance meets the budgets without spreadsheet-specific code.
6. No ActorsLite execution path falls back to another engine.
7. No ActorsLite verification path uses `--skip-persistence`.
8. No static acceptance JSON or fake-green flag is required to expose the engine.
9. The playground treats ActorsLite as a real persistent engine.
10. Fabric is no longer treated as a competing future main engine.
11. Old engines, if kept, are clearly dev/archive-only and never part of hidden production execution.

If any one of the above is false, the engine is **not done**.

---

## 18. Codex operating rule

Codex must work in this order:

1. make the generic architecture real;
2. make semantics correct;
3. make persistence real;
4. make milestone examples pass;
5. make `cells` and `cells_dynamic` fast generically;
6. remove temporary structures;
7. cut over to one real engine.

Codex must not optimize for a green screenshot or one example at the cost of genericity.

The best implementation is the one that leaves Boon with one clean, persistent, generic, fast ActorsLite engine that can keep growing without special-case rescue work.
