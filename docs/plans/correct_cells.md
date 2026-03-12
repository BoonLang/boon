# Correct Cells and Zoon Parity Plan

## Status

- Historical milestone plan.
- Engine-independence constraints here are still valid.
- The active semantic-oracle direction now lives in [reference_kernel_plan.md](/home/martinkavik/repos/boon/docs/plans/reference_kernel_plan.md).
- Important clarification:
  - we still do **not** want a shared production runtime core across Actors, DD, and Wasm
  - the new internal reference kernel is only for semantics, diagnostics, and conformance tests
  - each engine still keeps its own runtime and later crate boundary

## 1. Goal

Use Cells as the forcing benchmark for a broader milestone:

- all important playground examples must work in all three engines
- all 7GUIs examples must work in all three engines
- Zoon is the only required renderer for this milestone
- `Scene/new` must work under Zoon with semantic parity
- engines stay fully independent and fully general
- no engine-specific spreadsheet evaluator is allowed

This document replaces the earlier Cells-only rescue plan. The previous SRG-only Actors rewrite, DD late-binding registry, and Wasm "11 missing functions" framing were not aligned with the current goals.

## 2. Milestone Order

### Milestone 1: Zoon parity across all engines

Required examples:

- `counter`
- `todo_mvc`
- `todo_mvc_physical`
- all 7GUIs examples
- relevant `Scene/new` playground examples

Required properties:

- correct behavior in Actors, DD, and Wasm
- fast enough for normal interaction without visible jank
- reliable enough for repeated automated runs
- monitorable enough to explain regressions

### Milestone 2: persistence

Only after Milestone 1 passes:

- keep localStorage support
- add IndexedDB support
- verify persistence on the same example set

### Milestone 3: canvas renderer

Only after persistence passes:

- add a canvas renderer crate
- match Zoon semantics before optimizing visuals

### Later: RayBox

- integrate RayBox after canvas is stable
- target `Scene/new` first

## 3. Acceptance Criteria for Milestone 1

### Examples

All of the following must work with Zoon in Actors, DD, and Wasm:

- `counter`
- `todo_mvc`
- `todo_mvc_physical`
- all 7GUIs examples

### UI semantics

- `Document/new` must work on all engines
- `Scene/new` must work on all engines under Zoon
- state, events, identity, and reactivity must match across engines
- true 3D or physical visuals are not required yet under Zoon

### Cells specifics

The short-term target is honest `26 x 30` parity across all engines. The official `26 x 100` 7GUIs size is a later expansion after the architecture is stable.

Required Cells behavior:

- visible full `A..Z` and `1..30` range
- double-click edit
- `Enter` commit
- `Escape` cancel
- formula evaluation
- dependency propagation
- range formulas
- invalid formula handling
- explicit cycle policy

## 4. Architecture Boundaries

### Shared crates are allowed for contracts only

Shared crates may define:

- syntax and lowering support
- stable ids
- scene and UI contracts
- monitor protocol
- storage API and persistence envelopes
- test fixtures and shared expectations

Shared crates must not become a shared runtime core.

### Engines remain independent

Actors, DD, and Wasm keep independent:

- schedulers
- execution models
- memory layout
- optimization strategy
- monitoring internals

The target is future compatibility with:

- multithreaded execution
- server execution
- cluster execution

## 5. Crate Split Plan

### Create now

- `boon-scene`
- `boon-monitor-protocol`
- `boon-renderer-zoon`

These crates define the first shared seams needed for Milestone 1 without forcing a whole-engine rewrite in one step.

### Create after Zoon parity

- `boon-storage-api`
- `boon-storage-localstorage`
- `boon-storage-indexeddb`

### Create after persistence

- `boon-renderer-canvas2d`

### Create later

- `boon-renderer-raybox`

## 6. Rendering Direction

### Zoon first

Zoon remains the only supported renderer in Milestone 1.

The desired boundary is:

`engine -> boon-scene -> boon-renderer-zoon`

### Shared render contract

The shared scene crate should own:

- stable node ids
- event port ids
- `RenderRoot`
- scene diffs
- renderer-owned UI facts
- UI event batches

Renderer-specific code should move out of engine crates over time. During migration, direct engine-to-Zoon code is allowed where needed, but new work should prefer the shared boundary.

## 7. Engine Directions

### Actors

- no spreadsheet-specialized evaluator
- no browser-only `thread_local` runtime assumptions in the new direction
- optimize toward a general shardable runtime with explicit dependencies, revisions, and transport boundaries
- single-threaded browser execution is only one deployment mode

### DD

- keep DD pure and explicit
- do not hide dependencies behind runtime registries or AST-eval closures
- compile reactive structure into explicit DD-visible collections and arrangements
- support early cutoff when outputs do not change

### Wasm

- move toward a standalone Wasm VM or VM-like core
- use a general value model that supports numbers, tags, text, lists, and objects
- avoid browser-only assumptions in the runtime core
- keep the host boundary narrow: DOM, events, timers, persistence, optional IO

## 8. Cells Program Guidance

Engine-specific spreadsheet fast paths are not allowed.

Program-level or stdlib-level refactors are allowed when they are general improvements, for example:

- parse formulas on commit instead of on every downstream recompute
- store formula source separately from parsed form and displayed value
- track dependencies explicitly in example state

Those changes must stay engine-agnostic.

## 9. Persistence Plan

Persistence is not part of Milestone 1 acceptance, but Milestone 1 must not block it.

The intended direction is:

- async storage API everywhere
- localStorage remains supported
- IndexedDB becomes first-class
- logical persisted state should be shareable across engines where schemas match
- engine-specific warm-start caches may live in separate versioned namespaces

## 10. Monitoring Plan

All engines should converge on a shared monitor protocol that can represent:

- revisions
- dependency edges
- queue depth
- propagation traces
- storage operations
- render diff telemetry

Monitoring must help explain both correctness bugs and performance regressions.

## 11. Validation

### Functional coverage

- static rendering checks
- interactive editing checks
- route and event checks
- `Scene/new` semantic checks
- repeated-run reliability checks

### Performance coverage

- cold render latency
- interaction latency
- fanout recompute latency
- long dependency chain latency
- DOM patch count
- runtime memory

### Commands

Use the actual browser-connected verification workflow in `tools/scripts/verify_7guis_complete.sh` or the tools workspace. Do not rely on stale root-level commands.

## 12. Research Anchors

- Excel recalculation:
  https://learn.microsoft.com/en-us/office/client-developer/excel/excel-recalculation
- Salsa revisions and verification:
  https://salsa-rs.github.io/salsa/reference/algorithm.html
- Nominal Adapton:
  https://arxiv.org/abs/1503.07792
- DBSP:
  https://arxiv.org/abs/2203.16684
- Build Systems a la Carte:
  https://simon.peytonjones.org/build-systems-a-la-carte/
- Calculation View:
  https://simon.peytonjones.org/calculation-view/
