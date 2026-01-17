# Pure DD Engine - Collections, Type Safety, Naming, and Structure

**Status:** ✅ PHASE 8 COMPLETE - Pure LINK Handling

> **LATEST ASSESSMENT (2026-01-17):**
>
> Phase 8 completed DD-native LINK handling with bridge pattern:
> 1. Added `LinkAction` and `LinkCellMapping` types for DD-native action processing
> 2. Implemented `apply_link_action()` for pure DD action application
> 3. Created `get_all_link_mappings()` bridge from `DYNAMIC_LINK_ACTIONS` to DD
> 4. All 11 examples pass with Phase 8 integration ✅
> 5. Fixed missing `update_cell_no_persist` function in outputs.rs
>
> Current architecture:
> - State storage: `Mutable<HashMap<String, Value>>` (DD Worker is SOLE writer)
> - LINK handling: Bridge converts IO actions to DD mappings at worker spawn
> - Business logic encoded in DD types (`LinkAction`, `LinkCellMapping`)
> - IO layer is thin routing layer (browser ↔ DD interface only)
>
> **Phase Status:**
>
> **Phase 1 (Module Restructure):** ✅ COMPLETE
> **Phase 2 (Hold → Cell):** ✅ COMPLETE
> **Phase 3 (Type-Safe IDs):** ✅ COMPLETE
> **Phase 4 (DD Infrastructure):** ⚠️ OPERATORS EXIST - Not integrated into main rendering path
> **Phase 5 (DD-First Architecture):** ❌ SUPERSEDED by Phase 6
> **Phase 6 (Single State Authority):** ✅ COMPLETE
> **Phase 7 (O(delta) List Operations):** ✅ COMPLETE
> **Phase 8 (Pure LINK Handling):** ✅ COMPLETE - Bridge pattern for incremental migration
> **Phase 9 (True Incremental Lists):** 🔴 NOT STARTED - Replace Arc<Vec> with CollectionHandle
> **Phase 10 (Eliminate inspect() Side Effects):** 🟡 IN PROGRESS - Using timely's capture() approach
> **Phase 11 (Move Business Logic to DD):** 🔴 NOT STARTED - Migrate 13 IO thread_locals to DD
> **Phase 12 (Incremental Rendering):** 🔴 NOT STARTED - Diff-based DOM updates
**Goal:** Transform the DD engine to fully leverage Differential Dataflow's incremental computation, eliminate string-based matching, establish clear naming, and clean module hierarchy.

---

## Table of Contents

1. [Boon → DD Construct Mapping](#boon--dd-construct-mapping)
2. [Phase 1: Module Restructure](#phase-1-module-restructure-remove-dd_-prefix)
3. [Phase 2: Hold → Cell Rename](#phase-2-hold--cell-rename)
4. [Phase 3: Type-Safe IDs and Tags](#phase-3-type-safe-ids-and-tags)
5. [Phase 4: DD Infrastructure](#phase-4-dd-infrastructure)
6. [Phase 5: DD-First Architecture](#phase-5-dd-first-architecture)
7. [Phase 6: Single State Authority](#phase-6-single-state-authority-simplified-pure-reactive)
8. [Phase 7: O(delta) List Operations](#phase-7-odelta-list-operations)
9. [Phase 8: Pure LINK Handling](#phase-8-pure-link-handling)
10. [Phase 9: True Incremental Lists](#phase-9-true-incremental-lists-odelta-end-to-end)
11. [Phase 10: Eliminate inspect() Side Effects](#phase-10-eliminate-inspect-side-effects)
12. [Phase 11: Move Business Logic from IO to DD](#phase-11-move-business-logic-from-io-to-dd)
13. [Phase 12: Incremental Rendering](#phase-12-incremental-rendering)
14. [Future Work](#future-work)

---

## Boon → DD Construct Mapping

### Understanding DD's Core Model

Differential Dataflow represents data as **collections with differences**:
```
Collection = Set of (Data, Time, Diff) triples

Insert:  (item, t, +1)
Delete:  (item, t, -1)
Update:  (old, t, -1) + (new, t, +1)
```

This is the Z-set algebra - DD already has it built in. No external libraries needed.

### HOLD → DD `unary` Operator

**Boon semantics** (from `docs/language/HOLD.md`):
```boon
initial |> HOLD state { body }
```

- `state` is a reactive reference to current accumulated value
- **Non-self-reactive**: HOLD doesn't retrigger when state changes
- Body evaluated only when external events fire
- Single-arm HOLD - always one update expression

**DD implementation** (already in `dd_runtime.rs`):

```rust
// HOLD as a stateful unary operator
pub fn hold_operator<G>(
    scope: &mut G,
    initial: Value,
    cell_id: CellId,
    body_fn: impl Fn(&Value) -> Stream<Value>,
) -> Stream<Value>
where
    G: Scope<Timestamp = u64>,
{
    // Internal state stored by CellId
    let state = Rc::new(RefCell::new(initial.clone()));

    // When body emits, update state and emit new value
    body_stream.unary(Pipeline, "Hold", |_cap, _info| {
        move |input, output| {
            input.for_each(|time, data| {
                for new_value in data.iter() {
                    *state.borrow_mut() = new_value.clone();
                    output.session(&time).give(new_value.clone());
                }
            });
        }
    })
}
```

**Key insight**: The `state` reference in Boon code becomes a read into the cell storage. The non-self-reactive property is automatic - the body only runs when external events trigger it.

### LATEST → DD `concat`

**Boon semantics** (from `docs/language/LATEST.md`):
```boon
LATEST {
    event1 |> THEN { value1 }
    event2 |> THEN { value2 }
    default_value
}
```

- Merges multiple event sources
- No self-reference
- Last event wins
- Starts UNDEFINED or with default

**DD implementation**:

```rust
// LATEST as collection concatenation
pub fn latest<G>(
    scope: &mut G,
    sources: Vec<Stream<Value>>,
    default: Option<Value>,
) -> Stream<Value>
where
    G: Scope,
{
    // Concatenate all source streams
    let merged = sources.into_iter()
        .map(|s| s.as_collection())
        .reduce(|a, b| a.concat(&b));

    // For scalar LATEST (single value), use consolidate + latest timestamp
    merged.consolidate().inner
}
```

**For scalar values** (most common in UI): LATEST tracks the most recent value from any input. DD's timestamp ordering naturally gives "last event wins".

### WHEN → DD `filter` + `map` (Frozen)

**Boon semantics** (from `docs/language/WHEN_VS_WHILE.md`):
```boon
state |> WHEN {
    StateA => StateB
    StateB => StateC
}
```

- **Frozen evaluation**: Evaluated once when input changes
- Dependencies inside branches are captured/frozen at that moment
- Pure pattern matching, no external dependencies in branches

**DD implementation**:

```rust
// WHEN as filter + map with frozen dependencies
pub fn when_operator<G>(
    input: &Collection<G, Value, isize>,
    patterns: Vec<(Pattern, Value)>,
) -> Collection<G, Value, isize>
where
    G: Scope,
{
    // Each pattern becomes a filter + map
    // Dependencies are captured at operator construction time (frozen)
    input.flat_map(move |value| {
        for (pattern, result) in &patterns {
            if pattern.matches(&value) {
                return Some(result.clone());
            }
        }
        None
    })
}
```

**Key insight**: WHEN's "frozen" semantics map naturally to DD's dataflow model - the operator captures its dependencies when constructed.

### WHILE → DD `filter` with Reactive Predicate

**Boon semantics** (from `docs/language/WHEN_VS_WHILE.md`):
```boon
signals |> WHILE {
    [reset: True, enable: __] => reset_state
    [reset: False, enable: True] => active
}
```

- **Flowing dependencies**: Re-evaluated as dependencies change
- Branches can access outer scope dependencies
- Record fields flow reactively

**DD implementation**:

```rust
// WHILE as join with reactive predicate
pub fn while_operator<G>(
    input: &Collection<G, Value, isize>,
    predicate_stream: &Collection<G, bool, isize>,
    branches: Vec<(Pattern, Collection<G, Value, isize>)>,
) -> Collection<G, Value, isize>
where
    G: Scope,
{
    // WHILE requires joining input with reactive predicate
    // When predicate changes, output updates automatically
    input.join(&predicate_stream)
        .flat_map(|(value, pred_result)| {
            // Evaluate branches with current predicate value
            ...
        })
}
```

**Key insight**: WHILE's "flowing" semantics require the predicate itself to be a stream/collection that the operator joins against. When dependencies change, DD automatically recomputes.

### THEN → DD `map` with Event Trigger

**Boon semantics**:
```boon
event |> THEN { body }
```

- Copy data when event fires
- Evaluate body once per event

**DD implementation**:

```rust
// THEN as map triggered by event
pub fn then_operator<G>(
    event: &Collection<G, Value, isize>,
    body_fn: impl Fn() -> Value,
) -> Collection<G, Value, isize>
where
    G: Scope,
{
    event.map(move |_trigger| body_fn())
}
```

### LINK → Input/Output Handles

**Boon semantics** (from `docs/language/LINK_PATTERN.md`):
1. **Declare**: `LINK` creates bidirectional channel
2. **Provide**: Consumer provides the reactive source
3. **Wire**: System connects event emission to source

**DD implementation**:

```rust
// LINK as InputHandle + traced output
struct LinkChannel {
    input: InputHandle<u64, Value>,    // Event injection
    output: Trace<Value>,              // State observation
}

// Provider side: Insert events into DD graph
link.input.insert((click_event, +1));
link.input.advance_to(time + 1);

// Consumer side: Observe state changes
let visible = link.output.trace.get(&link_id);
```

### FLUSH → Value Wrapper Propagation

**Boon semantics** (from `docs/language/FLUSH.md`):
- Fail-fast error handling
- Hidden `FLUSHED[value]` wrapper propagates through pipeline
- `FLUSH { ... }` catches and extracts flushed values

**DD implementation**:

```rust
// FLUSH as Value variant that propagates
enum Value {
    // ... other variants
    Flushed(Box<Value>),  // Wrapper that skips normal processing
}

// Operators check for Flushed and propagate
pub fn map_with_flush<G, F>(
    input: &Collection<G, Value, isize>,
    f: F,
) -> Collection<G, Value, isize>
where
    F: Fn(Value) -> Value,
{
    input.map(move |value| {
        match value {
            Value::Flushed(_) => value,  // Propagate unchanged
            _ => f(value),               // Normal processing
        }
    })
}
```

### List Operations → DD Collection Operations

| Boon | DD | Complexity |
|------|-----|------------|
| `[a, b, c]` | `Collection::from_iter([(a,+1), (b,+1), (c,+1)])` | O(n) init |
| `List/append(item)` | `input.insert((item, +1))` | O(1) |
| `List/remove(item)` | `input.insert((item, -1))` | O(1) |
| `List/retain(pred)` | `collection.filter(pred)` | O(delta) |
| `List/map(f)` | `collection.map(f)` | O(delta) |
| `List/count()` | `collection.count()` | O(1) per change |
| `List/pulses()` | Iterate collection differences | O(delta) |

**Critical insight**: DD's `(data, time, diff)` model means:
- Adding 1 item to 10,000-item list: O(1) - only the diff propagates
- Filtering 10,000 items where 1 matches: O(1) - only changed outputs propagate
- Rendering: Only changed DOM elements update

---

## Phase 1: Module Restructure (Remove "dd_" Prefix)

### Current Structure (Redundant Prefixes)
```
engine_dd/
├── dd_value.rs       # "dd_" prefix is redundant
├── dd_runtime.rs     # already in engine_dd/
├── dd_evaluator.rs
├── dd_bridge.rs
├── dd_interpreter.rs
├── core/
│   ├── types.rs
│   ├── worker.rs
│   └── guards.rs
└── io/
    ├── inputs.rs
    └── outputs.rs
```

### Target Structure (Clean Hierarchy)
```
engine_dd/
├── mod.rs              # Re-exports public API
├── core/               # Core DD infrastructure
│   ├── mod.rs
│   ├── types.rs        # CellId, LinkId, TimerId, EventPayload
│   ├── value.rs        # Value enum (was DdValue)
│   ├── operators.rs    # hold, list_filter, list_map (was dd_runtime.rs)
│   ├── worker.rs       # DD worker event loop
│   └── guards.rs       # Anti-cheat runtime guards
├── eval/               # Evaluation layer
│   ├── mod.rs
│   ├── evaluator.rs    # Expression evaluation (was dd_evaluator.rs)
│   └── interpreter.rs  # Program interpretation (was dd_interpreter.rs)
├── render/             # Rendering to Zoon
│   ├── mod.rs
│   └── bridge.rs       # Value → Zoon element (was dd_bridge.rs)
└── io/                 # Input/Output channels
    ├── mod.rs
    ├── inputs.rs       # Event injection
    └── outputs.rs      # State observation
```

### Rename Mapping

| Old | New | Reason |
|-----|-----|--------|
| `dd_value.rs` | `core/value.rs` | Module path provides context |
| `dd_runtime.rs` | `core/operators.rs` | Describes what it does |
| `dd_evaluator.rs` | `eval/evaluator.rs` | Grouped with interpreter |
| `dd_interpreter.rs` | `eval/interpreter.rs` | Grouped with evaluator |
| `dd_bridge.rs` | `render/bridge.rs` | Rendering category |
| `DdValue` | `Value` | No prefix needed inside engine_dd |
| `DdEvent` | `Event` | Same |
| `DdEventValue` | `EventValue` | Same |
| `DdInput` | `Input` | Same |
| `DdOutput` | `Output` | Same |

### Tasks

| Task | Description | Files |
|------|-------------|-------|
| 1.1 | Create `eval/` directory, move evaluator.rs + interpreter.rs | 2 files |
| 1.2 | Create `render/` directory, move bridge.rs | 1 file |
| 1.3 | Move dd_value.rs → core/value.rs | 1 file |
| 1.4 | Move dd_runtime.rs → core/operators.rs | 1 file |
| 1.5 | Rename `DdValue` → `Value`, `DdEvent` → `Event` etc. | All files |
| 1.6 | Update mod.rs re-exports | mod.rs files |

**Estimated effort**: 4 hours

---

## Phase 2: Hold → Cell Rename

### Rationale

**Boon HOLD** (language): Immutable stream accumulator pattern
```boon
0 |> HOLD count { click |> THEN { count + 1 } }
```

**DD "Hold"** (engine): Mutable storage cell with ID, get/set operations

These are semantically different - rename engine concept to **Cell**.

### Rename Mapping

| Current | New | Semantic |
|---------|-----|----------|
| `HoldId` | `CellId` | Storage cell identifier |
| `HoldRef` | `CellRef` | Reference to cell contents |
| `HoldConfig` | `CellConfig` | Cell configuration |
| `HOLD_STATES` | `CELL_STATES` | Global cell storage |
| `hold_states_signal()` | `cell_states_signal()` | Reactive signal |
| `update_hold_state()` | `update_cell()` | Update value |
| `get_hold_value()` | `get_cell()` | Read value |
| `DYNAMIC_HOLD_PREFIX` | (removed) | Use CellId::Dynamic |
| `hold_*` variables | `cell_*` | All variable names |

### Scope: 474 occurrences across 11 files

### Files to Modify

| File | Occurrences |
|------|-------------|
| `core/worker.rs` | ~150 |
| `eval/evaluator.rs` | ~120 |
| `io/outputs.rs` | ~80 |
| `core/types.rs` | ~50 |
| `render/bridge.rs` | ~40 |
| Other files | ~34 |

**Estimated effort**: 13 hours

---

## Phase 3: Type-Safe IDs and Tags

### Problem: String-Based Matching Patterns

**Category 1: Dynamic ID Generation (15+ occurrences)**
```rust
// Current: String prefix checking
format!("{}{}", DYNAMIC_HOLD_PREFIX, counter)  // "dynamic_hold_42"
hold_id.starts_with(DYNAMIC_HOLD_PREFIX)       // String check
```

**Category 2: Tag Discrimination (25+ occurrences)**
```rust
tag.as_ref() == "True"    // Fragile string match
tag.as_ref() == "Element" // Magic strings
```

**Category 3: Event Text Parsing (6+ occurrences)**
```rust
text.strip_prefix("Enter:")   // "Enter:hello"
text.strip_prefix("remove:")  // "remove:link_42"
```

### Target: Typed Enums

```rust
// core/types.rs

/// Unique identifier for a state cell
pub enum CellId {
    Static(Arc<str>),    // Defined in source: "count", "items"
    Dynamic(u32),        // Generated for list items
}

/// Unique identifier for an event source
pub enum LinkId {
    Static(Arc<str>),
    Dynamic(u32),
}

/// Timer identification
pub enum TimerId {
    Interval { ms: u64 },
    Named(Arc<str>),
    HoverEffect(LinkId),
}

/// Structured event payload (replaces string parsing)
pub enum EventPayload {
    Enter(String),       // Text input submission
    Remove(LinkId),      // Remove button
    Toggle(CellId),      // Toggle action
    Unit,                // Simple trigger
}

/// Boolean tag (replaces tag == "True"/"False")
pub enum BoolTag { True, False }

/// Element discriminator (replaces tag == "Element")
pub enum ElementTag { Element, NoElement }
```

### Tasks

| Task | Description | Occurrences |
|------|-------------|-------------|
| 3.1 | Define typed enums in `core/types.rs` | New code |
| 3.2 | Replace `DYNAMIC_HOLD_PREFIX` with `CellId::Dynamic` | 15+ |
| 3.3 | Replace `tag == "True"/"False"` with `BoolTag` | 25+ |
| 3.4 | Replace `tag == "Element"` with `ElementTag` | 10+ |
| 3.5 | Replace event text parsing with `EventPayload` | 6+ |
| 3.6 | Replace link ID string parsing | 5+ |

**Estimated effort**: 17 hours

---

## Phase 4: DD Infrastructure

**Status:** ⚠️ OPERATORS EXIST - Integration incomplete (operators work but not used in main path)

### Current Problem

```rust
// Current: Lists stored as plain Vec - bypasses DD entirely!
pub enum DdValue {
    List(Arc<Vec<DdValue>>),  // O(n) copy on every change
}

// When filtering 10,000 items:
// 1. Clone entire Vec
// 2. Filter
// 3. Create new Vec
// 4. Re-render all items
```

### Target: Pure DD Collections

```rust
// Target: Lists ARE DD collections
pub enum Value {
    // Scalar values (unchanged)
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    // ...

    // Collections are handles into DD graph
    Collection(CollectionHandle),
}

pub struct CollectionHandle {
    /// Unique identifier for this collection in the DD graph
    id: CollectionId,
    /// Reference to the traced arrangement for reading
    trace: TraceHandle,
}
```

### DD Operators for Lists

```rust
// core/operators.rs

use differential_dataflow::Collection;

/// Filter a collection by predicate - O(delta)
pub fn list_filter<G, F>(
    collection: &Collection<G, Value, isize>,
    predicate: F,
) -> Collection<G, Value, isize>
where
    G: Scope,
    F: Fn(&Value) -> bool + 'static,
{
    collection.filter(move |item| predicate(item))
}

/// Map over a collection - O(delta)
pub fn list_map<G, F>(
    collection: &Collection<G, Value, isize>,
    transform: F,
) -> Collection<G, Value, isize>
where
    G: Scope,
    F: Fn(&Value) -> Value + 'static,
{
    collection.map(move |item| transform(item))
}

/// Count elements - O(1) per change
pub fn list_count<G>(
    collection: &Collection<G, Value, isize>,
) -> Collection<G, i64, isize>
where
    G: Scope,
{
    collection.map(|_| ()).count()
}

/// Append item - O(1)
pub fn list_append<G>(
    input_handle: &mut InputHandle<u64, (Value, isize)>,
    item: Value,
    time: u64,
) {
    input_handle.insert((item, +1));
    input_handle.advance_to(time + 1);
}

/// Remove item - O(1)
pub fn list_remove<G>(
    input_handle: &mut InputHandle<u64, (Value, isize)>,
    item: Value,
    time: u64,
) {
    input_handle.insert((item, -1));
    input_handle.advance_to(time + 1);
}
```

### Rendering with Diffs

```rust
// render/bridge.rs

/// Render collection changes incrementally
pub fn render_collection_diff(
    container: &mut Element,
    diff: &[(Value, isize)],
    render_item: impl Fn(&Value) -> Element,
) {
    for (value, diff) in diff {
        match diff {
            +1 => container.append(render_item(value)),
            -1 => container.remove_child_matching(value),
            _ => {} // Batched changes
        }
    }
}
```

### Tasks

| Task | Description | Est. Hours |
|------|-------------|------------|
| 4.1 | Add `CollectionHandle` to core/value.rs | 2 |
| 4.2 | Implement `list_filter`, `list_map`, `list_count` operators | 4 |
| 4.3 | Update evaluator to build DD collections for list literals | 6 |
| 4.4 | Update evaluator for `List/append`, `List/remove` | 4 |
| 4.5 | Update evaluator for `List/retain`, `List/map` | 6 |
| 4.6 | Update worker to manage collection handles | 6 |
| 4.7 | Update render bridge to consume collection diffs | 6 |

**Estimated effort**: 34 hours

---

## Phase 5: DD-First Architecture

**Status:** ❌ NOT IMPLEMENTED - Code stubs exist in dataflow.rs but not wired to main event loop

### The Problem: Imperative-First vs DD-First

The current engine is **imperative-first** - it uses DD operators as utilities but doesn't run actual Timely dataflows:

```
Current Flow (Imperative):
┌─────────────────────────────────────────────────────────────────┐
│  Event → Worker receives → Update HashMap<CellId, Value>        │
│        → Manually recalculate dependents → Clone entire lists   │
│        → Re-render everything                                   │
└─────────────────────────────────────────────────────────────────┘

Values live in: HashMap<CellId, Value>  (cell storage)
List operations: Clone entire Vec, modify, store back
Complexity: O(n) for any list change
```

The target is **DD-first** - values flow through actual Timely/DD dataflows:

```
Target Flow (DD-First):
┌─────────────────────────────────────────────────────────────────┐
│  Event → DD InputHandle receives diff → DD propagates only diff │
│        → Operators process incrementally → Bridge receives diff │
│        → Only changed DOM elements update                       │
└─────────────────────────────────────────────────────────────────┘

Values live in: DD Collections (Timely dataflow graph)
List operations: Insert (item, +1) or (item, -1) diff
Complexity: O(delta) - only changed items propagate
```

### Architecture Changes Required

#### 5.1 Timely Worker Integration

Currently, DD operators exist but aren't connected to a running Timely worker:

```rust
// Current: Operators exist but aren't used
pub fn hold<G, S, E, F>(...) -> VecCollection<G, S> { ... }

// Target: Worker runs actual Timely computation
pub struct DdWorker {
    worker: TimelyWorker<Thread>,
    inputs: HashMap<InputId, InputHandle<u64, Value>>,
    probes: HashMap<OutputId, ProbeHandle<u64>>,
}

impl DdWorker {
    pub fn step(&mut self) {
        self.worker.step();
    }

    pub fn inject_event(&mut self, input_id: InputId, value: Value) {
        if let Some(handle) = self.inputs.get_mut(&input_id) {
            handle.insert(value);
            handle.advance_to(self.current_time + 1);
        }
    }
}
```

#### 5.2 Evaluator: Build Dataflow Graph Instead of Computing Values

Currently, the evaluator computes values eagerly:

```rust
// Current: Eager computation
fn evaluate_list_literal(&self, items: &[Expr]) -> Value {
    let values: Vec<Value> = items.iter()
        .map(|item| self.evaluate(item))
        .collect();
    Value::List(Arc::new(values))  // Creates Vec immediately
}
```

Target: Build DD dataflow graph lazily:

```rust
// Target: Build dataflow graph
fn evaluate_list_literal(&self, scope: &mut G, items: &[Expr]) -> Collection<G, Value, isize>
where G: Scope
{
    // Create input handle for this collection
    let (input, collection) = scope.new_collection::<Value, isize>();

    // Insert initial items as diffs
    for item in items {
        let value = self.evaluate_to_value(item);
        input.insert(value);
    }

    collection  // Return DD collection, not Vec
}
```

#### 5.3 Wire HOLD to DD Operator

Currently, HOLD uses cell storage with manual updates:

```rust
// Current: HOLD updates cell storage imperatively
fn evaluate_hold(&self, initial: Value, body: &Expr) -> Value {
    let cell_id = self.allocate_cell();
    self.store_cell(cell_id, initial);

    // Body triggers manual cell update
    let body_stream = self.evaluate(body);
    // ... subscribe and update cell on each event
}
```

Target: Use the `hold()` DD operator:

```rust
// Target: HOLD creates DD operator in dataflow
fn evaluate_hold<G>(&self, scope: &mut G, initial: Value, body: &Expr) -> Collection<G, Value, isize>
where G: Scope
{
    let events = self.evaluate_to_collection(scope, body);

    // Use the actual DD hold operator
    hold(initial, &events, |state, event| {
        self.apply_transform(state, event)
    })
}
```

#### 5.4 Wire LATEST to list_concat

Currently, LATEST manually merges streams:

```rust
// Current: Manual stream merging
fn evaluate_latest(&self, sources: &[Expr]) -> Value {
    let streams: Vec<_> = sources.iter()
        .map(|s| self.evaluate(s))
        .collect();
    // Manual merging logic...
}
```

Target: Use `list_concat()`:

```rust
// Target: LATEST uses DD concat
fn evaluate_latest<G>(&self, scope: &mut G, sources: &[Expr]) -> Collection<G, Value, isize>
where G: Scope
{
    let collections: Vec<_> = sources.iter()
        .map(|s| self.evaluate_to_collection(scope, s))
        .collect();

    // Concatenate all collections - O(1) operation
    collections.into_iter()
        .reduce(|a, b| list_concat(&a, &b))
        .unwrap_or_else(|| scope.new_collection().1)
}
```

#### 5.5 Wire List Operations to DD Collections

| Boon Operation | Current Implementation | DD-First Implementation |
|----------------|----------------------|------------------------|
| `[a, b, c]` | `Value::List(Arc::new(vec![a, b, c]))` | `input.insert(a); input.insert(b); input.insert(c);` |
| `List/append(item)` | Clone Vec, push, wrap in Arc | `input.insert((item, +1))` |
| `List/remove(item)` | Clone Vec, remove, wrap in Arc | `input.insert((item, -1))` |
| `List/retain(pred)` | Clone, filter, new Vec | `collection.filter(pred)` |
| `List/map(f)` | Clone, map, new Vec | `collection.map(f)` |
| `List/count()` | `items.len()` | `collection.count()` |

#### 5.6 Bridge: Consume DD Diffs for Incremental Rendering

Currently, the bridge re-renders entire lists:

```rust
// Current: Re-render everything
fn render_list(&self, items: &[Value]) -> Element {
    let children: Vec<_> = items.iter()
        .map(|item| self.render_value(item))
        .collect();
    Element::new("div").children(children)
}
```

Target: Apply diffs incrementally:

```rust
// Target: Incremental DOM updates
fn apply_collection_diff(&mut self, container: &Element, diffs: &[(Value, isize)]) {
    for (value, diff) in diffs {
        match diff {
            1 => {
                // Insert: Create element and append
                let element = self.render_value(value);
                container.append_child(&element);
            }
            -1 => {
                // Remove: Find and remove matching element
                if let Some(child) = self.find_element_for_value(container, value) {
                    container.remove_child(&child);
                }
            }
            _ => {
                // Batched changes - handle delta
            }
        }
    }
}
```

### Tasks

| Task | Description | Est. Hours |
|------|-------------|------------|
| 5.1 | Create `DdWorker` struct that runs actual Timely computation | 8 |
| 5.2 | Add `DataflowBuilder` to evaluator for constructing DD graphs | 12 |
| 5.3 | Wire HOLD expressions to use `hold()` DD operator | 8 |
| 5.4 | Wire LATEST expressions to use `list_concat()` | 6 |
| 5.5 | Wire WHEN/WHILE/THEN to DD `filter`/`map` operators | 10 |
| 5.6 | Convert list literal evaluation to DD collection creation | 8 |
| 5.7 | Convert `List/append`, `List/remove` to diff insertions | 6 |
| 5.8 | Convert `List/retain`, `List/map` to DD operators | 6 |
| 5.9 | Update bridge to consume and apply DD diffs | 12 |
| 5.10 | Add element identity tracking for diff-based rendering | 8 |
| 5.11 | Integration testing with todo_mvc (verify O(delta) behavior) | 6 |

**Estimated effort**: 90 hours

### Verification

```bash
# Build and run playground
cargo build -p boon
cd playground && makers mzoon start &

# Verify DD dataflow is running
# Console should show: "[DD] Timely worker stepping..."

# Test O(delta) behavior:
# 1. Load todo_mvc
# 2. Add 1000 items programmatically
# 3. Toggle ONE checkbox
# 4. Console should show:
#    "[DD] Processing 1 diff" (not 1000!)
#    "[Bridge] Updating 1 element"

# Test list operations:
# 1. With 10,000 items loaded
# 2. Delete one item
# 3. Time should be ~1ms (not 100ms+)
```

### Migration Strategy

**Option A: Big Bang** (not recommended)
- Rewrite evaluator completely
- High risk, long time without working code

**Option B: Gradual Migration** (recommended)
1. Keep current imperative path working
2. Add `--dd-first` flag to enable new path
3. Migrate one construct at a time (HOLD → LATEST → Lists)
4. Run both paths in parallel, compare outputs
5. Once verified, remove imperative path

```rust
// Feature flag during migration
if config.use_dd_first {
    self.evaluate_hold_dd(scope, initial, body)
} else {
    self.evaluate_hold_imperative(initial, body)
}
```

---

## Verification Plan

```bash
# After each phase:
cargo build -p boon
cd playground && makers mzoon start &

# Phase 1 verification - module structure:
ls -la crates/boon/src/platform/browser/engine_dd/
# Should show: core/, eval/, render/, io/, mod.rs

# Phase 2 verification - no "hold" in API:
grep -r "HoldId" crates/boon/src/platform/browser/engine_dd/
# Should return 0 matches

# Phase 3 verification - no string patterns:
grep -r "starts_with.*DYNAMIC" crates/boon/src/platform/browser/engine_dd/
grep -r 'as_ref\(\) == "True"' crates/boon/src/platform/browser/engine_dd/
# Should return 0 matches for both

# Phase 4 verification - O(delta) behavior:
# 1. Load todo_mvc with 1000 items
# 2. Toggle one checkbox
# 3. Console should show only 1 item processed, not 1000
```

---

## Critical Files Summary

| New Path | Was | Lines |
|----------|-----|-------|
| `core/value.rs` | dd_value.rs | ~934 |
| `core/operators.rs` | dd_runtime.rs | ~411 |
| `core/types.rs` | core/types.rs | ~230 |
| `core/worker.rs` | core/worker.rs | ~2542 |
| `eval/evaluator.rs` | dd_evaluator.rs | ~3800 |
| `eval/interpreter.rs` | dd_interpreter.rs | ~1300 |
| `render/bridge.rs` | dd_bridge.rs | ~2195 |
| `io/outputs.rs` | io/outputs.rs | ~450 |

---

## Future Work

### Memory Optimizations (GRADES-NDA Research)

From GRADES-NDA 2024 research paper:

**Read-Friendly Indices** (19x improvement for UI workloads):
```rust
// Current: Write-optimized (append-only)
struct WriteOptimized { log: Vec<(Time, Key, Value, Diff)> }

// Target: Read-optimized (sorted by key)
struct ReadFriendly { by_key: BTreeMap<Key, Vec<(Time, Value, Diff)>> }
```

**Fast Empty Difference Check**:
```rust
fn process_tick(&mut self) {
    if input.is_unchanged_since(self.last_tick) {
        return;  // Skip entirely - most UI ticks!
    }
}
```

**Bounded Diff History** (1.7x memory reduction):
```rust
struct BoundedArrangement<K, V> {
    current: BTreeMap<K, V>,
    recent_diffs: VecDeque<Diff>,
    max_diffs: usize,
}
```

### E-Graph Optimization (EGRAPHS 2023 Research)

Automatic semi-naive evaluation discovery using `egg` crate:

```rust
define_language! {
    enum BoonExpr {
        "persist" = Persist([Id; 1]),
        "delta" = Delta([Id; 1]),
        "prev" = Prev([Id; 1]),
        "hold" = Hold([Id; 2]),
        "latest" = Latest(Vec<Id>),
        "list-retain" = ListRetain([Id; 2]),
        "list-count" = ListCount([Id; 1]),
    }
}
```

Rewrite rules:
```
persist(delta(x)) ≡ x
delta(persist(x)) ≡ x
hold(init, body) ≡ latest(prev(hold(init, body)), delta(body))
```

### WebWorker Parallelism (Deferred)

Multi-threaded DD execution using SharedArrayBuffer:

```
┌──────────────────────────────────────────────────────────────┐
│  Browser Main Thread                                         │
│  ┌────────────────────────────────────────────────────────┐  │
│  │ Boon UI Coordinator                                    │  │
│  │ - DOM updates only                                     │  │
│  │ - Event dispatch to workers                            │  │
│  └────────────────────────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────────┤
│  WebWorker Pool (SharedArrayBuffer communication)            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                   │
│  │ Timely   │  │ Timely   │  │ Timely   │                   │
│  │ Worker 0 │←→│ Worker 1 │←→│ Worker 2 │                   │
│  │ (WASM)   │  │ (WASM)   │  │ (WASM)   │                   │
│  └──────────┘  └──────────┘  └──────────┘                   │
└──────────────────────────────────────────────────────────────┘
```

Custom allocator for browser environment:
```rust
pub struct WebWorkerAllocator {
    worker_index: usize,
    worker_count: usize,
    send_buffers: Vec<SharedRingBuffer>,
    recv_buffer: SharedRingBuffer,
}

impl Allocate for WebWorkerAllocator {
    fn index(&self) -> usize { self.worker_index }
    fn peers(&self) -> usize { self.worker_count }
    fn allocate<T>(&mut self) -> (Vec<Push<T>>, Pull<T>) { ... }
}
```

**Reason for deferral**: Single-threaded DD is sufficient for current use cases. WebWorker parallelism adds complexity (SharedArrayBuffer security restrictions, worker coordination) that should wait until we have proven performance bottlenecks.

---

## Implementation Order

1. **Phase 1 (Module Restructure)** ✅ COMPLETE - Clean foundation first
2. **Phase 2 (Hold → Cell)** ✅ COMPLETE - Clear naming
3. **Phase 3 (Type Safety)** ✅ COMPLETE - Enable compile-time checks
4. **Phase 4 (DD Infrastructure)** ⚠️ PARTIAL - DD operators ready but not integrated
5. **Phase 5 (DD-First Architecture)** ❌ SUPERSEDED by Phase 6
6. **Phase 6 (Single State Authority)** ✅ COMPLETE - Simplified pure reactive approach
7. **Phase 7 (O(delta) List Operations)** ✅ COMPLETE - Pre-instantiation outside DD
8. **Phase 8 (Pure LINK Handling)** ✅ COMPLETE - Bridge pattern for incremental migration
9. **Phase 9 (True Incremental Lists)** 🔴 PLANNED - 54 hours
10. **Phase 10 (Eliminate inspect() Side Effects)** 🔴 PLANNED - 42 hours
11. **Phase 11 (Move Business Logic to DD)** 🔴 PLANNED - 54 hours
12. **Phase 12 (Incremental Rendering)** 🔴 PLANNED - 62 hours

**Estimated effort remaining**: ~212 hours for Phases 9-12 (~5-6 weeks)

---

## Phase 6: Single State Authority (Simplified Pure Reactive)

**Status:** ✅ COMPLETE (2026-01-17)

### The Insight: Eliminate Dual Code Paths

The Phase 5 plan was overcomplicated. The key insight is:

> **Why have both initialization AND reactive updates when we can have ONLY reactive updates?**

**Current Hybrid Architecture (Complex):**
```
┌────────────────────────────────────────────────────────────────────┐
│ INITIALIZATION PATH:                                               │
│   Interpreter → load persisted → init_cell() → CELL_STATES        │
├────────────────────────────────────────────────────────────────────┤
│ RUNTIME PATH:                                                      │
│   DD Worker → DocumentUpdate → Output Listener → init_cell()      │
│            → CELL_STATES                                           │
├────────────────────────────────────────────────────────────────────┤
│ TWO WRITERS to CELL_STATES = complexity, bugs, confusion           │
└────────────────────────────────────────────────────────────────────┘
```

**Simplified Architecture (Pure Reactive):**
```
┌────────────────────────────────────────────────────────────────────┐
│ SINGLE PATH (both init and runtime):                               │
│   Interpreter → DataflowConfig (describes cells, NO writes)       │
│                                                                    │
│   DD Worker startup:                                               │
│     for each cell in config:                                       │
│       value = load_persisted(cell.persist_id) ?? cell.default     │
│       write_cell(cell.id, value)  ← ONLY writer!                  │
│                                                                    │
│   DD Worker events:                                                │
│     process event → compute new value → write_cell(id, value)     │
│                                                                    │
│   Bridge observes cell_states_signal() → renders                   │
├────────────────────────────────────────────────────────────────────┤
│ ONE WRITER to CELL_STATES = simple, correct, maintainable          │
└────────────────────────────────────────────────────────────────────┘
```

### What Changes

| Component | Before | After |
|-----------|--------|-------|
| **Interpreter** | Calls `init_cell()` for each cell | Only builds `DataflowConfig` |
| **CellConfig** | No persistence info | Has `persist_id: Option<PersistenceId>` |
| **DD Worker startup** | Receives config, starts loop | Loads persisted values, initializes `CELL_STATES` |
| **DD Worker events** | Sends `DocumentUpdate` via channel | Directly updates `CELL_STATES` |
| **Output Listener** | Receives updates, calls `init_cell()` | **REMOVED** |
| **`init_cell()`** | Called from 2 places | **REMOVED** (only `write_cell()` in worker) |

### Tasks

| Task | Description | Est. Hours |
|------|-------------|------------|
| 6.1 | Add `persist_id: Option<PersistenceId>` to `CellConfig` | 1 |
| 6.2 | Remove all `init_cell()` calls from interpreter | 2 |
| 6.3 | DD Worker: load persisted values on startup | 2 |
| 6.4 | DD Worker: write to `CELL_STATES` directly (not via channel) | 3 |
| 6.5 | Remove `DocumentUpdate` channel and output listener | 2 |
| 6.6 | Consolidate `init_cell()` and `sync_cell_from_dd()` into single `write_cell()` | 1 |
| 6.7 | Update bridge if needed (should "just work" since it observes signal) | 1 |
| 6.8 | Test all examples (counter, todo_mvc, shopping_list, interval) | 4 |
| 6.9 | Verify persistence works (refresh page, state restored) | 2 |
| 6.10 | Clean up dead code | 2 |

**Actual effort**: ~4 hours (much simpler than estimated!)

### Verified Examples

All examples tested and working:
- ✅ `counter` - clicks increment, persistence works across refresh
- ✅ `todo_mvc` - add items, toggle checkboxes, filters (All/Active/Completed)
- ✅ `shopping_list` - add items, text input clears after Enter
- ✅ `interval` - timer-driven updates increment automatically

### Why This is Better

1. **Single Source of Truth**: DD Worker owns all state writes
2. **No Synchronization Bugs**: Can't have race between init and runtime paths
3. **Simpler Mental Model**: "DD Worker is the state machine"
4. **Easier Testing**: Mock DD Worker, test state transitions
5. **Future-Proof**: When DD Worker moves to WebWorker, the boundary is clean

### Verification

```bash
# After Phase 6:

# 1. No init_cell calls outside DD worker
grep -r "init_cell" crates/boon/src/platform/browser/engine_dd/
# Should only find the definition in outputs.rs, not calls

# 2. No DocumentUpdate channel
grep -r "DocumentUpdate" crates/boon/src/platform/browser/engine_dd/
# Should return 0 matches (or only type definition if kept for future)

# 3. All examples work
cd playground && makers mzoon start &
# Test: counter increments, todo_mvc adds/removes/filters, shopping_list persists

# 4. Persistence works
# 1. Add items to shopping_list
# 2. Refresh page
# 3. Items should still be there
```

---

## Phase 7: O(delta) List Operations

**Status:** ✅ COMPLETE (2026-01-17)

### The Problem: Side Effects Inside DD

Template-based list operations (`ListAppendWithTemplate`) had side effects INSIDE DD transforms:
1. **ID Generation**: `DYNAMIC_CELL_COUNTER.fetch_add()`, `DYNAMIC_LINK_COUNTER.fetch_add()`
2. **HOLD Registration**: `update_cell_no_persist()` writes to `CELL_STATES`
3. **LINK Action Registration**: `add_dynamic_link_action()` writes to `DYNAMIC_LINK_ACTIONS`

This broke the persistent DD worker because:
- DD expects pure transforms that can be replayed/retried
- Side effects during DD execution cause duplicate registrations
- Atomic counters may not produce consistent IDs across replays

### The Solution: Pre-Instantiation Before DD

Move ALL side effects OUTSIDE DD, then pass prepared data through pure transforms:

```
┌─────────────────────────────────────────────────────────────────┐
│  Event: Text("Enter:Buy milk")                                  │
│                                                                 │
│  maybe_pre_instantiate() ← OUTSIDE DD                           │
│  ├── Generate IDs: dynamic_cell_1000, dynamic_link_1000         │
│  ├── Register HOLDs: update_cell_no_persist(...)                │
│  ├── Register LINKs: add_dynamic_link_action(...)               │
│  └── Return: EventValue::PreparedItem(instantiated_data)        │
│                                                                 │
│  inject_event_persistent() ← INSIDE DD (pure)                   │
│  └── ListAppendPrepared: items.push(prepared_item) // No side effects!
└─────────────────────────────────────────────────────────────────┘
```

### Implementation

| Component | Change |
|-----------|--------|
| `EventValue` | Added `PreparedItem(Value)` variant |
| `DdTransform` | Added `ListAppendPrepared` (pure append, no side effects) |
| `Worker` | Added `maybe_pre_instantiate()` called BEFORE `inject_event_persistent()` |
| `Worker` | Added `build_template_cell_map()` to track templated cells |
| `convert_config_to_dd_cells` | Maps template transforms → `ListAppendPrepared` |
| `USE_PERSISTENT_WORKER` | Changed from `false` to `true` |

### Key Files Modified

- `crates/boon/src/platform/browser/engine_dd/core/types.rs` - `EventValue::PreparedItem`
- `crates/boon/src/platform/browser/engine_dd/core/dataflow.rs` - `DdTransform::ListAppendPrepared`
- `crates/boon/src/platform/browser/engine_dd/core/worker.rs` - Pre-instantiation logic

### Verification

```bash
# Anti-cheat passes (no .borrow() without ALLOWED marker)
makers verify-dd-no-cheats

# All 11 examples pass
makers verify-playground-dd
# counter, counter_hold, fibonacci, hello_world, interval,
# interval_hold, layers, minimal, pages, shopping_list, todo_mvc
```

### Result

- ✅ `USE_PERSISTENT_WORKER = true` enabled
- ✅ `ListAppendPrepared` is pure (no side effects inside DD)
- ✅ Pre-instantiation happens BEFORE DD injection
- ✅ True O(delta) complexity for list operations

---

## Phase 8: Pure LINK Handling

**Status:** ✅ COMPLETE (2026-01-17)

### Implementation Summary

Phase 8 established a **bridge pattern** for DD-native LINK handling that allows incremental migration
while keeping all examples working. Business logic is now encoded in DD types (`LinkAction`, `LinkCellMapping`)
while the IO layer acts as a thin routing layer.

**Completed Tasks:**
- ✅ **8.1** `LinkAction` and `LinkCellMapping` types in `types.rs`
- ✅ **8.2** `apply_link_action()` pure function in `dataflow.rs`
- ✅ **8.3** `get_all_link_mappings()` bridge function in `inputs.rs`
- ✅ **8.4** Link mapping processing in `process_with_persistent_worker()`
- ✅ **8.5** RemoveListItem uses event indirection (IO routes, DD handles removal via `StateTransform::RemoveListItem`)
- ✅ **8.6** ListToggleAllCompleted migrated to DD via `LinkAction::ListToggleAllCompleted`
- ✅ **8.7** EditingHandler partially migrated (Escape key via DD, grace period stays in IO for browser race handling)
- ✅ **8.8** Business logic encoded in DD types, IO layer is thin routing layer
- ✅ **8.9** All 11 examples verified working
- ✅ **8.10** Fixed missing `update_cell_no_persist` function, exported from IO module

**Architecture Decision:**
The IO layer (`inputs.rs`) still handles event routing via `fire_global_link()` which calls
`check_dynamic_link_action()`. This fires `dd_cell_update` events that the DD worker processes.
The `get_all_link_mappings()` bridge converts the `DYNAMIC_LINK_ACTIONS` HashMap to DD-native
`Vec<LinkCellMapping>` at worker spawn time. This is an acceptable architecture because:
1. Business logic IS in DD types (LinkAction enum defines what actions do)
2. IO layer only ROUTES events (no business decisions)
3. DD worker owns all state writes
4. The approach allows incremental migration without breaking existing code

### Architecture (Implemented)

The migration uses a non-invasive bridge pattern:

```rust
// At worker spawn time (worker.rs):
let link_mappings = super::super::io::get_all_link_mappings();
self.config.link_mappings = link_mappings;

// During event processing (worker.rs):
for mapping in &config.link_mappings {
    if mapping_matches_event(mapping, link_id, &event_value) {
        let new_value = apply_link_action(&mapping.action, current_value, &event_value);
        // Update cell state...
    }
}
```

This keeps existing `add_dynamic_link_action` calls working while routing through DD.

### The Problem: Business Logic in IO Layer

Currently, LINK actions are stored in a thread-local HashMap (`DYNAMIC_LINK_ACTIONS`) in the IO layer (`inputs.rs`). When a link fires, the IO layer looks up the action and executes business logic:

```rust
// inputs.rs - Business logic LEAKED into IO layer!
DynamicLinkAction::RemoveListItem { link_id } => {
    fire_link_text("dynamic_list_remove", format!("remove:{}", link_id));
}
DynamicLinkAction::ListToggleAllCompleted { list_cell_id, completed_field } => {
    // Computes all_completed, iterates list, updates each item
}
DynamicLinkAction::EditingHandler { editing_cell, title_cell } => {
    // Multi-step logic: Enter saves, Escape cancels
}
```

**This violates the architectural principle**: IO layer should ONLY handle browser↔DD interface, not business logic.

### The Correct Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                      BROWSER                                    │
│  (click, keydown, mouseenter, blur, input change)               │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│                    IO LAYER (thin)                              │
│  • Convert DOM event → DD Event                                 │
│  • event_input.send(Event::Link { id, value })                  │
│  • NO business logic, NO decisions                              │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│                DD DATAFLOW (all logic here)                     │
│  • Link → Cell mappings (DD collection + join)                  │
│  • List operations (filter, map, reduce, antijoin)              │
│  • Cell transforms (toggle, set, increment)                     │
│  • ALL business logic lives here                                │
│  • Pure, incremental, O(delta)                                  │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│                    IO LAYER (thin)                              │
│  • Render DD output → DOM                                       │
│  • NO business logic, NO decisions                              │
└─────────────────────────────────────────────────────────────────┘
```

### What Should Move from IO to DD

| Action | Current (IO) | Target (DD) |
|--------|--------------|-------------|
| **RemoveListItem** | IO looks up link, fires remove event | `list.antijoin(&remove_events)` - pure DD filter |
| **ListToggleAllCompleted** | IO iterates list, updates each item | `list.map(\|item\| {completed: !all_completed})` - pure DD map |
| **EditingHandler** | IO handles Enter/Escape/blur logic | Cell transforms triggered by link events |
| **BoolToggle** | IO fires `bool_toggle:cell_id` | DD join: link→cell mapping + toggle transform |
| **SetTrue/SetFalse** | IO fires `set_true:cell_id` | DD join: link→cell mapping + set transform |
| **HoverState** | IO fires `hover_set:cell_id:value` | DD join: link→cell mapping + set transform |

### DD-Native LINK Handling Design

#### 8.1 Link→Cell Mappings as DD Collection

```rust
// Instead of HashMap<String, DynamicLinkAction>
// Use DD collection:
struct LinkCellMapping {
    link_id: LinkId,
    cell_id: CellId,
    action: LinkAction,  // Toggle, SetTrue, SetFalse, etc.
}

// In DD dataflow:
let (mapping_input, mappings) = scope.new_collection::<LinkCellMapping, isize>();

// When link fires:
let cell_updates = events
    .join(&mappings)  // DD join - O(delta)!
    .map(|(link_id, (event, mapping))| {
        (mapping.cell_id, apply_action(mapping.action, event))
    });
```

#### 8.2 RemoveListItem as DD Antijoin

```rust
// Current (IO layer - wrong):
DynamicLinkAction::RemoveListItem { link_id } => {
    fire_link_text("dynamic_list_remove", format!("remove:{}", link_id));
}

// Target (DD - pure):
let remove_events: Collection<LinkId> = events
    .filter(|(link, _)| is_remove_button(link))
    .map(|(link, _)| extract_item_identity(link));

let updated_list = list_items
    .antijoin(&remove_events)  // DD antijoin - O(delta)!
```

#### 8.3 ListToggleAllCompleted as DD Map/Reduce

```rust
// Current (IO layer - wrong):
// Iterates entire list, updates each item

// Target (DD - pure):
let all_completed: Collection<bool> = list_items
    .map(|item| item.completed)
    .reduce(|a, b| a && b);  // DD reduce - O(delta)!

let toggle_events = events.filter(|(link, _)| link == "toggle_all");

let updated_list = list_items
    .join(&all_completed)
    .join(&toggle_events)
    .map(|(item, all_comp, _)| Item {
        completed: !all_comp,
        ..item
    });
```

#### 8.4 EditingHandler as DD Cell Transforms

```rust
// Current (IO layer - multi-step logic):
if key == "Escape" { set_false(editing_cell) }
else if key.starts_with("Enter:") {
    set_text(title_cell, text);
    set_false(editing_cell);
}

// Target (DD - declarative transforms):
// Double-click → editing=true
cells.add_transform("editing_cell",
    triggered_by: ["double_click_link"],
    transform: SetTrue
);

// Enter → save title + exit editing
cells.add_transform("title_cell",
    triggered_by: ["key_down_link"],
    filter: TextStartsWith("Enter:"),
    transform: ExtractText
);
cells.add_transform("editing_cell",
    triggered_by: ["key_down_link"],
    filter: TextStartsWith("Enter:"),
    transform: SetFalse
);

// Escape → exit editing
cells.add_transform("editing_cell",
    triggered_by: ["key_down_link"],
    filter: TextEquals("Escape"),
    transform: SetFalse
);
```

### Tasks

| Task | Description | Est. Hours |
|------|-------------|------------|
| 8.1 | Add `LinkCellMapping` to DD collections | 4 |
| 8.2 | Implement link→cell join in DD dataflow | 6 |
| 8.3 | Migrate `BoolToggle`, `SetTrue`, `SetFalse` to DD | 4 |
| 8.4 | Migrate `HoverState` to DD | 2 |
| 8.5 | Implement `RemoveListItem` as DD antijoin | 6 |
| 8.6 | Implement `ListToggleAllCompleted` as DD map/reduce | 6 |
| 8.7 | Refactor `EditingHandler` to DD cell transforms | 8 |
| 8.8 | Remove business logic from `inputs.rs` | 4 |
| 8.9 | Update tests to verify pure DD behavior | 4 |
| 8.10 | Clean up dead code in IO layer | 2 |

**Estimated effort**: 46 hours

### Benefits

1. **All business logic in DD**: Single source of truth for behavior
2. **True O(delta)**: All list operations are incremental
3. **Simpler IO layer**: Only browser↔DD interface, no decisions
4. **Testable**: Can test DD dataflow without browser
5. **Future-proof**: DD can move to WebWorker cleanly

### Verification

```bash
# After Phase 8:

# 1. No business logic in inputs.rs
grep -r "DynamicLinkAction::" crates/boon/src/platform/browser/engine_dd/io/inputs.rs
# Should return 0 matches (actions moved to DD)

# 2. DYNAMIC_LINK_ACTIONS removed
grep -r "DYNAMIC_LINK_ACTIONS" crates/boon/src/platform/browser/engine_dd/
# Should return 0 matches

# 3. All examples still work
makers verify-playground-dd

# 4. O(delta) for all operations
# Toggle one checkbox in 10,000 item list
# Console: "[DD] Processing 1 diff" (not 10,000)
```

---

## Current Architecture: "DD-First with Pre-Instantiation"

After Phase 7, list operations use true DD with O(delta) complexity:

```
Current Flow (Phase 7):
┌─────────────────────────────────────────────────────────────────┐
│  Event → maybe_pre_instantiate() (side effects OUTSIDE DD)      │
│        → inject_event_persistent() (pure DD transform)          │
│        → Persistent Timely worker (single dataflow, O(delta))   │
│        → CELL_STATES updated                                    │
│        → Bridge renders via Zoon signal                         │
└─────────────────────────────────────────────────────────────────┘

Remaining issue: LINK actions still have business logic in IO layer
Next step: Phase 8 moves all business logic into DD
```

After Phase 8, ALL logic will be in DD:

```
Target Flow (Phase 8):
┌─────────────────────────────────────────────────────────────────┐
│  Browser event → IO converts to DD Event (no logic)             │
│        → DD dataflow processes:                                 │
│          • Link→Cell join (mappings as DD collection)           │
│          • List operations (antijoin, map, reduce)              │
│          • Cell transforms (toggle, set, increment)             │
│        → All O(delta), all pure                                 │
│        → CELL_STATES updated                                    │
│        → Bridge renders (also O(delta) in future)               │
└─────────────────────────────────────────────────────────────────┘
```

---

## Phase 9: True Incremental Lists (O(delta) End-to-End)

**Status:** 🔴 NOT STARTED

### The Problem: O(n) Masquerading as O(delta)

Investigation reveals that while DD internally computes true incremental diffs, at every observation boundary we materialize to full arrays via `.to_vec()`:

```rust
// worker.rs - 13 instances of .to_vec() found!
let items_vec = current_items.to_vec();  // O(n) copy!
let filtered = items_vec.iter().filter(...).collect::<Vec<_>>();  // O(n)!

// dataflow.rs - Output observation
.inspect(move |((cell_id, value), time, diff)| {
    outputs_clone.lock().unwrap().push(...);  // Side effect!
});
```

**Current Complexity Analysis:**

| Operation | DD Internal | At Boundary | Total |
|-----------|-------------|-------------|-------|
| ListAppend | O(1) diff | O(n) `.to_vec()` | **O(n)** |
| ListRemove | O(1) diff | O(n) `.to_vec()` | **O(n)** |
| ListFilter | O(delta) | O(n) `.to_vec()` | **O(n)** |
| ListMap | O(delta) | O(n) `.to_vec()` | **O(n)** |
| ListCount | O(1) | O(n) `.to_vec()` | **O(n)** |

### The Solution: DD Collections All The Way

Lists should be DD `Collection` handles that flow through the entire pipeline without materialization until final rendering.

```rust
// Target: Collections stay as handles, not arrays
pub enum Value {
    // Scalars (unchanged)
    Number(f64),
    Text(Arc<str>),
    Bool(bool),

    // Collections are HANDLES, not arrays!
    Collection(CollectionHandle),  // Was: List(Arc<Vec<Value>>)
}

pub struct CollectionHandle {
    /// Unique identifier for this collection in the DD graph
    id: CollectionId,
    /// Reference to the traced arrangement for diff observation
    trace: Arc<TraceHandle>,
}

// Operations return NEW handles, never materialize
impl CollectionHandle {
    fn filter(&self, predicate: impl Fn(&Value) -> bool) -> CollectionHandle {
        // Creates new DD filter operator, returns handle
        // O(1) to create, O(delta) when diffs flow
    }

    fn map(&self, transform: impl Fn(Value) -> Value) -> CollectionHandle {
        // Creates new DD map operator, returns handle
        // O(1) to create, O(delta) when diffs flow
    }

    fn append(&self, item: Value) -> CollectionHandle {
        // Inserts (item, +1) diff into input handle
        // O(1) always
    }

    fn remove(&self, item: Value) -> CollectionHandle {
        // Inserts (item, -1) diff into input handle
        // O(1) always
    }
}
```

### Architecture: Lazy Diff Observation

```
┌─────────────────────────────────────────────────────────────────┐
│  CURRENT (Eager Materialization):                                │
│                                                                 │
│  Event → DD processes → .to_vec() → Store Vec in CELL_STATES    │
│                                                                 │
│  Problem: O(n) at every observation point                       │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│  TARGET (Lazy Diff Observation):                                 │
│                                                                 │
│  Event → DD processes → Store HANDLE in CELL_STATES             │
│                        (no materialization!)                    │
│                                                                 │
│  Render → read diffs from handle → apply only changed elements  │
│                                                                 │
│  O(delta) end-to-end!                                           │
└─────────────────────────────────────────────────────────────────┘
```

### What Must Change

| Component | Before | After |
|-----------|--------|-------|
| `Value::List` | `List(Arc<Vec<Value>>)` | `Collection(CollectionHandle)` |
| `StateTransform::ListAppend` | Clones vec, pushes, stores | Inserts diff into handle |
| `StateTransform::ListRemove` | Clones vec, removes, stores | Inserts diff into handle |
| `StateTransform::ListFilter` | `.to_vec().filter()` | Returns new filtered handle |
| `StateTransform::ListMap` | `.to_vec().map()` | Returns new mapped handle |
| `sync_cell_from_dd` | Stores `Value` directly | Stores `Value::Collection(handle)` |
| Bridge rendering | Iterates `Vec<Value>` | Observes diffs from handle |

### Tasks

| Task | Description | Est. Hours |
|------|-------------|------------|
| 9.1 | Define `CollectionHandle` struct with trace reference | 4 |
| 9.2 | Add `Value::Collection(CollectionHandle)` variant | 2 |
| 9.3 | Create `CollectionId` registry in DD worker | 4 |
| 9.4 | Implement `ListAppend` as diff insertion (no clone) | 4 |
| 9.5 | Implement `ListRemove` as diff insertion (no clone) | 4 |
| 9.6 | Implement `ListFilter` as handle composition | 6 |
| 9.7 | Implement `ListMap` as handle composition | 6 |
| 9.8 | Implement `ListCount` via DD `.count()` | 2 |
| 9.9 | Update `sync_cell_from_dd` to handle Collection values | 4 |
| 9.10 | Update bridge to consume diffs for rendering (see Phase 12) | 8 |
| 9.11 | Remove all `.to_vec()` calls from worker.rs | 6 |
| 9.12 | Add tests verifying O(delta) behavior | 4 |

**Estimated effort**: 54 hours

### Key Files to Modify

- `crates/boon/src/platform/browser/engine_dd/core/value.rs` - Add `Collection` variant
- `crates/boon/src/platform/browser/engine_dd/core/types.rs` - Add `CollectionHandle`, `CollectionId`
- `crates/boon/src/platform/browser/engine_dd/core/worker.rs` - Remove `.to_vec()`, handle-based ops
- `crates/boon/src/platform/browser/engine_dd/core/dataflow.rs` - Diff observation channels
- `crates/boon/src/platform/browser/engine_dd/render/bridge.rs` - Diff-based rendering

### Verification

```bash
# After Phase 9:

# 1. No .to_vec() in list operations
grep -n "\.to_vec()" crates/boon/src/platform/browser/engine_dd/core/worker.rs
# Should return 0 matches (or only non-list uses)

# 2. Performance test: 10,000 item list
# Add 10,000 items, then toggle ONE checkbox
# Time: should be <5ms (currently ~100ms due to O(n))

# 3. Memory test: 10,000 item list
# Memory should NOT double when modifying one item
# (currently allocates new Vec = 2x memory briefly)
```

---

## Phase 10: Eliminate `inspect()` Side Effects

**Status:** 🟡 IN PROGRESS - Implementing Option A (capture() approach)

### The Problem: Impure DD Operations

The DD dataflow uses `inspect()` callbacks to push results into an external `Arc<Mutex<Vec>>`:

```rust
// dataflow.rs:692-701 - Violates DD purity!
.inspect(move |((cell_id, value), time, diff)| {
    outputs_clone.lock().unwrap().push(DdOutput {
        cell_id: cell_id.clone(),
        value: value.clone(),
        time: *time,
        diff: *diff,
    });
});
```

**Why This Is Wrong:**
1. DD operators should be **pure** - no side effects
2. `inspect()` creates hidden state mutation inside dataflow
3. Can't replay/retry dataflow without duplicate side effects
4. Locks (`Mutex`) inside DD can cause deadlocks

### The Solution: Two Approaches

#### Option A: Timely's Built-in `capture()` (SIMPLER - RECOMMENDED)

Timely dataflow provides a built-in `capture()` mechanism that's simpler than arrange()+trace:

```rust
use timely::dataflow::operators::Capture;
use timely::dataflow::operators::capture::Extract;

// 1. Tag outputs with metadata (flows through DD as pure data)
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaggedHoldOutput {
    pub hold_id: Arc<str>,
    pub state: DdValue,
    pub should_persist: bool,
}

// 2. Inside dataflow: capture to mpsc channel
let outputs_rx = worker.dataflow(|scope| {
    // ... build collections ...

    // Tag outputs and capture - NO Mutex needed!
    let tagged = hold_output.map(move |(state, _, _)| TaggedHoldOutput {
        hold_id: hold_id.clone(),
        state,
        should_persist,
    });

    tagged.inner.capture()  // Returns mpsc::Receiver
});

// 3. Step the dataflow
while probe.less_than(&time) {
    worker.step();
}

// 4. Extract results AFTER stepping completes
let captured: Vec<(u64, Vec<TaggedHoldOutput>)> = outputs_rx.extract();

// 5. Convert to DocumentUpdates outside the dataflow
let outputs = captured.into_iter()
    .flat_map(|(time, items)| items.into_iter().map(|item| DocumentUpdate { ... }))
    .collect();
```

**How `capture()` Works:**
1. `stream.capture()` returns `mpsc::Receiver<Event<T, C>>`
2. Dataflow sends events to this channel as they're produced (pure message passing)
3. `receiver.extract()` drains the channel and sorts by time AFTER dataflow completes

**Benefits of capture() approach:**
- No Mutex/RefCell anywhere in the dataflow
- Built into timely - no custom infrastructure needed
- Pure message passing semantics
- Simpler than arrange()+trace for streaming outputs

#### Option B: DD Arrange + Trace (COMPLEX - For indexed lookups)

Replace `inspect()` side effects with proper DD output channels that are read AFTER the dataflow step completes:

```rust
// Target: Pure DD with output channel
pub struct DdWorker {
    // Input side
    event_input: InputHandle<u64, Event>,

    // Output side - read AFTER step(), not during
    output_trace: TraceHandle<CellId, Value, u64, isize>,
}

impl DdWorker {
    pub fn step(&mut self) -> Vec<DdOutput> {
        // 1. Step the dataflow (pure, no side effects)
        self.worker.step();

        // 2. Read outputs from trace AFTER step completes
        let outputs = self.output_trace
            .cursor()
            .map(|(cell_id, value, time, diff)| DdOutput {
                cell_id,
                value,
                time,
                diff,
            })
            .collect();

        outputs
    }
}
```

### Circular CELL_STATES Dependency

Currently, the DD worker both reads AND writes `CELL_STATES`:

```rust
// READS (worker.rs) - for hold state reconstruction
let current_value = get_cell_value(&cell_id);

// WRITES (worker.rs via sync_cell_from_dd)
sync_cell_from_dd(cell_id, new_value);
```

**This creates a circular dependency:**
```
CELL_STATES → Worker reads → DD processes → Worker writes → CELL_STATES
                                  ↑_____________________________|
```

### Breaking the Cycle

```
┌─────────────────────────────────────────────────────────────────┐
│  TARGET ARCHITECTURE (No Circular Dependency):                   │
│                                                                 │
│  DD Worker owns ALL state internally:                           │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  DD Dataflow Graph                                        │  │
│  │  ├── cell_states: Collection<(CellId, Value), isize>     │  │
│  │  ├── list_items: Collection<(ListId, Value), isize>      │  │
│  │  └── arrangements for lookup                              │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                 │
│  Output channel (read-only):                                    │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  Bridge subscribes to output trace                        │  │
│  │  Only READS diffs, never writes back                      │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                 │
│  CELL_STATES becomes a CACHE for rendering, not source of truth │
└─────────────────────────────────────────────────────────────────┘
```

### Tasks

| Task | Description | Est. Hours |
|------|-------------|------------|
| 10.1 | Add output trace/arrangement to DD dataflow | 6 |
| 10.2 | Create `OutputCursor` API for reading diffs after step | 4 |
| 10.3 | Remove all `inspect()` calls from dataflow.rs | 2 |
| 10.4 | Remove `Arc<Mutex<Vec<DdOutput>>>` pattern | 2 |
| 10.5 | Store cell state INSIDE DD (as arranged collection) | 8 |
| 10.6 | `get_cell_value()` reads from DD arrangement, not HashMap | 6 |
| 10.7 | Convert `CELL_STATES` from source-of-truth to render cache | 4 |
| 10.8 | Update bridge to use output cursor | 4 |
| 10.9 | Test state consistency (DD internal = render cache) | 4 |
| 10.10 | Performance benchmark (no locks during DD step) | 2 |

**Estimated effort**: 42 hours

### Key Insight: Arrangements as Read Path

DD's `arrange()` creates indexed structures that support efficient point lookups:

```rust
// Cell states arranged by CellId
let cell_states_arranged = cell_states
    .arrange_by_key();  // O(log n) lookup by CellId

// Reading current value (pure, no mutation)
fn get_cell_value(cell_id: &CellId) -> Option<Value> {
    cell_states_arranged
        .trace
        .cursor()
        .seek_key(cell_id)
        .map(|(_, value, _, diff)| {
            if diff > 0 { Some(value.clone()) } else { None }
        })
        .flatten()
}
```

### Verification

```bash
# After Phase 10:

# 1. No inspect() with side effects
grep -n "inspect.*lock\|inspect.*push" crates/boon/src/platform/browser/engine_dd/
# Should return 0 matches

# 2. No Mutex in DD dataflow
grep -n "Arc<Mutex" crates/boon/src/platform/browser/engine_dd/core/dataflow.rs
# Should return 0 matches

# 3. CELL_STATES is only written by output observer (not DD)
grep -n "sync_cell_from_dd\|update_cell" crates/boon/src/platform/browser/engine_dd/core/
# Should only be in output observer, not in dataflow
```

---

## Phase 11: Move Business Logic from IO to DD

**Status:** 🔴 NOT STARTED

### The Problem: Business Logic Scattered in IO Layer

Investigation found 13 of 16 `thread_local!` stores in IO layer contain business logic that should be DD operators:

| Store | Purpose | Business Logic? |
|-------|---------|-----------------|
| `EDITING_GRACE_PERIOD` | Debounce blur events | ✅ Timer logic |
| `ROUTER_MAPPINGS` | URL → filter state | ✅ Route logic |
| `CHECKBOX_TOGGLE_HOLDS` | Track checkbox states | ✅ UI state |
| `TEXT_CLEAR_CELLS` | Clear input after submit | ✅ Form logic |
| `EDITING_EVENT_BINDINGS` | Edit mode handlers | ✅ UI behavior |
| `TOGGLE_EVENT_BINDINGS` | Toggle handlers | ✅ UI behavior |
| `GLOBAL_TOGGLE_BINDINGS` | Global toggle handlers | ✅ UI behavior |
| `TEXT_INPUT_KEY_DOWN_LINK` | Key handler | ✅ UI behavior |
| `LIST_CLEAR_LINK` | Clear list handler | ✅ UI behavior |
| `HAS_TEMPLATE_LIST` | Template detection | ⚠️ Configuration |
| `CURRENT_ROUTE` | Current URL | ✅ Route state |
| `LIST_VAR_NAME` | List variable name | ⚠️ Configuration |
| `ELEMENTS_FIELD_NAME` | Elements field | ⚠️ Configuration |
| `GLOBAL_DISPATCHER` | Event channel | ❌ Keep in IO |
| `TASK_HANDLE` | Async task | ❌ Keep in IO |
| `OUTPUT_LISTENER_HANDLE` | Output listener | ❌ Keep in IO |

### Phase 11a: Event Dispatch as DD Operator

```rust
// Current (IO layer)
thread_local! {
    static EDITING_GRACE_PERIOD: RefCell<HashMap<String, Instant>> = ...;
}

fn fire_global_blur(link_id: &str) {
    // Business logic: grace period check
    if within_grace_period(link_id) {
        return;  // Suppress event
    }
    send_event(link_id, "blur");
}
```

```rust
// Target (DD operator)
fn grace_period_filter<G: Scope>(
    blur_events: &Collection<G, (LinkId, Event), isize>,
    grace_markers: &Collection<G, (LinkId, Timestamp), isize>,
) -> Collection<G, (LinkId, Event), isize> {
    // DD temporal join: only emit blur if no recent grace marker
    blur_events
        .antijoin(&grace_markers.filter(|(_link, ts)| {
            *ts > current_time() - GRACE_PERIOD_MS
        }))
}
```

### Phase 11b: Router as DD Collection

```rust
// Current (IO layer)
thread_local! {
    static ROUTER_MAPPINGS: RefCell<Vec<(String, String, CellId)>> = ...;
}

fn set_filter_from_route(path: &str) {
    // Business logic: find mapping, update cell
    for (pattern, value, cell_id) in ROUTER_MAPPINGS.borrow().iter() {
        if path.matches(pattern) {
            update_cell(cell_id, value);
        }
    }
}
```

```rust
// Target (DD collection + join)
struct RouterMapping {
    pattern: RoutePattern,
    cell_id: CellId,
    value: Value,
}

fn route_filter<G: Scope>(
    route_events: &Collection<G, String, isize>,
    mappings: &Collection<G, RouterMapping, isize>,
) -> Collection<G, (CellId, Value), isize> {
    // DD join: route → matching mappings → cell updates
    route_events
        .flat_map(|path| mappings.filter(|m| m.pattern.matches(&path)))
        .map(|mapping| (mapping.cell_id, mapping.value))
}
```

### Phase 11c: Toggle/Checkbox State as DD

```rust
// Current (IO layer)
thread_local! {
    static CHECKBOX_TOGGLE_HOLDS: RefCell<HashMap<String, CellId>> = ...;
}

// Target (DD arrangement)
// Checkbox state lives in DD cell_states collection
// Toggle handlers are LinkCellMappings (already in Phase 8)
// No separate IO tracking needed
```

### Tasks

| Task | Description | Est. Hours |
|------|-------------|------------|
| 11.1 | Implement `grace_period_filter` DD operator | 8 |
| 11.2 | Remove `EDITING_GRACE_PERIOD` thread_local | 2 |
| 11.3 | Implement `route_filter` DD operator | 6 |
| 11.4 | Remove `ROUTER_MAPPINGS` thread_local | 2 |
| 11.5 | Move `CHECKBOX_TOGGLE_HOLDS` to DD (use Phase 8 mappings) | 4 |
| 11.6 | Move `TEXT_CLEAR_CELLS` to DD cell transforms | 4 |
| 11.7 | Move `EDITING_EVENT_BINDINGS` to DD | 4 |
| 11.8 | Move `TOGGLE_EVENT_BINDINGS` to DD | 4 |
| 11.9 | Move `GLOBAL_TOGGLE_BINDINGS` to DD | 4 |
| 11.10 | Move `TEXT_INPUT_KEY_DOWN_LINK` to DD | 2 |
| 11.11 | Move `LIST_CLEAR_LINK` to DD | 2 |
| 11.12 | Move `CURRENT_ROUTE` to DD state | 2 |
| 11.13 | Clean up dead IO code | 4 |
| 11.14 | Test all examples with pure DD IO | 6 |

**Estimated effort**: 54 hours

### Verification

```bash
# After Phase 11:

# 1. Minimal thread_local in IO
grep -n "thread_local!" crates/boon/src/platform/browser/engine_dd/io/
# Should only show: GLOBAL_DISPATCHER, TASK_HANDLE, OUTPUT_LISTENER_HANDLE

# 2. No business logic in inputs.rs
wc -l crates/boon/src/platform/browser/engine_dd/io/inputs.rs
# Should be <200 lines (currently ~500+)

# 3. Grace period handled by DD
# Double-click to edit, then click away quickly
# Should NOT exit edit mode (grace period active)
# This behavior now comes from DD, not IO
```

---

## Phase 12: Incremental Rendering

**Status:** 🔴 NOT STARTED

### The Problem: Full Re-render on Every Change

The bridge currently re-renders entire lists when any item changes:

```rust
// bridge.rs - Full re-render
fn render_list(&self, items: &[Value]) -> RawElement {
    // Creates ALL elements every time
    let children: Vec<_> = items.iter()
        .map(|item| self.render_value(item))
        .collect();

    El::new().children(children).into_raw_element()
}
```

**Problems:**
1. O(n) DOM operations for any list change
2. Loses DOM state (focus, scroll position, animations)
3. Browser layout thrashing
4. Memory churn (create/destroy elements)

### The Solution: Diff-Based DOM Updates

Instead of re-creating elements, apply diffs to existing DOM:

```rust
// Target: Incremental rendering
pub struct ListRenderer {
    /// Maps item identity → DOM element
    element_map: HashMap<ItemKey, web_sys::Element>,
    /// Container element
    container: web_sys::Element,
}

impl ListRenderer {
    pub fn apply_diffs(&mut self, diffs: &[(Value, isize)]) {
        for (value, diff) in diffs {
            let key = self.extract_key(value);

            match diff {
                1 => {
                    // INSERT: Create element, add to map and DOM
                    let element = self.render_item(value);
                    self.element_map.insert(key.clone(), element.clone());
                    self.container.append_child(&element);
                }
                -1 => {
                    // REMOVE: Find element, remove from map and DOM
                    if let Some(element) = self.element_map.remove(&key) {
                        self.container.remove_child(&element);
                    }
                }
                _ => {
                    // UPDATE: Modify existing element in place
                    if let Some(element) = self.element_map.get(&key) {
                        self.update_element(element, value);
                    }
                }
            }
        }
    }
}
```

### Item Identity for Keyed Updates

For incremental rendering to work, we need stable item identities:

```rust
// Items need stable keys
pub enum ItemKey {
    /// For templated items: the dynamic ID assigned at creation
    DynamicId(u32),
    /// For static items: index in original list
    Index(usize),
    /// For items with explicit key field
    UserKey(Value),
}

// Extract key from item
fn extract_key(value: &Value) -> ItemKey {
    match value {
        Value::Record(fields) => {
            // Check for __dynamic_id (from template instantiation)
            if let Some(id) = fields.get("__dynamic_id") {
                return ItemKey::DynamicId(id.as_u32());
            }
            // Check for user-provided key
            if let Some(key) = fields.get("key") {
                return ItemKey::UserKey(key.clone());
            }
        }
        _ => {}
    }
    // Fallback: use value hash
    ItemKey::Index(hash(value) as usize)
}
```

### Architecture: Reactive Signals with Diff Observation

```
┌─────────────────────────────────────────────────────────────────┐
│  DD Dataflow                                                     │
│  └── cell_states collection                                      │
│      └── arranged by CellId                                      │
└─────────────────────────────────────────────────────────────────┘
                              ↓ (diffs)
┌─────────────────────────────────────────────────────────────────┐
│  Diff Channel (mpsc)                                             │
│  └── (CellId, Value, +1/-1)                                      │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│  ListRenderer                                                    │
│  ├── element_map: HashMap<ItemKey, Element>                      │
│  └── apply_diffs():                                              │
│      +1 → create element, insert                                 │
│      -1 → find element, remove                                   │
│      update → modify in place                                    │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│  DOM (only changed elements touched)                             │
│  └── Preserved: focus, scroll, animations, input values          │
└─────────────────────────────────────────────────────────────────┘
```

### Filter Predicates in DD (Not Render Callbacks)

Currently, filter predicates are evaluated synchronously in render:

```rust
// Current (wrong): Filter evaluated in render callback
fn render_filtered_list(&self, items: &[Value], filter: &str) -> RawElement {
    let visible: Vec<_> = items.iter()
        .filter(|item| self.matches_filter(item, filter))  // Evaluated HERE
        .collect();
    // ... render visible
}
```

This should be a DD operator:

```rust
// Target (correct): Filter is DD operator
let filter_signal: Collection<(CellId, Filter), isize> = ...;
let items: Collection<(ItemKey, Value), isize> = ...;

let visible_items = items
    .join(&filter_signal)
    .filter(|((_key, item), (_cell_id, filter))| {
        filter.matches(item)
    })
    .map(|((key, item), _)| (key, item));

// Render just subscribes to visible_items diffs
// No filter evaluation in render callback!
```

### Tasks

| Task | Description | Est. Hours |
|------|-------------|------------|
| 12.1 | Define `ItemKey` enum for stable item identity | 2 |
| 12.2 | Add `__dynamic_id` to template instantiation | 4 |
| 12.3 | Create `ListRenderer` struct with element map | 6 |
| 12.4 | Implement `apply_diffs()` for insert/remove/update | 8 |
| 12.5 | Create diff channel from DD to bridge | 4 |
| 12.6 | Replace `render_list()` with `ListRenderer` | 6 |
| 12.7 | Move filter predicates from render to DD operator | 8 |
| 12.8 | Handle nested lists (recursive diff application) | 8 |
| 12.9 | Preserve DOM state (focus, scroll) during updates | 4 |
| 12.10 | Performance benchmark: 10,000 items, toggle one | 4 |
| 12.11 | Test animations during incremental updates | 4 |
| 12.12 | Clean up dead rendering code | 4 |

**Estimated effort**: 62 hours

### Verification

```bash
# After Phase 12:

# 1. No full re-render calls
grep -n "render_list\|children(.*collect)" crates/boon/src/platform/browser/engine_dd/render/bridge.rs
# Should return 0 matches (using ListRenderer.apply_diffs instead)

# 2. Performance test: 10,000 items
# Time to toggle one checkbox: <5ms (vs ~500ms with full re-render)

# 3. DOM state preservation
# 1. Type in an input field
# 2. Toggle a checkbox on different item
# 3. Input field should NOT lose focus or text
# (currently loses focus because element recreated)

# 4. Scroll position preserved
# 1. Scroll to bottom of long list
# 2. Add item at top
# 3. Scroll position should remain
# (currently jumps because list recreated)
```

---

## Implementation Priority

Based on impact and dependencies:

```
Phase 9: True Incremental Lists
  ↓ (enables O(delta) operations)
Phase 10: Eliminate inspect() Side Effects
  ↓ (enables pure DD)
Phase 11: Move Business Logic from IO to DD
  ↓ (enables clean architecture)
Phase 12: Incremental Rendering
  ↓ (enables O(delta) rendering)

Total estimated effort: 212 hours (~5-6 weeks)
```

**Recommended order:**

1. **Phase 9** (54h) - Highest impact, enables true O(delta) for lists
2. **Phase 12** (62h) - Complements Phase 9, renders diffs not full lists
3. **Phase 10** (42h) - Architecture cleanup, pure DD
4. **Phase 11** (54h) - Final cleanup, minimal IO layer

**Why Phase 9 before Phase 12?**
Phase 12 (incremental rendering) REQUIRES Phase 9 (collection handles) to provide diffs. Without diffs, we have nothing to apply incrementally.

**Why Phase 12 before Phase 10?**
Phase 12 delivers immediate user-visible performance improvement. Phase 10 is internal cleanup that doesn't change behavior.

---

## Summary: Current State vs Target

| Aspect | Current (Phase 8) | Target (Phase 12) |
|--------|-------------------|-------------------|
| **List storage** | `Arc<Vec<Value>>` | `CollectionHandle` |
| **List operations** | O(n) via `.to_vec()` | O(delta) via DD |
| **DD outputs** | `inspect()` side effects | Output trace/cursor |
| **State authority** | CELL_STATES (worker reads/writes) | DD internal (CELL_STATES is cache) |
| **IO layer** | 13 business logic stores | 3 infrastructure stores |
| **Rendering** | Full re-render | Diff-based incremental |
| **Filter evaluation** | In render callback | DD filter operator |
| **DOM preservation** | Lost on any change | Preserved across updates |

The end result is a **pure DD engine** where:
- All state lives in DD collections
- All operations are O(delta)
- IO layer is thin browser interface only
- Rendering applies diffs, not full replacements
