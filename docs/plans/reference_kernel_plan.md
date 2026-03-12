# Reference Kernel Plan

## Status

- Active direction as of 2026-03-13.
- This plan replaces the old assumption that full Cells parity should be reached by continuing separate tactical rescue work inside the current Wasm, DD, and Actors implementations.
- Phase 1 foundation has started:
  - internal reference-kernel ids
  - deterministic runtime state containers
  - renderer-owned UI event state
  - reference `LATEST` semantics
  - reference `HOLD` change-tracking semantics
  - reference `LINK` binding/read semantics
  - reference list item key/scope stability semantics
- Phase 2 has a first concrete foothold:
  - DD worker now has a small `LATEST` press-sequence conformance test that checks engine output against the reference-kernel selector
  - Wasm lowering now has a focused `LATEST` press-sequence conformance test that checks lowered arm structure against the same reference-kernel selector
  - Actors evaluator now has a focused `LATEST` press-sequence conformance test that verifies the parsed/evaluator shape keeps two distinct piped `THEN` arms aligned with the same reference-kernel selector
  - Wasm and Actors now also have focused `HOLD` conformance tests that keep the initial state and `THEN` body shape aligned with the reference-kernel `HOLD` expectations
  - Wasm and Actors now also have focused `LINK` conformance tests that keep link placeholder/event-path structure aligned with the reference-kernel `LINK` expectations
  - kernel, Actors, and Wasm now have initial focused list-identity footholds that lock stable item keys/scopes or stable list-map item-store tracking
  - Wasm bridge/runtime `cells` regressions now also have focused row-template and edit-activation coverage for nested `row_data -> cells` binding/materialization
  - Wasm `cells` runtime now also has a structural sparse per-item storage fix:
    - `ItemCellStore` no longer allocates a dense vector up to the highest list memory index
    - persistence now iterates explicit item indices instead of assuming dense item slots
    - this directly targets the live edit-path `ItemCellStore::ensure_item` OOMs seen after A1 double-click
  - browser automation now also has a targeted double-click fix in `tools/extension/background.js`:
    - `DoubleClickAt` dispatches an explicit DOM double-click sequence on the hit element instead of relying only on CDP click-count synthesis
  - Wasm `cells` now also has a codegen-side nested-template isolation fix:
    - `init_item` and `refresh_item` now skip nested render-template ranges instead of eagerly initializing inner cell-template nodes inside outer row item contexts
    - this removed the live outer-row pollution where row items carried bogus nested `cell.row` / `cell.column` / `is_editing` state
    - focused guardrail: `cells_outer_row_init_excludes_nested_cell_template_nodes` now passes under `--features engine-wasm`

## Core Decision

- The three engines must remain independent and later live in separate crates.
- Because of that, this plan does **not** introduce a shared production runtime.
- Instead, add a small internal reference kernel inside `crates/boon` that acts only as:
  - a semantic oracle
  - a diagnostics model
  - a conformance-test target
- Each engine keeps its own execution model and implementation, but must match the reference semantics.

## Why This Direction

- Current failures are similar across engines, but the implementations fail in different ways:
  - Wasm still reconstructs object/list/template structure in the bridge at runtime.
  - DD still falls back to evaluator-style closures and broad reprocessing for dynamic UI workloads.
  - Actors still pays runtime wiring and per-event allocation costs that scale poorly for `cells`.
- Replacing them with one shared runtime would violate the engine-independence goal.
- Keeping the current engine-specific rescue loops as the main path is too slow and too hard to reason about.
- A reference kernel gives one place to define and test semantics without coupling the engines together.

## Scope

### Reference kernel responsibilities

- stable ids:
  - `ExprId`
  - `SourceId`
  - `ScopeId`
  - `SlotKey`
- deterministic tick/update ordering
- `HOLD`, `LINK`, `LIST`, `LATEST`, `SKIP` reference semantics
- UI event-port and element-identity model
- per-slot diagnostics:
  - value
  - `last_changed`
  - dependency edges
  - triggering event

### Non-goals

- no engine depends on the reference kernel at runtime in production
- no shared scheduler
- no shared memory layout
- no shared renderer implementation
- no engine-specific fast path for `cells`

## Execution Order

### Phase 1: Build the oracle

- Add the internal reference-kernel module and semantics tests.
- Make it pure and small enough that it can later move into a dedicated test/support crate if needed.
- Use it to lock down semantics for:
  - `HOLD`
  - `LINK`
  - `LATEST`
  - list item identity
  - deterministic event ordering

### Phase 2: Use it to drive parity

- Add conformance tests that run against:
  - reference kernel
  - Actors
  - DD
  - Wasm
- Current conformance footholds:
  - Actors:
    - evaluator-level `LATEST` press-sequence shape test is landed and verified under `--features engine-actors`
    - evaluator-level `HOLD` press-sequence shape test is landed and verified under `--features engine-actors`
    - evaluator-level `LINK` press-path shape test is landed and verified under `--features engine-actors`
    - list-item scope-id stability tests are landed and verified under `--features engine-actors`
  - DD:
    - worker-level `LATEST` press-sequence test is landed in code
    - worker-level `HOLD` press-sequence test is now also landed in code
    - full targeted test execution is still limited by the heavy `boon` lib-test compile path
  - Wasm:
    - lowering-level `LATEST` press-sequence test is landed and verified under `--features engine-wasm`
    - lowering-level `HOLD` press-sequence test is landed and verified under `--features engine-wasm`
    - lowering-level `LINK` rebinding test is landed and verified under `--features engine-wasm`
    - list-map item-store tracking regression is landed and verified under `--features engine-wasm`
- Start with:
  - `counter`
  - `todo_mvc`
  - `cells` at `26 x 30`
- Only once semantics are stable should `26 x 100` become a required scale gate.

### Phase 3: Rework each engine independently

  - Wasm:
    - stop adding bridge-time reconstruction heuristics
    - current concrete `cells` bridge/runtime state:
      - first-row item binding, row-number resolution, and first-row list materialization regressions are fixed
      - global editing-state aliases now resolve from live `CellStore` values for alias-like cells
      - nested `row_data -> cells` field reconstruction now preserves distinct inner field cells instead of collapsing later cell items onto A1-style bindings
      - live browser state is narrower but still not at parity:
        - clean Wasm `cells` still currently fails the first browser assertion with `expected cell (1, 1) to be '5', got '.'`
        - a nested-row buffered render experiment briefly moved the browser spec forward to the double-click step, but it triggered edit-path `ItemCellStore::ensure_item` borrow/OOM failures and was reverted
        - next Wasm work should therefore avoid duplicate or buffered nested child construction during render/event re-entry and instead stabilize nested selector/input state without introducing extra row-tree builds
      - focused Wasm regressions now pass for:
        - first-row cells-list materialization
        - first-cell seed formula/value resolution
        - `is_editing` activation only for A1 when `editing_cell = { row: 1, column: 1 }`
      - active-root switching only for the first cell under the same edit state
      - latest live checkpoint after the nested-template init isolation fix:
        - fresh Wasm `cells` render is clean again:
          - no initial stray inputs
          - outer row items no longer carry bogus nested `is_editing` state
        - newer focused bridge regression is also green:
          - outer row template seeding now populates `row_data.cells.row` / `row_data.cells.column` with real per-row coordinates before Wasm event handling
        - the remaining live blocker is narrower:
          - the strengthened Wasm browser spec now gets past initial render and fails at `Double-click A1 enters edit mode with current value`
          - actual failure: focused element remains `DIV` instead of a text input
        - current operational blocker in this turn:
          - after the latest rebuild, the browser page stopped reaching `API Ready`, so the fresh live rerun is pending even though the Rust-side seeding regression is green
        - next Wasm work should therefore resume from the live edit-entry path once the page is booting again, not the old nested selector pollution
  - redesign the runtime boundary so object/list/template structure is explicit
  - return later as an independent crate/backend
- DD:
  - reduce evaluator/fallback closure dependence
  - make list/item reactivity explicit instead of heuristic
  - keep DD-specific execution, not shared runtime code
- Actors:
  - reduce event allocation and dynamic wiring costs
  - improve list/item identity and scope stability
  - keep the actor model independent

## Cells-Specific Direction

- Keep `cells.bn` engine-agnostic.
- Do not add engine-specific spreadsheet evaluators.
- Short-term acceptance target:
  - `26 x 30`
  - exact edit semantics
  - exact dependent recomputation
- Later scale target:
  - `26 x 100`
  - acceptable interaction latency
- General program-level improvements remain allowed if they help all engines, for example:
  - parse formulas on commit
  - store parsed formula state separately from displayed value
  - track explicit formula dependencies

## External References

- Excel recalculation:
  - <https://learn.microsoft.com/en-us/office/client-developer/excel/excel-recalculation>
- Salsa algorithm:
  - <https://salsa-rs.github.io/salsa/reference/algorithm.html>
- Nominal Adapton:
  - <https://arxiv.org/abs/1503.07792>
- Grain CLI:
  - <https://grain-lang.org/docs/tooling/grain_cli/>
- AssemblyScript runtime:
  - <https://www.assemblyscript.org/runtime.html>
- Binaryen:
  - <https://github.com/WebAssembly/binaryen>

## Acceptance

- The reference kernel is green on its own semantics suite.
- Each engine matches the same semantics suite without depending on the reference kernel at runtime.
- The active plan files point future work toward this document instead of the older engine-specific rescue loop.
