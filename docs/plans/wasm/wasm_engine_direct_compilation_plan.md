# Plan: Direct Boon to Wasm Engine for Playground (No Fallback)

**Status:** Draft (implementation deferred)
**Date:** 2026-02-18
**Owner:** Boon runtime/compiler team

---

## 1. Goal

Add a third Boon playground engine that:

- compiles Boon source directly to Wasm instructions,
- runs the generated Wasm module in the playground preview pipeline,
- never falls back to Actors or DD when `Wasm` is selected.

This is a planning document only. No implementation is included here.

---

## 2. Hard Requirements

1. **No fallback semantics in Wasm mode**
   - `EngineType::Wasm` must execute only `compile -> instantiate -> run`.
   - Unsupported constructs must be compile errors with source spans.
   - Runtime failures must surface as Wasm engine errors, not engine switching.

2. **Cargo feature-gated engine compilation**
   - Add `engine-wasm` feature.
   - Rename `engine-both` to `engine-all`.
   - Keep compatibility alias for transition (recommended):
     - `engine-both = ["engine-all"]`

3. **Playground integration**
   - Add `Wasm` as a third selectable engine in UI and JS API.
   - Integrate Wasm run path into preview rendering.
   - Respect feature-gated compilation for all engine combinations.

4. **Tooling integration**
   - `boon-tools`, extension, MCP, and WebSocket protocol must accept/report `Wasm`.

---

## 3. Current Baseline (Observed)

- Engine enum currently has 2 variants: `Actors`, `DifferentialDataflow`.
- Cargo feature model currently uses `engine-actors`, `engine-dd`, `engine-both`.
- Playground API `setEngine/getEngine` currently accepts only `Actors` and `DD`.
- Tooling validators currently reject any engine value besides `Actors` and `DD`.
- Test automation currently forces DD before running expected examples.
- Playground execution branch currently supports DD path and actor-interpreter path only.

---

## 4. Target Architecture

## 4.1 Compile and Run Pipeline

```text
Boon source
  -> parser + scope + persistence resolution
  -> typed reactive IR
  -> Wasm codegen (wasm-encoder)
  -> instantiate module in browser
  -> send events to Wasm runtime
  -> read patch stream from Wasm memory
  -> apply patches to preview state/render bindings
```

No AST interpreter executes in Wasm mode.

## 4.2 Runtime Boundary

- **Wasm module owns:** reactive state transitions, list mutations, route/timer state.
- **Host owns:** DOM event capture, route forwarding, patch application, render bridge.
- **Core ABI exports:** `init`, `on_event`, and patch-buffer accessors.

## 4.3 No-Fallback Contract

When engine is `Wasm`:

- compile source to Wasm,
- instantiate Wasm,
- run Wasm.

If any step fails, report failure in preview and logs. Do not execute DD or Actors.

---

## 5. Operator Lowering Strategy (Boon -> IR -> Wasm)

- **`THEN`**
  - Lower to trigger-bound compute node.
  - Executes only on trigger event.

- **`LATEST`**
  - Lower to one target cell with multiple trigger arms.
  - Optional default arm executes in `init`.
  - Last event wins by deterministic event queue order.

- **`WHEN`**
  - Lower to frozen pattern-match node.
  - Triggered only by source value updates.
  - Wasm uses typed comparisons (`if` / `br_table`).

- **`WHILE`**
  - Lower to flowing dependency match node.
  - Trigger set includes source and dependent cells.
  - Re-evaluates on any dependency change.

- **`HOLD`**
  - Lower to mutable cell + generated update function.
  - Runtime stores typed state slots (bool/int/float/object refs).

- **`FLUSH`**
  - Lower to hidden internal `Flushed(T)` representation.
  - Generated pipeline functions start with flushed bypass check.
  - List loops short-circuit on first flushed value (fail-fast).

---

## 6. List Runtime in Wasm

Per-list store in Wasm linear memory:

- `len`, `cap`, item storage pointer,
- key index map (`item_key -> index`) for O(1) keyed operations,
- optional derived view caches (retain/map/count/is_empty).

Item events carry `{ list_id, action_id, item_key, payload }`:

- resolve row by key index,
- execute row-scoped transition,
- emit patch ops.

Patch ops (minimum set):

- `SetCell`
- `ListPush`
- `ListInsertAt`
- `ListRemoveByKey`
- `ListRemoveAt`
- `ListClear`
- `ListItemFieldUpdate`
- `RouteChanged`

---

## 7. Cargo Feature Plan

## 7.1 Core crate (`crates/boon/Cargo.toml`)

Target features:

- `engine-actors`
- `engine-dd`
- `engine-wasm`
- `engine-all = ["engine-actors", "engine-dd", "engine-wasm"]`

Compatibility during migration:

- `engine-both = ["engine-all"]` (temporary alias)

## 7.2 Playground crate (`playground/frontend/Cargo.toml`)

Target passthrough features:

- `engine-actors = ["boon/engine-actors"]`
- `engine-dd = ["boon/engine-dd"]`
- `engine-wasm = ["boon/engine-wasm"]`
- `engine-all = ["engine-actors", "engine-dd", "engine-wasm"]`
- `engine-both = ["engine-all"]` (temporary alias)

---

## 8. Engine Registry and Playground Integration

## 8.1 Shared engine utilities

- Add `EngineType::Wasm`.
- Add helper `available_engines() -> Vec<EngineType>`.
- Define switchability as `available_engines().len() > 1`.
- Update default-engine logic for all feature combinations.

## 8.2 URL, localStorage, and API

- URL parameter supports `?engine=actors|dd|wasm`.
- Storage supports `Actors`, `DD`, `Wasm`.
- `setEngine` validates all three options.
- `getEngine` reports selected engine and switchability.

## 8.3 Playground engine switcher

- Replace binary toggle with multi-engine cycle/menu.
- Show only engines available in current build.
- Keep state-clearing logic engine-aware.

## 8.4 Runtime branch wiring

- Add Wasm execution branch in playground runner.
- Gate imports and execution code with `#[cfg(feature = "engine-wasm")]`.
- Maintain build correctness for all single-engine and multi-engine builds.

---

## 9. Tooling and Protocol Integration

Update all hardcoded `Actors/DD` assumptions to include `Wasm` in:

- `tools/src/ws_server/protocol.rs`
- `tools/src/main.rs`
- `tools/src/mcp/mod.rs`
- `tools/extension/background.js`
- `tools/src/commands/test_examples.rs`

Testing helpers should become engine-generic, not DD-specific.

---

## 10. Milestones (Deferred Implementation)

## M0 - Feature model and surface integration

Deliverables:

- `engine-wasm`, `engine-all`, compatibility alias,
- `EngineType::Wasm` end-to-end wiring,
- 3-option playground switching,
- tooling accepts `Wasm`.

Exit criteria:

- build matrix passes for all feature sets,
- playground can select Wasm,
- no fallback branch in Wasm mode.

## M1 - IR lowering

Deliverables:

- typed reactive IR,
- AST -> IR lowering,
- span diagnostics.

Exit criteria:

- target examples lower successfully or fail with clear diagnostics.

## M2 - Wasm codegen and ABI runtime

Deliverables:

- Wasm module generation,
- event dispatch,
- patch-buffer protocol.

Exit criteria:

- simple examples run in playground via Wasm path.

## M3 - Semantic parity for core operators

Deliverables:

- correct `LATEST`, `THEN`, `WHEN`, `WHILE`, `HOLD`, `FLUSH` semantics.

Exit criteria:

- operator-focused tests pass.

## M4 - List runtime parity

Deliverables:

- append/remove/clear/retain/map/count/is_empty behavior,
- keyed identity stability.

Exit criteria:

- shopping list behavior parity in Wasm mode.

## M5 - TodoMVC parity

Deliverables:

- full todo_mvc interaction parity,
- persistence parity.

Exit criteria:

- `todo_mvc.expected` passes in Wasm mode.

## M6 - Hardening

Deliverables:

- performance pass,
- diagnostics polish,
- docs cleanup.

---

## 11. Build and Test Matrix Requirements

Required compile checks:

- actors only,
- dd only,
- wasm only,
- actors+dd,
- actors+wasm,
- dd+wasm,
- all three (`engine-all`).

Required runtime checks:

- expected example suite in Wasm mode,
- explicit no-fallback assertions in Wasm path.

---

## 12. Risks and Mitigation

1. **Feature-gate breakage**
   - Mitigation: enforce matrix compile checks early (M0).
2. **WHILE dependency mistakes**
   - Mitigation: dependency graph tests and focused fixtures.
3. **List identity bugs**
   - Mitigation: strict `__key` invariants and debug assertions.
4. **FLUSH propagation regressions**
   - Mitigation: dedicated flushed-bypass tests.
5. **Tool/API drift**
   - Mitigation: update protocol + validators in same milestone as enum changes.

---

## 13. Definition of Done

- `Wasm` is fully selectable in playground and tooling.
- `Wasm` mode does not fallback to DD/Actors.
- expected examples pass in Wasm mode.
- `engine-all` is canonical multi-engine feature.

---

## 14. Execution Note

This is a planning artifact only. Implementation is intentionally deferred.
