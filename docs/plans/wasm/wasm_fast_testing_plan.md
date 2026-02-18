# Plan: Fast Testing Strategy for Wasm Engine

**Status:** Draft (implementation deferred)
**Date:** 2026-02-18
**Depends on:**
- `wasm_engine_direct_compilation_plan.md`
- `wasm_todomvc_parity_plan.md`

---

## 1. Goal

Provide fast, deterministic feedback loops for Wasm engine development while preserving comprehensive browser-level parity checks.

Key objective: make daily Wasm iteration fast without sacrificing final confidence.

---

## 2. Baseline Observations

- Existing expected-runner currently forces DD engine.
- Existing expected suites already classify examples (`static`, `interactive`, `timer`, `computation`).
- Browser integration path is accurate but slower due to refresh/polling/extension round-trips.
- `RunAndCaptureInitial` command already exists and can reduce initial-state wait cost.
- `todo_mvc.expected` is long and should not be in the default fastest loop.

---

## 3. Testing Pyramid for Wasm

## Tier 0: IR and lowering unit tests (fastest)

Scope:

- parser + resolver + lowering shape,
- operator lowering snapshots (`LATEST`, `THEN`, `WHEN`, `WHILE`, `HOLD`, `FLUSH`),
- diagnostics tests (span + message quality).

Runtime target: seconds.

## Tier 1: Wasm runtime harness tests (fast)

Scope:

- generate Wasm from fixtures,
- instantiate in lightweight host harness,
- inject synthetic events,
- assert emitted patch stream and final state.

No browser required.

Runtime target: under ~30s for common suite.

## Tier 2: Playground smoke tests (medium)

Scope:

- run built-in examples in real playground with `Wasm` selected,
- verify preview output appears and no runtime errors.

Runtime target: ~1-3 minutes.

## Tier 3: Full expected parity tests (slowest)

Scope:

- full `.expected` interaction + persistence suites,
- includes long `todo_mvc` scenario.

Runtime target: several minutes.

---

## 4. Test Profiles

## `quick` profile (local default)

- Tier 0 + selected Tier 1 fixtures,
- no browser startup,
- includes:
  - static examples,
  - operator semantics fixtures,
  - list mutation fixtures,
  - compact todo trace fixture.

## `smoke` profile

- Tier 2 browser smoke with `Wasm` engine,
- broad integration confidence at moderate cost.

## `parity` profile

- Tier 3 full expected suite in `Wasm` mode,
- includes persistence and timer-sensitive checks.

---

## 5. Fast-Path Optimizations

## 5.1 Engine selection helper must be generic

Replace DD-only helper logic with:

- `ensure_engine(port, "Wasm")`

This is required to run existing expected infrastructure against Wasm quickly and consistently.

## 5.2 Use `RunAndCaptureInitial` for immediate checks

For tests that validate initial render state:

- trigger run and capture preview in one atomic command,
- reduce initial polling and wait overhead,
- reduce timer race windows.

## 5.3 Deterministic virtual time in Tier 1 harness

For timer scenarios (`interval`, `interval_hold`):

- drive ticks explicitly in harness,
- avoid real sleeps in fast loops,
- keep browser timer checks for parity profile.

## 5.4 Keep persistence checks in parity profile by default

Persistence checks require refresh and are expensive. They should run in:

- `parity` by default,
- optional in `smoke` when needed.

## 5.5 Prefer textual/assertion checks over screenshots

- use preview text, console, focus, and style assertions first,
- keep screenshots as failure artifacts only.

---

## 6. TodoMVC Testing Strategy (Fast + Full)

## 6.1 Two-level TodoMVC coverage

1. **`todo_mvc_wasm_smoke`** (new compact scenario)
   - add item,
   - toggle item,
   - filter switch,
   - edit enter/escape,
   - clear completed,
   - one persistence check.

2. **`todo_mvc` full parity**
   - existing comprehensive `.expected` sequence.

## 6.2 Harness-level todo trace fixture

Before browser parity, run deterministic event-trace fixture that validates:

- row identity stability,
- aggregate counts,
- edit save/cancel behavior,
- filter-membership transitions.

This catches most logic regressions quickly.

---

## 7. CI Scheduling

## On every PR

- Tier 0 mandatory,
- Tier 1 mandatory,
- Tier 2 smoke when Wasm-related files change.

## Pre-merge gate

- Tier 2 smoke mandatory,
- focused parity subset (counter/pages/shopping/todo smoke).

## Nightly

- full Tier 3 parity in Wasm mode.

---

## 8. Metrics and Targets

Track:

- profile duration (`quick`, `smoke`, `parity`),
- flaky failure count,
- first-failure localization quality.

Initial timing targets:

- `quick` < 60s,
- `smoke` < 3m,
- `parity` < 12m.

---

## 9. Planned Deliverables (Implementation Phase)

1. Generic engine selection helper in test runner (`Wasm` support).
2. Wasm test profiles and command switches.
3. Tier 1 Wasm harness fixtures.
4. TodoMVC smoke split from full parity path.
5. CI profile wiring and reporting.

---

## 10. Open Questions

1. **Tier 1 Harness Design**: What runtime hosts the WASM module outside the browser?
   Wasmtime? wasm3? Custom harness? This affects how events are injected and patches
   are read. Need a concrete fixture example showing input/output format.

2. **Deterministic Virtual Time**: How is time virtualized in the Tier 1 harness?
   Does the WASM module import a `now()` function that the harness controls? Or does
   the harness inject synthetic timer events? Design needed.

3. **Timing Target Justification**: The 60s/3m/12m targets are estimates. After
   implementing Tier 0+1, measure actual times and adjust. The parity target (12m)
   may be too generous — if TodoMVC is slow, something is wrong architecturally.

4. **Flaky Test Handling**: Timer-based tests (interval, interval_hold) are inherently
   timing-sensitive. The virtual time approach in Tier 1 solves this, but Tier 2/3
   run in a real browser. Need a tolerance/retry policy for browser-based timer tests.

---

## 11. Deferred Execution Note

This is a planning artifact only. Implementation is deferred.
