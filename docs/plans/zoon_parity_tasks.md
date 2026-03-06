# Zoon Parity Tasks

## Current Completed Base

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

- add stronger `Scene/new` semantic checks for `todo_mvc_physical` and related examples
- rerun the full non-persistence Zoon suites for Actors, DD, and Wasm with `cells` no longer skipped
- add exact interactive `cells` edit-flow checks:
  - double-click edit
  - Enter commit
  - Escape cancel
  - dependent recomputation
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

- remove the remaining Wasm `cells` debug output from lowering/codegen
- add a smaller regression for the header/`List/range -> List/map` path so future changes do not reintroduce the old stack/render failure
- add edit-behavior coverage on the official grid

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
    - the old recursion/lowering failure is fixed:
      - `cargo check -p boon --features engine-wasm` passes
      - `cargo test -p boon --features engine-wasm cells_example_lowers_and_emits_wasm -- --nocapture` passes
    - nested row-list binding is also fixed:
      - outer `row_data` items now resolve real `cells` lists instead of `Void`
      - bridge-side reseeding after `init_item` prevents `row_cells` from being clobbered back to `0`
    - current first live blocker is actually shared frontend example selection:
      - `target/debug/boon-tools exec set-engine Wasm`
      - `target/debug/boon-tools exec select cells.bn`
      - console then shows:
        - `[selectExample] setting files for cells.bn`
        - followed immediately by `RuntimeError: unreachable`
      - the abort occurs on the first `files.set(...)` inside the playground `selectExample` task before `current_file`, `source_code`, or Wasm `finish_setup` begin
    - implication:
      - Wasm runtime/render investigation is currently blocked behind the shared frontend `files` signal write poisoning during `cells` selection
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
