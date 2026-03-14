# Wasm Pro Architecture Plan

> Superseded by [wasm_single_engine_cutover.md](/home/martinkavik/repos/boon/docs/plans/wasm_single_engine_cutover.md).
> Keep this file as implementation history for the parallel-backend migration phase.

**Status:** Proposed redesign direction
**Date:** 2026-03-14
**Audience:** Boon compiler/runtime maintainers

## Summary

- Build a new Wasm backend, "Wasm Pro", that produces materially smaller and faster browser modules than the current `engine_wasm`.
- Keep the three engines independent. This plan does not introduce a shared production runtime.
- Keep the reference kernel as the semantic oracle and rollout gate. Wasm Pro is allowed to move fast only after kernel-backed semantics are locked.
- Move Boon semantics, list/template execution, and render diffing into Wasm.
- Keep the host mechanical: forward UI events and facts into Wasm, then apply renderer-agnostic diffs coming out of Wasm.
- Use `docs/plans/wasm_pro_execution_backlog.md` as the concrete implementation backlog for this architecture.

## Why The Current Wasm Engine Does Not Scale

The current pipeline in `crates/boon/src/platform/browser/engine_wasm/` is:

1. parse source
2. lower AST into `IrProgram`
3. emit a raw Wasm module with `wasm-encoder`
4. instantiate that module in the host runtime
5. rebuild UI semantics in the host bridge

This is only partial compilation.

The current engine still pays for:

- one mutable Wasm global per cell
- large event and `set_global` dispatch functions
- per-item entrypoints such as `init_item`, `on_item_event`, and `refresh_item`
- host-side text and list storage
- host-side item context switching
- bridge-side list/template reconstruction
- bridge-side DOM/render-tree building and event routing

That split is the main reason `cells` is both large and slow. `cells` combines nested `ListMap`, per-item events, display/edit switching, repeated object fields, and spreadsheet-style fanout. The current architecture duplicates those semantics across lowering, Wasm codegen, runtime stores, and bridge logic.

## Goals

- Generate smaller `.wasm` modules for dynamic UI programs, especially `cells`.
- Reduce imports and exports to a tiny stable ABI.
- Remove bridge-time semantic reconstruction.
- Make the Wasm backend renderer-agnostic so it can feed Zoon today and Canvas Pi / wgpu-style renderers later through shared scene contracts.
- Keep `wasm32-unknown-unknown` as the browser target.
- Preserve engine independence and avoid coupling Wasm Pro to DD or Actors runtime code.

## Non-Goals

- No DOM-only ABI.
- No new shared runtime used by all engines in production.
- No dependency on Wasm GC for phase 1.
- No requirement to rewrite the current engine in place.
- No Cells-specific fallback evaluator.

## Core Design

### 1. Wasm Owns Semantics

Wasm Pro owns:

- reactive state
- event ordering
- `HOLD`, `LATEST`, `WHEN`, `WHILE`, `THEN`, `LINK`
- list identity and keyed item lifecycle
- template instantiation and per-item state
- render shadow state
- diff generation

The host owns only:

- module instantiation
- event collection
- fact collection
- timer/navigation plumbing
- persistence I/O
- renderer adapter application of diffs

The host must not:

- rebuild object or list semantics
- seed template-local cells by inference
- rescan IR to find template roots or field maps
- decide item-local vs global event routing
- reconstruct render structure from high-level Boon objects

### 2. Renderer-Agnostic Output

The target output is not DOM patch opcodes.

Wasm Pro emits renderer-neutral render diffs aligned with `boon-scene`. Zoon is the first adapter, but the same output must also be consumable by future Canvas Pi and wgpu-based renderers.

This requires extending the current `boon-scene` surface from a minimal `SceneDiff` enum into a richer diff protocol that can represent:

- root replacement
- keyed child insertion, removal, and move
- node text updates
- node property/style/class updates
- event-port attachment and detachment
- input/selection/checked state updates
- scene parameter updates
- document and scene surfaces through one protocol

### 3. Data-Oriented Wasm Runtime

Replace the current one-global-per-cell model with dense linear-memory stores:

- `values[]` for slot payloads
- `versions[]` for change tracking
- `deps_index[]` and `deps_data[]` for dependency edges
- `ops[]` for operator records
- `dirty_queue[]` for propagation work
- `string_pool` for interned literals and dynamic string handles
- typed arenas for objects, lists, and item frames
- `render_nodes[]` and `render_regions[]` for shadow render state
- `event_ports[]` for browser-facing event bindings

Compile operator instances into data records, not duplicated control-flow branches. Code size should scale mainly with operator kinds, not with every cell and item instance.

### 4. Parameterized List Templates

`ListMap` must compile once into:

- a template program
- a fixed item-frame layout
- keyed item lifecycle hooks in the runtime

Per-item execution becomes data plus one shared program, not one exported entrypoint family plus repeated host workspace setup.

The target design removes the production need for:

- `init_item`
- `on_item_event`
- `refresh_item`
- host item context globals
- bridge-managed per-item cell seeding

### 5. Ownership-Driven Memory

Phase 1 memory strategy is explicit ownership plus typed arenas, not tracing GC.

Use deterministic subtree drop for:

- removed list items
- inactive `WHILE` branches
- replaced object/list roots
- render subtrees that leave the active graph

This matches Boon's existing ownership narrative well enough to avoid a general-purpose collector in the first version. Wasm GC can be reconsidered later, but it is not a prerequisite for Wasm Pro.

## Compiler Pipeline

Wasm Pro uses three internal layers.

### Semantic IR

Lower the parser AST into a typed semantic IR that preserves:

- slot types and value shapes
- object field layout
- list item layout
- template regions
- event ports and payload shapes
- render regions
- ownership/drop boundaries

This IR is where kernel-backed semantics must line up.

### Execution IR

Lower semantic IR into a compact execution IR that is close to the runtime layout:

- dense slot ids
- operator tables
- dependency tables
- item-frame layouts
- render region plans
- string and symbol intern tables

This IR should be serializable and inspectable in tests.

### Wasm Emission

Emit Wasm from execution IR with a small set of runtime helpers and structured loops. Avoid generating giant `br_table` dispatches proportional to cell count where table-driven data will do the job better.

## Stable ABI

The stable release ABI should be export-driven and small:

- `memory`
- `init()`
- `dispatch_events(ptr, len) -> u64`
- `apply_facts(ptr, len) -> u64`
- `take_commands() -> u64`

Optional only if needed for inbound variable-size data:

- `alloc(len) -> ptr`
- `free(ptr, len)`

Returned `u64` values are `(ptr << 32) | len` descriptors into linear memory.

Steady-state semantic host imports should be eliminated. If any remain temporarily during migration, they must be treated as transitional debt and removed before Wasm Pro becomes the default backend.

Debug-only exports are allowed behind feature flags, for example:

- IR dumps
- slot snapshots
- render-shadow dumps
- trace hooks

These must not be part of the stable production ABI.

## Scene Protocol Changes

Extend `crates/boon-scene` into the stable renderer boundary for Wasm Pro.

Add a diff batch API centered on:

- `RenderDiffBatch`
- `RenderOp`
- existing `UiEventBatch`
- existing `UiFactBatch`

`RenderOp` should cover at least:

- `ReplaceRoot`
- `InsertChild`
- `RemoveNode`
- `MoveChild`
- `SetText`
- `SetProperty`
- `SetStyle`
- `SetClassFlag`
- `AttachEventPort`
- `DetachEventPort`
- `SetInputValue`
- `SetChecked`
- `SetSelectedIndex`
- `UpdateSceneParam`

Keep snapshot-style types such as `RenderRoot`, `UiNode`, and `SceneNode` for full-replace, debug, and test flows, but make diff batches the standard incremental path.

## Renderer Adapters

Renderer adapters consume `RenderDiffBatch` plus event/fact batch contracts.

### Zoon adapter

- converts render diffs into the existing DOM-facing behavior
- remains mechanical
- does not infer semantics from values or object shapes

### Canvas Pi adapter

- consumes the same render diff protocol
- uses fact batches for hover, focus, draft text, and layout information where needed

### Future wgpu adapter

- consumes the same render diff protocol
- may ignore document-only ops that do not apply to scene rendering

The protocol must allow renderers to advertise capabilities, but the compiler/runtime boundary stays the same.

## Toolchain Strategy

Binaryen is a required release-stage optimization tool, not a mandatory phase-1 codegen library dependency.

### Dev profile

- direct Wasm emission
- fast compile/instantiate loop
- no mandatory heavy post-pass

### Release-small profile

- strip names and debug metadata
- metadata DCE
- `wasm-opt -Oz`

### Release-fast profile

- dead-code elimination
- `wasm-opt -O3`

### Optional later work

- switch to Binaryen IR emission if the direct emitter blocks optimization quality
- split a reusable runtime prelude and merge it with program-specific code only if it reduces total shipped size in practice

Lessons to adopt from other Wasm-first languages and toolchains:

- keep the runtime inside the module instead of recreating semantics in the host
- keep the host ABI narrow
- treat release size optimization as a standard pipeline stage

Binaryen is the relevant optimizer toolchain. Grain and AssemblyScript are useful references for packaging runtime support into Wasm modules and for keeping release size as an explicit compiler concern.

## Migration Plan

### Phase 1: Semantic lock

- keep following `reference_kernel_plan.md`
- finish kernel-backed conformance for:
  - `HOLD`
  - `LATEST`
  - `WHEN`
  - `WHILE`
  - `LINK`
  - list identity
  - deterministic event ordering
  - UI event and fact handling

Wasm Pro does not become the main path before this is green.

### Phase 2: Renderer protocol first

- extend `boon-scene`
- add binary encoding/decoding tests for render diffs
- add a fake renderer for deterministic diff assertions
- add a first Zoon adapter against the new protocol

### Phase 3: Parallel backend bring-up

Implement Wasm Pro beside the current `engine_wasm`, not as a risky in-place rewrite.

First bring-up scope:

- `counter`
- `LATEST`
- simple document rendering
- event round-trip through the new ABI

### Phase 4: Lists and templates

- keyed item frames
- template execution in Wasm
- render diff emission for list updates
- subtree drop/reuse
- `todo_mvc` parity

### Phase 5: `cells` as the stress gate

Use `cells` as the structural benchmark and parity gate:

- first `26 x 30`
- then `26 x 100`
- no Cells-specific fallback
- correct edit/display semantics
- correct dependent recomputation
- materially smaller module than the current Wasm engine
- materially fewer imports/exports than the current Wasm engine
- materially lower instantiate and interaction latency than the current Wasm engine

### Phase 6: Cutover and deletion

After parity and benchmarks pass:

- make Wasm Pro the preferred Wasm backend
- delete bridge-time template reconstruction for production Wasm execution
- delete per-item export churn
- delete import-heavy helper ABI kept only for the old engine

## Explicit Deletions From The Old Design

Wasm Pro should delete these current architectural patterns from the production Wasm path:

- one global per cell
- host-side text/list/item stores as semantic truth
- IR rescanning in the renderer bridge
- item context imports for per-item reads
- per-item Wasm exports
- compile-time generation of huge dispatch bodies proportional to cell count

## Testing And Acceptance

### Semantic tests

- kernel conformance for core operators and event ordering
- focused lowering/execution tests for typed semantic IR and execution IR

### Render protocol tests

- batch encoding/decoding
- fake renderer golden tests
- keyed insert/move/remove correctness
- focus/input/layout fact round-trips

### Browser tests

- Zoon adapter smoke tests for document and scene surfaces
- `todo_mvc` expected flow
- `cells` expected flow

### Benchmarks

Track current Wasm engine vs Wasm Pro on:

- `.wasm` byte size
- import count
- export count
- instantiation time
- first render time
- event latency
- `cells` edit latency

The intended implementation path is a stable tooling command, not ad hoc test output scraping.
For the current repo shape that means a dedicated metrics entry point such as:

- `cargo run --manifest-path tools/Cargo.toml -- metrics cells-backend --json`
- `cargo run --manifest-path tools/Cargo.toml -- metrics cells-backend --check`

Success means Wasm Pro wins on those metrics for `cells`, not just on tiny examples.

## Relationship To Existing Plans

- `reference_kernel_plan.md` remains the semantic source of truth and rollout gate.
- `wasm/WASM_ENGINE_ARCHITECTURE.md` remains the best description of the current engine.
- `wasm_cells_performance_plan.md` remains useful as rescue history, but it is not the main forward architecture.
- This file is the redesign memo for the next Wasm architecture.

## Open Risks

- `boon-scene` is currently too small to serve as the full renderer boundary and will need a deliberate v2 expansion.
- The first version of the diff protocol must not overfit DOM rendering if Canvas Pi and scene rendering are first-class goals.
- A direct emitter plus `wasm-opt` may be sufficient, but if the generated Wasm shape stays hostile to optimization, switching the backend to Binaryen IR generation may become necessary.
- The fixed runtime core may make tiny programs only slightly smaller, even while making `cells` and similar programs much better.

## Default Decisions

- browser target: `wasm32-unknown-unknown`
- renderer boundary: renderer-agnostic scene/render diffs
- semantic gate: reference kernel first
- phase-1 memory strategy: typed arenas plus ownership-driven subtree drop
- optimization toolchain: Binaryen release pipeline
- rollout model: parallel backend, then cutover
- during cutover, prefer `WasmPro` over legacy `Wasm` in Wasm-family engine selection before deletion
- during cutover, user-facing pickers should stop advertising legacy `Wasm` as the normal Wasm choice when `WasmPro` is compiled, while explicit fallback/debug paths may remain temporarily available
