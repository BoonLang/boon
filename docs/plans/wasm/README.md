# WASM Engine Plans

Boon's third engine: compile Boon source directly to WASM. Standalone `engine_wasm/`
module — does NOT depend on DD or Actors engine internals. Consumes the shared parser
AST, has its own analysis + codegen pipeline.

## Reading Order

1. **`wasm_engine_direct_compilation_plan.md`** — THE implementation plan.
   Milestones M0-M6, operator lowering with IR/WASM examples, host callback ABI,
   feature gating, build matrix. **Start here.**

2. **`wasm_todomvc_parity_plan.md`** — Behavioral spec for M5 (TodoMVC).
   What must work, parity phases, correctness invariants.

3. **`wasm_fast_testing_plan.md`** — Testing pyramid (Tiers 0-3).
   How to test quickly during development without full browser round-trips.

## Supporting Context (in parent directory)

- `../boon_as_systems_language.md` **sections 5.6-5.8** — IR design sketch,
  memory management strategy (manual refcount recommended), error handling
  (sentinel values for FLUSH).
- `../performance_landscape.md` — Realistic performance expectations. Compilation
  matters for large lists and rapid events, not simple interactions.

## Target Architecture

```
crates/boon/src/platform/browser/
├── engine_actors/    # existing — peer, not dependency
├── engine_dd/        # existing — peer, not dependency
└── engine_wasm/      # NEW
    ├── mod.rs
    ├── analysis/     # AST -> reactive IR
    │   ├── ir.rs     # IR node types
    │   └── lower.rs  # AST -> IR lowering
    ├── codegen/      # IR -> WASM binary
    │   └── emit.rs   # wasm-encoder tree-walk emitter
    └── runtime/      # host-side glue
        ├── host.rs   # imported functions (patches, timers, persistence)
        └── dispatch.rs
```

## First File to Touch

`crates/boon/src/platform/browser/common.rs` — add `EngineType::Wasm` variant,
then `crates/boon/Cargo.toml` — add `engine-wasm` feature. That's M0.
