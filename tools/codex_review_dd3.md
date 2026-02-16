You are reviewing a Differential Dataflow (DD) engine implementation for the Boon programming language.

Read all .rs files under crates/boon/src/platform/browser/engine_dd/ and also read docs/plans/dd_v2_architecture.md for the intended architecture.

The core principle: ALL reactive computation must flow through Differential Dataflow collections and operators. No imperative interpreter fallback.

## Checks (report PASS or FAIL with file:line evidence for each):

1. File `io/general.rs` does NOT exist (imperative interpreter must be deleted)
2. No `CompiledProgram::General` variant in compile.rs (no raw AST fallback)
3. Files in `core/` have ZERO imports of: zoon, web_sys, RefCell, Mutable, thread_local
4. A `DataflowGraph` struct exists with `CollectionSpec` entries in topological order
5. A `materialize()` function exists that turns DataflowGraph into live DD collections using timely/differential-dataflow
6. Event injection goes through InputSession.update() + worker.step() — not Rc<RefCell> mutation
7. Output observation uses DD inspect/capture — not synchronous Rc<RefCell> reads
8. Per-item list state (like TodoMVC per-todo HOLDs) uses keyed DD collections, not Vec<Value>
9. No `to_vec()` or `Arc<Vec<Value>>` anywhere in engine_dd/
10. CollectionSpec enum covers at minimum: Literal, Input, HoldLatest, HoldState, Then, Map, FlatMap, Join, Concat, ListCount, ListRetain, ListRetainReactive, ListMap, ListAppend, ListRemove, SideEffect

## Output format:
```
CHECK 1: PASS/FAIL — [evidence]
CHECK 2: PASS/FAIL — [evidence]
...
OVERALL: PASS/FAIL (X/10 checks passed)
```
