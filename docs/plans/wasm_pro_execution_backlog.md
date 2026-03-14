# Wasm Pro Execution Backlog

> Superseded by [wasm_single_engine_cutover.md](/home/martinkavik/repos/boon/docs/plans/wasm_single_engine_cutover.md).
> Keep this file as implementation history for the parallel-backend migration phase.

**Status:** Ready for implementation
**Date:** 2026-03-14
**Depends on:** `wasm_pro.md`, `reference_kernel_plan.md`

## Summary

This is the execution backlog for Wasm Pro.

It translates the architecture in `docs/plans/wasm_pro.md` into concrete implementation slices with:

- target files and modules
- required outputs
- acceptance checks
- explicit sequencing

Execution rule:

- do not try to rescue the current `engine_wasm` into the final architecture
- build Wasm Pro as a parallel backend first
- do not let renderer adapters recover semantics that should live in Wasm

## Proposed Code Layout

Add a parallel backend:

```text
crates/boon/src/platform/browser/
├── engine_wasm/          # current backend, kept during migration
├── engine_wasm_pro/      # new backend
│   ├── mod.rs
│   ├── semantic_ir.rs
│   ├── lower.rs
│   ├── exec_ir.rs
│   ├── codegen.rs
│   ├── runtime.rs
│   ├── abi.rs
│   └── debug.rs
```

Planned shared contract changes:

```text
crates/boon-scene/src/lib.rs
crates/boon-renderer-zoon/src/lib.rs
tools/
playground/
```

## Milestone 0: Scaffolding and Feature Gating

### Goal

Create the backend shell without changing the current Wasm path.

### Files

- `crates/boon/src/platform/browser.rs`
- `crates/boon/src/platform/browser/api.rs`
- `crates/boon/src/platform/browser/mod.rs` if needed
- `crates/boon/src/platform/browser/engine_wasm_pro/mod.rs`
- `crates/boon/Cargo.toml`
- `playground/frontend/src/main.rs`
- tooling files that expose engine selection

### Deliverables

- `engine_wasm_pro` feature flag
- backend module skeleton
- engine selection wiring for playground and tools
- no fallback from Wasm Pro to other engines

### Acceptance

- project compiles with the feature enabled
- selecting Wasm Pro reaches a deliberate "not implemented" backend result, not fallback
- existing Wasm engine behavior remains unchanged

## Milestone 1: `boon-scene` v2 Renderer Protocol

### Goal

Define the renderer-neutral boundary before building the backend runtime.

### Files

- `crates/boon-scene/src/lib.rs`
- `crates/boon-renderer-zoon/src/lib.rs`
- new tests in `crates/boon-scene`

### Deliverables

- add `RenderDiffBatch`
- add `RenderOp`
- keep existing snapshot types (`RenderRoot`, `UiNode`, `SceneNode`)
- preserve `UiEventBatch` and `UiFactBatch`
- support both document and scene surfaces

Minimum `RenderOp` set for phase 1:

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

### Acceptance

- contract types compile and serialize cleanly
- unit tests cover encode/decode round-trips
- unit tests cover keyed insert/move/remove ordering
- protocol is renderer-neutral and does not mention DOM-only concepts

## Milestone 2: Fake Renderer and Zoon Adapter Boundary

### Goal

Prove the new renderer protocol before any full backend work.

### Files

- `crates/boon-renderer-zoon/src/lib.rs`
- new fake-renderer test module under `crates/boon-renderer-zoon` or `crates/boon`
- browser adapter glue files if a separate adapter module is needed

### Deliverables

- fake renderer that applies `RenderDiffBatch` to an in-memory tree
- Zoon adapter path that can consume diff batches mechanically
- event-port registry model aligned with `UiEventBatch`
- fact ingestion path aligned with `UiFactBatch`

### Acceptance

- fake-renderer golden tests pass
- Zoon adapter smoke tests can render simple root replacement and text updates
- adapter code does not inspect Boon values or infer object/list structure

## Milestone 3: Batch Codec and ABI Utilities

### Goal

Lock the binary boundary before implementing semantics.

### Files

- `crates/boon/src/platform/browser/engine_wasm_pro/abi.rs`
- `crates/boon/src/platform/browser/engine_wasm_pro/runtime.rs`
- tests in the same module tree

### Deliverables

- memory buffer format for:
  - `UiEventBatch`
  - `UiFactBatch`
  - `RenderDiffBatch`
- helper functions for `(ptr << 32) | len` descriptors
- host-side readers/writers for the Wasm Pro ABI

Stable ABI target:

- `memory`
- `init()`
- `dispatch_events(ptr, len) -> u64`
- `apply_facts(ptr, len) -> u64`
- `take_commands() -> u64`

Optional only if required:

- `alloc(len) -> ptr`
- `free(ptr, len)`

### Acceptance

- codec round-trips are deterministic
- malformed buffers fail cleanly in debug/test paths
- no host import surface is introduced for semantic work

## Milestone 4: Semantic IR

### Goal

Lower AST into a typed Wasm Pro semantic IR aligned with the reference kernel.

### Files

- `crates/boon/src/platform/browser/engine_wasm_pro/semantic_ir.rs`
- `crates/boon/src/platform/browser/engine_wasm_pro/lower.rs`
- tests near those files

### Deliverables

- typed slot/value representations
- object field layout
- list and template regions
- event ports and payload definitions
- render regions
- ownership/drop boundaries

First covered Boon constructs:

- constants
- `HOLD`
- `THEN`
- `LATEST`
- `WHEN`
- `WHILE`
- `LINK`
- simple document roots

### Acceptance

- lowering tests are stable and human-readable
- shape is preserved without bridge-time repair rules
- kernel-backed conformance tests pass for the covered operators

## Milestone 5: Execution IR

### Goal

Convert semantic IR into a compact runtime-oriented IR.

### Files

- `crates/boon/src/platform/browser/engine_wasm_pro/exec_ir.rs`
- `crates/boon/src/platform/browser/engine_wasm_pro/lower.rs`
- tests near those files

### Deliverables

- dense slot ids
- operator tables
- dependency tables
- render region plans
- string intern tables
- event-port table
- first item-frame layout support, even if list execution is not enabled yet

### Acceptance

- execution IR is serializable or snapshot-friendly in tests
- no one-node-one-function expansion pattern reappears
- dependency ordering is explicit and testable

## Milestone 6: Minimal Wasm Pro Runtime and Codegen

### Goal

Bring up a working module for the smallest useful examples.

### Files

- `crates/boon/src/platform/browser/engine_wasm_pro/codegen.rs`
- `crates/boon/src/platform/browser/engine_wasm_pro/runtime.rs`
- `crates/boon/src/platform/browser/engine_wasm_pro/mod.rs`

### Deliverables

- codegen from execution IR to Wasm
- linear-memory slot stores
- dirty queue / propagation loop
- render-shadow state sufficient for root replacement and text updates
- ABI exports working end-to-end

### Acceptance

- `counter` works through Wasm Pro
- a simple `LATEST` example works through Wasm Pro
- a simple document with button, label, and text update renders through the Zoon adapter
- no per-item exports
- no one-global-per-cell layout

## Milestone 7: Event and Fact Round-Trip

### Goal

Make browser-driven updates work through the new ABI.

### Files

- `crates/boon/src/platform/browser/engine_wasm_pro/runtime.rs`
- `playground/`
- `tools/`

### Deliverables

- browser events forwarded as `UiEventBatch`
- focus/hover/input/layout facts forwarded as `UiFactBatch`
- deterministic event ordering through the runtime
- batch-triggered render diffs returned after each dispatch

### Acceptance

- focused smoke tests for click, input, blur, and focus
- no fallback host-side event routing by template inference
- event port ids remain stable across incremental updates

## Milestone 8: Lists, Templates, and Keyed Item Frames

### Goal

Move list and template semantics into Wasm.

### Files

- `crates/boon/src/platform/browser/engine_wasm_pro/semantic_ir.rs`
- `crates/boon/src/platform/browser/engine_wasm_pro/exec_ir.rs`
- `crates/boon/src/platform/browser/engine_wasm_pro/runtime.rs`
- `crates/boon/src/platform/browser/engine_wasm_pro/codegen.rs`

### Deliverables

- keyed item frame layout
- template program compiled once
- keyed item insert/remove/move support
- deterministic subtree drop/reuse
- list-driven render region diffs

### Acceptance

- `ListMap` works without bridge-side item seeding
- keyed reorder tests pass
- no `init_item`, `on_item_event`, or `refresh_item` exports exist

## Milestone 9: TodoMVC Parity

### Goal

Reach the first realistic end-to-end app on Wasm Pro.

### Files

- Wasm Pro backend files
- `tools/`
- `playground/frontend/src/examples/todo_mvc*`

### Deliverables

- full basic TodoMVC interaction path
- adapter integration in playground tooling
- parity tests for editing, filtering, counts, clear completed, and persistence hooks if enabled

### Acceptance

- TodoMVC smoke passes
- no bridge-time semantic reconstruction is added to get parity

## Milestone 10: `cells` Bring-Up at 26x30

### Goal

Prove that the new architecture actually handles the structural stress case.

### Files

- Wasm Pro backend files
- `playground/frontend/src/examples/cells/*`
- `tools/`

### Deliverables

- nested list/template execution
- display/edit mode switching
- correct event routing through event ports
- stable identity for visible cells and editors

### Acceptance

- `cells` at `26 x 30` passes interaction semantics
- no host-side structure recovery is reintroduced
- module size and export/import counts are already better than the current Wasm engine

## Milestone 11: `cells` Scale Gate at 26x100

### Goal

Validate that Wasm Pro solves the current scale problem, not just correctness.

### Deliverables

- benchmark harness for current Wasm vs Wasm Pro
- stable command/report path for benchmark capture, e.g. `boon-tools metrics cells-backend`
- metrics capture for:
  - `.wasm` byte size
  - import count
  - export count
  - instantiation time
  - first render time
  - edit latency
  - dependent recomputation latency

### Acceptance

- `cells` at `26 x 100` is correct
- Wasm Pro is materially smaller
- Wasm Pro is materially faster on instantiate + first render + edit path

## Milestone 12: Cutover and Deletion

### Goal

Switch the preferred Wasm path and remove obsolete architecture.

### Files

- `crates/boon/src/platform/browser/engine_wasm/*`
- backend selection wiring
- docs

### Deliverables

- Wasm Pro becomes the preferred Wasm backend
- Wasm Pro becomes the preferred Wasm-family engine in backend selection wiring before deletion
- legacy `Wasm` is demoted in user-facing selection UX while explicit fallback access remains available during cutover
- old import-heavy helper ABI is deleted from the production Wasm path
- old bridge-time list/template reconstruction is deleted from the production Wasm path
- docs are updated to demote the old architecture to legacy status

### Acceptance

- current Wasm bridge/runtime rescue logic is no longer the active direction
- docs point new work to Wasm Pro

## Fast Inner Loop

During implementation prefer this loop:

1. semantic IR tests
2. execution IR tests
3. fake-renderer diff tests
4. Wasm Pro harness tests without the browser
5. browser smoke tests
6. full expected tests only after a milestone is coherent

Do not use the full browser-driven `cells` flow as the default inner loop before the corresponding milestone is structurally complete.

## Things That Must Not Reappear

- one mutable Wasm global per cell
- host-side semantic truth for lists, text, or items
- bridge-time discovery of template roots or field maps
- per-item Wasm exports
- import-heavy helper ABI for semantic operations
- renderer adapters that infer Boon object semantics

## First Recommended Coding Sequence

If implementation starts now, do this order:

1. scaffold `engine_wasm_pro`
2. extend `boon-scene` with `RenderDiffBatch` and `RenderOp`
3. build fake-renderer tests
4. add ABI codec helpers
5. implement semantic IR for `counter`, `LATEST`, and simple document roots
6. emit first working Wasm Pro module

That sequence keeps risk low and exposes architecture mistakes early.
