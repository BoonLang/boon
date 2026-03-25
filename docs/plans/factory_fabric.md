# FactoryFabric Engine Plan

## Status

- Proposed engine direction for Boon.
- This document is the source-of-truth implementation plan for a candidate new engine named `FactoryFabric`.
- Suggested new crate: `crates/boon-engine-factory-fabric`.
- The repo currently already exposes four engines in the playground: `Actors`, `ActorsLite`, `DD`, and `Wasm`.
- `FactoryFabric` is a fifth engine experiment.
- It should be exposed in the playground from the start rather than hidden behind a dev-only path.

## Why this engine exists

`FactoryFabric` is a new physical runtime architecture for Boon.

It keeps Boon's **semantic** actor model, but it does **not** implement the program as thousands of independent async tasks or generic channels.
Instead it compiles Boon into a deterministic factory-style simulation:

- **machines** for stateful behavior
- **conveyors** for fused pure dataflow
- **buses** for local signal transport
- **depots** for object/list storage with stable identity
- **regions** for scheduling and memory locality
- **a retained host bridge** for browser rendering and host events

This engine exists to make the milestone examples:

- `counter`
- `todo_mvc`
- `cells`
- `cells_dynamic`

both **correct** and **fast**, without:

- spreadsheet-specific builtins
- generated Boon source
- fixed runtime grid assumptions
- fallback to the current `Actors` engine

## Hard constraints

### Must preserve

- Boon actor semantics
- deterministic behavior
- `HOLD`, `THEN`, `WHEN`, `WHILE`, `LATEST`, `LINK` semantics
- ownership / scope drop behavior
- stable list item identity
- dynamic user-defined functions and nested `List/map`
- generic dynamic authoring style for `cells`

### Must not rely on

- spreadsheet-specific runtime logic
- special handling for the current `cells.bn` file shape only
- per-variable async tasks
- generic unbounded channels on the hot path
- renderer-specific types inside the semantic runtime core
- source-classification dispatch or handwritten per-example preview implementations like the current `ActorsLite` stopgap crate

### Initial non-goals

- persistence
- multithreading
- native/CLI/console host parity beyond the browser bridge required to keep the design host-agnostic
- automatic actor fusion across regions beyond the explicit conveyor/region design in this plan
- full language parity outside the milestone surface
- avoiding visible playground exposure for process reasons alone

## Repository grounding

This plan assumes the repo shape that currently exists:

- workspace root `Cargo.toml` uses `members = ["crates/*"]`, so a new engine crate under `crates/` joins the workspace automatically
- `crates/boon/src/platform/browser.rs` now only exposes `common` and `kernel`
- `crates/boon/src/platform/browser/common.rs` defines the public `EngineType` enum and labels/descriptions for the currently exposed engines
- `crates/boon/src/platform/browser/kernel` is the semantic oracle and test fixture, not a shared production runtime
- playground engine selection and example execution live in `playground/frontend`, not in `playground/backend`
- `playground/frontend/Cargo.toml` gates engines behind optional dependencies and feature flags, and `engine-all` is the default public build surface
- `crates/boon-scene` and `crates/boon-renderer-zoon` already provide shared render/event contracts that new browser engines should reuse where practical
- the current playground milestone examples already exist at:
  - `playground/frontend/src/examples/counter/counter.bn`
  - `playground/frontend/src/examples/todo_mvc/todo_mvc.bn`
  - `playground/frontend/src/examples/cells/cells.bn`
  - `playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn`
- the corresponding `.expected` files already exist, and some newer examples are currently engine-skipped rather than duplicated per engine
- the repo already uses `.expected` files and `boon-tools exec test-examples` as the normative scripted example harness
- tool, browser-launch, MCP, and ws-server surfaces currently enumerate engine names explicitly rather than discovering them dynamically

Because GitHub code search is not always accessible anonymously, implementation instructions below intentionally include `rg`-based discovery commands for integration points.

## Repo-specific implementation guardrails

- Treat `FactoryFabric` as a visible experimental engine in the playground. Prefer a straightforward integration over hidden feature gates or private routing tricks.
- The first dev hook belongs in `playground/frontend/src/main.rs` and related frontend feature flags, not in a backend boot path.
- Prefer a renderer boundary that reuses `boon-scene` / `boon-renderer-zoon` contracts or a thin adapter to them. Do not invent a second long-lived DOM/Zoon patch protocol unless the shared contracts are proven insufficient.
- Reuse the existing milestone examples and `.expected` files. Do not fork `counter`, `todo_mvc`, `cells`, or `cells_dynamic` into Fabric-specific copies.
- Avoid adding a new engine-specific automation protocol surface like `GetFactoryFabricDebug`. If extra debugging is needed, prefer a generic engine-debug hook or page-level debug snapshot surface that other engines could also use.

## Design idea in one paragraph

Compile Boon into a **deterministic ticked simulation of regions**.

Within a region:

- pure nodes are fused into linear **conveyor segments** over typed bus slots
- stateful semantics are implemented by **machines** with tiny local queues or staged triggers
- object/list state lives in **depots** with stable identity
- only host events, real pulses, and cross-region wakeups use queued delivery
- everything else is wired through local bus slots and dirty-frontier propagation

After each host event batch:

1. capture a coherent host snapshot
2. inject source events and mirror-state writes
3. run regions until quiescent
4. diff the retained view graph
5. flush renderer patches

This is closer to a factory simulation or an ECS-like dataflow scheduler than to a classic actor runtime.

---

# Part I — Semantic contract

## 1. Kernel alignment

The semantic source of truth is the browser kernel in `crates/boon/src/platform/browser/kernel`.

`FactoryFabric` must align with the kernel on behavior, but **not** share the same production runtime internals.

The kernel is used as:

- semantic oracle
- reference for example traces
- differential testing target
- debugging comparison surface

The kernel is **not** the target runtime architecture.

## 2. Deterministic tick model

Unlike the earlier `ActorsLite` idea, `FactoryFabric` is allowed to use an explicit deterministic global turn counter because the engine is intentionally modeled like a simulation.

Suggested names:

- `FabricTick(u64)`
- `FabricSeq { tick: FabricTick, order: u32 }`

Rules:

- one external host batch enters the engine
- `FabricTick` increments once per external host batch, not once per internal region loop
- `FabricSeq.order` increments monotonically within that batch for each accepted semantic write or delivery point that needs recency
- the engine processes work in deterministic order only
- all semantic writes are stamped with a `FabricSeq`
- `LATEST` and stale-event logic use those stamps
- no behavior may depend on async task wake order, OS scheduler timing, or browser timing races

This is simpler to implement than trying to avoid a runtime-wide notion of causal order, and it mirrors the game-inspired architecture directly.

## 3. `LATEST` semantics are normative

`FactoryFabric` must implement the same reference `LATEST` behavior as the kernel:

- ignore `SKIP` candidates unless all candidates are `SKIP`
- choose the candidate with the greatest `last_changed`
- on ties, choose the lowest input index / earliest source order

This rule must be written into code comments and tests, not left implicit.

## 4. Quiescence-before-render is mandatory

For one host event batch:

1. bridge captures one coherent host snapshot
2. bridge injects `SourcePort` pulses and `MirrorCell` updates
3. runtime drains to quiescence
4. only after quiescence does the bridge read dirty sinks/exports and apply retained diffs

The bridge must not render intermediate half-updated states in v1.

## 5. Same-snapshot host batching

If one host event implies both:

- one or more event pulses (for example `key_down`, `blur`, `double_click`)
- one or more mirrored host-state updates (for example current text value or focus state)

then the bridge must enqueue them from the **same host snapshot** before the single quiescence cycle starts.

This prevents edit-flow bugs where the pulse sees old mirror state or vice versa.

## 6. Stale-event contract

`FactoryFabric` uses a deterministic tick, but stale delivery still needs a precise contract.

Each event-producing output maintains:

- `emission_seq: FabricSeq`

Each dynamic subscription edge stores:

- `source_output_id`
- `cutoff_seq`
- `edge_epoch`

Each queued delivery carries:

- `source_seq`
- `edge_epoch`

A delivery is accepted only if:

- the edge still exists
- the delivery `edge_epoch` matches the currently live edge epoch
- `source_seq > cutoff_seq`

This is normative for:

- `THEN`
- `WHEN`
- `WHILE`
- `LATEST` inputs that are event-like
- dynamic `List/map` rewiring
- `LINK` rebinding
- branch activation/deactivation
- dropped scopes

## 7. Value cutoff semantics

### Hold / cell semantics

- scalars compare by value
- mirrored host state compares by value
- objects and lists compare by stable representation identity or versioned handle
- never use recursive deep equality in the hot path

### Pulse / queue semantics

- repeated equal payloads are still distinct when the language requires them
- pulses are stamped and delivered even if the payload equals the previous payload

## 8. Dirty-closure-only rule

After initial mount, one source change may only enqueue the **transitive dependent closure** of the changed source or sources.

Steady-state execution must not fall back to:

- full-document recompute
- full-grid recompute
- full-scope reevaluation

This rule is especially important for `cells` and `cells_dynamic`.

## 9. Ownership and identity

### Required ids

Use generational ids for all reclaimed semantic/runtime entities that can be referenced after creation:

- `RegionId`
- `MachineId`
- `ScopeId`
- `FunctionInstanceId`
- `ViewNodeId` (retained identity)

### List identity

List item identity is first-class and is never derived from render order or value equality.

Rules:

- `ListLiteral` creates fresh stable item ids
- `ListRange(from: Int, to: Int)` keys by produced integer value, not ordinal position
- `ListAppend` creates fresh ids only for appended items
- `ListRemove` removes ids and preserves survivors
- `ListRetain` preserves ids of survivors
- `ListMap` instances are keyed by upstream item id + mapper-site id
- nested `ListMap` uses nested scope tables keyed by parent item identity

### Retained node identity

Retained view node identity must be keyed by:

- view site id
- owning function-instance identity
- mapped-item identity when present

This is required for:

- stable focus
- stable `Reference[element: ...]`
- low DOM churn
- low retained-node churn in `todo_mvc` and `cells`

---

# Part II — Physical architecture

## 10. Core metaphor

### Machines

Stateful semantic units:

- `HoldMachine`
- `ThenMachine`
- `WhenMachine`
- `WhileMachine`
- `LatestMachine` (only if not fully folded into conveyor arbitration)
- `LinkMachine`
- `ListStoreMachine`
- `SourcePortMachine`
- `MirrorCellMachine`
- `SinkPortMachine`
- host command sinks such as route change requests

### Conveyors

Fused linear pipelines of pure operators over typed bus slots.

Examples:

- constants
- object construction
- field access
- arithmetic
- simple text operators
- pure list projections like count/sum/is_empty/get over store handles
- boolean negation / comparison

### Buses

Typed local signal frames within a region.

A bus slot is the primary medium for local communication.
Most local communication must be wired through bus slots, not queued.

### Depots

Stores for stable identity and versioned state:

- object handles
- list handles and entries
- maybe interned text/value handles if needed later

### Regions

Scheduling, memory-locality, and wakeup units.

### Bridge

The only layer that knows about retained view graphs, route state, focus, hover, and renderer patches.
Browser-specific DOM / Zoon details should stay in the thinnest adapter layer possible, and the bridge should prefer the existing shared render contracts where they fit.

## 11. Why this architecture is different from classic actors

Classic actor implementations usually pay per logical actor:

- mailbox object
- queue allocation
- scheduling overhead
- indirection overhead
- repeated wakeups

`FactoryFabric` pays only where semantics actually need dynamic delivery:

- pulses
- host events
- cross-region wakeups
- stateful machine tasks

Everything else is compiled into:

- contiguous data
- fused pure pipelines
- stable depots
- deterministic region steps

This is the key simplification.

## 12. Runtime overview

Suggested top-level runtime structure:

```rust
pub struct RuntimeCore {
    pub tick: FabricTick,
    pub regions: SlotMap<RegionId, RegionState>,
    pub scopes: SlotMap<ScopeId, ScopeState>,
    pub function_instances: SlotMap<FunctionInstanceId, FunctionInstanceState>,
    pub ready_regions: VecDeque<RegionId>,
    pub pending_cross_region: Vec<CrossRegionWake>,
    pub dirty_sink_ports: Vec<SinkPortId>,
    pub trace_turn_id: u64, // optional diagnostics only
}
```

Suggested region structure:

```rust
pub struct RegionState {
    pub owner_scope: ScopeId,
    pub scheduled: bool,
    pub sleeping: bool,

    pub bus_layout: Vec<BusSlotMeta>,
    pub bus_values: Vec<FabricValue>,
    pub bus_last_changed: Vec<FabricSeq>,
    pub bus_dirty: BitVec,

    pub conveyors: Vec<ConveyorSegment>,
    pub machines: SlotMap<MachineId, MachineState>,

    pub import_edges: Vec<ImportEdge>,
    pub export_edges: Vec<ExportEdge>,

    pub pending_machine_tasks: VecDeque<MachineTask>,
    pub pending_bus_writes: Vec<BusWrite>,
    pub pending_export_notifications: Vec<ExportNotification>,
}
```

Suggested scope structure:

```rust
pub struct ScopeState {
    pub parent: Option<ScopeId>,
    pub children: Vec<ScopeId>,
    pub owner_region: RegionId,
    pub owned_function_instances: Vec<FunctionInstanceId>,
    pub owned_regions: Vec<RegionId>,
    pub owned_retained_nodes: Vec<ViewNodeId>,
    pub live: bool,
}
```

## 13. Types and stamps

Suggested base types:

```rust
pub struct FabricTick(pub u64);

pub struct FabricSeq {
    pub tick: FabricTick,
    pub order: u32,
}

pub enum FabricValue {
    Skip,
    Bool(bool),
    Number(f64),
    Text(TextHandle),
    Object(ObjectHandle),
    List(ListHandle),
    ElementRef(ElementRefHandle),
    // extend only as needed for milestone examples
}
```

`FabricSeq` should be comparable lexicographically.

## 14. Region partitioning strategy

### V1 rule

Do not overcomplicate regionization in the first version.

Bootstrap rule:

- before `todo_mvc` is targeted, it is acceptable for the whole program to run in one root region
- do not implement cross-region imports/exports before `counter` semantics, kernel differential tests, and retained-diff correctness are green

Use these deterministic rules:

1. program root lives in one root region
2. every mapped item scope under `List/map` gets its own child region **only if** it contains stateful machines, link bindings, or retained view nodes
3. pure child scopes without local state may stay folded into the parent region
4. repeated user-defined function calls inside a mapped item belong to the mapped item region
5. cross-region edges exist only where region boundaries exist

This keeps the first implementation simple while still giving `todo_mvc` and `cells` the locality benefits of item-level isolation.

### Later optimization

Later, hot large scopes may be split into more regions, but the initial implementation should not try to infer sophisticated partitions.

---

# Part III — IR and lowering

## 15. Split lowering into semantic IR and view IR

There are two lowering outputs.

### Semantic lowering

```text
Boon AST -> FactoryFabricIR -> RuntimeCore
```

### View lowering

```text
Boon UI AST -> HostViewIR -> retained/keyed bridge
```

These must remain separate.

The semantic core must not contain:

- DOM nodes
- Zoon nodes
- renderer patch types
- style diff objects
- document tree structures

## 16. FactoryFabric IR

Required IR families for v1:

### Pure/data nodes

- constants
- object construction
- field reads
- arithmetic and comparison
- pure text operators used by milestone examples
- pure list projection operators used by milestone examples

### Stateful/behavior nodes

- `Block`
- `Hold`
- `Then`
- `When`
- `While`
- `Latest`
- `Skip`
- `LinkCell`
- `LinkRead`
- `LinkBind`
- function call instance creation
- list store and mutation nodes
- source / mirror / sink boundary nodes

### Host-boundary nodes

- `SourcePort`
- `MirrorCell`
- `SinkPort`
- `HostCommandSink`

### Suggested IR types

```rust
pub enum FabricIrNodeKind {
    Const(FabricValue),
    Object(ObjectTemplate),
    FieldRead { object: ValueRef, field: FieldId },

    Add { left: ValueRef, right: ValueRef },
    Sub { left: ValueRef, right: ValueRef },
    Eq { left: ValueRef, right: ValueRef },
    Ge { left: ValueRef, right: ValueRef },
    Not { input: ValueRef },

    Latest { inputs: Vec<ValueRef> },
    Hold { initial: ValueRef, update: ValueRef },
    Then { trigger: ValueRef, body: ValueRef },
    When { selector: ValueRef, arms: Vec<WhenArmIr> },
    While { selector: ValueRef, arms: Vec<WhileArmIr> },
    Skip,

    LinkCell,
    LinkRead { cell: ValueRef },
    LinkBind { source: ValueRef, target: ValueRef },

    ListLiteral { items: Vec<ValueRef> },
    ListRange { from: ValueRef, to: ValueRef },
    ListMap { list: ValueRef, mapper: FunctionTemplateId },
    ListAppend { list: ValueRef, item: ValueRef },
    ListRemove { list: ValueRef, key: ValueRef },
    ListRetain { list: ValueRef, predicate: FunctionTemplateId },
    ListCount { list: ValueRef },
    ListGet { list: ValueRef, index: ValueRef },
    ListIsEmpty { list: ValueRef },
    ListSum { list: ValueRef },

    SourcePort(SourcePortId),
    MirrorCell(MirrorCellId),
    SinkPort(SinkPortId),
    HostCommandSink(HostCommandKind),
}
```

The exact enum may differ, but the separation between pure operators, stateful behavior, list store ops, and host ports must remain explicit.

## 17. HostViewIR

`HostViewIR` is passive retained structure.
It is **not** a second reactive engine.

In this repo, the first implementation should either:

- lower `HostViewIR` into `boon_scene::RenderDiffBatch`, or
- keep `HostViewIR` internal but adapt it through one thin layer that targets `boon-renderer-zoon`

It should **not** become a second bespoke public rendering protocol if the shared crates already cover the needed retained diff/event surface.

It should model:

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
- style trees required by milestone examples

Each retained node stores:

- stable identity key
- element kind
- children list or child slot refs
- property bindings to sink ports
- event port registrations
- optional host handle

## 18. Lowering strategy for `LINK`

`LINK` must be explicit in the IR and runtime.

Rules:

- `foo: LINK` lowers to `LinkCell`
- reading `foo.bar` through a link lowers through `LinkRead`
- `expr |> LINK { target }` lowers to `LinkBind`
- each `LinkBind` has a binding epoch
- rebinding increments the epoch
- stale deliveries carrying an older binding epoch must be ignored

This must be covered by kernel-aligned tests.

## 19. Lowering strategy for user-defined functions

Pure user-defined functions must lower into **reusable templates**, not into boxed closures or ad hoc interpreter calls.

Template key:

- `function_def_id`
- `call_site_id`
- `parent_scope_id`
- mapped item identity when present

Rule:

- if the key is stable, reuse the same function instance
- if only arguments change, update parameter bus slots on the existing instance
- recreate only when the key changes or the owning scope dies

This is required to keep:

- `todo_mvc` item helpers
- `cells` helpers such as `cell_formula(...)`, `compute_value(...)`, `make_cell_element(...)`

fast and stable.

---

# Part IV — Machines, conveyors, depots

## 20. Machine categories

### `HoldMachine`

Responsibilities:

- own one piece of durable-in-memory state for v1
- cutoff repeated equal semantic values
- emit a new stamp only when state semantically changes

### `ThenMachine`

Responsibilities:

- subscribe to a pulse/event source
- capture the required snapshot values at trigger time
- evaluate the body once per pulse
- emit pulse or hold output depending on lowering context

### `WhenMachine`

Responsibilities:

- on selector pulse, choose one arm from a committed snapshot
- freeze arm body values at trigger time
- produce copied output

### `WhileMachine` / `SwitchGate`

Responsibilities:

- maintain active arm selection
- gate live flow from one selected arm
- advance arm epoch when switching
- prevent old-arm deliveries after switch

### `LinkMachine`

Responsibilities:

- store current binding
- store binding epoch
- serve link reads
- apply link rebinds deterministically

### `ListStoreMachine`

Responsibilities:

- own list handle and entries
- allocate stable item ids
- append/remove/retain deterministically
- expose list version / last_changed
- maintain mapped-item scope table for downstream `ListMap`

### `SourcePortMachine`

Responsibilities:

- receive host pulses
- stamp them
- emit into semantic runtime

### `MirrorCellMachine`

Responsibilities:

- store mirrored host state (text, focus, hover, route)
- cutoff repeated equal values
- emit change stamps when changed

### `SinkPortMachine`

Responsibilities:

- observe bus slots
- record dirty outputs for the bridge
- expose the latest sink-bound value after quiescence

### `HostCommandSink`

Responsibilities:

- turn semantic outputs into host commands
- example: `Router/go_to()` route-change request
- commands are buffered and applied after quiescence in deterministic order

## 21. Conveyor segments

A conveyor segment is a fused array of pure operations.

Suggested representation:

```rust
pub struct ConveyorSegment {
    pub id: ConveyorId,
    pub input_slots: SmallVec<[BusSlot; 8]>,
    pub output_slots: SmallVec<[BusSlot; 8]>,
    pub ops: Vec<ConveyorOp>,
    pub topo_order: Vec<OpId>,
}
```

Suggested op shape:

```rust
pub enum ConveyorOp {
    Copy { src: BusSlot, dst: BusSlot },
    Add { left: BusSlot, right: BusSlot, dst: BusSlot },
    Sub { left: BusSlot, right: BusSlot, dst: BusSlot },
    Eq { left: BusSlot, right: BusSlot, dst: BusSlot },
    Ge { left: BusSlot, right: BusSlot, dst: BusSlot },
    Not { src: BusSlot, dst: BusSlot },
    MakeObject { fields: Vec<(FieldId, BusSlot)>, dst: BusSlot },
    ReadField { object: BusSlot, field: FieldId, dst: BusSlot },
    TextTrim { src: BusSlot, dst: BusSlot },
    TextLength { src: BusSlot, dst: BusSlot },
    // extend only as needed for milestone builtins
}
```

### Conveyor rules

- pure only
- no host side effects
- no internal queues
- no ownership mutation
- no list item allocation
- no route change commands

### Conveyor evaluation rule

Run only when at least one input slot is dirty.

For each op:

- read current bus values
- compute output
- if output differs semantically, write to staged bus slot and stamp it

## 22. Depot design

### Objects

Objects should usually be cheap values if small enough.
If object copying becomes hot, introduce object handles and interned object layouts later.

### Lists

Lists need stable entry identity from day one.

Suggested list representation:

```rust
pub struct ListStore {
    pub alloc_site: SourceId,
    pub next_item_id: u64,
    pub items: Vec<ListEntry>,
    pub last_changed: FabricSeq,
}

pub struct ListEntry {
    pub key: ItemId,
    pub value: FabricValue,
}
```

Each list handle is a depot handle, not a copied deep vector in the hot path.

## 23. Mapped scope table

Each `ListMap` site owns a table:

```rust
pub struct MapInstanceTable {
    pub mapper_site: SourceId,
    pub by_item: BTreeMap<ItemId, FunctionInstanceId>,
}
```

Required behavior:

- preserve unaffected instances when siblings change
- preserve local `HOLD` state for unaffected items
- update external dependencies without recreating instances unnecessarily
- drop removed items before any stale deliveries can reach them

---

# Part V — Scheduling and execution

## 24. Queue philosophy

Use queues sparingly.

### Queued

- host event pulses
- stateful machine tasks
- cross-region wakeups
- host commands

### Not queued

- local pure data propagation
- local stable value reads
- local mirror-cell reads
- local pure object/field/arithmetic flow

That is the core simplification.

## 25. Scheduling units

The global ready queue schedules **regions**, not individual pure nodes.

Within a region:

- bus dirty bits drive conveyor evaluation
- machine task queue drives stateful behavior
- export change queue drives cross-region wakeups

## 26. Read / commit / wake cycle

Each region step follows this shape:

1. apply incoming imports and host writes to local bus or machine task queue
2. run conveyor segments whose inputs are dirty
3. collect triggered machine tasks in stable order
4. execute machine tasks, producing staged writes or host commands
5. commit changed bus slots
6. if exports changed, create cross-region wakeups in stable order
7. repeat within the region until no more local work remains

## 27. Stable ordering rules

### Global ordering

- ready regions are drained FIFO
- first transition from sleeping to ready schedules region once
- repeated wakeups while already scheduled only merge additional work

### Local ordering

Within a region, use fixed deterministic ordering:

1. imports / host writes by arrival order
2. conveyor segments by segment id / lowering order
3. machine tasks by `(trigger_seq, machine_order)`
4. export notifications by export index order

### Cross-region ordering

Cross-region wakeups are buffered and then merged in deterministic order after the producing region step completes.

Do not wake target regions immediately from inside the producer step.

## 28. Snapshot capture for stateful tasks

Stateful tasks that require snapshot semantics must capture the required inputs at enqueue time.

Examples:

- `THEN` captures the triggering pulse and any body reads needed by the body
- `WHEN` captures selector pulse and chosen-arm snapshot inputs
- `WHILE` switch captures selector state and active-arm epoch

Suggested task shape:

```rust
pub enum MachineTask {
    ThenFire {
        machine: MachineId,
        trigger_seq: FabricSeq,
        trigger_value: FabricValue,
        snapshot_inputs: SmallVec<[FabricValue; 4]>,
    },
    WhenFire {
        machine: MachineId,
        trigger_seq: FabricSeq,
        selector_value: FabricValue,
        snapshot_inputs: SmallVec<[FabricValue; 8]>,
    },
    WhileSwitch {
        machine: MachineId,
        trigger_seq: FabricSeq,
        selector_value: FabricValue,
    },
    HoldUpdate {
        machine: MachineId,
        trigger_seq: FabricSeq,
        input_value: FabricValue,
    },
    LinkBind {
        machine: MachineId,
        trigger_seq: FabricSeq,
        binding: LinkBindingValue,
    },
    ListMutation {
        machine: MachineId,
        trigger_seq: FabricSeq,
        op: ListMutationOp,
    },
}
```

Do not re-read unrelated live bus slots later if the machine's semantics depend on the snapshot from trigger time.

## 29. Pseudocode: full host cycle

```rust
fn handle_host_batch(batch: HostBatch, runtime: &mut RuntimeCore, bridge: &mut HostBridge) {
    runtime.begin_batch_tick();
    let snapshot = bridge.capture_snapshot(batch);
    let injections = bridge.lower_snapshot_to_inputs(snapshot);

    for injection in injections {
        runtime.inject_host_input(injection);
    }

    runtime.run_until_quiescent();

    let sink_values = runtime.take_dirty_sinks();
    let commands = runtime.take_host_commands();

    bridge.apply_commands(commands);
    bridge.diff_and_flush(sink_values);
}
```

## 30. Pseudocode: region step

```rust
fn run_region(region: &mut RegionState, ctx: &mut RuntimeCore) {
    while region.has_local_work() {
        region.apply_pending_imports_and_host_writes();

        let mut progress = false;

        progress |= region.run_dirty_conveyors(ctx.next_seq());
        progress |= region.enqueue_triggered_machines(ctx.next_seq());
        progress |= region.run_machine_tasks(ctx.next_seq());
        progress |= region.commit_pending_bus_writes();
        progress |= region.queue_export_notifications();

        if !progress {
            break;
        }
    }

    let wakes = region.take_cross_region_wakes();
    ctx.merge_cross_region_wakes(wakes);
}
```

In real code, sequence allocation should happen at the exact stamped write / delivery points inside those helpers.
The pseudocode uses `next_seq()` only to show that stamps advance monotonically within one host batch and must not bump the outer `FabricTick`.

---

# Part VI — Host bridge and retained view

## 31. Host bridge responsibilities

The host bridge owns:

- retained node graph
- event listener registration
- mirrored host state capture
- DOM / Zoon / browser integration
- route state and route change commands
- focus and hover state capture
- text input current value capture
- `Reference[element: ...]` host handles
- diffing and renderer patch application

The runtime core owns none of those.

## 32. Host input split

The bridge exposes two input classes.

### Pulse/event inputs via `SourcePort`

At minimum:

- `press`
- `click`
- `change`
- `key_down`
- `blur`
- `focus`
- `double_click`
- optionally `hover_change` if needed by examples

### Mirrored host state via `MirrorCell`

At minimum:

- current text input text
- current focus state
- current hover state
- current route state

## 33. Event fanout ordering

For one host event that fans out to multiple `SourcePort` pulses:

- the bridge must enqueue them in stable lowering order for the retained node
- then one quiescence cycle runs for the whole batch

This rule must be explicit in tests.

## 34. Route semantics

- `Router/route()` reads from a route `MirrorCell`
- `Router/go_to()` lowers to a host command sink
- bridge applies route command after quiescence in deterministic order
- subsequent host snapshot reflects the new route

## 35. Element references

`Reference[element: ...]` resolves against stable retained node ids.

Rules:

- references become valid only after the retained node exists
- references become invalid immediately when the node is removed in the retained diff
- no stale host handle survives past the diff that removed the node

## 36. DOM churn rule

After initial mount:

- unrelated edits must not recreate unrelated retained nodes
- `todo_mvc` unaffected items must preserve retained identity
- `cells` steady-state single-cell edits must not rebuild the whole grid

## 37. Optional view virtualization

Do **not** make DOM virtualization a prerequisite for v1 correctness.

For the first selectable engine version:

- retained diffing and low churn are mandatory
- viewport-based materialization may be added later if the `cells` budgets are not met

This keeps the first implementation achievable.

---

# Part VII — Mapping Boon constructs to FactoryFabric

## 38. `BLOCK`

Usually lowers to region-local scope grouping.

- no standalone runtime cost unless it introduces a scope boundary required for ownership or function instance grouping
- pure inner expressions should be folded into conveyors

## 39. `HOLD`

Lower to `HoldMachine` plus one output bus slot.

- initial value may come from a conveyor or literal
- updates arrive from a trigger input
- machine emits only on semantic change

## 40. `THEN`

Lower to `ThenMachine`.

- trigger source is pulse-like
- body snapshot inputs are captured at trigger time
- output is stamped with trigger-derived recency

## 41. `WHEN`

Lower to `WhenMachine`.

- selector pulse chooses one arm
- chosen arm inputs are captured from the committed snapshot
- output is copied/frozen

## 42. `WHILE`

Lower to `WhileMachine` / `SwitchGate`.

- one active arm at a time
- active arm changes advance epoch
- live flow from active arm only
- stale old-arm deliveries rejected by edge epoch

## 43. `LATEST`

If all inputs are local bus values, `LATEST` can often be compiled into pure arbitration logic in a conveyor segment.

If it mixes dynamic event-like sources that need explicit task handling, lower it to a small `LatestMachine`.

Start simple:

- allow a tiny `LatestMachine` in v1 if that reduces implementation risk
- optimize into conveyor arbitration later when the semantics are stable

## 44. `LINK`

- storage: `LinkMachine`
- read: conveyor op or dedicated `LinkRead` op
- bind: machine task with epoch bump

## 45. Lists

### `LIST {}`

Lower to `ListStoreMachine` construction.

### `List/append`

- allocate new `ItemId`
- append entry
- update last_changed
- schedule downstream mapped instances only where needed

### `List/remove`

- remove the matching stable item identity, aligned with the current kernel/runtime model and the existing `List/remove(item, on: ...)` example shape
- preserve surviving ids
- drop removed item scopes before any further deliveries can hit them

### `List/retain`

- evaluate predicate per item while preserving surviving ids
- do not rebuild the full list if only a subset changes

### `List/map`

Lower to:

- upstream list store handle
- `MapInstanceTable`
- one child function instance per live item id
- optional child region per item if the mapped subtree contains stateful machines or retained nodes

## 46. Builtins

For v1, implement the exact milestone builtin surface as explicit conveyor ops or tiny machines, not as generic reflection-based calls.

Required milestone builtins include at least:

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

Guideline:

- pure builtins -> conveyor ops
- stateful/host builtins -> machines or host command sinks

---

# Part VIII — Milestone examples and proof targets

## 47. Required milestone examples

### `counter`

Purpose:

- prove pulse reliability
- prove basic `THEN` + `Math/sum` path
- prove retained identity stability under repeated updates

Normative invariants:

- one press produces exactly one increment
- burst clicking drops no pulses
- the button retained node remains stable across increments

### `todo_mvc`

Purpose:

- prove list identity
- prove mapped item state reuse
- prove route/focus/blur/reference stability
- prove edit-mode correctness

Normative invariants:

- unaffected items keep scope identity
- unaffected items keep local state
- unaffected items keep focus where semantically expected
- unaffected items keep element-reference identity where semantically expected
- edit-save flow works with an active, unskipped harness trace

### `cells`

Purpose:

- prove dynamic list-heavy reactive dataflow
- prove repeated helper-function instance reuse
- prove dependency-closure updates
- prove no whole-grid steady-state rebuild

Normative invariants:

- editing one cell only affects the edited cell state, committed override, dependency closure, and directly affected retained nodes
- steady-state single-cell edits do not rebuild the entire grid
- repeated edit/reopen/commit/cancel/blur/dependency traces are green in the harness

### `cells_dynamic`

Purpose:

- prove the engine is generic, not tuned to the current partially hand-shaped `cells.bn`

Normative requirements:

- both axes are driven by normal Boon values
- rows and columns are both created using nested `List/range |> List/map`
- it is a peer acceptance target, not an optional extra test

Suggested canonical shape:

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

## 48. Phase completion rule for Cells

The Cells implementation phase is not complete unless **both**:

- `cells`
- `cells_dynamic`

are green and performant.

---

# Part IX — Implementation plan

## 49. Suggested crate layout

Create:

```text
crates/boon-engine-factory-fabric/
  Cargo.toml
  src/
    lib.rs
    engine.rs
    ir.rs
    lower.rs
    debug.rs
    metrics.rs
    runtime/
      mod.rs
      ids.rs
      value.rs
      bus.rs
      region.rs
      machine.rs
      conveyor.rs
      depot.rs
      scheduler.rs
      tasks.rs
    host/
      mod.rs
      view_ir.rs
      retained.rs
      bridge.rs
  tests/
    semantics.rs
    lists.rs
    host.rs
    examples.rs
    perf_smoke.rs
```

Suggested public browser-facing API from the crate:

```rust
pub fn run_factory_fabric(source: &str) -> impl Element;

pub fn factory_fabric_metrics_snapshot() -> Result<FactoryFabricMetricsReport, String>;
pub fn factory_fabric_debug_snapshot() -> Option<DebugSnapshot>;
```

Suggested internal runtime-facing API:

```rust
pub struct FactoryFabricRunner { ... }

impl FactoryFabricRunner {
    pub fn new(compiled: CompiledProgram, host: Box<dyn HostBridgeAdapter>) -> Self;
    pub fn handle_host_batch(&mut self, batch: HostBatch) -> HostFlushResult;
    pub fn read_sink(&self, sink: SinkPortId) -> Option<&FabricValue>;
    pub fn debug_snapshot(&self) -> DebugSnapshot;
}
```

## 50. Phase 0 — plan only

Deliver this plan and freeze the adoption path.

Required decisions before code:

- confirm `FactoryFabric` is a fifth experimental engine exposed in the playground
- freeze the exact supported v1 semantic + host subset and require lower-time errors for everything else
- keep the integration path simple even if that means exposing an incomplete experimental engine earlier

No production code yet.

## 51. Phase 1 — crate skeleton and experimental playground integration

### Goals

- add new crate
- compile the workspace with the new crate present
- expose it in the playground as an experimental engine with the simplest viable integration path

### Checklist

1. create `crates/boon-engine-factory-fabric`
2. add minimal `Cargo.toml` and `lib.rs`
3. verify the workspace picks it up automatically because root workspace uses `crates/*`
4. add `boon-engine-factory-fabric` as an optional dependency in `playground/frontend/Cargo.toml`
5. add a dedicated frontend feature such as `engine-factory-fabric`
6. add it to the normal playground build surface instead of introducing hidden-only selection logic
7. extend `EngineType`, the playground picker, and frontend query/storage mappings early so the experimental engine is selectable in the normal UI
8. update the tool/browser/ws/MCP engine enumerations in the same integration pass so the exposed engine name stays consistent everywhere
9. add placeholder `boon-tools verify factory-fabric` and `boon-tools metrics factory-fabric` entrypoints early so the experimental engine has a stable repo-level command surface

### Useful discovery commands

From repo root, use:

```sh
rg "EngineType" crates playground tools
rg "engine-actors|engine-actors-lite|engine-dd|engine-wasm" playground/frontend/Cargo.toml playground/frontend/src/main.rs
rg "picker_label|full_name|description" crates playground tools
rg "actorslite|DifferentialDataflow|Wasm|Actors" crates playground tools
rg "boon_set_engine|GetStatus|availableEngines|preferredWasmEngine" playground tools
rg "test-examples|expected" tools playground crates
```

These commands should be part of the implementation checklist so a simpler agent can discover exact integration points.

## 52. Phase 2 — core ids, bus, and scheduler kernel

### Deliverables

- generational ids
- `FabricTick` / `FabricSeq`
- `RuntimeCore`
- `RegionState`
- bus slots and dirty bits
- minimal global ready-region queue
- deterministic ordering tests

### Required tests

- region schedules once on first wakeup
- repeated wakeups while already scheduled do not duplicate scheduling
- region drain order is deterministic
- stamp ordering is deterministic
- stale ids are rejected safely

## 53. Phase 3 — semantic IR and pure conveyors

### Deliverables

- `FactoryFabricIR`
- parser-backed lowering from `boon::parser` for the supported milestone subset
- lowering for pure constants/objects/fields/basic arithmetic/comparison
- conveyor segment compiler
- region-local pure propagation

### Required tests

- lowering works from parsed Boon source, not by matching known example text or filenames
- unsupported constructs fail explicitly at lower time with clear errors
- pure dataflow graphs stabilize correctly
- unchanged pure outputs do not restamp
- `LATEST` semantics match kernel oracle on tie-break and `SKIP`

## 54. Phase 4 — stateful machines

### Deliverables

- `HoldMachine`
- `ThenMachine`
- `WhenMachine`
- `WhileMachine`
- `LinkMachine`
- `SourcePortMachine`
- `MirrorCellMachine`
- `SinkPortMachine`

### Required tests

- `THEN` snapshot timing
- `WHEN` frozen-arm timing
- `WHILE` old-arm event leakage rejection
- `LINK` rebinding epoch behavior
- mirror-cell same-snapshot batching with pulse sources

## 55. Phase 5 — minimal HostViewIR and counter

### Deliverables

- minimal HostViewIR
- retained node graph
- one explicit renderer boundary decision for v1:
  - `HostViewIR -> boon_scene::RenderDiffBatch`, or
  - `HostViewIR -> thin boon-renderer-zoon adapter`
- generic quiescence/debug hook exposed through the page or a generic engine-debug tool path so the harness can wait for "host batch drained + retained diff flushed"
- event registration for `press`
- text sink binding
- `Document/new`, `Element/button`, `Element/stripe`, label/text sink plumbing sufficient for `counter`

### Required tests

- `counter` harness trace green in non-persistence mode
- dedicated burst-click trace green
- no button retained-node churn across increments

## 56. Phase 6 — lists, mapped scopes, and TodoMVC

### Deliverables

- list depot and `ListStoreMachine`
- `ListMap` instance tables
- mapped item function instances
- retained node identity keyed by view site + instance + item id
- route mirror / route command support
- focus / blur / text mirror support
- `Reference[element: ...]` host handles

### Required tests before full `todo_mvc`

- sibling add/remove preserves unaffected item-local `HOLD` state
- external dependency updates mapped outputs without recreating unaffected instances
- old links/subscriptions are torn down before dropped scopes can receive delivery
- edit-save harness trace is active and green

### Exit criteria

- `todo_mvc` non-persistence interactive traces green
- edit-save trace green
- retained-node churn for unrelated items near zero during common edits

## 57. Phase 7 — cells and cells_dynamic

### Deliverables

- repeated helper-function instance reuse
- efficient dependency closure propagation
- no whole-grid steady-state recompute
- existing `cells_dynamic` example and harness brought green on `FactoryFabric` without introducing a Fabric-only duplicate example

### Required tests

- repeated edit/reopen/dependency traces green
- no mapped-scope recreation on unrelated single-cell edits
- dependency closure only, no whole-grid invalidation
- `cells_dynamic` green and similar performance class as `cells`

### Exit criteria

- `cells` green
- `cells_dynamic` green
- fast harness traces green

## 58. Phase 8 — broader tooling and hardening

After the experimental engine is already exposed in the playground:

1. tighten labels, descriptions, and UX wording so the engine reads as experimental rather than production-ready
2. keep query string value `factoryfabric` stable once introduced
3. expand `verify` / `metrics` / harness coverage as the supported subset grows
4. remove temporary integration shortcuts only when they are genuinely blocking progress or correctness
5. revisit whether the engine should remain a fifth long-lived engine after milestone evidence exists

### Engine naming recommendation

- enum variant: `FactoryFabric`
- picker label: `Fabric (Exp)` or `FactoryFabric`
- full name: `FactoryFabric`
- query value: `factoryfabric`
- crate name: `boon-engine-factory-fabric`

If terse labels are preferred, `Fabric` is also acceptable, but the experimental status should be visible somewhere in the UI copy.

---

# Part X — Reliability plan

## 59. Harness is normative

The existing `.expected` harness remains the primary browser reliability check, but milestone sign-off should use three layers:

1. unit tests and kernel differential tests for semantic correctness
2. `boon-tools exec test-examples --engine FactoryFabric --skip-persistence` for browser behavior
3. `boon-tools verify factory-fabric --check` as the aggregate milestone gate once the engine has enough tooling support

Quantitative budgets should **not** live inside `.expected` files.
Use `boon-tools metrics factory-fabric --check` for performance/churn gates.

Required harness files:

- `counter.expected`
- `todo_mvc.expected`
- `cells.expected`
- `cells_dynamic.expected`

For this repo, prefer extending the existing expectation files before introducing Fabric-only variants.

Use:

```sh
boon-tools exec test-examples
```

as the normative scripted interaction runner.

Do not copy the current `ActorsLite`-specific harness branches blindly.
Prefer to generalize engine selection, readiness, and debug snapshot fetching where possible so `FactoryFabric` does not multiply special-case automation paths.

## 60. Capability-gated persistence during milestone phase

The current `counter.expected` and `todo_mvc.expected` already include persistence-after-rerun sections.

Because persistence is not part of `FactoryFabric` v1:

- milestone green means **non-persistence** interactive sequences are green
- use the already-supported `--skip-persistence` path for milestone verification rather than inventing a Fabric-only expectation format
- if dedicated milestone expectation variants are introduced, they must preserve the same non-persistence behavioral coverage and only exclude persistence-specific sections

## 61. Required harness additions

### `counter.expected`

Add:

- burst-click trace
- repeated burst trace
- only add retained-node identity assertions after a generic debug/query hook exists to expose stable ids or churn counters

### `todo_mvc.expected`

Add:

- active unskipped edit-save trace
- focus preservation trace across sibling list changes
- route/filter trace that checks unaffected item identity behavior once generic debug/query hooks can expose the needed state

### `cells.expected`

Add:

- repeated edit / reopen / commit / cancel / blur traces
- dependency-update trace covering transitive closure recomputation
- tighter waits or readiness-based assertions replacing long blind delays

### `cells_dynamic.expected`

The file already exists in the repo. Extend and keep it as a peer acceptance file rather than creating a second FactoryFabric-specific expectation unless the generic file structure proves insufficient.

### Missing hooks the plan must budget for

- a generic engine-debug snapshot path that tools can query without adding a Fabric-only ws command
- a quiescence/idle signal so the harness can wait for engine drain + retained flush instead of stacking blind delays
- churn/identity counters if retained-node churn, dirty-closure size, or mapped-scope reuse are part of acceptance

## 62. Kernel differential tests

Add deterministic differential tests between `FactoryFabric` and the kernel for the supported subset.

Required areas:

- `LATEST`
- `HOLD`
- `THEN`
- `WHEN`
- `WHILE`
- `LINK`
- list identity and retain/remove behavior
- route mirror / host mirror flows where modeled by the kernel

## 63. Randomized trace tests

For the supported subset, generate randomized event traces and compare:

- final visible sink values
- key intermediate sink values if the harness records them
- retained identity invariants where possible

Start with small graphs and list sizes first.

---

# Part XI — Performance plan

## 64. Performance targets

Use a pinned environment for milestone sign-off.

Measure and enforce these budgets through `boon-tools metrics factory-fabric --check` or an equivalent repeatable metrics runner, not through one-shot `.expected` traces.

Before any absolute latency budget becomes a merge gate, add a reproducible `boon-tools` measurement path for this engine, similar in spirit to the current `ActorsLite` metrics/verify flow.
Before that exists, earlier phases should gate on structural counters and reuse/churn metrics rather than absolute milliseconds.

Suggested pinned environment:

- Linux 24.04 x86_64 desktop
- Intel Core i7-9700K
- Chromium stable
- release build
- warmed playground and extension
- one visible tab
- DevTools closed

If the repo already uses a different benchmark machine, replace this section with the actual maintained lab environment.

## 65. Metrics to record

### Runtime core

- region creation cost
- machine creation cost
- host batch processing time
- cross-region wake count
- bus writes per host event
- machine task count per host event
- conveyor ops executed per host event
- messages/second for queued paths only

### View bridge

- retained node creations per event
- retained node deletions per event
- dirty sink count per event
- renderer patch count per event

### Instance reuse

- function-instance reuse hit rate
- recreated mapped-scope count
- recreated retained-node count for unaffected items

## 66. Budget targets

Suggested initial budgets:

- `counter` press-to-paint: p50 <= 8 ms, p95 <= 16 ms
- `todo_mvc` add/toggle/filter/edit-to-paint: p50 <= 25 ms, p95 <= 50 ms
- `cells` cold mount to stable first paint: p50 <= 1200 ms, p95 <= 2000 ms
- `cells` steady-state single-cell edit-to-paint: p50 <= 50 ms, p95 <= 100 ms
- `cells_dynamic` steady-state single-cell edit-to-paint: same class as `cells`
- retained node creations per steady-state `cells` edit: <= 6
- retained node deletions per steady-state `cells` edit: <= 6
- dirty sink/export count per steady-state `cells` edit: <= 32
- function-instance reuse hit rate after warm mount: >= 95%
- recreated mapped-scope count per steady-state `cells` edit: 0
- no browser freeze on repeated `cells` editing flows

## 67. If budgets are missed

Apply optimizations in this order:

1. verify no accidental whole-grid dirty closure
2. verify function-instance reuse
3. verify retained-node identity and DOM churn
4. fuse more pure ops into conveyors
5. reduce cross-region boundaries where unnecessary
6. add small-vector / small-ring storage for hot queues
7. consider optional view virtualization only if core + retained diff are already correct

Do **not** jump to multithreading first.

---

# Part XII — Debugging and observability

## 68. Build debug data early

Inspired by factory-game tooling, build machine-readable debug surfaces early.
Polished visualizers are useful, but they should follow the data model rather than block the core milestone path.

Recommended debug surfaces:

- region graph
- scope tree
- conveyor graph
- bus dirty frontier per host event
- machine task timeline
- cross-region wake list
- stale delivery rejections with `edge_epoch`
- function-instance reuse heatmap
- retained-node churn heatmap
- `cells` dependency closure visualizer

## 69. Debug snapshot API

Add a debug snapshot structure from the engine crate:

```rust
pub struct DebugSnapshot {
    pub tick: FabricTick,
    pub quiescent: bool,
    pub ready_regions: Vec<RegionId>,
    pub regions: Vec<RegionDebugState>,
    pub dirty_sinks: Vec<SinkPortId>,
    pub host_commands: Vec<HostCommandDebug>,
    pub retained_node_creations: usize,
    pub retained_node_deletions: usize,
    pub recreated_mapped_scopes: usize,
}
```

This makes it much easier for a simpler agent to diagnose performance failures without rewriting instrumentation each time.

Repo-specific guardrail:

- prefer a generic debug surface that tools can query without hard-coding a new engine-specific ws command
- expose enough data for the harness and metrics commands to assert quiescence, churn, and reuse without screen-scraping DOM internals
- the current `GetActorsLiteDebug` path is a special-case expedient, not the model to copy

---

# Part XIII — Exact milestone surface

## 70. Core language surface for v1

### Control/data

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
- primitive arithmetic/comparison needed by milestone examples, at minimum:
  - `+`
  - `-`
  - `==`
  - `>=`

### Lists

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

### Builtins

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

### HostView surface

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

### HostView property/style surface

The constructor list above is necessary but not sufficient.
The checked-in milestone examples also require lowering the actual property/style/value surface used by those constructors, at minimum:

- element payload fields such as `event`, `tag`, `label`, `placeholder`, `text`, `focus`, `checked`, `is_visible`, `url`, `icon`, `child`, `items`, `contents`, `direction`, `gap`, and `style`
- helper/value forms such as `Hidden[text: ...]`, `NoOutline`, `Fill`, `Center`, `Row`, `Column`, `SansSerif`, `Antialiased`, `Light`, `ExtraLight`, and `Oklch[...]`
- style groups exercised by the current examples such as width/height, padding, align, background/color, font, line height, font smoothing, shadows, and outline

Treat this as a repo-derived acceptance surface from the checked-in milestone examples, not as an aspirational hand-maintained summary.
Before broadening support, re-run inventory commands against the example files and keep the implementation subset aligned to what the repo actually uses.

Unsupported constructs must fail explicitly at lower time with clear errors.

---

# Part XIV — Concrete coding checklist

## 71. Week-by-week implementation order (suggested)

### Step A — crate and ids

- create new crate
- compile empty crate in workspace
- implement ids + stamps
- add unit tests for ordering

### Step B — runtime skeleton

- add region state
- add ready queue
- add bus slots + dirty bits
- add debug snapshot

### Step C — IR + pure lowering

- parse/lower minimal subset for `counter` through `boon::parser`, not source matching
- implement conveyors
- verify pure propagation tests

### Step D — machines

- implement `HoldMachine`, `ThenMachine`, `SourcePort`, `SinkPort`
- verify `counter`

### Step E — HostViewIR minimal path

- add retained button/label/stripe/doc support
- lock the renderer-output shape before broadening widget coverage
- wire generic quiescence/debug plumbing for the harness before broader interaction coverage
- make `counter` selectable in dev mode

### Step F — lists and mapped scopes

- implement list store, map instance table, function instance reuse
- add list identity tests

### Step G — TodoMVC

- route mirror / host command
- text mirror / focus / blur / element refs
- make all non-persistence traces green

### Step H — Cells

- optimize dependency closure
- verify no whole-grid steady-state recompute
- add `cells_dynamic`
- tighten waits / readiness in harness

### Step I — experimental integration polish

- keep `FactoryFabric` exposed in `EngineType`, the picker, and the standard query/tool surfaces
- tighten experimental labeling and remove only the shortcuts that are causing real pain
- update frontend storage/query parsing plus CLI/MCP/tool enums together whenever the exposed engine contract changes
- document the engine as experimental and keep the rollout path simple

## 72. Search commands an implementing agent should run

```sh
rg "pub enum EngineType" crates playground tools
rg "short_name\(|picker_label\(|full_name\(|description\(" crates playground tools
rg "engine-actors|engine-actors-lite|engine-dd|engine-wasm|engine-all" playground/frontend/Cargo.toml playground/frontend/src/main.rs
rg "availableEngines|preferredWasmEngine|boon_set_engine|Invalid engine" playground tools
rg "Document/new|Element/button|Element/text_input|Reference\[element" playground/frontend/src/examples
rg -o 'Element/[A-Za-z_]+' playground/frontend/src/examples/{counter,todo_mvc,cells,cells_dynamic} -g '*.bn' | sort -u
rg -n 'Hidden\[|NoElement|NoOutline|Reference\[element|Oklch\[|SansSerif|Fill|Center|Column|Row' playground/frontend/src/examples/{counter,todo_mvc,cells,cells_dynamic} -g '*.bn'
rg "test-examples|\.expected|skip-persistence|persistence" tools playground
rg "LATEST|TickSeq|LinkCell|ListCell|EventType|text_inputs|Focus|DoubleClick" crates/boon/src/platform/browser/kernel
```

These commands should be run before each integration phase to discover exact code locations.

---

# Part XV — Risks and guardrails

## 73. Biggest likely failure modes

### Failure mode 1: too many regions

Symptom:

- high cross-region wake traffic
- low locality
- scheduling overhead dominates

Fix:

- fold tiny pure child scopes into parent region
- keep regions for mapped item stateful subtrees and other truly useful boundaries only

### Failure mode 2: fake correctness via hidden full recompute

Symptom:

- examples are correct but slow
- `cells` edit dirties most of the graph

Fix:

- enforce dirty-closure-only metrics
- instrument per-edit dirty closure size

### Failure mode 3: snapshot bugs in `THEN` / `WHEN`

Symptom:

- edit flows use wrong text/focus values
- event ordering becomes flaky

Fix:

- capture snapshot inputs at task enqueue time
- test same-snapshot batching with mirror cells and event pulses together

### Failure mode 4: unstable retained identity

Symptom:

- focus jumps in `todo_mvc`
- element references break
- DOM churn too high

Fix:

- use retained identity key = view site + function instance + item id
- add churn counters and identity assertions

### Failure mode 5: optimizing `cells` around one file

Symptom:

- `cells.bn` passes but nearby dynamic variants fail or slow down

Fix:

- keep `cells_dynamic` mandatory and equal status

## 74. Explicit anti-goals for milestone implementation

Do **not** do these as the first answer to performance trouble:

- add spreadsheet-specific engine builtins
- generate special Boon code for `cells`
- special-case row/column counts in runtime code
- make DOM virtualization mandatory before the core is correct
- add multithreading before dirty closure, retained identity, and function-instance reuse are correct

---

# Part XVI — Final acceptance checklist

## 75. Semantic acceptance

- kernel-aligned supported subset tests green
- deterministic ordering tests green
- `LATEST` tie-break and `SKIP` semantics match kernel
- `LINK` rebinding semantics green
- list identity tests green

## 76. Browser reliability acceptance

- `counter.expected` green in non-persistence mode
- `todo_mvc.expected` green in non-persistence mode
- `cells.expected` green
- `cells_dynamic.expected` green
- burst-click trace green
- `todo_mvc` edit-save trace green and unskipped
- repeated `cells` edit/reopen/dependency traces green

## 77. Performance acceptance

- all milestone performance budgets met on the pinned environment
- no whole-grid steady-state `cells` recompute
- no browser freeze on repeated `cells` editing flows
- retained-node churn budgets met
- mapped-scope recreation budget met

## 78. Public playground acceptance

- `FactoryFabric` selectable in the playground
- query parameter can select it
- engine label/description shown correctly
- switching engines does not break the harness or example loading flow

---

# References and inspiration

## Boon repo grounding

- Workspace root: `https://github.com/BoonLang/boon/blob/main/Cargo.toml`
- Browser entry: `https://github.com/BoonLang/boon/blob/main/crates/boon/src/platform/browser.rs`
- Public engine enum: `https://github.com/BoonLang/boon/blob/main/crates/boon/src/platform/browser/common.rs`
- Kernel module: `https://github.com/BoonLang/boon/blob/main/crates/boon/src/platform/browser/kernel/mod.rs`
- Kernel `LATEST` semantics: `https://github.com/BoonLang/boon/blob/main/crates/boon/src/platform/browser/kernel/semantics.rs`
- Kernel UI model: `https://github.com/BoonLang/boon/blob/main/crates/boon/src/platform/browser/kernel/ui.rs`
- Kernel runtime cells/links/lists: `https://github.com/BoonLang/boon/blob/main/crates/boon/src/platform/browser/kernel/runtime.rs`
- Counter example: `https://github.com/BoonLang/boon/blob/main/playground/frontend/src/examples/counter/counter.bn`
- TodoMVC example: `https://github.com/BoonLang/boon/blob/main/playground/frontend/src/examples/todo_mvc/todo_mvc.bn`
- Cells example: `https://github.com/BoonLang/boon/blob/main/playground/frontend/src/examples/cells/cells.bn`

## Game/runtime inspiration

- Factorio belts as grouped segments and distance-based updates: `https://www.factorio.com/blog/post/fff-176`
- Factorio sleep/wakeup and deterministic merge of wake lists: `https://factorio.com/blog/post/fff-364`
- Factorio “do less”, sleeping entities, grouped segments: `https://www.factorio.com/blog/post/fff-148`
- Factorio belt reader as aggregate read over whole lines and read-mostly parallel candidate: `https://factorio.com/blog/post/fff-421`
- Factorio circuit network pulse/hold/hold-all-belts modes: `https://wiki.factorio.com/Circuit_network`
- Shapez 2 multithreaded processing and off-screen simulation throttling: `https://store.steampowered.com/news/posts/?appids=1318690&enddate=1695136696&feed=steam_community_announcements`
- Shapez 2 simulation visualizer as debugging inspiration: `https://github.com/tobspr-games/shapez2-simulation-visualizer`
