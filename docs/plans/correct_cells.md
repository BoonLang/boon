# Correct Cells: Full 7GUIs Spreadsheet on All Three Engines

## 1. Goal

The Cells example (7GUIs Task 7) must work correctly on all three Boon engines — Actors, DD, and Wasm — with the full 7GUIs specification: 26 columns (A-Z) × 30 rows (1-30), formula evaluation, reactive propagation, and interactive editing. No engine may be skipped. Performance must be acceptable (render < 5s) on all engines.

## 2. Current State

| Engine | Grid | Status | Blocker |
|--------|------|--------|---------|
| Actors | 10×10 | PASS (6s) | Per-actor async task overhead limits grid size |
| DD | — | SKIP | Self-referencing `cells` → infinite recursion in eval_static_with_scope |
| Wasm | — | SKIP | 11 missing pipe functions + inline depth guard blocks recursive functions |

## 3. 7GUIs Cells Specification

Per the [7GUIs spec](https://eugenkiss.github.io/7guis/tasks#cells):
- Grid of cells A0-Z99 (we use A1-Z30 = 780 cells)
- Each cell displays a value (number or text)
- Double-click to edit formula
- Formulas: plain numbers, cell references (=A1), binary ops (=add(A1, B1)), range ops (=sum(A1:A3))
- Reactive propagation: changing a cell updates all dependents
- Enter confirms, Escape cancels editing

---

## 4. Actors Engine: Synchronous Reactive Graph (SRG)

### 4.1 Problem

Each actor spawns: 4 bounded mpsc channels + 1 oneshot channel + 1 async task via `Task::start_droppable` + 1 `Arc<AtomicU64>` version. Cost: ~600 bytes per actor.

For 26×30 grid: 780 cells × ~15 actors/cell = ~12,000 async tasks on single-threaded WASM runtime.

The bottleneck is **channel indirection**, not executor scheduling. Each value hop costs: `sender.send()` → buffer lock → executor wakeup → task poll → `.next().await` → process (~10-20x overhead vs direct function call). With 15 hops/cell × 780 cells = 11,700 channel sends per single user event. A custom topological-aware executor helps ~2-3x (batching), but eliminating channels entirely gives **10-20x** improvement. A custom executor essentially converges toward SRG anyway.

### 4.2 Solution: Synchronous Push-Based Propagation

Replace per-node async tasks with synchronous reactive nodes. **Target: 0 async tasks** for Cells (and most examples). Only Timer/interval needs async.

#### 4.2.1 SyncNode

Replaces `ValueActor` for all non-async reactive expressions:

```rust
pub struct SyncNode {
    value: Value,                          // stored inline, no channel
    kind: NodeKind,                        // how to compute (see below)
    dependents: SmallVec<[NodeId; 4]>,     // downstream edges
    topo_order: u32,                       // for glitch-free propagation
    scope_id: ScopeId,                     // lifecycle management
    dirty: bool,                           // propagation flag
    on_change: Option<Box<dyn Fn(&Value)>>,// bridge notification
    #[cfg(feature = "boon-monitor")]
    history: Vec<(u64, Value)>,            // value history per epoch
}
```

#### 4.2.2 NodeKind

Uniform enum replacing heterogeneous actor types:

```rust
pub enum NodeKind {
    Constant,                              // never changes, zero cost
    Compute { inputs, evaluate },          // pure function of inputs
    Source,                                // external injection (LINK events)
    HoldState { body_events_node, state_update }, // stateful accumulator
    LatestMerge { input_slots, last_keys },       // merge multiple inputs
    WhenMatch { piped, match_fn },         // one-shot pattern match
    WhileMatch { piped, match_fn },        // continuous pattern match
    ThenCopy { piped, body_fn },           // copy on trigger
    Wire { source },                       // alias/forward
    AsyncReceiver,                         // bridge from Timer/network
}
```

#### 4.2.3 NodeRegistry

Centralized, index-based (no Rc<RefCell>):

```rust
pub struct NodeRegistry {
    nodes: Vec<Slot<SyncNode>>,            // arena with generational indices
    async_actors: Vec<Slot<OwnedActor>>,   // Timer/interval only
    propagation_queue: PropagationQueue,   // reused across cycles
    next_topo_order: u32,
}
thread_local! { pub static NODE_REGISTRY: RefCell<NodeRegistry> = ...; }
```

### 4.3 Propagation Algorithm (Iterative, Glitch-Free)

When an external event arrives (DOM click, timer tick):
1. **Inject**: Set source node's value directly
2. **Mark dirty**: Add all dependents to min-heap (sorted by topo_order)
3. **Propagate**: Process queue iteratively (NOT recursive — no stack overflow for 780+ cells):
```
while let Some(node_id) = queue.dequeue_min() {
    new_value = evaluate_node(node_id);
    if value_changed {
        node.value = new_value;
        fire on_change callback (DOM update);
        enqueue all dependents;
    }
}
```
4. **Batch persistence**: Write all changed HOLD states to localStorage once at end

**Glitch prevention**: Topological ordering ensures a node is only evaluated after ALL its inputs have been updated in this cycle. Same principle as DD engine's `worker.step()`.

**Stack overflow prevention**: Entire loop is iterative. 780-cell chain = 780 iterations, not 780 stack frames.

### 4.4 Constructs Without Async

#### HOLD

State stored directly in SyncNode. Body evaluation is synchronous (closures read from registry by NodeId). Sequential processing for Stream/pulses: iterate values, update state between each. No LazyValueActor, no BackpressureCoordinator, no async permits.

#### LATEST

Track latest value per input slot. When any input changes (becomes dirty in propagation), LATEST re-evaluates. Deduplication: only propagate if output actually changed.

#### LINK/Events

DOM addEventListener callback → `inject_value(registry, link_node, event_value)` → propagate(). No per-element ActorLoop, no per-event NamedChannel, no switch_map chains.

### 4.5 Hybrid Architecture: Sync + Async Coexistence

After migration, the Actors engine contains **both** sync and async actors in the same registry:

| Type | Storage | When Used | Examples |
|------|---------|-----------|----------|
| **SyncNode** | `registry.nodes` | Default for everything | HOLD, LATEST, WHEN, WHILE, THEN, LINK, constants, computations |
| **Async Actor** | `registry.async_actors` | Only genuinely async ops | Timer/interval, future network requests, web workers |

They connect through `AsyncReceiver` SyncNodes:
```
Timer (async task) --inject_value()--> AsyncReceiver SyncNode --> propagate() --> dependents (all sync)
```

Per-example breakdown:
- **Cells (26×30)**: ~8,060 SyncNodes + **0 async actors**
- **Timer**: ~50 SyncNodes + **1 async actor** (interval)
- **Counter**: ~20 SyncNodes + **0 async actors** (click events enter via inject_value)
- **Todo MVC**: ~300 SyncNodes + **0 async actors**

The existing `ValueActor` code stays in the codebase — it's just used rarely. Both types share the same `ActorHandle` interface, so the bridge and evaluator work with either transparently.

### 4.6 AsyncBridge (Timer/interval only)

```rust
// Timer is the ONLY async task in the entire system
let task = ActorLoop::new(async move {
    loop {
        Timer::sleep(ms).await;
        NODE_REGISTRY.with(|reg| inject_value(&mut reg.borrow_mut(), node_id, tick));
    }
});
```

Single-threaded WASM guarantees: `inject_value` + `propagate()` runs to completion before any other task.

### 4.7 Migration: Incremental via Dual ActorHandle

```rust
pub enum ActorHandle {
    Async { /* existing channels + ActorLoop */ },
    Sync { node_id: NodeId },  // NEW: just an index
}
```

Bridge compatibility via stream adapter:
```rust
fn sync_node_stream(node_id: NodeId) -> LocalBoxStream<'static, Value> {
    // Returns current value + on_change notifications as stream
    // Bridge works UNCHANGED during migration
}
```

Migration order (each step independently testable):
1. Literals/Constants → SyncNode::Constant
2. BLOCK vars, binary ops, pipes → SyncNode::Compute
3. WHILE/WHEN → SyncNode::WhileMatch/WhenMatch
4. LATEST → SyncNode::LatestMerge
5. HOLD → SyncNode::HoldState
6. THEN → SyncNode::ThenCopy
7. LINK events → SyncNode::Source + direct inject_value
8. Bridge event dispatch → remove ActorLoop per element

### 4.8 Node Count: 26×30 Grid

| Component | Current (Async Actors) | SRG (SyncNodes) |
|-----------|----------------------|-----------------|
| Per cell | ~15 ValueActors | ~10-12 SyncNodes |
| Constants (775 empty cells) | 775 actors | 775 Constant nodes (zero cost) |
| LINK sources (780×2) | 1,560 actors | 1,560 Source nodes |
| HOLD state (780×2) | 1,560 actors | 1,560 HoldState nodes |
| Grid rendering | ~200 actors | ~200 nodes |
| **Total** | **~12,000 async tasks** | **~8,060 SyncNodes + 0 async tasks** |
| **Memory** | **~7.2 MB** (600 bytes/actor) | **~1.6 MB** (200 bytes/node) |

### 4.9 Files to Modify

| File | Change |
|------|--------|
| `engine_actors/engine.rs` | SyncNode, NodeKind, NodeRegistry, PropagationQueue, propagation algorithm |
| `engine_actors/evaluator.rs` | Emit SyncNode creation instead of create_actor for most constructs |
| `engine_actors/bridge.rs` | ActorHandle::Sync variant, inject_value dispatch, on_change callbacks |

---

## 5. Wasm Engine: Function Call Stack + Missing Functions

### 5.1 Problem: Inline Explosion

The Wasm engine inlines ALL function calls at compile time. Cells has 12 mutually recursive functions (formula evaluator). With inline depth guard = 64, the recursive evaluator hits the limit immediately:

```
evaluate_expression → parse_function_call → evaluate_expression (recursive)
evaluate_expression → parse_cell_ref → cell_lookup (nested List/get)
find_comma_depth → find_comma_depth (recursive)
```

For 780 cells, each inlining the full formula evaluator: estimated 100K-500K nodes (factorial explosion from recursion × cells).

### 5.2 Solution: WASM Function Compilation

Instead of inlining all Boon function calls, compile user-defined functions as separate WASM functions and use `call` instructions.

#### 5.2.1 IR Changes

```rust
// ir.rs — new node types

/// A compiled function body (lives in the function table)
pub struct IrFunction {
    pub func_id: FuncId,
    pub params: Vec<(String, CellId)>,  // param name → param cell (as WASM locals)
    pub return_cell: CellId,
    pub body_nodes: Vec<IrNode>,  // nodes inside this function
}

/// Call a compiled function at runtime
IrNode::Call {
    cell: CellId,          // result cell
    func_id: FuncId,       // which function to call
    args: Vec<CellId>,     // argument cells
}
```

#### 5.2.2 Lowering Changes

In `lower.rs`, when encountering a function call:

```
fn lower_function_call(name, args) {
    if self.should_inline(name) {
        // Current behavior: inline for small, non-recursive functions
        self.inline_function_call(name, args)
    } else {
        // NEW: compile as Call node
        let func_id = self.get_or_compile_function(name);
        self.emit(IrNode::Call { cell, func_id, args })
    }
}

fn should_inline(name) -> bool {
    // Inline if: function is small (< 20 nodes) AND not recursive
    let def = self.get_function(name);
    !def.is_recursive && def.estimated_nodes < 20
}
```

**Recursion detection**: Build a call graph during lowering. If function A calls B which calls A (directly or transitively), mark both as recursive. All 12 formula evaluator functions would be marked recursive.

#### 5.2.3 Codegen Changes

**IMPORTANT: Parameters use WASM locals, not globals.** Globals would be overwritten by recursive calls.

```rust
// codegen.rs — emit function bodies as separate WASM functions

fn emit_function(func: &IrFunction) {
    // Declare params as WASM function parameters (locals)
    // Each recursive call gets its own locals on the WASM call stack
    // Body reads params from locals, not globals
    // Return value via WASM return value
}

// For IrNode::Call, emit:
fn emit_call(f: &mut Function, call: &CallNode) {
    // Push args onto WASM value stack
    for arg in &call.args {
        f.instruction(&Instruction::GlobalGet(arg.0));  // read current cell value
    }
    // Call the function (args consumed from WASM stack, result pushed)
    f.instruction(&Instruction::Call(func_wasm_idx));
    // Store result in output cell
    f.instruction(&Instruction::GlobalSet(call.cell.0));
}
```

#### 5.2.4 Parameter Passing

Functions use WASM locals for parameters and WASM return for results:
- Caller pushes argument values onto WASM value stack → calls function → pops result
- Recursive calls each get their own locals on the WASM call stack (no global corruption)
- Non-param cells inside function bodies still use globals (shared state for reactive cells)

#### 5.2.5 Node Count Impact

| Component | Inlined (current) | With Call Stack |
|-----------|-------------------|----------------|
| Formula evaluator (12 functions) | ~500 nodes × 780 cells = 390K | ~500 nodes (shared) + 1 Call per cell × 780 = 1,280 |
| Cell constructor (new_cell) | ~50 nodes × 780 cells = 39K | ~50 nodes × 780 = 39K (still inlined, small) |
| Grid rendering | ~5K | ~5K |
| **Total** | **~434K nodes** | **~45K nodes** |

The function call stack reduces node count by **~10×** for the Cells example.

### 5.3 Missing Pipe Functions (11 functions)

These must be implemented regardless of the call stack. Each follows the established pattern.

| # | Function | Type Sig | Host Import | Notes |
|---|----------|----------|-------------|-------|
| 1 | Text/length | (i32)→f64 | host_text_length | `text.chars().count()` |
| 2 | Text/char_at(index:) | (i32,i32,i32)→() | host_text_char_at | NEW type sig |
| 3 | Text/char_code | (i32)→f64 | host_text_char_code | First char as u32 |
| 4 | Text/from_char_code | (i32,i32)→() | host_text_from_char_code | Read f64, write char |
| 5 | Text/find(search:) | (i32,i32)→f64 | host_text_find | Position or -1 |
| 6 | Text/substring(start:,length:) | (i32,i32,i32,i32)→() | host_text_substring | NEW type sig |
| 7 | Text/is_empty | (i32)→f64 | host_text_is_empty | IrNode already exists |
| 8 | Math/modulo(divisor:) | — | — | Pure WASM: `a - floor(a/b)*b` |
| 9 | List/product | (i32)→f64 | host_list_product | Multiply all values |
| 10 | List/range(from:,to:) | (i32,i32,i32)→() | host_list_range | Create [from..=to] |
| 11 | List/get(index:) | (i32,i32)→f64 | host_list_get | 1-based access |

**Per function, 5 files changed**: ir.rs (IrNode variant), lower.rs (dispatch), codegen.rs (import + emit), runtime.rs (host closure), bridge.rs (dep tracking).

**New WASM type signatures needed**:
- Type 13: `(i32, i32, i32) → ()` — for Text/char_at, List/range
- Type 14: `(i32, i32, i32, i32) → ()` — for Text/substring

### 5.4 Files to Modify

| File | Change |
|------|--------|
| `engine_wasm/ir.rs` | ~11 IrNode variants + IrFunction struct + Call node + BinOp::Mod |
| `engine_wasm/lower.rs` | Recursion detection, should_inline(), function compilation, 11 pipe arms |
| `engine_wasm/codegen.rs` | Function table, Call emit with WASM locals, 2 new type sigs, ~10 host imports |
| `engine_wasm/runtime.rs` | ~10 host function closures |
| `engine_wasm/bridge.rs` | ~11 match arms + function-related dep tracking |

---

## 6. DD Engine: Deferred Collection Mechanism

### 6.1 Problem

The DD compiler's `compile_list_map` (compile.rs:6377-6419) creates a closure that calls `eval_static_with_scope` on the map's `new:` expression. When that expression references `cells` (the variable being defined), infinite recursion occurs because:

1. Variables are NOT pre-allocated before compilation
2. The closure captures the static `Compiler`, not the reactive runtime
3. `resolve_alias_static` finds the AST for `cells` and re-enters `eval_static_with_scope`
4. There is NO mechanism for closures to access reactive runtime values

### 6.2 Solution: Late-Binding Variable Resolution

Add a runtime variable registry that closures can query, plus cycle detection during compilation.

#### 6.2.1 Compilation-Time: Cycle Detection + VarId Pre-allocation

**Step 1: Add Pass 0** — Pre-allocate VarIds for all reactive variables before compiling any expressions.

```rust
// compile.rs — modify compile_program()

fn compile_program(&mut self) {
    // Pass 0 (NEW): Pre-allocate VarIds
    for (name, _expr) in &self.compiler.variables {
        let var_id = self.alloc_var();
        self.pre_allocated_vars.insert(name.clone(), var_id);
    }

    // Pass 1 (existing): Compile reactive variables
    // Now can reference pre_allocated_vars during compilation

    // Pass 2 (existing): Compile document
}
```

**Step 2: Add cycle detection** to `resolve_alias_static`:

```rust
// compile.rs — add to BoonDdCompiler

currently_evaluating: RefCell<HashSet<String>>,

fn resolve_alias_static(&self, name: &str, scope: &IndexMap<String, Value>) -> Result<Value, String> {
    // Check local scope first
    if let Some(v) = scope.get(name) { return Ok(v.clone()); }

    // NEW: Cycle detection
    if self.currently_evaluating.borrow().contains(name) {
        // Return a deferred reference marker
        if let Some(var_id) = self.pre_allocated_vars.get(name) {
            return Ok(Value::DeferredRef(name.to_string(), *var_id));
        }
    }

    self.currently_evaluating.borrow_mut().insert(name.to_string());
    let result = /* existing evaluation */;
    self.currently_evaluating.borrow_mut().remove(name);
    result
}
```

#### 6.2.2 Runtime: Collection Registry for Late Binding

**Step 3: Add a runtime collection registry** that map closures can query:

```rust
// runtime.rs — add to materialization context

pub struct MaterializationContext {
    /// Registry of materialized collections, accessible by name.
    /// Map closures use this for late-binding variable resolution.
    pub collection_values: RefCell<HashMap<String, Value>>,
}
```

**Step 4: Modify CollectionSpec::Map** to support deferred resolution:

```rust
// compile.rs — modify compile_list_map closure

CollectionSpec::Map {
    source: source_var,
    f: Arc::new(move |list: &Value, ctx: &MaterializationContext| {
        list.list_map(|item| {
            let mut scope = IndexMap::new();
            scope.insert(param_name.clone(), item.clone());

            // NEW: Resolve DeferredRef values from the runtime registry
            let resolved_scope = resolve_deferred_refs(&scope, &ctx.collection_values);

            compiler.eval_static_with_scope(&new_expr, &resolved_scope)
        })
    }),
}
```

**Step 5: Update materialization** to populate the registry:

```rust
// runtime.rs — modify materialization loop

for (var_id, name, spec) in &graph.collections {
    let collection = materialize_spec(spec, &ctx);
    collections.insert(var_id, collection);

    // NEW: Update the registry with the latest snapshot
    if let Some(snapshot) = collection.current_value() {
        ctx.collection_values.borrow_mut().insert(name.clone(), snapshot);
    }
}
```

#### 6.2.3 NaN Pattern Matching

Add NaN tag support to `match_pattern` (compile.rs:2736):

```rust
Pattern::ValueComparison { op: CmpOp::Eq, value } => {
    match (input_value, value) {
        // Tag comparison: NaN, True, False, etc.
        (Value::Tag(t1), Value::Tag(t2)) if t1 == t2 => Some(input_value.clone()),
        // Number comparison
        (Value::Number(n1), Value::Number(n2)) if n1 == n2 => Some(input_value.clone()),
        _ => None,
    }
}
```

#### 6.2.4 Depth Limit

Increase `MAX_EVAL_DEPTH` from 150 to 500 for deeply nested formula evaluation across 780 cells.

### 6.3 Files to Modify

| File | Change |
|------|--------|
| `engine_dd/core/compile.rs` | Pass 0 VarId pre-alloc, cycle detection, DeferredRef, NaN patterns, depth |
| `engine_dd/core/types.rs` | Add `Value::DeferredRef` variant, `CollectionSpec::Map` signature change |
| `engine_dd/io/runtime.rs` | MaterializationContext, collection registry, deferred resolution |
| `engine_dd/io/operators.rs` | Pass context to map closure evaluation |
| `engine_dd/render/bridge.rs` | Handle DeferredRef in document rendering |

---

## 7. cells.bn: Full 26×30 Grid

The current cells.bn already supports configurable grid size. Restore to full 7GUIs spec:

```diff
-List/range(from: 1, to: 10) |> List/map(...)  -- rows
+List/range(from: 1, to: 30) |> List/map(...)  -- rows

-List/range(from: 1, to: 10) |> List/map(...)  -- columns
+List/range(from: 1, to: 26) |> List/map(...)  -- columns
```

Four `List/range` calls to change (data rows, data columns, header columns, rendering rows).

---

## 8. Testing

### 8.1 Updated cells.expected

```toml
[test]
category = "interactive"
description = "7GUIs Task 7: spreadsheet with formulas and change propagation"
# No skip_engines — works on all three engines

[output]
text = "Cells"
match = "contains"

[timing]
timeout = 15000
poll_interval = 500
initial_delay = 3000

# Static assertions
[[sequence]]
description = "Title visible"
actions = [["assert_contains", "Cells"]]
expect = "Cells"

[[sequence]]
description = "Column A header"
actions = [["assert_contains", "A"]]
expect = "Cells"

[[sequence]]
description = "Column Z header"
actions = [["assert_contains", "Z"]]
expect = "Cells"

[[sequence]]
description = "Default A1=5"
actions = [["assert_contains", "5"]]
expect = "Cells"

[[sequence]]
description = "Default A2=10"
actions = [["assert_contains", "10"]]
expect = "Cells"

[[sequence]]
description = "Default B1=15 (add(A1,A2))"
actions = [["assert_contains", "15"]]
expect = "Cells"

[[sequence]]
description = "Default C1=30 (sum(A1:A3))"
actions = [["assert_contains", "30"]]
expect = "Cells"

# Interactive editing (if supported by engine)
[[sequence]]
description = "Row 30 visible (scroll)"
actions = [["assert_contains", "30"]]
expect = "Cells"
```

### 8.2 Per-Engine Verification

```bash
# After each phase, verify:
boon-tools exec test-examples --filter cells --engine Actors
boon-tools exec test-examples --filter cells --engine DD
boon-tools exec test-examples --filter cells --engine Wasm

# Full regression:
boon-tools exec test-examples --engine Actors  # expect 13/17
boon-tools exec test-examples --engine DD      # expect 11/14 (cells now passes)
boon-tools exec test-examples --engine Wasm    # expect 10/13 (cells now passes)
```

### 8.3 Performance Criteria

| Engine | Grid | Max Render Time | Max Node/Actor Count |
|--------|------|----------------|---------------------|
| Actors | 26×30 | < 5s | 0 async tasks, < 10,000 SyncNodes |
| DD | 26×30 | < 3s | N/A (no actors) |
| Wasm | 26×30 | < 3s | < 50K IR nodes (with function call stack) |

---

## 9. Implementation Order

### Phase A: Wasm Missing Pipe Functions
- Add 11 pipe functions (mechanical, well-patterned)
- Verify on 10×10 grid first
- Files: ir.rs, lower.rs, codegen.rs, runtime.rs, bridge.rs

### Phase B: Wasm Function Call Stack
- Recursion detection in lowerer
- IrFunction + IrNode::Call with WASM locals for parameters
- WASM function table generation
- Verify recursive formula evaluator works
- Restore 26×30 grid on Wasm

### Phase C: DD Deferred Collections
- Pass 0 VarId pre-allocation
- Cycle detection in resolve_alias_static
- Value::DeferredRef + MaterializationContext
- NaN pattern matching
- Verify 26×30 grid on DD

### Phase D: Actors Synchronous Reactive Graph (SRG)
- Core: SyncNode + NodeKind + NodeRegistry + PropagationQueue
- Propagation: iterative topological-order processing (no stack overflow)
- Incremental migration via dual ActorHandle (Async | Sync)
- Migrate construct-by-construct: Constants → Compute → WHILE/WHEN → LATEST → HOLD → THEN → LINK
- AsyncBridge for Timer/interval only
- **0 async tasks for Cells, ~200 bytes/node instead of ~600 bytes/actor**

### Phase E: Integration + Monitoring
- Restore cells.bn to 26×30
- Update cells.expected (remove all skip_engines)
- Full test suite on all engines
- Add `#[cfg(feature = "boon-monitor")]` value history + propagation tracing
- Performance validation

---

## 10. Persistence Design (SRG)

localStorage is **synchronous** in browsers. SRG handles persistence without async:

1. **HOLD state persistence**: At end of propagation cycle, batch-write all changed HOLD values to localStorage.
2. **State loading**: On startup, before first propagation, load persisted values synchronously.
3. **No async persistence path needed** for browser. Future IndexedDB (async) would go through AsyncBridge.

---

## 11. Monitoring & Logging Design (SRG)

The centralized NodeRegistry enables powerful observability that scattered async actors cannot provide:

- **Value History**: Per-node time series `Vec<(epoch, Value)>` behind `#[cfg(feature = "boon-monitor")]`
- **Propagation Tracing**: Log trigger node, every evaluation (old/new value, changed flag), duration per cycle
- **Graph Export**: Nodes + edges + current values for live dependency visualization
- **Future Boon Graph Monitor**: Real-time graph visualization, propagation replay, performance profiling, breakpoints, time-travel debugging

---

## 12. Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| DD deferred resolution doesn't propagate formula updates | Formulas show 0 | Ensure CollectionSpec::Map closure re-evaluates when registry changes |
| Wasm function call stack breaks existing examples | Regression | Only use Call for recursive/large functions; keep inlining for small ones |
| WHILE arm switching lifecycle (creating/destroying sub-graphs) | Leaked nodes | Use child scopes per arm, destroy scope on switch |
| List/map creates sub-graphs dynamically | Complex lifecycle | Lists are single nodes holding Value::List; map creates per-item SyncNodes in child scopes |
| Reentrant propagation from on_change callbacks | Infinite loop | on_change is for side effects only; queue re-injections for after cycle |
| Self-referencing `cells` cycle | Infinite recursion | Pre-allocate node with placeholder → evaluate grid → set final value |
| Deep recursion in DD formula eval | Stack overflow | Increase MAX_EVAL_DEPTH to 500 |
| Wasm NaN tag vs SKIP sentinel conflict | Wrong formula results | Verify NaN tag index differs from SKIP_SENTINEL_BITS |
| Existing tests rely on async timing | False failures | Run full suite after each migration step |

---

## 13. Architecture Principles

1. **Correctness over speed**: Option B everywhere. No placeholders, no hacks, no fallbacks.
2. **Sync by default, async by exception**: Only Timer/interval and future network/workers use async tasks.
3. **Centralized state**: All reactive state in one NodeRegistry — enables monitoring, persistence, debugging.
4. **Iterative propagation**: No recursive callbacks — handles any graph depth without stack overflow.
5. **No language changes**: The cells.bn code stays the same across all engines. Engine differences are internal.
6. **Incremental verification**: Each phase delivers testable progress. No big-bang integration.
7. **Incremental migration**: Each construct migrated independently via dual ActorHandle, tested after each step.
