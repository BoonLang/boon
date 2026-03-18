# Single Wasm Engine Cutover Plan

**Status:** Completed
**Date:** 2026-03-20
**Supersedes:** `wasm_pro.md`, `wasm_pro_execution_backlog.md`

## Goal

Finish the WebAssembly migration by deleting the old legacy engine, promoting the surviving renderer-agnostic backend to the canonical `Wasm` name, and making the browser/runtime/tooling surface behave as a single-engine system.

## Required Outcomes

1. Legacy `engine_wasm` is removed from the build and no longer exposed publicly.
2. The surviving backend is named `Wasm` everywhere user-facing.
3. Playground examples run on `Wasm` without parser-backed “unsupported” fallback UX.
4. All official 7GUIs pass in the real browser on `Wasm`.
5. Cells backend metrics stay within absolute Wasm budgets after the legacy comparison path is gone.

## Cutover Order

### 1. Remove legacy Wasm first

- Delete legacy feature wiring and selection surfaces before renaming the surviving backend.
- Stop advertising or validating two different Wasm-family engine names.
- Replace legacy-vs-pro metrics gates with Wasm-only budgets.

### 2. Rename WasmPro to Wasm

- `EngineType::WasmPro` becomes `EngineType::Wasm`.
- Public CLI/MCP/ws/playground strings accept only `Actors`, `DD`, and `Wasm`.
- Engine pickers, help text, storage/URL parsing, and docs stop mentioning `WasmPro`.

### 3. Eliminate unsupported-example fallback UX

- No example should render “This example is not supported by Wasm yet” or similar.
- Any example that still hits fallback lowering is a compiler/runtime bug to fix.

### 4. Browser-first correctness gate

`Wasm` must satisfy the official 7GUIs behavior in the real browser for:

- `counter`
- `temperature_converter`
- `flight_booker`
- `timer`
- `crud`
- `circle_drawer`
- `cells`

`cells` remains the highest-value interactive gate:

- double-click enters edit mode
- Enter commits the edited cell
- dependent cells recompute
- Escape cancels
- blur exits cleanly
- large-grid behavior still works

## Metrics Baseline

Use absolute budgets rather than legacy-engine comparison:

- module bytes: `<= 1_000_000`
- first render: `<= 300 ms`
- A1 commit batch bytes: `<= 2_000`
- A1 commit batch ops: `<= 16`
- A1 commit path: `<= 300 ms`
- dependent recompute path: `<= 300 ms`

Reference snapshot at completion:

- encoded bytes: `934301`
- first render: about `245 ms`
- edit path: about `186 ms`
- dependent recompute: about `180 ms`

## Verification

- `cargo check -p boon --no-default-features --features engine-wasm`
- `cargo check --manifest-path playground/frontend/Cargo.toml`
- `cargo check --manifest-path tools/Cargo.toml`
- `cargo run --manifest-path tools/Cargo.toml -- metrics cells-backend --check`
- browser-driven 7GUI verification on `Wasm`

## Completion Notes

- Legacy-vs-pro browser/backend split has been removed from the live Wasm surface.
- User-facing engine selection now exposes a single `Wasm` engine.
- Live browser verification passes for all official 7GUIs on `Wasm`.
- Cells absolute Wasm budgets pass without relying on legacy-engine comparison.
