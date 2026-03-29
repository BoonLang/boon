# ActorsLite Plan

## Status

- Superseded for implementation by `docs/plans/ACTORSLITE_STRICT_UNIFIED_ENGINE_PLAN.md` on 2026-03-30.
- Use this document as historical/supporting context only while migrating off the milestone-only path.
- Proposed new engine direction as of 2026-03-25.
- This document is no longer the implementation source of truth.
- `ActorsLite` is a new crate, `boon-engine-actors-lite`.
- It is hidden and dev-only first. Public engine selection comes only after the milestone examples are green.

## Purpose

`ActorsLite` is a generic virtual-actor engine for Boon:

- preserve Boon actor semantics
- do not add spreadsheet-specific builtins
- do not rely on generated source
- make the current dynamic `cells` example fast enough by improving the generic runtime only

Initial scope:

- `counter`
- `todo_mvc`
- current dynamic `cells`

Initial constraints:

- single-threaded only
- no persistence in v1
- no shared production runtime with the other engines
- no fallback to the current `Actors` engine

Because the current `.expected` harness files for `counter` and `todo_mvc` already include persistence-after-rerun sections, the plan must treat persistence as capability-gated during the milestone phase:

- `ActorsLite` v1 may pass milestone browser tests through `--skip-persistence` or equivalent capability-aware gating
- if a dedicated `ActorsLite` milestone expectation variant is introduced, it must preserve the same non-persistence behavioral coverage and only exclude persistence-specific sections
- Phase 2 through Phase 4 “green” must therefore mean non-persistence interactive sequences are green unless persistence is explicitly implemented later

## Kernel Alignment

The internal reference kernel under `crates/boon/src/platform/browser/kernel` is the semantic oracle and test fixture for `ActorsLite`:

- it is the engine-agnostic reference/oracle surface for semantics, diagnostics, and tests
- it is not a shared production runtime
- `ActorsLite` may use a different physical runtime and different internal data structures
- kernel ticks and sequences remain oracle concepts; `ActorsLite` does not need a global hot-path tick to be correct

`ActorsLite` may keep an optional opaque per-turn trace id for diagnostics or kernel comparison, but that id must not become a required semantic mechanism in the runtime hot path.

## Core Decision

Split the engine into two distinct layers:

- `Boon AST -> ActorsLite IR -> RuntimeCore`
- `Boon UI AST -> HostViewIR -> retained/keyed bridge -> renderer`

The runtime core owns generic semantic execution only.
The bridge owns UI structure, host state, retained view identity, and renderer integration.

## Semantic Contract

### Deterministic execution

- `ActorsLite` is single-threaded in v1.
- One mailbox delivery is one atomic turn.
- A turn reads committed state from before that turn started.
- Writes become visible only through deterministic queued delivery after that turn.
- Runtime behavior must not depend on async task wake order, channel timing, or host scheduling.

### Quiescence and flush

- One external host event enters the bridge.
- The bridge snapshots the host state for that event.
- The bridge injects all `SourcePort` pulses and all affected `MirrorCell` writes from that same host snapshot into `RuntimeCore`.
- `RuntimeCore` drains until quiescent.
- Only after quiescence does the bridge read dirty exports or sink ports and flush retained renderer patches.

Turn budgets may be used for fairness inside a quiescence cycle, but the bridge must not render intermediate half-updated states in v1.

### Replacement for Lamport semantics

`ActorsLite` v1 does not require a global Lamport or tick clock.

Instead:

- every event-producing output maintains `emission_seq`
- every dynamic subscription edge stores:
  - `source_output_id`
  - `cutoff_seq`
  - `edge_epoch`
- every queued delivery carries:
  - `source_seq`
  - `edge_epoch`

A delivery is accepted only if:

- the edge is still live
- `edge_epoch` matches the current edge epoch
- `source_seq > cutoff_seq`

Branch switching, scope drop, and link rebinding must remove old edges before replacement edges can receive delivery.

This is the stale-event contract for:

- `THEN`
- `WHEN`
- `WHILE`
- `LATEST`
- dynamic `List/map` rewiring
- branch activation and deactivation
- link rebinding

`LATEST` selection is fully normative:

- ignore `SKIP` unless all candidates are `SKIP`
- choose the candidate with the greatest causal recency
- on ties, choose the earliest input in source order

### Identity and scope safety

Keep both ids generational:

- `ActorId { index, generation }`
- `ScopeId { index, generation }`

This is required because actor slots and scope slots will be reclaimed and reused, and stale queued work must be rejected safely.

### Value cutoff rules

- scalars compare by value
- pulses and queues never cut off repeated equal payloads
- objects and lists cut off by stable representation identity or explicit versioned handle, not recursive deep equality in hot paths
- list updates should propagate as item-id-preserving diffs where possible

### Dirty-closure only rule

After initial mount, one source change may only enqueue the transitive dependent closure of the changed source or sources.

Steady-state execution must not fall back to:

- full-scope recompute
- full-document recompute
- full-grid recompute

This rule is normative for `ActorsLite`, especially for `cells`.

## Runtime, IR, and Host Boundary

### ActorsLite IR

Do not lower Boon AST directly into mailbox handlers.

First lower to an explicit renderer-agnostic `ActorsLite IR`.

Required v1 IR families:

- scalar/value nodes
- object/field nodes
- `Block`
- `Hold`
- `Then`
- `When`
- `While`
- `Latest`
- `Skip`
- link nodes:
  - `LinkCell`
  - `LinkRead`
  - `LinkBind`
- function call with captured scope
- list nodes:
  - `ListLiteral`
  - `ListRange`
  - `ListMap`
  - `ListAppend`
  - `ListRemove`
  - `ListRetain`
  - `ListCount`
  - `ListGet`
  - `ListIsEmpty`
  - `ListSum`
- host-boundary nodes:
  - `SourcePort`
  - `MirrorCell`
  - `SinkPort`

The IR must not contain renderer-specific nodes such as `Element`, `Document`, `UiNode`, or `UiPatch`.

`LINK` lowering must be explicit:

- `LINK` placeholders lower to `LinkCell`
- reading a link target lowers to `LinkRead`
- `expr |> LINK { target }` lowers to `LinkBind`
- rebinding a `LinkBind` must advance the binding epoch used by stale-delivery filtering so old deliveries cannot leak into the new target

### RuntimeCore

`RuntimeCore` v1 contains only semantic runtime state:

- `actors: Arena<ActorSlot>`
- `scopes: Arena<ScopeSlot>`
- `ready: VecDeque<ActorId>`

Optional, if needed for flush efficiency:

- `dirty_exports: Vec<ExportId>`

`RuntimeCore` must not own:

- renderer state
- DOM handles
- Zoon nodes
- retained view trees
- renderer patch structures

### HostViewIR and bridge

Boon UI constructs lower into a bridge-owned retained view graph or host IR that subscribes to `SinkPort`s.

`HostViewIR` owns the UI layer that the core does not own:

- `Document/new`
- `Element/*`
- `NoElement`
- `Reference[element: ...]`
- style trees and other host-facing view structure needed by the milestone examples

`HostViewIR` is passive retained structure, not a second reactive engine. All reactivity lives in `RuntimeCore`; `HostViewIR` only binds retained view nodes to sink values and is diffed after quiescence.

Retained node identity is normative:

- key retained nodes by view site identity plus function-instance identity plus mapped-item identity
- stable retained identity is required for focus stability, element-reference stability, and low churn in `todo_mvc` and `cells`

The bridge owns:

- retained and keyed diffing
- host handles for element references
- renderer updates
- browser-specific behavior such as DOM/Zoon integration

### Host bridge contract

The host bridge must support more than raw “events in, patches out”.

It must own:

- pulse/event inputs exposed through `SourcePort`, using `emission_seq`
- mirrored host state exposed through `MirrorCell`, using value-cutoff semantics
- opaque host handles for `Reference[element: ...]`
- deterministic fanout ordering from one host event into multiple `SourcePort` messages

Pulse/event inputs must cover at least:

- `press`
- `click`
- `change`
- `key_down`
- `blur`
- `focus`
- `double_click`

Mirrored host state must cover at least:

- current text-input text
- current focus state
- current hover state
- current route state

For one host event that fans out to multiple source ports, the bridge must enqueue those messages in stable lowering order for that view node, then run the runtime to quiescence once for the whole batch.

Routing must cross the bridge as host boundary state and commands:

- `Router/route()` reads from a route `MirrorCell`
- `Router/go_to()` lowers to a sink-side host command that requests a route change

`Reference[element: ...]` must resolve against stable retained node ids. A reference becomes invalid immediately when the retained node disappears; no stale handle may survive past the diff that removed the node.

### Physical actor categories

Expected physical categories in v1:

- `ValueCell`
- `Pulse`
- `Queue`
- `ListStore`
- `SwitchGate`
- `SourcePort`
- `SinkPort`

This list is descriptive, not normative.

The normative source of truth is:

- semantic contract
- `ActorsLite IR`
- host boundary contract
- tests

Implementation may split or merge physical categories as long as semantics and tests hold.

## Milestone Surface

### Core control and data

Support the following in v1:

- `BLOCK`
- `HOLD`
- `THEN`
- `WHEN`
- `WHILE`
- `LATEST`
- `SKIP`
- `LINK`
- object construction
- field access
- primitive arithmetic and comparison used by the milestone examples, including `+`, `-`, `==`, and `>=`

### List operations

Support the following in v1:

- `LIST {}`
- `List/append`
- `List/count`
- `List/get`
- `List/is_empty`
- `List/map`
- `List/range`
- `List/remove`
- `List/retain`
- `List/sum`

### Builtins used by the milestone examples

Support the following in v1:

- `Math/sum`
- `Bool/not`
- `Router/go_to`
- `Router/route`
- `Text/empty`
- `Text/find`
- `Text/is_empty`
- `Text/is_not_empty`
- `Text/length`
- `Text/space`
- `Text/starts_with`
- `Text/substring`
- `Text/to_number`
- `Text/trim`

### HostViewIR surface used by the milestone examples

Support the following view constructs through `HostViewIR` and the bridge:

- `Document/new`
- `Element/button`
- `Element/checkbox`
- `Element/container`
- `Element/label`
- `Element/link`
- `Element/paragraph`
- `Element/stripe`
- `Element/text_input`
- `NoElement`
- `Reference[element: ...]`

## Example Invariants

The milestone examples are normative behavior targets, not just “green” examples.

### `counter`

- one press must produce exactly one increment
- no pulses may be dropped under burst clicking
- the retained button identity must stay stable across repeated presses
- burst-click reliability must be exercised by a dedicated harness trace, not only by prose

### `todo_mvc`

- unaffected todo items must keep scope identity across sibling add, remove, filter, and route changes
- unaffected todo items must keep local state across sibling add, remove, filter, and route changes
- unaffected todo items must keep focus and element-reference identity across sibling add, remove, filter, and route changes
- edit-save behavior must be exercised by an unskipped harness trace, not left commented out

### `cells`

- editing one cell may only affect:
  - the edited-cell editor state
  - the committed override
  - the transitive dependency closure
  - the directly affected retained nodes
- steady-state edits must not rebuild the whole grid
- repeated edit, reopen, commit, cancel, and dependency-update traces must be exercised by the harness, not only by manual inspection

## Function Calls and Dynamic Lists

### User-defined function strategy

Pure user-defined functions must not devolve into ad hoc runtime interpretation or per-call spawned actor graphs.

The v1 rule is:

- lower each user-defined function into a reusable IR template
- instantiate function scopes as child scopes keyed by:
  - `function_def_id`
  - `call_site_id`
  - `parent_scope_id`
  - mapped-item identity when present
- reuse the same function instance while that key remains stable
- when arguments change but the function-instance key stays stable, update parameter cells on the existing instance instead of recreating it
- recreate the function instance only when the key changes or the owning scope is dropped

This is the required strategy for helpers such as:

- `new_todo(...)`
- `cell_formula(...)`
- `compute_value(...)`
- `make_cell_element(...)`

Optional whole-body inlining may be added later as an optimization, but it is not the v1 semantic strategy.

### List identity and mapped scope semantics

List item identity is first-class and must never be derived from render order or value equality.

Operator rules:

- `ListLiteral` creates fresh stable item ids
- `ListRange(from: Int, to: Int)` keys items by the produced integer value, not ordinal position
- `ListAppend` creates fresh ids for appended items
- `ListRemove` removes ids and preserves surviving ids
- `ListRetain` preserves ids of surviving items
- `ListMap` maps by upstream item id plus mapper-site identity
- nested `ListMap` uses nested scope tables keyed by parent item identity

Mapped item scopes must:

- preserve item-local `HOLD` state across sibling changes
- react to external dependency changes without unnecessary scope recreation
- tear down old links and subscriptions before dropped scopes can receive delivery

This contract must explicitly cover the patterns represented by:

- `list_object_state`
- `list_map_external_dep`
- dynamic `cells`

## Cells-Specific Direction

`ActorsLite` must make the current dynamic `cells` style fast enough without:

- generated Boon source
- fixed runtime grid assumptions
- spreadsheet business logic in the engine
- `Sheet/*` builtins

Allowed optimizations are generic only:

- sparse runtime state
- stable mapped-item scopes
- generic dependency invalidation
- retained/keyed host patching

Cells acceptance must include two checks:

- the current official-size `cells.bn` works unchanged in style
- one additional nearby source-only variant, using different row or column counts or a nearby nested-list shape, also works correctly and performs acceptably

That second check exists specifically to prove `ActorsLite` did not accidentally optimize for the exact current file shape.

Add a second canonical proof target:

- `playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn`
- matching `cells_dynamic.expected`

`cells_dynamic` must use both axes as normal Boon values and create both rows and columns with nested `List/range |> List/map`. It exists to prove that `ActorsLite` was fixed generically rather than around the current partially hand-shaped `make_row_cells` structure.

Those files do not exist in the repo yet. Phase 4 is not complete until they are added and passing.

The canonical shape should look like this:

```bn
row_count: 100
col_count: 26

all_row_cells:
    List/range(from: 1, to: row_count)
    |> List/map(row_number, new: [
        row: row_number
        cells:
            List/range(from: 1, to: col_count)
            |> List/map(column, new:
                make_cell(column: column, row: row_number)
            )
    ])
```

This snippet is illustrative of the required dynamic authoring style. The exact example may add surrounding helpers and UI structure, but it must preserve this two-axis dynamic construction pattern.

`cells_dynamic` is a peer acceptance target, not a secondary proof or optional regression. Phase 4 is not complete unless both `cells` and `cells_dynamic` are green.

## Execution Order

### Phase 0: Plan document

- add `docs/plans/actors_lite.md`
- no implementation in this phase

### Phase 1: IR, kernel alignment, and semantic microtests

Implement:

- `ActorsLite IR`
- kernel-aligned semantic fixtures
- generational ids
- scope tree
- ready queue
- mailbox processing
- stale-event cutoff semantics
- scope reuse safety tests
- snapshot and branch-activation tests

Exit criteria:

- kernel microtests green
- no UI required yet

### Phase 2: Scalar/control-flow plus minimal host bridge

Implement:

- constants, objects, fields
- `BLOCK`
- `HOLD`
- `THEN`
- `WHEN`
- `WHILE`
- `LATEST`
- `SKIP`
- minimal `SourcePort` / `SinkPort` bridge and `HostViewIR` needed for `counter`

Exit criteria:

- scalar/control-flow conformance tests green
- `counter` non-persistence interactive traces green
- burst-click counter trace green

### Phase 3: List identity, retained bridge, and TodoMVC

Implement:

- list runtime and list IR execution
- stable mapped-item scope reuse
- external dependency invalidation for mapped items
- retained/keyed bridge behavior needed by `todo_mvc`
- host-state mirrors and element-reference support required by `todo_mvc`

Add focused killer tests before full `todo_mvc`:

- mapped item local state survives sibling changes
- mapped item reacts to external dependency changes
- old links are torn down before dropped scopes can receive delivery
- edit-save harness trace is unskipped and green

Exit criteria:

- `todo_mvc` non-persistence interactive traces green
- edit-save trace green

### Phase 4: Dynamic cells

Implement remaining generic runtime behavior needed by the current `cells` example, including the repeated helper-function and edit-flow patterns used there.

Acceptance:

- current dynamic source style stays unchanged
- no generated source
- no fixed runtime dimensions
- correct edit, commit, cancel, and blur behavior
- correct dependent recomputation
- no browser freeze on repeated edit/reopen flows
- variant source check also passes
- fast harness behavior also passes:
  - much tighter waits than the current `cells.expected`, or
  - readiness-based assertions replacing fixed long waits in the milestone harness
- repeated edit/reopen/dependency traces are green in the harness

Exit criteria:

- official-size `cells` green
- `cells_dynamic` green
- fast harness `cells` traces green

### Phase 5: Public exposure and broader parity

Only after Phase 4:

- add public engine enum, picker, CLI, ws, and MCP exposure
- broaden example coverage
- make the full playground example catalog work under `ActorsLite`, not just the milestone subset:
  - every example shown in the playground UI for `ActorsLite` must either run correctly or be intentionally hidden from the `ActorsLite` example surface until implemented
  - shipped `ActorsLite` playground examples must not fall back to subset-marker / unsupported-source errors such as:
    - missing top-level `store`
    - missing source-marker strings used by the current subset detectors
    - other temporary “subset requires …” gating failures
  - the long-term target is lowering/runtime support, not brittle source-marker admission checks
- replace temporary subset-marker gating for shipped examples with real lowering/runtime support or explicit engine gating at the UI/catalog layer
- bring bridge-applied visual parity up to the level of the supported examples:
  - host/bridge style application must preserve meaningful styling, spacing, and layout instead of rendering examples as mostly unstyled text flows
  - supported examples in the playground UI must render with the intended structural layout, not merely with correct text content
  - style/layout parity belongs in the retained/keyed bridge + renderer integration, not as example-specific hacks
- then consider later work such as:
  - persistence
  - mailbox storage optimization
  - actor fusion
  - multithreaded shards

## Test Plan

### Harness-driven reliability

Use the existing `.expected` example harness as the primary browser reliability check:

- `counter.expected`
- `todo_mvc.expected`
- `cells.expected`
- later `cells_dynamic.expected`

`boon-tools exec test-examples` remains the normative scripted interaction runner for the milestone examples.

Add kernel-aligned differential tests on top of that:

- deterministic scripted traces for `counter`, `todo_mvc`, and `cells`
- randomized event traces for the supported subset, compared against kernel/oracle behavior

Required harness additions for the milestone plan:

- `counter.expected` gets a burst-click trace
- `todo_mvc.expected` gets an active, unskipped edit-save trace
- `cells.expected` gets repeated edit/reopen/dependency traces
- `cells` milestone harness must move toward tighter waits or readiness-based assertions instead of 20s/90s-class timing assumptions
- `cells_dynamic.expected` is added as a peer acceptance file

### Semantic and kernel tests

- stale-event cutoff on dynamic resubscribe
- `edge_epoch` rejection of old deliveries
- scope reuse rejects stale ids
- branch switch cannot leak old arm events
- link rebind cannot leak pre-rebind events
- deterministic turn order
- quiescence-before-render behavior
- kernel-aligned semantic comparisons where the kernel already models the behavior

### List and function-instance tests

- `ListMap` preserves item-local `HOLD` state across sibling updates
- external dependencies update mapped outputs without unnecessary scope recreation
- nested `ListMap` preserves unaffected descendants
- `ListRemove` and `ListRetain` preserve surviving identity
- repeated helper-function calls reuse stable instances instead of rebuilding whole subgraphs

### Example acceptance

Initial required examples under `ActorsLite`:

- `counter`
- `todo_mvc`
- current official-size `cells`
- canonical dynamic proof target `cells_dynamic`
- one nearby `cells` variant using different shape parameters or nearby nested-list structure

For Phase 2 through Phase 4, these examples are green only in the non-persistence sense unless `ActorsLite` persistence is explicitly implemented and enabled.

Phase 5 broader-parity acceptance:

- every example exposed in the playground UI when `ActorsLite` is selected opens successfully without unsupported-source/subset-marker errors
- examples that are still not implemented are not exposed as runnable `ActorsLite` examples until support lands
- the example catalog used by the playground UI is aligned with the real `ActorsLite` support set instead of relying on stale source-marker heuristics
- supported playground examples retain meaningful bridge-applied styling and layout, rather than degrading to bare/unlaid-out text with only semantic content preserved

### Performance acceptance

Pinned benchmark environment for v1:

- local Linux 24.04 x86_64 desktop
- Intel Core i7-9700K
- Chromium 146 stable
- release build
- warmed playground and extension
- single visible browser tab
- no DevTools open

Measure at least:

- actor creation cost
- send latency
- messages per second
- peak actor count
- peak queue depth
- startup latency for `counter`, `todo_mvc`, and `cells`
- `cells` edit latency
- retained node creations and deletions per `cells` edit
- dirty sink or export count per `cells` edit
- function-instance reuse hit rate
- recreated mapped-scope count per `cells` edit

Performance budgets on the pinned environment:

- `counter` press-to-paint: p50 <= 8 ms, p95 <= 16 ms
- `todo_mvc` add/toggle/filter/edit-to-paint: p50 <= 25 ms, p95 <= 50 ms
- `cells` cold mount to stable first paint: p50 <= 1200 ms, p95 <= 2000 ms
- `cells` steady-state single-cell edit-to-paint: p50 <= 50 ms, p95 <= 100 ms
- retained node creations per steady-state `cells` edit: <= 6
- retained node deletions per steady-state `cells` edit: <= 6
- dirty sink/export count per steady-state `cells` edit: <= 32
- function-instance reuse hit rate after warm mount: >= 95%
- recreated mapped-scope count per steady-state `cells` edit: 0
- no browser freeze on repeated `cells` editing flows

## Defaults

- crate name: `boon-engine-actors-lite`
- engine name later: `ActorsLite`
- query value later: `actorslite`
- frontend feature name later: `engine-actors-lite`
- hidden/dev-only integration first
- single-threaded v1
- no persistence in v1
- no global Lamport/tick clock is semantically required in `ActorsLite`
- an optional opaque internal turn id is allowed for tracing and diagnostics
- both `ActorId` and `ScopeId` are generational
- runtime core contains no UI or renderer-specific types
- retained/keyed diffing lives in the host bridge
- unsupported constructs fail explicitly at lower time
- `cells` must remain generic and dynamic, not generated or spreadsheet-specific
