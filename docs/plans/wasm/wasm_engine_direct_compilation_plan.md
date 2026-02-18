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

Each operator maps to an IR pattern, which then lowers to WASM. The IR design is
sketched in `boon_as_systems_language.md` §5.6.

### `THEN` — Trigger-Bound Compute Node

Executes body only when trigger event fires. No-op on other events.

```boon
button.press |> THEN { value + 1 }
```
```
IR:  Then { trigger: LinkId("button.press"), body: BinOp(Add, CellRead("value"), Const(1)) }
```
```wasm
;; In on_event dispatcher:
(if (i32.eq (local.get $event_id) (i32.const BUTTON_PRESS))
  (then
    ;; evaluate body and store result
    (global.set $result (f64.add (global.get $value) (f64.const 1)))
    (call $emit_patch ...)
  )
)
```

### `LATEST` — Multi-Arm Cell with Last-Event-Wins

One target cell, multiple trigger sources. Each event updates the target from its arm.

```boon
LATEST { increment_button.press |> THEN { count + 1 }, reset_button.press |> THEN { 0 } }
```
```
IR:  Latest {
       target: CellId("count"),
       arms: [
         LatestArm { trigger: "increment.press", body: BinOp(Add, CellRead("count"), Const(1)) },
         LatestArm { trigger: "reset.press",     body: Const(0) },
       ]
     }
```
```wasm
;; Dispatches to whichever event fired; both update same cell
(if (i32.eq (local.get $event_id) (i32.const INCREMENT_PRESS))
  (then (global.set $count (f64.add (global.get $count) (f64.const 1))))
)
(if (i32.eq (local.get $event_id) (i32.const RESET_PRESS))
  (then (global.set $count (f64.const 0)))
)
```

### `WHEN` — Frozen Pattern Match (Triggered by Source Update)

Pattern-matches on source cell value. Only fires when source changes, not when
dependencies change.

```boon
route |> WHEN { "/" => "all", "/active" => "active", "/completed" => "completed" }
```
```
IR:  When {
       source: CellId("route"),
       arms: [
         (Pattern::Text("/"),          Const("all")),
         (Pattern::Text("/active"),    Const("active")),
         (Pattern::Text("/completed"), Const("completed")),
       ]
     }
```
```wasm
;; When route cell changes, evaluate pattern match
;; Uses br_table for integer patterns, if-chains for text
(call $str_eq (global.get $route) (i32.const STR_SLASH))
(if (then (call $set_text_cell (i32.const FILTER) (i32.const STR_ALL))))
;; ... more arms
```

### `WHILE` — Flowing Dependency Match (Re-evaluates on ANY Dependency Change)

Like WHEN but also re-evaluates when dependency cells change (not just source).

```boon
selected_filter |> WHILE { "all" => all_todos, "active" => active_todos }
```
```
IR:  While {
       source: CellId("selected_filter"),
       deps: [CellId("all_todos"), CellId("active_todos")],
       arms: [
         (Pattern::Text("all"),    CellRead("all_todos")),
         (Pattern::Text("active"), CellRead("active_todos")),
       ]
     }
```
```wasm
;; Triggered by changes to selected_filter OR all_todos OR active_todos
;; Re-evaluates currently-matched arm
(call $str_eq (global.get $selected_filter) (i32.const STR_ALL))
(if (then
  ;; output = all_todos (re-read on every trigger)
  (call $set_list_cell (i32.const VISIBLE_TODOS) (global.get $all_todos))
))
;; ... more arms
```

### `HOLD` — Mutable Cell + Update Function

State cell with initial value. Update function reads previous state and computes next.

```boon
0 |> HOLD state { button.press |> THEN { state + 1 } }
```
```
IR:  Hold {
       cell: CellId("count"),
       init: Const(0),
       body: Then { trigger: "button.press", body: BinOp(Add, CellRead("count"), Const(1)) },
       triggers: ["button.press"],
     }
```
```wasm
(global $count (mut f64) (f64.const 0))
;; On button press: read previous count, add 1, store
(global.set $count (f64.add (global.get $count) (f64.const 1)))
```

### `FLUSH` — Sentinel Value with Bypass Check

Error propagation. Every transform checks flushed bit first.

```
IR:  No dedicated FLUSH node — flushed state is a per-cell bit flag.
     Transforms receive (value, flushed) pairs and propagate flushed downstream.
```
```wasm
;; At start of every generated transform function:
(if (local.get $flushed) (then (return (local.get $input) (i32.const 1))))
;; Normal body follows...
```

### 5.5 Host Callback ABI

The WASM module imports host functions for side effects. The host provides:

```wasm
;; === Patch emission (WASM → Host) ===
(import "boon" "emit_set_cell" (func $emit_set_cell (param i32 f64)))      ;; (cell_id, number_value)
(import "boon" "emit_set_text" (func $emit_set_text (param i32 i32 i32)))  ;; (cell_id, str_ptr, str_len)
(import "boon" "emit_list_push" (func $emit_list_push (param i32 i32)))    ;; (list_id, item_ptr)
(import "boon" "emit_list_remove" (func $emit_list_remove (param i32 i32)));; (list_id, key)
(import "boon" "emit_list_clear" (func $emit_list_clear (param i32)))      ;; (list_id)

;; === Timer management ===
(import "boon" "timer_start" (func $timer_start (param i32 f64) (result i32))) ;; (timer_id, interval_ms) -> handle
(import "boon" "timer_cancel" (func $timer_cancel (param i32)))                ;; (handle)

;; === Persistence ===
(import "boon" "persist_write" (func $persist_write (param i32 i32 i32)))  ;; (key_ptr, val_ptr, val_len)
(import "boon" "persist_read" (func $persist_read (param i32 i32) (result i32 i32))) ;; (key_ptr, key_len) -> (val_ptr, val_len)

;; === Text input ===
(import "boon" "clear_text_input" (func $clear_text_input (param i32)))    ;; (input_id)
(import "boon" "focus_element" (func $focus_element (param i32)))          ;; (element_id)
```

The host calls the WASM module's exported `$on_event` function:
```wasm
(export "on_event" (func $on_event))  ;; (event_id: i32, payload_ptr: i32, payload_len: i32)
(export "init" (func $init))          ;; () -> void, called once at startup
(export "memory" (memory $mem))       ;; shared linear memory for string/struct passing
```

Event payload encoding in linear memory:
- `KeyDown`: `{ key: u8, text_ptr: u32, text_len: u32 }` (12 bytes)
- `Change`: `{ text_ptr: u32, text_len: u32 }` (8 bytes)
- `Press/Click/Blur`: no payload (event_id sufficient)
- `Route`: `{ path_ptr: u32, path_len: u32 }` (8 bytes)

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

## 14. Open Questions (Unresolved Design Decisions)

1. **IR Design**: What are the reactive IR node types? How are HOLD cycles (back-edges)
   represented? How do dynamic lists map to IR nodes? No IR has been sketched yet —
   this is the #1 technical risk.

2. **Memory Management**: How does compiled Boon manage memory? Options: WASM GC
   (managed types), manual reference counting in linear memory, region-based allocation,
   or uniqueness types. This decision affects ABI design, list runtime, and codegen.

3. **Host Callback ABI**: How does the WASM module call back to the host for DOM
   operations, timer setup/cancel, persistence read/write? The plan mentions
   "patch-buffer accessors" but doesn't define imported host functions.

4. **Event Serialization**: How are events passed from host to WASM? Struct layout
   in linear memory? Function parameters? What's the encoding for complex events
   like `KeyDown { key: Enter, text: "Milk" }`?

5. **GC for List Items**: When a list item is removed, who frees its memory? If using
   linear memory, how is fragmentation handled? If using WASM GC, how do list items
   interact with host-side DOM references?

6. **Dynamic WHILE Arms**: `WHILE { pattern => body }` can have runtime-dependent
   patterns. How does ahead-of-time compilation handle this? Compile all possible
   arms and branch? Fall back to interpretation?

7. **Error Semantics**: What happens when compiled Boon encounters a runtime error
   (type mismatch, division by zero, FLUSH)? WASM trap? Return error code? Use
   WASM exception handling proposal?

8. **Patch Buffer Sizing**: How large is the patch buffer? Fixed? Growable? What
   happens on overflow (multiple patches from a single event)?

---

## 15. Execution Note

This is a planning artifact only. Implementation is intentionally deferred.
