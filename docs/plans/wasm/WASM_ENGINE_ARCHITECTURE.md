# WASM Engine Architecture

## Overview

The WASM engine compiles Boon source code directly to WebAssembly, bypassing the
actor-based and differential-dataflow engines entirely. It provides a third engine
option in the playground, selectable via the engine dropdown.

## Pipeline

```
Boon source
    |
    v
Parser (shared with other engines)
    |  lexer → parser → scope resolver → static expressions
    v
IR Lowering (lower.rs)
    |  AST → typed reactive IR (cells, events, nodes)
    v
WASM Codegen (codegen.rs)
    |  IR → wasm-encoder → raw WASM bytes
    v
Host Runtime (runtime.rs)
    |  WebAssembly.Instance + host imports + CellStore/ListStore
    v
Bridge (bridge.rs)
    |  IR + WasmInstance → Zoon UI elements with reactive signals
    v
Playground Preview
```

## Key Files

| File | Purpose |
|------|---------|
| `mod.rs` | Entry point: `run_wasm_engine(source)` orchestrates the pipeline |
| `ir.rs` | IR types: `CellId`, `EventId`, `IrNode`, `IrExpr`, `IrProgram` |
| `lower.rs` | AST → IR lowering with span tracking for diagnostics |
| `codegen.rs` | IR → WASM binary using `wasm-encoder` crate |
| `runtime.rs` | Host-side WASM instantiation, `CellStore`, `ListStore`, `WasmInstance` |
| `bridge.rs` | Converts IR + runtime into Zoon UI elements with reactive bindings |

## Core Concepts

### Cells

Every reactive value is a **cell** — a WASM global (f64) paired with a host-side
`Mutable<f64>`. The WASM module stores numeric values in globals; the host mirrors
them in `CellStore` for reactive signal propagation to the UI.

Text values are stored separately in `CellStore::text_cells` since WASM globals
only hold f64. Text operations (trim, is_not_empty, copy) are host imports.

### Events

Events represent triggers: button clicks, timer ticks, text input changes, etc.
Each event has an `EventId` (u32 index) and optional payload cells (set by the
host before firing). The WASM `on_event(event_id)` export dispatches to handlers.

### Nodes

IR nodes represent the reactive graph:

| Node | Purpose |
|------|---------|
| `Derived` | Static computation (constant or expression) |
| `Then` | Trigger-bound: evaluate body when event fires |
| `Hold` | Stateful accumulator with self-reference |
| `Latest` | Multi-arm merge (last-event-wins) |
| `When` | Pattern match on source (frozen on match) |
| `While` | Pattern match with dependency tracking (re-evaluates) |
| `MathSum` | Running accumulator (sum of input values) |
| `PipeThrough` | Value pass-through (identity pipe) |
| `Timer` | Periodic event source |
| `Element` | UI element with LINK event bindings |
| `Document` | Root document node |
| `TextInterpolation` | Reactive text with embedded cell references |
| `ListAppend/Clear/Remove/Retain/Count/IsEmpty/Map` | List operations |
| `TextTrim/TextIsNotEmpty` | Text operations |
| `RouterGoTo` | Navigation |
| `CustomCall` | User-defined function inlining |

### Tags

Tags (e.g., `Active`, `Completed`, `True`, `False`) are interned as 1-based
indices in the tag table. The f64 cell value stores the encoded tag index.
This allows pattern matching via simple f64 equality comparison in WASM.

## WASM Module Structure

The generated module contains:

- **Globals**: One mutable f64 per cell + 1 temp global
- **Memory**: 1 page (for future text data in linear memory)
- **Imports**: 10 host functions (set_cell, list ops, text ops, etc.)
- **Exports**: `init()`, `on_event(i32)`, `set_global(i32, f64)`, `memory`

### Event Dispatch

`on_event` uses `br_table` for O(1) dispatch to event handlers. Each handler
evaluates its body expression, updates the target cell global, notifies the
host via `host_set_cell_f64`, and propagates to downstream nodes.

### Downstream Propagation

After a cell update, `emit_downstream_updates` scans for dependent nodes
(MathSum, PipeThrough, Derived/CellRead, When, While, TextTrim, etc.) and
emits inline code to re-evaluate and propagate changes. This is done at
compile time — the propagation logic is baked into the WASM binary.

## Host Runtime

### CellStore

Holds `Vec<Mutable<f64>>` for reactive signals and `Vec<String>` for text.
The bridge reads cell signals; host imports write cell values.

### ListStore

Manages host-side lists with:
- `Vec<Vec<f64>>` for numeric items
- `Vec<Vec<String>>` for text items
- `Vec<Mutable<f64>>` version counters (trigger reactive updates on mutation)

### WasmInstance

Wraps `WebAssembly::Instance` with cached function references (`on_event_fn`,
`set_global_fn`) to avoid repeated JS reflection lookups. Provides:
- `call_init()` — initialize all cells
- `fire_event(id)` — dispatch an event
- `set_cell_value(id, val)` — set cell on both host and WASM sides
- `start_timers()` — start JS `setInterval` for Timer nodes

## Bridge

The bridge walks IR nodes and builds Zoon UI elements:
- Elements read cell signals for reactive text, visibility, styles
- LINK events connect DOM events to WASM event dispatch
- `attach_common_events` wraps elements with hover/focus/blur/click handlers
- Lists use `build_list_map` with version signals for re-rendering
- `resolve_static_text` follows CellRead chains to find static button labels

## Feature Flags

The engine is behind the `engine-wasm` Cargo feature. All 7 combinations compile:
- Single: `engine-actors`, `engine-dd`, `engine-wasm`
- Dual: `actors+dd`, `actors+wasm`, `dd+wasm`
- All: `engine-all`

## Performance

- **br_table dispatch**: O(1) event routing and global setting
- **Cached JS functions**: No repeated `Reflect::get` for hot-path calls
- **Compile-time propagation**: Downstream updates baked into WASM binary
- **Signal-based reactivity**: Only changed cells trigger UI updates via Mutable signals

## Limitations

- **f64-only values**: All cell values are f64; text stored host-side
- **No per-item templates**: List items use simplified rendering (no per-item
  hover/edit/delete events). Full template instantiation deferred.
- **No FLUSH**: Error/flush semantics not yet implemented
- **No persistence**: Durable state not wired through WASM path
