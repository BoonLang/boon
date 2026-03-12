# Wasm Cells Performance Plan

## Status

- Superseded as the primary direction on 2026-03-13.
- Keep this file as legacy Wasm rescue history and focused local notes.
- New work should follow [reference_kernel_plan.md](/home/martinkavik/repos/boon/docs/plans/reference_kernel_plan.md).

## Direction Reset

- We are no longer treating `cells` as something that should be fixed mainly by extending the current Wasm lowerer/bridge rescue loop.
- The three engines must remain independent and later move into separate crates.
- Because of that, the new direction is:
  - use a small internal reference kernel only as a semantic oracle and test target
  - keep Wasm independent
  - return to Wasm changes only after the shared semantics are pinned down by conformance tests
- Practical implication for this file:
  - do not add more bridge-time reconstruction heuristics as the default next step
  - do not keep expanding lowerer checkpoint history as the main plan
  - use this file only for Wasm-specific redesign notes once the reference semantics are stable

## Summary

- Legacy summary:
  - fix `cells` on the Wasm engine with a general-purpose engine change, not a Cells-specific fallback
  - keep `cells.bn` aligned with the original 7GUIs behavior
  - treat debug-mode responsiveness as the primary acceptance target
  - Current interpretation:
  - those goals still stand, but they are no longer the first implementation step
  - the first step is now semantic locking via the reference kernel plan
  - current live Wasm note:
    - the safe landed Wasm bridge/runtime fixes now cover nested `row_data -> cells` binding/materialization and A1-only focused unit regressions
    - the clean browser-driven `cells` flow still currently fails at the first visible-value assertion: `expected cell (1, 1) to be '5', got '.'`
    - a nested-row buffered render experiment briefly advanced the live flow to the double-click/focus step, but it caused edit-path `ItemCellStore::ensure_item` borrow/OOM failures and was reverted
    - do not revive that buffered nested-render approach as-is
    - newer local Wasm engine progress after that checkpoint:
      - `ItemCellStore` now uses sparse per-item storage keyed by item memory index instead of dense `Vec` growth up to the highest seen index
      - `init_item` / `refresh_item` now skip nested render-template ranges, so outer row items no longer pre-initialize inner cell-template selector state
      - outer row template seeding now also preloads `row_data.cells.row` / `row_data.cells.column` before Wasm event handling, matching the focused Rust regression
      - fresh live Wasm `cells` now renders cleanly again with no initial stray inputs
      - current remaining live blocker is later and narrower:
        - the strengthened Wasm browser spec now fails at the edit-entry step
        - exact failure: `Assert focused input value failed: focused element is DIV, not input/textarea`
      - current live verification gap:
        - after the last rebuild, the page is not consistently reaching `API Ready`, so the fresh browser rerun is pending even though the focused seeding regression is green
      - so the next Wasm work is live double-click event delivery / rendered cell-label item dispatch once the page is booting again, not more nested-template seeding cleanup

## Problem Statement

- The current Wasm `cells` path is too large and too slow even in debug mode for a simple benchmark.
- The main issue is not raw `.wasm` bytes alone. The active problem is structural duplication:
  - the lowerer loses list, template, and object shape through nested aliases and function-call-produced values
  - the bridge then tries to rediscover that structure at runtime
  - large `List/map` trees are seeded and rebuilt on the browser main thread with too much clone-heavy metadata work
- As a result, the system is paying both compile-time and runtime costs for the same information.

## Constraints

- No Cells-specific fallback path.
- No degraded "slow but works" debug mode.
- No more Wasm-driven source rewrites of `cells.bn` unless the change improves semantics for all engines.
- The final `cells` path must satisfy the same strengthened `cells.expected` flow already passing on DD.
- During implementation, prefer a larger structural rewrite over constant micro-testing. The current browser loop is too slow to be the main development driver.

## Research Notes

- Rust and wasm-bindgen guidance treats release size optimization as a separate concern from runtime responsiveness:
  - <https://rustwasm.github.io/book/reference/code-size.html>
  - <https://rustwasm.github.io/wasm-bindgen/reference/optimize-size.html>
- AssemblyScript is a useful reference because it pushes structure and runtime-mode decisions into compilation instead of runtime inference:
  - <https://www.assemblyscript.org/compiler.html>
  - <https://www.assemblyscript.org/runtime.html>
- Grain is useful because it treats type and debug metadata as compiler output choices, not mandatory runtime cost:
  - <https://grain-lang.org/docs/tooling/grain_cli>
- Web performance guidance is explicit that long main-thread tasks are the real UX bottleneck:
  - <https://web.dev/articles/optimize-long-tasks>

## Implementation Changes

## Execution Strategy

### Rewrite First, Then Verify

- Treat the current Wasm `cells` work as a structural rewrite, not a sequence of tiny optimizations.
- Do not stop after every small code change to run the full browser-driven `cells` flow.
- Instead:
  - implement the lowerer and bridge changes in larger coherent slices
  - use only cheap guardrails while the rewrite is in flight
  - defer the expensive live `cells.expected` run until the code shape is internally consistent

### Cheap Checks During the Rewrite

- Preferred checks while changing the lowerer and bridge:
  - `cargo check -p boon --no-default-features --features engine-wasm`
  - a very small number of focused Wasm unit tests only when they are fast and directly relevant
- Avoid using the browser-driven `cells` flow as the inner loop unless the code slice is ready for end-to-end verification.

### Full Verification Only After the Rewrite Slice Stabilizes

- Once the lowerer and bridge changes are coherent:
  - rebuild the playground
  - run the strengthened Wasm `cells.expected` flow in debug mode
  - fix the remaining concrete bugs found there
- Only after Wasm passes that end-to-end flow should Actors be rerun on the same strengthened spec.

### 1. Freeze Example Semantics

- Keep `cells.bn` as the shared 7GUIs benchmark.
- Preserve:
  - official `26 x 100`
  - correct seed values
  - double-click edit
  - Enter commit
  - reopen with committed formula text
  - Escape cancel
  - blur exit
- Do not add any Wasm-only fallback logic to the example.

### 2. Preserve Structure in the Wasm Lowerer

- Extend the Wasm IR with explicit compile-time metadata:
  - `ConstructorShape` for object/list constructor identity and field mapping
  - `TemplatePlan` for template root, seed cells, field-hold sources, active root, and concrete event links
  - `ListMapPlan` for source cell, fanout source, item root/store cell, and linked template plan
- Populate these plans during lowering once.
- Preserve constructor and field metadata through:
  - `CellRead`
  - `PipeThrough`
  - `FieldAccess`
  - block locals
  - function parameters
  - spread aliases
  - function-call-produced list/object variables
- Fail fast in lowering when a template item that requires field or event access loses shape.
- Remove any remaining lowering path that silently degrades real element events into synthetic `then_trigger`.

#### Current Rewrite Checkpoint

- `TemplatePlan` and `ListMapPlan` are now computed in the Wasm IR and consumed by the bridge.
- The lowerer now eagerly materializes more nested list-item object fields into concrete `cell_field_cells` instead of leaving them behind in `list_item_field_exprs`.
- Recent cleanup removed the old heuristic that only materialized function-call parameter object fields when the current body looked like it needed them.
- Helper boundaries in pipe position now also propagate list constructor and concrete field metadata from returned `CellRead` results instead of leaving that shape implicit behind a `PipeThrough`.
- When eager materialization succeeds, the lowerer now drops the stale deferred `list_item_field_exprs` copy for that same cell instead of carrying both representations forward.
- Inline-object fallback is now used in more compile-time paths:
  - alias field propagation
  - normal helper parameter binding
  - piped helper parameter binding
  - helper-result postprocessing
- Alias resolution and `List/get` representative-item resolution now also fall back to compile-time inline-object extraction, so nested object shape can survive even when it has not yet been distributed into `cell_field_cells`.
- `resolve_cell_field_cells(...)` now also harvests representative object fields when they are still expressed as `FieldAccess`, not only when they are already direct `CellRead`s.
- The lowerer now prefers turning resolvable inline objects into concrete nested field cells during lowering instead of waiting for bridge-time reconstruction.
- Inline helper calls now use the cached `function_requires_full_name_snapshot(...)` decision instead of rewalking the same function body on every call, which removes one repeated compile-time cost from hot helpers like `row_cell(...)` and `edit_started_from_cell(...)`.
- The remaining `resolve_field_access_expr_to_cell(...)` debug print on the nested event path has been removed so focused Wasm lowering runs no longer spend time flooding stdout.
- `resolve_event_from_cell(...)` now reuses an `IrExpr`-level event resolver, so nested chains like `cell.cell_elements.display.event.double_click` can resolve directly from field-access structure instead of depending only on intermediate event fields becoming concrete cells first.
- Unresolved `List/get` lowering now canonicalizes through metadata-source cells both in direct resolution and in the fallback `pipe_result` path, so field aliases like `row_data.cells` are less likely to drop list-item shape before helper results are returned.
- `materialize_list_item_field_cells(...)` now does the same canonicalization internally, so helper/alias callers that still pass a field alias automatically reuse the underlying list metadata source instead of each needing their own special-case patch.
- Helper-returned `CellRead(result_cell)` values now go through a shared shape-repair step for both normal and piped user-defined calls, instead of only the non-piped path attempting to materialize nested object/list fields on the returned cell.
- `Latest` extraction now follows resolvable `FieldAccess` through concrete field cells with a cycle guard, so outer paths like `row_data.cells |> List/latest()` no longer have to remain as raw `FieldAccess`.
- `List/get` shape repair now canonicalizes through metadata-source cells more aggressively, and helper parameter binding canonicalizes direct alias sources before exposing them as helper parameters.
- Field-alias propagation now canonicalizes each aliased field through its metadata source before copying list constructor, event, nested-field, and list-item metadata.
- Reduced pre-finish inspection now shows the outer source list is no longer the first blocker:
  - `all_row_cells` lowers as `ListConstruct([CellRead(item)])`
  - that first item is an object-namespace cell with real field cells like `cells: object.cells` and `row: object.row`
  - the remaining leak is later, where `projected = row_data.cells` still prefers a stale helper cell
    `row_cells = FieldAccess(row_data, "cells")` instead of the already-correct
    `row_data.cells -> object.cells -> row_1_cells` chain
- Remaining structural gap:
- the outer source list now has concrete shape, but `List/map(... row_data ...) |> List/latest()` still synthesizes `projected = row_data.cells` from the stale helper `row_cells = FieldAccess(row_data, "cells")`
- until that is fixed, the later `row_cell(...)` and `edit_started_from_cell(...)` paths will keep inheriting the wrong inner-cell source
- the newest reduced `selected_cell` probe shows the exact surviving shape:
  - latest arm body collapses to `object.cells`
  - `object.cells` is `Derived(CellRead(row_cells))`
  - `row_cells` is still `Derived(FieldAccess(CellRead(row_data), "cells"))`
  - both cells still have `fields=[]`
- that means the active Wasm leak is now specifically in the `row_data.cells -> row_cell(...) -> cell` handoff, not in the top-level `Latest` extraction anymore
- the focused event reducer for `edit_started` still fails with `cell.cell_elements.display` resolving from a hollow `pipe_result`, so that same handoff still blocks the real `double_click` path
- the newest structural attempts to escape alias-field loops now move the reducer off the old stable assertion and into a lowerer stack overflow instead:
  - `timeout 40s cargo test -p boon --no-default-features --features engine-wasm minimal_multi_column_row_data_latest_row_cell_preserves_cell_elements_shape -- --nocapture`
  - current result: test process aborts with `stack overflow`
  - that means the remaining blocker is no longer just missing shape; there is also still a recursive lowerer path in the same `row_data.cells -> row_cell(...) -> cell` chain
- after backing out the recursive field-resolution detour, the reduced signal is stable again and more precise:
  - `minimal_row_data_cells_projection_tracks_row_cells_source` still fails
  - but the failure now shows the wrong `row_cells` source node explicitly:
    - `Derived { cell: ..., expr: FieldAccess { object: CellRead(row_data), field: "cells" } }`
  - so the outer list representative for `cells` is still being synthesized from self-reference (`row_data.cells`) rather than from a concrete first-item source such as `row_1_cells`
  - that means the next structural target is not only the later `row_data.cells -> row_cell(...) -> cell` handoff, but also the earlier representative-item extraction for function-call-produced list items
- the latest rewrite pushes representative-source cleanup further upstream:
  - representative reduction for object fields now uses representative-source canonicalization instead of shape-source canonicalization, so list-shaped helper aliases are supposed to collapse to the concrete representative list source
  - inline-object recovery now returns reduced representative fields instead of replaying raw helper-parameter aliases, so later shape repair should no longer be able to resurrect stale `cells: row_cells` fields after earlier canonicalization succeeded
- the focused reducer still has not flipped after those changes:
  - `minimal_row_data_cells_projection_tracks_row_cells_source` still reports `projected = row_data.cells` sourcing from `row_cells = FieldAccess(row_data, "cells")`
  - so the remaining leak is earlier still: the helper-produced `row_data` object is being created with a stale `cells` field source before representative repair runs
- the newest reduced checks make the current location even sharper:
  - `minimal_row_data_list_map_item_cells_field_tracks_row_1_cells` confirms `all_row_cells` still carries constructor `make_row_data`, so the `List/map` source is entering constructor inlining
  - but that same test also shows the constructor-inlined `item_cell` still ends up with `cells_field -> row_cells` instead of `row_1_cells`
  - so the remaining leak is no longer “constructor metadata missing”; it is specifically inside `inline_list_constructor_for_template(...)` where the inlined `item_cell` field map is still rebuilt from a stale helper alias
- latest constructor-inlining checkpoint:
  - field alias creation now prefers representative canonicalization for list-shaped fields before falling back to plain shape canonicalization
  - projected constructor-param binding now prefers the better canonical field source over a stale representative alias, so a hollow helper like `row_cells` should no longer override a more concrete `row_1_cells` source if both are available
  - projected field synthesis for simple object-returning constructors now uses the same preference rule, so `projected_item_fields` can no longer freeze `cells: row_cells` before later constructor binding has a chance to repair it
  - representative canonicalization for list-shaped aliases now also falls back to the immediate `CellRead` source when metadata-source walking stops at an alias like `object.cells`
  - reduced constructor-inlining proof is now green:
    - `minimal_row_data_list_map_item_cells_field_tracks_row_1_cells`
    - result: the inlined `cells` field now tracks the concrete representative row list instead of the hollow `row_data.cells -> object.cells` alias chain
  - cheap validation still passes:
    - `cargo check -p boon --no-default-features --features engine-wasm`
  - the next focused signal should come from the same reduced constructor-inlining path, not from a full browser rerun yet
- latest structural checkpoint:
  - object-store field normalization now always propagates canonical list/object metadata from the resolved source onto the field cell, even when the field expression itself remains a same-source alias
  - simple helper calls now distinguish between exact param-name restore and prefix-scoped restore:
    - helpers that do not use dotted param paths like `cell.cell_elements.display` now save only exact parameter bindings
    - helpers that really do use dotted param paths still use the heavier prefix-aware restore
  - the reduced `row_data` event path is now green:
    - `minimal_multi_column_row_data_latest_preserves_all_double_click_triggers`
    - result: all three `edit_started_from_cell(...)` arms lower to real `object.cell_elements.display.double_click` triggers
  - the remaining blocker has therefore moved up from the reduced nested-event path into the larger `cells` graph:
    - the full `cells_edit_started_latest_preserves_double_click_triggers` regression now completes quickly instead of timing out
    - current failure is narrow:
      - `expected edit_started to preserve real double_click triggers, got ["object.cell_elements.display.double_click"]`
    - so the next target is no longer compile-time blowup, but making the full-sheet path preserve the expected trigger structure consistently enough for the live Wasm flow

### 3. Make the Bridge Consume Plans

- Refactor Wasm bridge list/template rendering to read `TemplatePlan` and `ListMapPlan` directly from `IrProgram`.
- Remove runtime rescanning of the IR for:
  - template roots
  - field maps
  - seed-cell discovery
  - field-hold discovery
  - cross-scope event discovery
- Seed only the cells declared by the template plan.
- Ensure parent templates do not walk nested child template ranges.
- Replace clone-heavy per-render `HashMap` analysis with precomputed stable plan data.

### 4. Keep Incremental Rendering, but Make It Cheap

- Keep incremental rendering as a scheduler, not as a substitute for fixing expensive bridge analysis.
- Use a time-budgeted yield policy for large list rendering.
- Do not accept a solution that only works by inflating websocket or browser-test timeouts.

### 5. Close Out Final Engine Verification

- Once Wasm passes the strengthened live `cells.expected`, rerun Actors on the same exact flow.
- Any remaining Actors issue must be fixed in the engine path, not by changing `cells.bn`.

## Tests and Acceptance

### Lowerer Tests

- Nested row-cell templates preserve field metadata.
- `edit_started` retains real `element.double_click` trigger names.
- Function-call-produced row and cell lists preserve constructor shape.

### Bridge and Runtime Tests

- First cell seeds formula `5` and display `5`.
- `A1` double-click enters edit mode in Wasm.
- Entering `7` in `A1` recomputes `B1=17` and `C1=32`.
- Reopening `A1` shows the committed formula text.
- Escape preserves committed values.
- Blur exits edit mode without corruption.

### Live Acceptance

- `DD`, `Wasm`, and `Actors` all pass the same strengthened `cells.expected`.
- No special websocket timeout inflation remains in the final Wasm path.
- Debug-mode `select cells` and initial render complete with normal automation responsiveness.
- The final verification phase happens after the structural rewrite is complete, not after every intermediate lowerer or bridge edit.

### Release Measurement

- After debug-mode correctness and responsiveness are restored:
  - measure release frontend Wasm size via `makers mzoon build -r -f`
  - inspect retained size with `twiggy`
- Release-size work is not acceptance for this plan.

## Current Checkpoint

- Reduced constructor-inlining proof is still green:
  - `minimal_row_data_list_map_item_cells_field_tracks_row_1_cells`
- Bridge/runtime binding is no longer the first outer-row blocker:
  - runtime now binds both `item_cell` and `resolved_item_store`
  - runtime object-field resolution now prefers live bindings on canonical object stores before reconstructing static field maps
- The focused outer-row reducer shows the remaining stale lowerer binding clearly:
  - outer `row_data` map is `ListMap { cell: 308, source: 0, item_name: "row_data", item_cell: 309, ... }`
  - but `row_data.row` in that template still lowers as `CellRead(11)`
  - `11` is the upstream `row_number` item cell from the source map, not the current outer item
- Forcing that outer row field dynamic immediately re-exposes the older inner blocker:
  - `cell.cell_elements.display` / `cell.cell_elements.editing` still lack complete concrete shape during constructor inlining
- So the current seam is explicit:
  - outer `row_data` representative rebasing is still incomplete
  - inner `cell_elements.*` shape preservation is still incomplete
  - both have to be fixed before live Wasm `cells` will be fast and correct

## Defaults

- Debug-mode responsiveness is a hard product requirement.
- No Cells-specific fallback path is acceptable.
- The primary fix belongs in the Wasm lowerer and bridge contract, not in release tooling or timeout masking.
- Development workflow should optimize for meaningful structural progress, not for frequent expensive browser reruns.
