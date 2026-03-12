# Zoon Parity Tasks

## Status

- Active milestone tracker, but the engine direction changed on 2026-03-13.
- New semantic-oracle work follows [reference_kernel_plan.md](/home/martinkavik/repos/boon/docs/plans/reference_kernel_plan.md).
- The three engines remain independent and later belong in separate crates.
- The reference kernel is for conformance, diagnostics, and tests only. It is not a shared production runtime.

## Current Completed Base

- internal reference-kernel foundations now exist for semantic-oracle work:
  - stable ids
  - deterministic runtime update ordering
  - renderer-owned UI event state
  - reference `LATEST` selection semantics
  - reference `HOLD` change-tracking semantics
  - reference `LINK` binding/read semantics
  - reference list item key/scope stability semantics
- first engine-facing semantic-oracle footholds now exist:
  - Actors evaluator-level `LATEST` conformance test landed and passes under `--features engine-actors`
  - Actors evaluator-level `HOLD` conformance test landed and passes under `--features engine-actors`
  - Actors evaluator-level `LINK` conformance test landed and passes under `--features engine-actors`
  - Actors list-item scope-id stability tests landed and pass under `--features engine-actors`
  - DD worker-level `LATEST` conformance test landed in code
  - DD worker-level `HOLD` conformance test landed in code
  - Wasm lowering-level `LATEST` conformance test landed and passes under `--features engine-wasm`
  - Wasm lowering-level `HOLD` conformance test landed and passes under `--features engine-wasm`
- Wasm lowering-level `LINK` conformance test landed and passes under `--features engine-wasm`
- Wasm list-map item-store tracking regression passes under `--features engine-wasm`
- Wasm bridge/runtime `cells` regressions now pass for:
  - first-row nested `row_data -> cells` list materialization
  - first-cell seed formula/value resolution
  - `is_editing` activation only for the first cell when editing `A1`
  - active-root switching only for the first cell under the same edit state
- Wasm `cells` now also has two broader structural fixes landed:
  - sparse `ItemCellStore` storage keyed by item memory index, replacing the old dense vector growth that caused edit-path `ensure_item` OOMs
  - extension-side `DoubleClickAt` now dispatches an explicit DOM double-click sequence on the hit element instead of relying only on CDP click-count synthesis
- Wasm `cells` now also has a codegen-side nested-template isolation fix:
  - outer row `init_item` / `refresh_item` no longer initialize nested inner cell-template nodes in the wrong item context
  - fresh live Wasm `cells` render is clean again with no initial stray inputs
  - focused codegen regression `cells_outer_row_init_excludes_nested_cell_template_nodes` now passes under `--features engine-wasm`
- shared crates exist for scene, monitor protocol, and Zoon renderer
- shared renderer placeholders are used by Actors, DD, and Wasm bridges
- `RenderSurface` is preserved in Actors bridge selection, DD compiled output, and Wasm IR
- shared `RenderRootHandle` is used by Actors, DD, and Wasm bridge entrypoints
- shared physical scene defaults live in `boon-scene` and are used by Actors and DD
- shared renderer seam has unit tests for Zoon render-mode mapping and capabilities
- shared render roots can now carry optional `lights` and `geometry` handles for scene outputs
- Wasm lowering preserves `Scene/new` lights and geometry handles in IR metadata
- DD static and templated compilation preserves a distinct `SceneNew` wrapper instead of flattening to root only
- DD reactive pipe compilation now recognizes `... |> Scene/new()` in the same fast-path family as `Document/new()`
- DD light constructors now compile to tagged values instead of `Unit`, so scene lights survive static and reactive document compilation
- Actors scene fallback now derives directional intensity, ambient intensity, spread, azimuth, altitude, and bevel angle from the runtime scene object
- DD documents now expose live `lights` and `geometry` mutables through `RenderRootHandle`
- Wasm scene-root metadata is verified under `--features engine-wasm`
- Wasm `Light/directional`, `Light/ambient`, and `Light/spot` now lower to tagged IR values instead of `Void`
- Wasm physical CSS now derives bevel angle and shadow parameters from the active `Scene/new` geometry and lights when those values can be resolved from current IR or cell state
- DD compiler tests now cover both static `Scene/new` light preservation and reactive scene root mapping
- DD renderer now has a unit test for `derive_scene_params`
- Wasm bridge now has a unit test for `resolve_scene_params`
- DD and Wasm now both have unit tests that scene params affect concrete CSS helper output
- Wasm bridge now collects scene dependency cells and maintains browser-side `scene_params` state for physical CSS
- Wasm bridge now has a local regression test that `refresh_scene_params` updates shared scene state after dependency-cell changes
- Cells example now targets the official `26 x 100` size
- DD `cells` row and column order is now fixed:
  - `List/range` static keys use fixed-width numeric ordering, so `ABCDEFGHIJKLMNOPQRSTUVWXYZ` renders in order instead of lexicographic `ABKLM...`
- static verification now also guards `counter`, `todo_mvc`, and `todo_mvc_physical` milestone basics
- static verification now also guards `todo_mvc_physical` theme-level scene ingredients across all four theme variants
- Wasm `todo_mvc` delete now works live again: per-item `ListRemove` item-event dispatch no longer relies only on raw template event ranges
- Wasm codegen now has a regression test covering TodoMVC delete-event item-dispatch context collection
- `boon-tools exec test-examples` now supports `--skip-persistence` so Milestone 1 Zoon parity can be verified separately from Milestone 2 persistence
- Wasm `fibonacci` now works live again: `Stream/skip` preserves object field maps through the `state.current` result path
- Wasm non-persistence Zoon suite is now green for all example tests except the intentional `cells` skip
- Wasm non-persistence Zoon suite is now green for all non-skipped examples:
  - `target/debug/boon-tools exec test-examples --engine Wasm --skip-persistence --no-launch --verbose`
  - result: `16/16 passed (1 skipped)`
- DD non-persistence Zoon suite is now green for all non-skipped examples:
  - `target/debug/boon-tools exec test-examples --engine DD --skip-persistence --no-launch --verbose`
  - result: `16/16 passed (1 skipped)`
- DD `cells` now compiles, boots in the worker, and renders the correct initial seed values again:
  - direct DD compile regression `cells_compile_to_dataflow_graph` passes
  - DD worker regressions `cells_worker_boot_reaches_output_clone` and `cells_initial_output_shows_seed_formula_values` pass
  - live preview at `?engine=dd&example=cells` now shows row 1 as `5 15 30`, row 2 starting with `10`, and row 3 starting with `15`
- DD now classifies `cells` as reactive by looking through user-defined helper bodies for external inputs, so it no longer falls into static evaluation and hang there
- DD `cells` document compilation now uses the generic single-dependency eval-closure path when the document contains user-defined function calls, avoiding the template-builder stall on the grid document
- direct DD worker regression for `cells` now shows the seed spreadsheet values again after removing the construction-time self-reference from `cells.bn`
- converting `ActorContext.parameters` and `ActorContext.object_locals` to shared `Arc<HashMap<...>>` removed one source of eager environment cloning
- user-defined function calls in Actors now stop inheriting the caller parameter map by default
- `ActorRegistry::destroy_scope()` now takes ownership of child/actor vectors instead of cloning them
- Actors document rendering now unwraps `Document/new(...).root_element` before converting to Zoon nodes, fixing the blank-preview bug across document-based examples
- frontend rebuild now catches the remaining browser-path compile surface for the Actors `Arc<HashMap<...>>` refactor; the missing `Arc::new(...)` call sites in `evaluator.rs` are fixed and the playground rebuilds again
- `cells.bn` no longer stores a recursive per-cell `value`; each cell now stores formula/edit state only, and displayed values are computed from the shared formula-text store at render time
- Wasm now has a direct local regression for the `cells` blocker:
  - `cargo test -p boon --features engine-wasm cells_example_lowers_and_emits_wasm -- --nocapture`
  - actual result: stack overflow / abort during Wasm emission
- DD `crud` no longer hangs in local compilation:
  - forward reactive aliases now defer and retry instead of falling through into event-source resolution
  - keyed wildcard detection now sees comparator references like `store.selected_id == item.id`
  - tolerant DD initial-state evaluation for keyed list items now avoids recursive strict evaluation of user functions, aliases, and HOLD bodies
  - local regression `crud_example_compiles_as_dataflow` now passes
  - initial keyed diffs now include real row labels instead of empty buttons
  - keyed CRUD filtering now compiles as a reactive retain and passes the filter + clear-filter browser steps
  - browser test runner now has `set_input_value` for deterministic text-input clearing in Milestone 1 specs
  - DD `crud` create, row selection, and delete now all pass live under `--skip-persistence`
  - keyed delete now samples `store.selected_id` correctly at event time via `SampleOnEvent`
  - wildcard DD events now advance all input frontiers, so wildcard and direct inputs no longer collapse into the same logical event boundary
  - DD `flight_booker` now passes live under `--skip-persistence`
  - HOLD bodies with `event |> THEN { ...reactive deps... }` now compile through the same sampled dep path as top-level `THEN`
  - reactive dep discovery now sees text interpolations inside `TEXT { ... }`
  - DD `counter_hold` now passes live under `--skip-persistence`
  - HOLD body event compilation no longer treats the HOLD state parameter as an external reactive dep, so self-references like `counter + 1` compile correctly

## Next Engine Tasks

- add the reference-kernel conformance layer:
  - lock `HOLD`, `LINK`, `LATEST`, list identity, and event-order semantics
  - use it as the oracle for Actors, DD, and Wasm behavior
- keep stronger `Scene/new` semantic checks for `todo_mvc_physical` and related examples
- keep exact interactive `cells` edit-flow checks:
  - double-click edit
  - Enter commit
  - Escape cancel
  - dependent recomputation
- stage `cells` acceptance as:
  - `26 x 30` parity first
  - `26 x 100` scale target second
- remove stale debug instrumentation from DD/Wasm `cells` work

## Current TODOs

### Cells Verification

- replace the remaining weak flattened-text checks with exact cell-oriented assertions
- add explicit interactive 7GUIs checks for edit and recomputation semantics
- keep `Row 100` and `Column Z` in the expected file so the official-size grid stays locked in

### Cells Example

- preserve the current official-size `26 x 100` grid
- keep the header rendering on the simpler `column_header(...) |> WHEN { ... }` path; it avoids the old Wasm stack blowup without changing 7GUIs semantics
- verify edit semantics remain aligned with the original Cells task

### DD Cells

- keep the new formula-text-only `cells` shape; it restores correct initial values on DD without reintroducing the old self-reference bug
- fix DD compilation of nested reactive item bodies inside constant `List/range |> List/map(...)`
- generic DD fixes landed:
  - reactive-dependency scanning now descends into objects, `HOLD`, postfix field access, and nested variables
  - static item-event compilation now supports direct `event |> WHEN { ... }` pipelines, not only `event |> THEN { ... }`
- current status:
  - DD can now compile concrete `cells` edit inputs when the example is rewritten around top-level sheet/edit state
  - but that workaround makes `cells.bn` so large that selecting the example crashes the playground frontend before any engine runs
- next DD step:
  - keep the generic compiler fixes
  - return to a smaller source shape that still exposes concrete per-cell edit inputs without crashing the frontend selection path

### Wasm Cells

- use [reference_kernel_plan.md](/home/martinkavik/repos/boon/docs/plans/reference_kernel_plan.md) as the current source of truth for semantic direction
- treat [wasm_cells_performance_plan.md](/home/martinkavik/repos/boon/docs/plans/wasm_cells_performance_plan.md) as legacy Wasm-specific notes, not the primary work queue
- preserve official 7GUIs semantics with no Cells-specific fallback or degraded slow path
- only return to deeper Wasm redesign after the reference semantics are pinned down
- when Wasm work resumes, make debug-mode responsiveness the hard gate:
  - `select cells`
  - initial grid render
  - double-click edit
  - Enter commit
  - reopen current formula text
  - Escape cancel

### Actors Cells

- keep an eye on actor count and scope churn during broader suite reruns
- after DD/Wasm `cells` interactions are stable, rerun the strengthened interactive `cells.expected` flow on Actors
- remove the stale blank-preview debugging notes only after the interaction checks pass

### Final Milestone Closeout

- rerun:
  - `DD --skip-persistence`
  - `Wasm --skip-persistence`
  - `Actors --skip-persistence`
- confirm all playground milestone examples plus 7GUIs pass with Zoon before moving on to persistence

## Current Blockers

- current known test-suite limitation is explicit:
  - full `test-examples` mixes Milestone 2 persistence rerun checks into Milestone 1 parity runs
  - use `--skip-persistence` for current Zoon parity verification until persistence work starts
- `cells` is still blocked on full interaction parity:
  - the shared frontend-side `cells` load/selection OOM was reduced:
    - loading `?engine=dd&example=cells` is stable again after caching the CodeMirror document string once per semantic-decoration pass
    - built-in example URLs now override stale stored editor files, so browser runs no longer silently pick up an old localStorage copy of `cells`
  - DD is now green on the strengthened live spec:
    - `target/debug/boon-tools exec test-examples --engine DD --filter cells --skip-persistence --no-launch --verbose`
    - covers:
      - official `26 x 100` grid
      - `A1=5`, `A2=10`, `B1=15`, `C1=30`
      - double-click edit
      - `Enter` commit
      - reopen with committed formula text
      - `Escape` cancel preserving committed values
      - blur exit without corruption
  - current concrete Wasm blocker:
    - `cells` still does not pass the strengthened live Wasm spec
    - the active issue is no longer release size, the old lowerer stack overflow, or the old outer-row nested-template pollution
    - the remaining work is now beyond the old nested-item binding collapse:
      - the focused Wasm bridge/runtime regressions for nested `row_data -> cells` binding, first-cell seed resolution, and A1-only edit activation all pass locally
      - the outer row template seeding regression is now also green:
        - `row_data.cells.row` / `row_data.cells.column` are preseeded with real per-row coordinates before Wasm item-event handling
      - temporary Wasm `cells` debug output from that reduction pass has been removed again
      - fresh live Wasm `cells` now renders the initial seed values again with no initial stray inputs
      - current strengthened live-spec failure is later:
        - `Double-click A1 enters edit mode with current value`
        - actual failure: `Assert focused input value failed: focused element is DIV, not input/textarea`
      - live state sampling after the nested-template isolation fix shows:
        - outer row items no longer carry bogus nested `cell.row` / `cell.column` / `is_editing` state
        - but after the latest rebuild the browser page is not consistently reaching `API Ready`, so the fresh live rerun is pending in browser automation
        - so the next meaningful Wasm step is to resume the rendered cell-label double-click / item-event dispatch investigation once the page is booting again, not more nested-template seeding cleanup or buffered nested rendering
    - the broader Wasm redesign track is still kept in [wasm_cells_performance_plan.md](/home/martinkavik/repos/boon/docs/plans/wasm_cells_performance_plan.md):
      - keep compile-time template and list-map plans as the source of truth
      - keep reducing bridge-side reconstruction work
      - restore prompt debug-mode full-grid responsiveness without timeout inflation
    - current rewrite checkpoint:
      - Wasm IR and bridge now use compile-time `TemplatePlan` / `ListMapPlan`
      - lowerer now eagerly materializes more nested list-item field cells instead of deferring through `list_item_field_exprs`
      - function-call parameter binding no longer uses a body-shape heuristic before materializing nested object fields
      - user-defined helper calls in pipe position now propagate list constructor and concrete field metadata onto the target instead of leaving that shape hidden behind `PipeThrough`
      - successful eager materialization now removes stale deferred `list_item_field_exprs` for the same cell instead of carrying both representations
      - alias propagation, helper parameter binding, piped parameter binding, and helper-result postprocessing now also use inline-object fallback during lowering instead of leaving nested shape for the bridge to rediscover
      - alias resolution and `List/get` representative-item resolution now also use compile-time inline-object fallback, reducing reliance on deferred `list_item_field_exprs`
      - `resolve_cell_field_cells(...)` now preserves representative object fields even when they are still `FieldAccess` expressions rather than direct `CellRead`s
      - inline helper calls now use the cached `function_requires_full_name_snapshot(...)` decision instead of rewalking the same helper body thousands of times during `cells` lowering
      - the leftover nested-event `resolve_field_access_expr_to_cell(...)` debug print has been removed from the hot path
      - `resolve_event_from_cell(...)` now also reuses an `IrExpr`-level event resolver, so nested `...event.double_click` chains do not depend entirely on intermediate event fields becoming concrete cells first
      - unresolved `List/get` lowering now canonicalizes through metadata-source cells both in direct resolution and in the `pipe_result` fallback path, reducing shape loss for aliases like `row_data.cells`
      - `materialize_list_item_field_cells(...)` now applies the same metadata-source canonicalization internally, so helper and alias callers automatically reuse the underlying list metadata source instead of each needing one-off fixes
      - helper-returned `CellRead(result_cell)` values now go through a shared shape-repair step for both normal and piped user-defined calls, instead of only the non-piped path attempting to materialize nested object/list fields on the returned cell
      - `Latest` extraction now follows resolvable `FieldAccess` through concrete field cells with a cycle guard, so the outer `row_data.cells |> List/latest()` path no longer has to remain as raw `FieldAccess`
      - helper parameter binding and field-alias propagation now canonicalize direct alias sources through metadata-source cells before exposing them to user-defined calls
      - reduced pre-finish inspection now shows the outer source list itself is no longer the first blocker:
        - `all_row_cells` lowers as `ListConstruct([CellRead(item)])`
        - that first item is an object-namespace cell with real field cells like `cells: object.cells`
        - so the surviving leak is later, not at the first source-list shape checkpoint
      - latest resolved Wasm structural slice:
        - focused nested `row_data -> cells` reconstruction no longer collapses later cell items onto A1-style bindings
        - runtime object-field reconstruction now keeps concrete inner field cells distinct instead of canonicalizing them onto one shared external field cell
        - list-item normalization now preserves nested list payloads until the correct item scope exists
        - that combination restores distinct `column` values for B1/C1 and fixes the `is_editing` / active-root regressions that were previously failing in unit tests
          - projected constructor-param binding now prefers the better canonical field source over a stale representative alias, so `row_cells` should stop overriding a more concrete `row_1_cells` source when both are available
          - projected field synthesis for simple object-returning constructors now uses the same preference rule, so `projected_item_fields` can no longer freeze `cells: row_cells` before later constructor binding has a chance to repair it
          - representative canonicalization for list-shaped aliases now also falls back to the immediate `CellRead` source when metadata-source walking stops at an alias like `object.cells`
          - reduced constructor-inlining proof is now green:
            - `minimal_row_data_list_map_item_cells_field_tracks_row_1_cells`
            - result: the inlined `cells` field now tracks the concrete representative row list instead of the hollow `row_data.cells -> object.cells` alias chain
          - cheap validation still passes:
            - `cargo check -p boon --no-default-features --features engine-wasm`
        - latest reduced checkpoint:
          - object-store field normalization now always propagates canonical list/object metadata from the resolved source onto the field cell, even when the field expression itself remains a same-source alias
          - simple helper calls now distinguish between exact param-name restore and prefix-scoped restore:
            - helpers without dotted param paths save only exact parameter bindings
            - helpers that really use dotted param paths still use the heavier prefix-aware restore
          - `minimal_multi_column_row_data_latest_preserves_all_double_click_triggers` now passes
          - so the remaining blocker has moved up from the reduced nested-event path into the larger `cells` graph:
            - the full `cells_edit_started_latest_preserves_double_click_triggers` regression now completes quickly instead of timing out
            - current failure is narrow:
              - `expected edit_started to preserve real double_click triggers, got ["object.cell_elements.display.double_click"]`
            - the next target is therefore the full-sheet `cells` trigger structure and then the live Wasm flow, not lowerer throughput
        - latest focused outer-row checkpoint:
          - runtime bridge binding is no longer the first blocker:
            - both `item_cell` and `resolved_item_store` are bound
            - runtime object-field resolution now prefers those live bindings over static reconstruction
            - first-row row-number resolution and first-row row-cells materialization regressions are fixed
            - editing-state alias cells now read live `CellStore` values again
          - the surviving stale lowerer binding is now explicit:
            - outer `row_data` map is `ListMap { cell: 308, source: 0, item_name: "row_data", item_cell: 309, ... }`
            - but `row_data.row` still lowers as `CellRead(11)`
            - `11` is the upstream `row_number` item cell, not the current outer item
          - current bridge/runtime blocker after those fixes:
            - the outer `row_data -> cells` resolution path still collapses later nested cell items so B1/C1 can inherit A1-style bindings before nested item context is restored
          - trying to force that outer row field dynamic immediately re-exposes the older inner blocker:
            - `cell.cell_elements.display` / `cell.cell_elements.editing` still lack complete concrete shape during constructor inlining
          - so the current seam is:
            - outer `row_data` representative rebasing still incomplete
            - inner `cell_elements.*` shape preservation still incomplete
      - so the next structural target is now specifically the `row_data.cells -> row_cell(...) -> cell` handoff before the next expensive live Wasm rerun
  - Actors still needs the strengthened live `cells.expected` rerun after the Wasm blocker is cleared

## Zoon Milestone Tasks

- verify `counter`, `todo_mvc`, `todo_mvc_physical`, and all 7GUIs examples on all engines with Zoon
- add stronger static and live checks for `Scene/new` semantic parity
- measure regressions separately for cold render, edit latency, and fanout recomputation

## Follow-up After Zoon

- introduce `boon-storage-api`
- add `boon-storage-localstorage`
- add `boon-storage-indexeddb`
- start `boon-renderer-canvas2d`
