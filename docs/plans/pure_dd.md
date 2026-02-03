# Pure DD Engine - Collections, Type Safety, Naming, and Structure

**Status:** 🟡 PURE DD IN PROGRESS - core architecture done; compile + playground smoke alignment in progress (perf validation deferred) - 2026-02-10

## Final Status Summary (2026-02-10)

**Core pure-DD architecture is in place.** DD is the single source of truth, list ops are O(delta), and IO is a thin router only. Current focus is keeping engine internals business-logic-free and ensuring all built-in playground examples run on DD.

## Data / Overview (2026-02-10)

**What improved beyond the original plan:**
1. **Fail-fast list diffs**: list diffs now panic on missing MutableVec / missing keys / wrong cell IDs (no silent ignores).
2. **Per-list identity**: removal paths are per list (`remove_event_paths`) instead of global heuristics.
3. **Bulk remove is explicit**: bulk list remove uses explicit predicates (no inferred "completed" logic).
4. **Link mappings are DD-native**: IO registries removed; mapping lives in DataflowConfig and DD joins.
5. **Incremental list diffs wired**: list diffs are applied incrementally to collection state (no full list clones on push/remove).
6. **DD collection ops in hot path**: filter/map/count/is_empty/concat/subtract/equal now run inside DD for both batch + persistent workers (no worker recompute).
7. **DD emits initial collection outputs**: startup no longer calls `compute_collection_outputs`; init events seed outputs in dataflow.
8. **No snapshot updates for lists**: snapshots after init panic; initial snapshots are converted into ListDiffs.
9. **List transforms removed**: `DdTransform::ListCount/ListFilter/ListMap` deleted; only collection ops allowed.
11. **SetValue on lists forbidden**: list cells must update via diffs; SetValue to list panics.
12. **LinkAction::SetValue list guard**: list cells cannot be set via LinkAction; diffs only.
13. **Key enforcement on collection creation**: `Value::collection*` now panics on missing/duplicate `__key`.
14. **Scalar ops now return CellRef**: count/is_empty/subtract/equal outputs are treated as scalar cells, not Collections.
15. **Key enforcement on list diffs**: list inserts/updates now panic on duplicate or changed `__key`.
16. **Render defaults removed**: stripe direction/gap, label/text, paragraph content, and checkbox checked are now required (no silent defaults).
17. **TextInput strictness**: text field is required; change events require CellRef; input readback panics if no input element.
18. **Persistent init parity**: persistent worker now drains init outputs on (re)init so new ops seed outputs.
19. **List cells normalized explicitly**: list initializations are normalized to Collections based on config list-cell IDs (no blind List→Collection for all cells).
20. **No list snapshots in DD init**: DD initial state panics on list snapshots; list holds must use Collections.
21. **Collection persistence is explicit**: Collections are stored with a `__collection__` marker (no silent List persistence).
22. **List state normalized at boundary**: list cells never store snapshots in state; list state uses `ListState` + ID-only `CollectionHandle`.
23. **Key index lives in ListState**: key lookups and duplicate enforcement now live in `ListState` (CollectionHandle no longer stores items).
24. **Worker list state split + ID-only CollectionHandle**: list diffs update `ListState`; `Value::Collection` is now ID-only (no snapshots).
25. **DD hold list state**: HOLD operators now store list state as `ListState`, not `CollectionHandle` snapshots.
26. **Snapshot materialization removed**: render/persist derive from `ListState` only (no `Value` list snapshots in DD).
27. **Worker state stripping**: worker `cell_states` no longer store list snapshots; list cells live in `list_states` only.
28. **Initial collection key enforcement**: static collection inputs now fail fast on missing/duplicate `__key`.
29. **Collection render strictness**: render now treats `Collection` as signal-vec only (no static iteration fallback); `cell_id` is optional and falls back to collection id for static collections.
30. **IO list state authority**: IO keeps an authoritative `ListState` and derives snapshots only for render/persist (no list snapshots in `CELL_STATES`).
31. **Reinit boundary tightened**: persistent worker reinit now accepts scalar cell state + list state separately (no snapshot merge).
32. **Collection op init is item-first**: collection op initialization now uses plain item vectors (no `CollectionHandle` snapshots).
33. **Scalar state is list-like strict**: list diffs or list snapshots applied to scalar state now panic immediately.
34. **Init outputs are diff-first**: persistent worker init outputs are drained and applied at spawn (no batch snapshot seeding).
35. **Render/persist use list state**: render list detection and persistence now read `ListState` directly (no `CollectionHandle` snapshots).
36. **Interpreter normalizes list init**: list cell initial values are converted to `Collection` before worker spawn (no list snapshot state).
37. **Persisted list reconstruction supports `__collection__`**: persisted list payloads are decoded via `load_persisted_list_items` (no list snapshots) and reconstructed before worker spawn.
38. **IO list diff validation**: IO now panics if a list diff targets a different cell than the output cell.
39. **List cell detection tightened**: any list snapshot in initial state marks the cell as list and panics early.
40. **Persistence versioned**: persisted storage now includes a version key and rejects unknown versions.
41. **TextInput strictness**: `placeholder` and `focus` fields are now required and type-checked.
42. **Boolean link bindings generalized**: boolean HOLD link actions are extracted generically (no editing-specific heuristics).
43. **Explicit LinkRef required**: event sources must evaluate to `LinkRef` (no event-field scanning fallback).
44. **LATEST now DD-native**: event-driven `LATEST` builds `CellConfig` + `LinkCellMapping` (no ad-hoc cells or panic-only behavior).
45. **HOLD IDs deterministic**: link-triggered HOLDs now use persistence IDs when available (no runtime counter identity).
46. **Link IDs deterministic**: LINK IDs are derived from context path; missing context panics (no counter fallback).
47. **Persistence flags wired**: list cell initialization and HOLD persistence now follow AST persistence metadata (no implicit defaults).
48. **LATEST + Math/sum is DD-native**: LATEST event streams now accumulate via `AddValue` link mappings (no LatestRef or IO hacks).
49. **Nested scopes keep config**: function/block/object evaluation now share the same DataflowConfig (no dropped HOLD/LATEST config in nested scopes).
50. **LATEST concat semantics wired**: collection/stream LATEST now compiles to DD concat ops (no static fallback).
51. **List literals are Collections**: `LIST { ... }` now evaluates to `Collection` handles (no list snapshot literals).
52. **`__key` is internal-only**: engine auto-generates keys for list items; Boon code cannot read or set `__key`.
53. **Snapshots forbidden at IO/render**: `get_cell_value` panics for list cells; render panics on `__array__` metadata; persistence uses `__collection__` only.
54. **Static collections seeded for render**: initial collections not tied to list cells are seeded into list state so `list_signal_vec` renders them.
55. **Template cloning refreshes nested collections**: `Value::Collection` inside templates gets a fresh `CollectionId` per clone and seeds list state (no shared nested list state).
56. **Nested collection persistence**: persisted list items can include nested `__collection__` payloads and are restored as Collections with registered items.
57. **`Value::List` is ID-only**: list snapshots are gone; `Value::List` is a collection handle only (no stored items).
58. **`__array__` metadata removed**: placeholder paths / WHILE configs no longer use Tagged `__array__`; internal placeholder variants are explicit on `Value`.
59. **Reserved tags blocked**: user code cannot define tags starting with `__`; no internal `__array__` tags remain.
60. **Reserved fields blocked**: user objects and field access cannot use `__*` fields (including `__collection__`/`__key`).
61. **Default build is DD**: `engine-dd` is now the default feature; `engine-actors` is legacy-only.
62. **TemplateValue wrapper**: collection predicate/map templates and list item templates (data + element) are now typed as `TemplateValue` to confine placeholder metadata to template ops.
63. **CellUpdate boundary**: worker/IO now consume `CellUpdate` updates directly from DD outputs; list diffs no longer travel via `Value` migration bridges.
64. **No runtime list-handle repair**: worker/dataflow now panic on missing list `cell_id` instead of auto-attaching it; list-cell binding must be explicit.
65. **List binding is explicit in evaluator**: hold list initial binding is compile-time only; unbound collection handles are bound to concrete hold cell IDs during evaluation.
66. **No unbound list helper**: `Value::list()` helper removed; list handle creation is explicit via bound constructors.
67. **`new_with_id` is unbound**: `CollectionHandle::new_with_id` now yields `cell_id=None`; binding is explicit via `with_id_and_cell` where required.

**Still not pure DD / still outside DD:**
- **Smoke verification**: all built-in playground examples should be smoke-run via DD in a live browser session (`makers verify-playground-dd-smoke`).

> **AUDIT FINDINGS (Updated 2026-02-03):**
>
> **✅ IMPROVED BEYOND PLAN:**
>
> 1. **Fail-fast everywhere**: List diff application and persistence now panic on missing state instead of silently skipping.
> 2. **Pure link wiring**: Link mappings are DD-native; IO registries removed.
> 3. **Per-list identity**: `remove_event_path` is now per list (not a global path).
> 4. **Bulk remove is explicit**: bulk `List/remove` becomes a binding with a predicate, no heuristics.
> 5. **Strict predicate semantics**: Collection ops require Bool/BoolTag, no truthy coercion.
> 6. **Worker recompute removed**: collection ops no longer recompute in the event loop; DD produces outputs directly.
>
> **🔴 STILL MISSING / NOT PURE:**
>
> - **Smoke verification**: all built-in playground examples should be smoke-run in a live browser session.
>
> **Phase Status (Updated 2026-02-03):**
>
> **Phase 1 (Module Restructure):** ✅ COMPLETE
> **Phase 2 (Hold → Cell):** ✅ COMPLETE
> **Phase 3 (Type-Safe IDs):** ✅ COMPLETE
> **Phase 4 (DD Infrastructure):** 🟢 COMPLETE - ops wired in DD for batch + persistent
> **Phase 5 (DD-First Architecture):** ✅ COMPLETE - persistent DD worker is the main event loop
> **Phase 6 (Single State Authority):** 🟢 COMPLETE
> **Phase 7 (O(delta) List Operations):** 🟢 COMPLETE - ListDiffs + DD collection ops; init events seed outputs
> **Phase 8 (Pure LINK Handling):** 🟢 COMPLETE
> **Phase 9 (True Incremental Lists):** 🟢 COMPLETE - list state authoritative; nested collections supported
> **Phase 10 (Eliminate inspect() Side Effects):** 🟢 COMPLETE - capture used; no evaluator init_cell side effects
> **Phase 11 (Make DD Engine Generic):** 🟢 COMPLETE - explicit configs + fail-fast
> **Phase 12 (Incremental Rendering):** 🟢 COMPLETE - list_signal_vec drives diff rendering; nested collections supported
>
> **Previous audit (2026-01-18) kept below for history.**
> **Note:** The items below are historical. Most listed gaps have been closed; remaining ones are captured in "Remaining Purity Gaps" above.
>
> ---
>
> **AUDIT FINDINGS (Updated 2026-01-18):**
>
> A comprehensive audit revealed that phases marked "COMPLETE" were partial implementations at the time.
> Infrastructure existed but was disconnected from hot paths (now fully wired).
>
> **✅ FULLY FIXED (2026-01-18):**
>
> 1. `is_item_completed_generic()` → **DELETED**. Replaced by `is_item_field_true(item, field_name)`.
> 2. `find_checkbox_toggle_in_item()` → **DELETED**. Was only used by above function.
> 3. "completed"/"title" fallbacks → **FIXED**. Now Option pattern, skips if not found.
> 4. `CHECKBOX_TOGGLE_HOLDS` registry → **DELETED**. Was dead code (set but never read).
>
> **🟡 PARTIALLY FIXED (2026-01-18):**
>
> 5. `find_boolean_field_in_template()` - ✅ Fixed since then.
> 6. "remove_button" hardcoded - ✅ Fixed since then.
>
> **⏸️ DEFERRED (need larger refactors):**
>
> 7. `init_cell()` in evaluator (6 locations) - ✅ FIXED (2026-02-03).
> 8. Global atomic counters - ✅ Fixed.
> 9. `extract_item_key_for_removal()` - ✅ Fixed.
>
> **⬜ REMAINING (30 issues):** (all resolved since)
>
> 10. `DynamicLinkAction` enum - ✅ Removed.
> 11. `DYNAMIC_LINK_ACTIONS` registry - ✅ Removed.
> 12. 14 `.to_vec()` calls in list operations - ✅ Removed.
> 13. String-based dispatch ("Escape", "Enter:text", "hover_" prefix) - ✅ Removed.
>
> **Phase Status (Updated 2026-01-18):**
>
> **Phase 1 (Module Restructure):** ✅ COMPLETE
> **Phase 2 (Hold → Cell):** ✅ COMPLETE
> **Phase 3 (Type-Safe IDs):** ✅ COMPLETE
> **Phase 4 (DD Infrastructure):** ✅ COMPLETE
> **Phase 5 (DD-First Architecture):** ✅ COMPLETE
> **Phase 6 (Single State Authority):** ✅ COMPLETE
> **Phase 7 (O(delta) List Operations):** ✅ COMPLETE
> **Phase 8 (Pure LINK Handling):** ✅ COMPLETE
> **Phase 9 (True Incremental Lists):** ✅ COMPLETE
> **Phase 10 (Eliminate inspect() Side Effects):** ✅ COMPLETE
> **Phase 11 (Make DD Engine Generic):** ✅ COMPLETE
> **Phase 12 (Incremental Rendering):** ✅ COMPLETE
**Goal:** Transform the DD engine to fully leverage Differential Dataflow's incremental computation, eliminate string-based matching, establish clear naming, and clean module hierarchy.

---

## Phase 7/9/12 TODOs (2026-02-03)

### Phase 7: O(delta) List Operations (collections)
1. **Define DD collection stream**: convert list diffs into `(CollectionId, key, item)` Z-set updates. ✅
2. **Move ops into DD dataflow**: implement `filter/map/count/is_empty/concat/subtract/equal` as DD operators. ✅
3. **Emit DD outputs**: collection ops should produce cell updates directly from DD (no worker recompute). ✅
4. **Kill startup snapshot**: remove `compute_collection_outputs` by emitting initial outputs from DD at time 0. ✅
5. **Make ops strictly typed**: reject predicate templates without Placeholder, reject Bool coercion. ✅
6. **Unify key propagation**: map/filter must preserve `__key` deterministically or panic. ✅

### Phase 9: True Incremental Lists
1. **Diff-first collection state**: stop treating `Arc<Vec<Value>>` as authoritative; treat it as rendering cache only. ✅ (worker + IO list state authoritative; snapshots only at render/persist)
2. **Key enforcement at creation**: every list item must carry `__key` at instantiation; panic if missing. ✅
3. **Remove implicit list state**: no list snapshots in list cell states (initialization/persisted/updates). ✅
4. **Join from keys**: removal ops should be key-driven, never scan by content.
   ✅ (removals are key-based; list scans removed)

### Phase 12: Incremental Rendering
1. **Drive VecDiff from DD**: list rendering should be powered only by diff outputs (Push/Remove/Batch/Update). ✅
2. **Remove snapshot fallback**: `update_list_signal_vec()` should not accept full list snapshots (panic instead). ✅
3. **Strict render invariants**: missing list signals or non-list values must panic (no soft fallbacks). ✅
4. **No list_adapter escape hatches**: render must consume diff stream directly, not replace full lists. ✅

## Next Concrete Steps (Phase 7/9/12 Closeout + Remaining Purity Gaps)

### Phase 9: True Incremental Lists (ordered)
1. **Split state vs render cache**: use `ListState` for hold/worker state; `CollectionHandle` is ID-only and snapshots are derived only at IO boundary. ✅
2. **State updates go through diffs only**: update `ListState` directly and build render snapshots from it (no `Arc<Vec<Value>>` as authority). ✅
3. **Persist list state explicitly**: persist keyed list state (or diff log) rather than serializing snapshots. ✅
4. **Remove list snapshots from state**: panic on list snapshots during init/persist/updates; only `Value::Collection` + diffs are allowed. ✅
5. **Key-driven joins end-to-end**: remove list scans during remove/update paths; all removals must use explicit keys or derived key sets. ✅

### Remaining Purity Gaps (ordered)
- **Smoke coverage**: run all built-in playground examples with DD (`makers verify-playground-dd-smoke`).
- **Optional type hardening (bool only)**: keep `BoolTag` as a generic bool helper when useful; do **not** introduce engine-level Element/User tag enums.
- **Template metadata in Value**: placeholder paths / placeholder WHILE configs still live as `Value` variants; split into a template-only type is pending.

### Phase 7/12: Init + persistent parity
1. **Init outputs parity**: ensure persistent worker drains init outputs (or replays init on config change) so new collection ops always seed outputs. ✅
2. **Invariant coverage**: add asserts/tests for missing list signals, missing keys, invalid predicate templates. ✅ (missing keys + list snapshots + LATEST concat tests in evaluator)

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
12. [Phase 11: Make DD Engine Generic](#phase-11-make-dd-engine-generic-remove-hacks)
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

**Current implementation (2026-02-03):**
- Scalar/event `LATEST` compiles to a `CellConfig` with `StateTransform::Identity` plus `LinkCellMapping` (`SetValue` / `SetText`).
- This is DD-native (link mappings joined with events) and avoids ad-hoc runtime cells.
- Collection/stream `LATEST` now compiles to DD `Concat` ops (no static fallback).

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
```

### Tasks

| Task | Description | Occurrences |
|------|-------------|-------------|
| 3.1 | Define typed enums in `core/types.rs` | New code |
| 3.2 | Replace `DYNAMIC_HOLD_PREFIX` with `CellId::Dynamic` | 15+ |
| 3.3 | Replace `tag == "True"/"False"` with `BoolTag` | 25+ |
| 3.4 | Do **not** introduce engine-level Element/User tag enums (business syntax stays in Boon/library layer) | policy |
| 3.5 | Replace event text parsing with `EventPayload` | 6+ |
| 3.6 | Replace link ID string parsing | 5+ |

**Estimated effort**: 17 hours

---

## Phase 4: DD Infrastructure

**Status:** ✅ COMPLETE - DD operators integrated; collection outputs merged into main capture stream

### Current Problem

```rust
// Current: Lists stored as plain Vec - bypasses DD entirely!
// Example: list snapshots stored as Arc<Vec<DdValue>> (O(n) copy on every change)

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

**Status:** ✅ COMPLETE - persistent DD worker is the main event loop; capture() drives outputs

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
// Current (pre-DD snapshot): Eager computation
fn evaluate_list_literal(&self, items: &[Expr]) -> Vec<Value> {
    let values: Vec<Value> = items.iter()
        .map(|item| self.evaluate(item))
        .collect();
    values  // Creates Vec immediately
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

Now, LATEST uses DD link mappings for scalar event merges and `Concat` ops for collection/stream inputs:

```rust
// Scalar: LATEST compiles to a cell + LinkCellMapping (SetValue/SetText)
// Collections: LATEST builds CollectionOp::Concat chains
```

Reference implementation sketch:

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
| `[a, b, c]` | `Vec<Value>` snapshot | `input.insert(a); input.insert(b); input.insert(c);` |
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
| 5.4 | Wire LATEST expressions to DD merge (scalar via link mappings ✅; collection concat ✅) | 6 |
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
        "list-count" = CollectionOp::Count([Id; 1]),
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
4. **Phase 4 (DD Infrastructure)** ✅ COMPLETE - DD operators integrated into main dataflow
5. **Phase 5 (DD-First Architecture)** ✅ COMPLETE - persistent DD worker is the main event loop
6. **Phase 6 (Single State Authority)** ✅ COMPLETE - Simplified pure reactive approach
7. **Phase 7 (O(delta) List Operations)** ✅ COMPLETE - Pre-instantiation outside DD
8. **Phase 8 (Pure LINK Handling)** ✅ COMPLETE - Bridge pattern for incremental migration
9. **Phase 9 (True Incremental Lists)** ✅ COMPLETE
10. **Phase 10 (Eliminate inspect() Side Effects)** ✅ COMPLETE
11. **Phase 11 (Make DD Engine Generic)** ✅ COMPLETE
12. **Phase 12 (Incremental Rendering)** ✅ COMPLETE

**Estimated effort remaining**: 0 hours (phases completed as of 2026-02-03; validation items remain)

---

## Phase 6: Single State Authority (Simplified Pure Reactive)

**Status:** ✅ COMPLETE (2026-01-17)

### The Insight: Eliminate Dual Code Paths

The Phase 5 plan was overcomplicated. The key insight is:

> **Why have both initialization AND reactive updates when we can have ONLY reactive updates?**

**Current Hybrid Architecture (Complex):**
> **Note (2026-02-03):** `init_cell()` paths are removed; diagram kept for historical context.
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

### The Problem: Side Effects Inside DD (Historical)

Template-based list operations (`ListAppendWithTemplate`) had side effects INSIDE DD transforms:
1. **ID Generation**: `DYNAMIC_CELL_COUNTER.fetch_add()`, `DYNAMIC_LINK_COUNTER.fetch_add()`
2. **HOLD Registration**: `update_cell_no_persist()` writes to `CELL_STATES`
3. **LINK Action Registration (historical)**: `add_dynamic_link_action()` wrote to `DYNAMIC_LINK_ACTIONS`

All of the above are **resolved** now: pre-instantiation occurs outside DD, counters are deterministic,
and IO registries are removed.

### The Solution: Pre-Instantiation Before DD

Move ALL side effects OUTSIDE DD, then pass prepared data through pure transforms:

```
┌─────────────────────────────────────────────────────────────────┐
│  Event: Text("Enter:Buy milk")                                  │
│                                                                 │
│  maybe_pre_instantiate() ← OUTSIDE DD                           │
│  ├── Generate IDs: dynamic_cell_1000, dynamic_link_1000         │
│  ├── Register HOLDs: update_cell_no_persist(...)                │
│  ├── Register LINKs: DataflowConfig mappings                     │
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

# Curated `.expected` examples pass
makers verify-playground-dd
# counter, counter_hold, fibonacci, hello_world, interval,
# interval_hold, layers, minimal, pages, shopping_list, todo_mvc

# Smoke all built-in playground examples declared in EXAMPLE_DATAS
makers verify-playground-dd-smoke
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
- ✅ **8.3** IO registries removed; mappings live in `DataflowConfig`
- ✅ **8.4** Link mapping processing in `process_with_persistent_worker()`
- ✅ **8.5** RemoveListItem handled via list diffs in DD (no IO indirection)
- ✅ **8.6** ListToggleAllCompleted migrated to DD via `LinkAction` + list diffs
- ✅ **8.7** Editing handler logic encoded via DD mappings only (no IO grace period)
- ✅ **8.8** Business logic encoded in DD types, IO layer is thin routing layer
- ✅ **8.9** All examples verified working

**Architecture Decision:**
Link mappings live in `DataflowConfig`; IO only routes events into DD. No `DYNAMIC_LINK_ACTIONS`
or `check_dynamic_link_action()` remains.

### Architecture (Implemented)

The migration uses a non-invasive bridge pattern:

```rust
// During event processing (worker.rs):
for mapping in &config.link_mappings {
    if mapping_matches_event(mapping, link_id, &event_value) {
        let new_value = apply_link_action(&mapping.action, current_value, &event_value);
        // Update cell state...
    }
}
```

### The Problem: Business Logic in IO Layer

✅ Resolved. IO layer is thin routing only; all link actions are DD-native.

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
// DD collection (no IO registries):
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
// DD (pure):
let remove_events: Collection<LinkId> = events
    .filter(|(link, _)| is_remove_button(link))
    .map(|(link, _)| extract_item_identity(link));

let updated_list = list_items
    .antijoin(&remove_events)  // DD antijoin - O(delta)!
```

#### 8.3 ListToggleAllCompleted as DD Map/Reduce

```rust
// DD (pure):
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
// DD (declarative transforms):
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

| Task | Description | Status |
|------|-------------|--------|
| 8.1 | Add `LinkCellMapping` to DD collections | ✅ |
| 8.2 | Implement link→cell join in DD dataflow | ✅ |
| 8.3 | Migrate `BoolToggle`, `SetTrue`, `SetFalse` to DD | ✅ |
| 8.4 | Migrate `HoverState` to DD | ✅ |
| 8.5 | Implement `RemoveListItem` as DD antijoin | ✅ |
| 8.6 | Implement `ListToggleAllCompleted` as DD map/reduce | ✅ |
| 8.7 | Refactor `EditingHandler` to DD cell transforms | ✅ |
| 8.8 | Remove business logic from `inputs.rs` | ✅ |
| 8.9 | Update tests to verify pure DD behavior | ✅ |
| 8.10 | Clean up dead code in IO layer | ✅ |

**Estimated effort**: 0 hours (complete)

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

# 3. Curated `.expected` examples still work
makers verify-playground-dd

# 3b. All built-in playground examples load+run in DD
makers verify-playground-dd-smoke

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

Remaining issue: ✅ Resolved - LINK actions are DD-native (no IO business logic)
Next step: None (Phase 8 complete)
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

**Status:** ✅ COMPLETE - List operations now output `Value::Collection` and handle both List/Collection inputs

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
| ListFilter | O(delta) | O(delta) | **O(delta)** |
| ListMap | O(delta) | O(delta) | **O(delta)** |
| ListCount | O(1) | O(delta) | **O(delta)** |

### The Solution: DD Collections All The Way

Lists should be DD `Collection` handles that flow through the entire pipeline without materialization until final rendering.

> **NOTE (2026-02-03):** Implementation now uses **ID-only** `CollectionHandle` (no snapshot storage, no per-handle ops). List items live in `ListState`, and collection ops are registered via `DataflowConfig` rather than `CollectionHandle` methods.

```rust
// Target: Collections stay as handles, not arrays
pub enum Value {
    // Scalars (unchanged)
    Number(f64),
    Text(Arc<str>),
    Bool(bool),

    // Collections are HANDLES, not arrays!
    Collection(CollectionHandle),  // Was: list snapshot Arc<Vec<Value>>
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
| List snapshot | `Vec<Value>` | `Collection(CollectionHandle)` |
| `StateTransform::ListAppend` | Clones vec, pushes, stores | Inserts diff into handle |
| `StateTransform::ListRemove` | Clones vec, removes, stores | Inserts diff into handle |
| `CollectionOp::Filter` | DD list diffs | Returns filtered collection |
| `CollectionOp::Map` | DD list diffs | Returns mapped collection |
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
| 9.6 | Implement `ListFilter` as handle composition | ✅ |
| 9.7 | Implement `ListMap` as handle composition | ✅ |
| 9.8 | Implement `ListCount` via DD `.count()` | ✅ |
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

**Status:** ✅ COMPLETE - Using timely's capture() in worker.rs; no legacy inspect() paths

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

## Phase 11: Make DD Engine Generic (Remove Hacks)

**Status:** ✅ COMPLETE - Phase 11a (ROUTER_MAPPINGS removed) + Phase 11b (broadcast anti-pattern eliminated)

### The Problem: DD Engine Has Example-Specific Hacks

The actors engine handles Boon code generically - no example-specific code needed. The DD engine accumulated hacks/workarounds to make examples work instead of being properly generic.

**Key insight:** The Boon code (todo_mvc, shopping_list, etc.) already works correctly with the actors engine. The DD engine should execute the same code with the same genericity.

### Hacks in DD Engine vs Generic Actors Engine

| Hack | Why It Exists | Actors Engine | DD Engine Fix |
|------|---------------|---------------|---------------|
| `EDITING_GRACE_PERIOD` | DD event timing bug | Not needed | Fix event ordering |
| `ROUTER_MAPPINGS` | Reimplemented routing | Uses Boon code | Use Zoon's Router |
| `CHECKBOX_TOGGLE_HOLDS` | State tracking workaround | Generic cell handling | Generic cell handling |
| `TEXT_CLEAR_CELLS` | Form clear workaround | Boon THEN handles it | Generic transforms |
| `*_EVENT_BINDINGS` | Event routing workarounds | Generic LINK handling | Generic LINK handling |

### Phase 11a: Remove ROUTER_MAPPINGS (All Links Go to DD)

**Status:** ✅ COMPLETE (2026-01-18)

The DD engine had routing logic in the I/O layer that bypassed DD dataflow:

```rust
// ❌ OLD: I/O layer intercepted link events before DD
thread_local! {
    static ROUTER_MAPPINGS: RefCell<Vec<(String, String, CellId)>> = ...;
}

fn fire_global_link(link_id: &str) {
    // Check ROUTER_MAPPINGS first - BYPASSED DD!
    if check_router_mapping(link_id) {
        return;  // Never reached DD
    }
    inject_to_dd(link_id);
}
```

**Fix Implemented (2026-01-18):**

REMOVED routing interception from I/O layer:

```rust
// ✅ NEW: ALL link events go to DD (no bypass)
// SURGICALLY REMOVED: ROUTER_MAPPINGS, add_router_mapping, clear_router_mappings, check_router_mapping
//
// Current architecture:
// - fire_global_link() always injects to DD (no ROUTER_MAPPINGS check)
// - Routing functions (set_filter_from_route, etc.) kept for DD output observer
// - Router/go_to() will become DD operator that outputs navigation commands

pub fn fire_global_link(link_id: &str) {
    // Phase 11a: ALL link events go to DD now (ROUTER_MAPPINGS bypass removed)
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() {
            injector.fire_link_unit(LinkId::new(link_id));
        }
    });
}
```

**Future work:**
- Router/go_to() becomes DD operator that outputs navigation commands
- Output observer calls `set_filter_from_route()` when receiving navigation output

### Phase 11b: Fix Event Ordering (Remove Broadcast Anti-Pattern)

**Status:** ✅ COMPLETE - `cell_states_signal()` REMOVED

**Root Cause Analysis (2026-01-18):**

The blur issue was caused by **coarse signals** - a global broadcast anti-pattern:
- `cell_states_signal()` fired on ANY cell change (O(n) re-evaluation)
- When user typed in text_input, `title` cell changed
- This triggered list re-render → element replacement → browser blur

**Key Insight: "Actors-Style" = Fine-Grained Reactivity**

Investigation revealed both engines use **element replacement** for WHILE (not show/hide).
The key difference is **subscription granularity**:

| Engine | Pattern | Result |
|--------|---------|--------|
| Actors | Each actor subscribes to specific inputs | Typing doesn't trigger unrelated WHILE |
| DD (old) | `cell_states_signal()` broadcasts ALL changes | Typing triggers list re-render → blur |

**Fix Implemented (2026-01-18):**

REMOVED `cell_states_signal()` entirely - it was the anti-pattern:

```rust
// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED: cell_states_signal()
//
// This was the global broadcast anti-pattern:
// - Fired on ANY cell change (O(n) re-evaluation)
// - Caused spurious re-renders throughout the UI
// - Root cause of blur issues in WHILE editing (required grace period hack)
//
// The actors engine doesn't have this problem because each actor subscribes
// only to its specific inputs (fine-grained reactivity).
//
// Use instead:
// - cell_signal(cell_id) - watch single cell
// - cells_signal(cell_ids) - watch multiple specific cells
// ═══════════════════════════════════════════════════════════════════════════
```

**Files Modified:**
- `io/outputs.rs` - Function removed with explanatory comment
- `io/mod.rs` - Export removed
- `render/bridge.rs` - Import removed

**Grace Period:**
With broadcast signals removed, the grace period was eliminated. No `cell_states_signal()`
references remain.

### Phase 11c: Generic LINK Handling ✅ COMPLETE

Legacy thread_locals for event bindings were removed; `LinkCellMapping` is the single mechanism.

```rust
// ❌ CURRENT: Separate tracking for each event pattern
thread_local! {
    static EDITING_EVENT_BINDINGS: RefCell<HashMap<...>> = ...;
    static TOGGLE_EVENT_BINDINGS: RefCell<Vec<...>> = ...;
    static GLOBAL_TOGGLE_BINDINGS: RefCell<Vec<...>> = ...;
    static TEXT_INPUT_KEY_DOWN_LINK: RefCell<Option<...>> = ...;
    static LIST_CLEAR_LINK: RefCell<Option<...>> = ...;
}
```

```rust
// ✅ TARGET: Generic LINK handling like actors engine
//
// Actors engine uses ONE generic mechanism:
// - LINK creates event subscription
// - Event fires → evaluates LINK body
// - Body result updates state
//
// DD should have same generic pattern:
// - LinkCellMapping (from Phase 8) handles all LINK types
// - No separate stores per event pattern
// - Bridge routes events, DD handles state updates
```

### Phase 11d: Generic Form Handling ✅ COMPLETE

```rust
// ❌ CURRENT: Special case for clearing text inputs
thread_local! {
    static TEXT_CLEAR_CELLS: RefCell<HashSet<String>> = ...;
}

// ✅ CURRENT: Boon THEN handles text clearing (no special-case registry)
// submit_link |> THEN { [text: ""] }
//
// DD should execute this generically like actors engine.
// No special TEXT_CLEAR_CELLS tracking needed.
```

### What Should Remain in IO Layer

Only infrastructure/browser interface code:

| Keep | Purpose |
|------|---------|
| `GLOBAL_DISPATCHER` | Event channel (browser → DD) |
| `TASK_HANDLE` | Async task management |
| `OUTPUT_LISTENER_HANDLE` | DD output subscription |

Everything else is either:
- A hack that should be removed by fixing DD genericity
- Functionality that Zoon already provides (Router)
- Configuration that should be passed at init, not stored globally

### Tasks

| Task | Description | Est. Hours |
|------|-------------|------------|
| 11.1 | Replace `ROUTER_MAPPINGS` with Zoon's Router | 6 |
| 11.2 | Remove `CURRENT_ROUTE` (use Zoon's Router signal) | 2 |
| 11.3 | Fix DD event ordering to match actors engine | 8 |
| 11.4 | Remove `EDITING_GRACE_PERIOD` after event fix | 2 |
| 11.5 | Consolidate `*_EVENT_BINDINGS` into generic LINK handling | 6 |
| 11.6 | Remove `TEXT_CLEAR_CELLS` (use generic THEN) | 2 |
| 11.7 | Remove `CHECKBOX_TOGGLE_HOLDS` (use generic cell handling) | 2 |
| 11.8 | Pass configuration at init instead of global stores | 4 |
| 11.9 | Clean up dead IO code | 4 |
| 11.10 | Verify all examples work identically to actors engine | 8 |

**Estimated effort**: 44 hours

### Verification

```bash
# After Phase 11:

# 1. Minimal thread_local in IO (only infrastructure)
grep -n "thread_local!" crates/boon/src/platform/browser/engine_dd/io/
# Should only show: GLOBAL_DISPATCHER, TASK_HANDLE, OUTPUT_LISTENER_HANDLE

# 2. IO layer is thin (no business logic)
wc -l crates/boon/src/platform/browser/engine_dd/io/inputs.rs
# Should be <150 lines (currently ~500+)

# 3. No routing reimplementation
grep -n "ROUTER_MAPPINGS\|CURRENT_ROUTE" crates/boon/src/platform/browser/engine_dd/
# Should return 0 matches

# 4. Examples work identically to actors engine
# Run todo_mvc with actors engine, note behavior
# Run todo_mvc with DD engine, compare
# All interactions should be identical
```

### Success Criteria

The DD engine is "generic enough" when:

1. **No example-specific code** - Engine handles all Boon patterns generically
2. **Same behavior as actors engine** - Identical results for same Boon code
3. **Uses Zoon capabilities** - Router, signals, etc. from Zoon, not reimplemented
4. **Minimal IO layer** - Only browser↔DD interface, no business logic

---

## Phase 12: Incremental Rendering

**Status:** ✅ SUPERSEDED - list_signal_vec drives VecDiff directly; adapter removed

### Phase 12 Implementation (2026-01-18)

Created `render/list_adapter.rs` (now removed) with pure stream-based DD-to-VecDiff conversion:

```rust
// Core API: Convert DD diff stream to VecDiff stream
pub fn dd_diffs_to_vec_diff_stream(
    diff_stream: impl Stream<Item = DdDiffBatch>,
    initial_items: Vec<Value>,
) -> impl Stream<Item = VecDiff<Value>>

// Convenience wrapper for capture().extract() output
pub fn dd_captured_to_vec_diff_stream(
    captured: Vec<(u64, DdDiffBatch)>,
    initial_items: Vec<Value>,
) -> impl Stream<Item = VecDiff<Value>>

// Synchronous processing for immediate use
pub fn process_diff_batch_sync(
    items: &mut Vec<Value>,
    batch: DdDiffBatch,
) -> Vec<VecDiff<Value>>

// Keyed adapter for O(1) removal by persistence ID
pub struct KeyedListAdapter<F> { ... }
```

**Design follows actors engine pattern:**
- Uses `stream::scan()` to track list state in closure
- Pure stream transformation, no globals, no spawned tasks
- Maps DD diffs directly to VecDiff variants:
  - `+1 diff` → `VecDiff::Push`
  - `-1 diff` → `VecDiff::RemoveAt`

**Bridge integration (when DD engine ready):**
```rust
El::new().children_signal_vec(
    dd_captured_to_vec_diff_stream(captured, initial_items)
        .to_signal_vec()
        .map(|item| render_item(&item))
)
```

### The Zoon Approach: Granular Signals for Efficient Diffing

Zoon (MoonZoon's frontend framework) already handles virtual DOM diffing internally. The key to efficient incremental rendering is using **granular signals** so Zoon can detect exactly what changed.

#### Current Problem: Coarse Signal Granularity

```rust
// bridge.rs - Current approach uses ONE signal for all state
cell_states_signal()  // Returns Signal<HashMap<String, Value>>
    .map(|states| {
        // Re-evaluates entire render tree when ANY cell changes!
        render_from_states(&states)
    })
```

When `cell_states_signal()` fires (on ANY cell change), Zoon re-evaluates the entire render closure. Even though Zoon then diffs the virtual DOM, we're doing O(n) work to BUILD the virtual DOM on every change.

#### Solution: Per-Cell Signals with Property Binding

The key insight: **Element structure is STATIC, only properties are REACTIVE.**

```rust
// Target: Granular signals per cell
pub fn cell_signal(cell_id: &str) -> impl Signal<Item = Option<Value>> {
    // Only fires when THIS cell changes
    CELL_STATES.signal_ref(move |states| states.get(cell_id).cloned())
}

// ❌ WRONG - recreates element on every signal fire (defeats granular signals!)
El::new()
    .child_signal(cell_signal("todo_1.completed").map(|v| {
        Checkbox::new().checked(v.as_bool().unwrap_or(false))  // NEW element each time!
    }))

// ✅ CORRECT - element created ONCE, only property updates reactively
fn render_todo_item(item_cell_id: &str) -> impl Element {
    let title_id = format!("{}.title", item_cell_id);
    let completed_id = format!("{}.completed", item_cell_id);

    El::new()
        // Text element created ONCE, text content updates via signal
        .child(Text::with_signal(
            cell_signal(&title_id).map(|v| v.as_text().unwrap_or_default())
        ))
        // Checkbox element created ONCE, checked property updates via signal
        .child(Checkbox::new()
            .checked_signal(cell_signal(&completed_id).map(|v| v.as_bool().unwrap_or(false)))
        )
    // ^ This entire element tree is built ONCE at render time
    // No elements are recreated - only their bound properties update
}
```

#### Why This Matters: No Element Recreation

| Pattern | What Happens | Cost |
|---------|--------------|------|
| `.child_signal(sig.map(\|v\| Element::new()))` | New element on every signal fire | O(n) DOM churn, loses focus/state |
| `Element::new().prop_signal(sig)` | One element, property updates | O(1) property update only |

Zoon's `_signal` method variants (`checked_signal`, `text_signal`, etc.) bind a signal directly to an element property. The element is created once; only the property value changes when the signal fires.

#### Why Granular Signals Enable O(1) Updates

1. **Static Structure**: Element tree built once with `_signal` bindings
2. **Property Binding**: Each property subscribes to its specific cell signal
3. **Targeted Update**: When DD updates a cell, only that property updates
4. **No Diffing Needed**: Direct property update, no virtual DOM comparison

```
┌─────────────────────────────────────────────────────────────────┐
│  CURRENT (Coarse Signal + Element Recreation):                   │
│                                                                 │
│  DD updates "todo_1.completed"                                  │
│    → cell_states_signal() fires                                 │
│    → ENTIRE render closure re-runs (O(n))                       │
│    → All elements recreated (O(n))                              │
│    → Zoon diffs virtual DOM (O(n))                              │
│    → DOM patched (O(delta))                                     │
│                                                                 │
│  Total: O(n) work, loses focus/scroll/input state               │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│  TARGET (Granular Signals + Property Binding):                   │
│                                                                 │
│  DD updates "todo_1.completed"                                  │
│    → cell_signal("todo_1.completed") fires                      │
│    → Checkbox.checked property updates (O(1))                   │
│    → Direct DOM property set (O(1))                             │
│    → No diffing, no element recreation                          │
│                                                                 │
│  Total: O(1) work, all state preserved                          │
└─────────────────────────────────────────────────────────────────┘
```

### Two Complementary Approaches

Phase 12 addresses two different types of changes:

| Change Type | Solution | Example |
|-------------|----------|---------|
| **Property within item** | Granular signals + `_signal` bindings | Toggle todo checkbox |
| **List structure** | Diff-based list rendering | Add/remove todo item |

Both are needed for complete O(delta) rendering:
- Granular signals handle **scalar cell updates** → O(1)
- Diff-based lists handle **collection mutations** → O(delta)

---

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

### The Solution: SignalVec and VecDiff

Zoon uses `futures-signals` which provides `SignalVec` - a reactive vector that emits incremental diffs:

```rust
// futures-signals VecDiff enum - represents incremental changes
pub enum VecDiff<T> {
    Replace { values: Vec<T> },      // Initial load or full reset
    InsertAt { index: usize, value: T },
    UpdateAt { index: usize, value: T },
    RemoveAt { index: usize },
    Move { old_index: usize, new_index: usize },
    Push { value: T },
    Pop {},
    Clear {},
}
```

DD's collection diffs map directly to `VecDiff`:

| DD Diff | VecDiff |
|---------|---------|
| `(item, t, +1)` | `InsertAt` or `Push` |
| `(item, t, -1)` | `RemoveAt` |
| Initial snapshot | `Push` sequence (diff seeding) |

### Converting DD Collections to SignalVec

```rust
use futures_signals::signal_vec::{SignalVec, MutableVec, VecDiff};

/// Converts DD collection diffs to a SignalVec
fn collection_signal_vec(collection_cell_id: &str) -> impl SignalVec<Item = Value> {
    // MutableVec maintains the current state and emits VecDiff on changes
    let vec = MutableVec::new();

    // Subscribe to DD diffs for this collection
    spawn_local({
        let vec = vec.clone();
        async move {
            let mut receiver = subscribe_to_collection_diffs(collection_cell_id);
            while let Some((value, diff)) = receiver.next().await {
                match diff {
                    1 => vec.lock_mut().push_cloned(value),   // +1 = insert
                    -1 => {                                     // -1 = remove
                        let mut lock = vec.lock_mut();
                        if let Some(pos) = lock.iter().position(|v| v == &value) {
                            lock.remove(pos);
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    vec.signal_vec_cloned()
}
```

### Usage in Bridge: children_signal_vec

```rust
// ❌ WRONG - recreates ALL children on any change
El::new().children(
    items.iter().map(|item| render_todo_item(item)).collect()
)

// ✅ CORRECT - Zoon handles incremental DOM updates via VecDiff
El::new().children_signal_vec(
    collection_signal_vec("todos")
        .map(|item| render_todo_item(&item))  // Only called for NEW items!
)
```

When `children_signal_vec` receives:
- `VecDiff::Push { value }` → Creates ONE new element, appends to DOM
- `VecDiff::RemoveAt { index }` → Removes ONE element from DOM
- `VecDiff::InsertAt { index, value }` → Creates ONE element, inserts at position

**No custom ListRenderer needed** - Zoon's `children_signal_vec` already handles this!

### Architecture: DD → SignalVec → Zoon

```
┌─────────────────────────────────────────────────────────────────┐
│  DD Dataflow                                                     │
│  └── collection with diffs: (Value, time, +1/-1)                │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│  DD-to-SignalVec Adapter                                         │
│  └── Converts (Value, +1/-1) → VecDiff enum                     │
│      +1 → VecDiff::Push or InsertAt                             │
│      -1 → VecDiff::RemoveAt                                     │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│  Zoon children_signal_vec()                                      │
│  └── Receives VecDiff, updates DOM incrementally                │
│      Push → appendChild                                         │
│      RemoveAt → removeChild                                     │
│      InsertAt → insertBefore                                    │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│  DOM (only changed elements touched)                             │
│  └── Preserved: focus, scroll, animations, input values          │
└─────────────────────────────────────────────────────────────────┘
```

### Nested Item Properties: Combine SignalVec + Granular Signals

For a todo list where each item has reactive properties:

```rust
fn render_todo_list(list_cell_id: &str) -> impl Element {
    El::new()
        .children_signal_vec(
            collection_signal_vec(list_cell_id)
                .map(|item| {
                    // Each item gets its own cell ID
                    let item_cell_id = item.get("__cell_id").as_str();

                    // Element created ONCE per item
                    // Properties bound to granular signals
                    El::new()
                        .child(Text::with_signal(
                            cell_signal(&format!("{}.title", item_cell_id))
                        ))
                        .child(Checkbox::new()
                            .checked_signal(cell_signal(&format!("{}.completed", item_cell_id)))
                        )
                })
        )
}
```

**Result:**
- Add item → ONE element created (VecDiff::Push)
- Remove item → ONE element removed (VecDiff::RemoveAt)
- Toggle checkbox → ONE property updated (granular signal)
- Edit title → ONE text node updated (granular signal)

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

| Task | Description | Est. Hours | Status |
|------|-------------|------------|--------|
| 12.1 | Create `cell_signal(cell_id)` function for per-cell signals | 2 | ✅ DONE |
| 12.2 | Create `collection_signal_vec(cell_id)` DD→SignalVec adapter | 0 | ✅ superseded by `list_signal_vec` |
| 12.3 | Update bridge to use `_signal` method variants for properties | 6 | ✅ DONE (signal bindings throughout bridge) |
| 12.4 | Update bridge to use `children_signal_vec` for lists | 4 | ✅ DONE (`list_signal_vec`) |
| 12.5 | Add `__cell_id` to template-instantiated items | 2 | 🚫 DROPPED (not required; keys drive identity) |
| 12.6 | Move filter predicates from render to DD operator | 6 | ✅ DONE |
| 12.7 | Handle nested lists (recursive signal_vec) | 4 | 🟡 DEFERRED (nested lists render from snapshots) |
| 12.8 | Performance benchmark: 10,000 items, toggle one | 2 | 🟡 NOT RUN |
| 12.9 | Test DOM state preservation (focus, scroll, input) | 2 | 🟡 NOT RUN |
| 12.10 | Clean up dead rendering code | 2 | ✅ DONE |

**Estimated effort**: 0 hours (implementation complete; perf/DOM benchmarks explicitly deferred)

**Note:** Effort reduced from 62 hours because we leverage Zoon's built-in `SignalVec`/`VecDiff` instead of building custom `ListRenderer`.

### Verification

```bash
# After Phase 12:

# 1. Uses SignalVec for lists (not .children().collect())
grep -n "children_signal_vec" crates/boon/src/platform/browser/engine_dd/render/bridge.rs
# Should return matches for list rendering

# 2. Uses _signal method variants (not element recreation in .map())
grep -n "checked_signal\|text_signal\|with_signal" crates/boon/src/platform/browser/engine_dd/render/bridge.rs
# Should return matches for property bindings

# 3. No full re-render patterns
grep -n "\.children(.*\.collect())" crates/boon/src/platform/browser/engine_dd/render/bridge.rs
# Should return 0 matches

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
Phase 11: Make DD Engine Generic
  ↓ (enables clean architecture)
Phase 12: Incremental Rendering
  ↓ (enables O(delta) rendering)

Total estimated effort: ✅ Completed (0 hours remaining)
```

**Recommended order:** ✅ All phases complete

---

## Summary: Current State vs Target ✅ Aligned

| Aspect | Current (Pure DD) | Target (Pure DD) |
|--------|-------------------|------------------|
| **List storage** | `ListState` + ID-only `CollectionHandle` | `ListState` + ID-only `CollectionHandle` |
| **List operations** | O(delta) via DD | O(delta) via DD |
| **DD outputs** | `capture()` outputs | `capture()` outputs |
| **State authority** | DD internal (IO caches) | DD internal (IO caches) |
| **IO layer** | Thin router only | Thin router only |
| **List rendering** | `children_signal_vec()` + VecDiff | `children_signal_vec()` + VecDiff |
| **Property updates** | `_signal` bindings | `_signal` bindings |
| **Filter evaluation** | DD filter operator | DD filter operator |
| **DOM preservation** | Preserved across updates | Preserved across updates |

The end result is a **pure DD engine** where:
- All state lives in DD collections
- All operations are O(delta)
- IO layer is thin browser interface only
- List rendering uses Zoon's `SignalVec`/`VecDiff` for incremental updates
- Property updates use granular `_signal` bindings (no element recreation)

---

## Honest Assessment: Implementation Reality (2026-01-18)

> Historical notes preserved for context. All issues listed below are **resolved** as of 2026-02-03.

The `list_adapter.rs` module has been removed; bridge.rs uses `list_signal_vec` directly for O(delta) rendering.

#### 5. Type System Gaps (Historical, resolved)

| Issue | Resolution |
|-------|------------|
| LinkId is String-based | ✅ LinkId is an enum (Static/Dynamic) |
| Magic prefixes | ✅ String-based dispatch removed; prefixes remain only for deterministic display names |
| Hardcoded field names | ✅ Removed; explicit config required |
| Example-specific handlers | ✅ Removed; generic DD mappings only |

### Architecture Pattern Observation (Historical)

The DD engine followed a common migration anti-pattern:
- **Marking phases "COMPLETE" when infrastructure existed, not when it was used**
- The `capture()` mechanism exists ✓
- Adapter removed; `list_signal_vec` provides VecDiff directly ✓
- The typed CellId exists ✓
- ✅ Actual data paths now run through DD; no bypasses remain.

### Estimated Work Remaining

| Category | Hours | Priority |
|----------|-------|----------|
| O(delta) List Operations | 0 | ✅ DONE |
| Pure IO Layer | 0 | ✅ DONE |
| Bridge Integration | 0 | ✅ DONE |
| Type System Cleanup | 0 | ✅ DONE |
| **Total** | **0** | ✅ Complete |

### Recommended Fix Order

1. **Wire VecDiff adapter to bridge** ✅
2. **Remove `.to_vec()` from list operations** ✅
3. **Move IO business logic to DD** ✅
4. **Type system cleanup** ✅

---

## Implementation Roadmap (2026-01-18)

### A. VecDiff Adapter Wiring (DONE ✅)

**Status:** ✅ COMPLETE

Changes made:
1. Added `MutableVec<Value>` storage in `LIST_SIGNAL_VECS` thread_local
2. Added `list_signal_vec(cell_id)` function returning `SignalVec<Item = Value>`
3. Modified `sync_cell_from_dd()` to update MutableVec via ListDiffs (no list snapshots)
4. Bridge CellRef rendering uses `items_signal_vec(list_signal_vec(cell_id))`

**Update (2026-01-26):** `update_list_signal_vec()` removed; full list snapshots now panic once a list SignalVec exists.

**Pending fix:** ✅ Resolved (bridge uses owned `String` for signals).

### B. Worker `.to_vec()` Removal (17 locations) ✅ COMPLETE

All worker list operations now flow through ListDiffs + ListState; no O(n) clones in hot paths.

#### Category 1: ListAppend Operations (8 locations)

| Line | Context | Status |
|------|---------|--------|
| 2281 | List/append text fallback | ✅ Removed |
| 2479 | List/append elements | ✅ Removed |
| 2487 | List/append data items | ✅ Removed |
| 2506 | List/append text items | ✅ Removed |
| 2671 | List/append elements (dup) | ✅ Removed |
| 2677 | List/append data items (dup) | ✅ Removed |
| 3016 | List/append instantiated | ✅ Removed |
| 3022 | List/append element | ✅ Removed |

**Strategy:** Modify `Operation::ListAppend` output to return only the appended item, not the full collection. The downstream consumer (outputs.rs) already has diff detection.

#### Category 2: ListRemove Operations (6 locations)

| Line | Context | Status |
|------|---------|--------|
| 2714 | Filter elements | ✅ Removed |
| 2789 | RemoveListItem | ✅ Removed |
| 2796 | Remove element | ✅ Removed |
| 2850 | Remove matched element | ✅ Removed |
| 2929 | Toggle remove | ✅ Removed |
| 2979 | Toggle remove element | ✅ Removed |

**Strategy:** Modify `Operation::ListRemove` to track indices. Instead of copying the entire list and removing, pass the removal index to outputs.rs for `VecDiff::RemoveAt`.

#### Category 3: Other Operations (3 locations)

| Line | Context | Status |
|------|---------|--------|
| 2176 | Events clone | ✅ Still necessary |
| 2898 | Field update | ✅ Removed |
| 2935 | Element lookup | ✅ Removed |

### C. IO Layer Business Logic Migration ✅ COMPLETE

The `inputs.rs` file contained 8 business logic violations that should be DD operators (resolved):

| Violation | Status |
|-----------|--------|
| Pattern matching | ✅ Removed |
| Grace period | ✅ Removed |
| Hover dedup | ✅ Removed |
| Event parsing | ✅ Removed |
| Cell lookups | ✅ Removed |
| Text encoding | ✅ Removed |
| Synthetic naming | ✅ Removed |
| Value change | ✅ Removed |

### D. Type System Cleanup ✅ COMPLETE

| Issue | Status |
|-------|--------|
| LinkId as String | ✅ Resolved |
| Magic prefixes | ✅ String-based dispatch removed; prefixes remain only for deterministic display names |
| Hardcoded fields | ✅ Removed |
| TodoMVC handlers | ✅ Removed |

### E. AtomicU64 Counter Removal ✅ COMPLETE

Location: `worker.rs` lines 64-68

```rust
// Current: Side effects inside DD transforms!
static DYNAMIC_CELL_COUNTER: AtomicU32 = AtomicU32::new(1000);
```

**Problem:** ID generation inside DD transform closures breaks referential transparency. ✅ Fixed

---

## Compilation Fixes Needed ✅ COMPLETE

After the VecDiff wiring changes, the following compilation errors need fixing:

### bridge.rs

**Error:** `cell_id` borrowed value does not live long enough (lines 1513-1524)

```rust
// Current (broken):
let cell_id = name.to_string();
Label::new()
    .label_signal(
        cell_signal(&cell_id)  // &cell_id borrowed
            .map(move |value| { ... })  // move closure needs 'static
    )
```

**Fix:** Clone `cell_id` before the closure:

```rust
let cell_id = name.to_string();
let cell_id_for_signal = cell_id.clone();
Label::new()
    .label_signal(
        cell_signal(cell_id_for_signal)
            .map(move |value| { ... })
    )
```

Or use `cell_signal` with owned string instead of borrow.

---

## Comprehensive Audit Findings (2026-01-18)

> Historical record. This section documents violations found by a comprehensive multi-agent audit of the DD engine.
> All items below were fixed before phases were marked truly complete.

### Category 1: Business Logic Functions in Worker Code

These functions encoded domain-specific business logic that should live in DD dataflow (now resolved):

#### 1.1 `is_item_completed_generic()` - CRITICAL

**Location:** `worker.rs` lines 215-249

```rust
fn is_item_completed_generic(item: &Value) -> bool {
    match item {
        Value::Object(obj) => {
            if let Some((_, is_completed)) = find_checkbox_toggle_in_item(obj) {
                return is_completed;
            }
            for (field_name, field_value) in obj.iter() {
                if field_name.contains("edit") {  // ❌ Heuristic string matching!
                    continue;
                }
                if let Value::CellRef(cell_id) = field_value {
                    if let Some(cell_value) = super::super::io::get_cell_value(&cell_id.name()) {  // ❌ IO read!
                        // ...
                    }
                }
            }
            false
        }
        _ => false,
    }
}
```

**Violations:**
- Encodes domain knowledge ("completed" vs "editing" fields)
- Reads from IO layer (`get_cell_value()`) inside DD transform
- String heuristics (`field_name.contains("edit")`) are brittle
- Called from `ListRemoveCompleted` transform (line 2695)

**Pure DD Fix:** Completion status should be explicitly tracked in item data. The Boon code should declare which field represents completion, not have the engine infer it.

#### 1.2 `find_checkbox_toggle_in_item()`

**Location:** `worker.rs` lines 176-210

**Violations:**
- Relies on `CHECKBOX_TOGGLE_HOLDS` registry (thread-local global)
- Reads from IO layer (`get_cell_value()`)
- Implicit field semantics based on registry membership

#### 1.3 `find_boolean_field_in_template()`

**Location:** `interpreter.rs` lines 1581-1614

**Violations:**
- Heuristic-based field inference ("likely the 'completed' field")
- Reads from IO layer
- Used with hardcoded fallback at lines 750 and 1424

#### 1.4 `extract_item_key_for_removal()`

**Location:** `worker.rs` lines 609-637

**Violations:**
- Brittle nested field search for identity
- Duplicate implementation at line 1220
- Fallback to `.to_display_string()` is unreliable

---

### Category 2: Hardcoded Field Name Fallbacks (NO FALLBACKS RULE VIOLATION)

These MUST be converted to explicit failures. Fallbacks silently use wrong field names:

| Location | Fallback | Problem |
|----------|----------|---------|
| `worker.rs:750` | `unwrap_or_else(\|\| "completed".to_string())` | Silent wrong field |
| `interpreter.rs:1424` | `unwrap_or_else(\|\| "completed".to_string())` | Silent wrong field |
| `worker.rs:2354` | `unwrap_or_else(\|\| "title".to_string())` | Silent wrong field |
| `worker.rs:2424` | `identity_path: vec!["remove_button".to_string()]` | Hardcoded path |

**Fix:** Replace with `expect("Bug: missing field metadata for ...")` to fail explicitly.

---

### Category 3: Side Effects During Evaluation

The evaluator should ONLY build `DataflowConfig`. Direct state mutations during evaluation were violations (resolved):

#### 3.1 Direct `init_cell()` Calls in Evaluator

| Location | Context |
|----------|---------|
| `evaluator.rs:784` | Object with list fields |
| `evaluator.rs:1222` | Router/route "current_route" |
| `evaluator.rs:1226` | Router/route fallback "/" |
| `evaluator.rs:1953` | HOLD with link trigger |
| `evaluator.rs:3329` | WHILE with LinkRef |

**Fix:** Move all `init_cell()` to interpreter after evaluation. Evaluator only builds config.

#### 3.2 Direct Registration in IO Module

| Location | Call |
|----------|------|
| `evaluator.rs:1972-1977` | `add_dynamic_link_action(...)` |
| `evaluator.rs:1981` | `self.dataflow_config.set_editing_bindings(...)` |
| `evaluator.rs:1988-1993` | `self.dataflow_config.add_toggle_binding(...)` |

**Fix:** Store in DataflowConfig, let interpreter register with IO after evaluation.

---

### Category 4: Global Atomic Counters (Non-Deterministic IDs)

**Location:** `evaluator.rs` lines 17, 20, 24-25

```rust
static GLOBAL_CELL_COUNTER: AtomicU32 = AtomicU32::new(0);
static GLOBAL_LINK_COUNTER: AtomicU32 = AtomicU32::new(0);
```

**Violations:**
- Non-deterministic across runs (flaky tests)
- Instance counters exist in `BoonDdRuntime` but aren't used
- `reset_cell_counter()` timing issues

**Fix:** Use `BoonDdRuntime` instance counters exclusively. Remove global atomics.

---

### Category 5: O(n) Operations (25+ Locations)

Every `.to_vec()` call copies the entire list, defeating O(delta):

#### List Append Operations (8 locations)

| Line | Context |
|------|---------|
| `worker.rs:2281` | List/append text fallback |
| `worker.rs:2479` | List/append elements |
| `worker.rs:2487` | List/append data items |
| `worker.rs:2506` | List/append text items |
| `worker.rs:2671` | List/append elements (dup) |
| `worker.rs:2677` | List/append data items (dup) |
| `worker.rs:3016` | List/append instantiated |
| `worker.rs:3022` | List/append element |

#### List Remove Operations (6 locations)

| Line | Context |
|------|---------|
| `worker.rs:2714` | Filter elements |
| `worker.rs:2789` | RemoveListItem |
| `worker.rs:2796` | Remove element |
| `worker.rs:2850` | Remove matched element |
| `worker.rs:2929` | Toggle remove |
| `worker.rs:2979` | Toggle remove element |

**Fix:** Return diffs instead of full collections. Use `VecDiff::Push`/`RemoveAt`.

---

### Category 6: DynamicLinkAction Business Logic (inputs.rs) ✅ RESOLVED

**Location:** `inputs.rs` lines 12-36

```rust
pub enum DynamicLinkAction {
    BoolToggle(String),
    SetTrue(String),
    SetFalse(String),
    SetFalseOnKeys { cell_id: String, keys: Vec<String> },
    EditingHandler { editing_cell: String, title_cell: String },  // ❌ TodoMVC-specific!
    HoverState(String),
    RemoveListItem { link_id: String },
    ListToggleAllCompleted {  // ❌ TodoMVC-specific!
        list_cell_id: String,
        completed_field: String,
    },
}
```

**Historical violations:**
- `EditingHandler` encodes todo_mvc editing workflow (double-click → edit, Escape → cancel)
- `ListToggleAllCompleted` encodes todo list "mark all" pattern
- `completed_field` hardcodes the field name concept

**Fix:** ✅ Completed — DD-native link mappings replaced these actions.

---

### Category 7: Thread-Local Registries ✅ RESOLVED

| Registry | Location | Status | Issue |
|----------|----------|--------|-------|
| `CHECKBOX_TOGGLE_HOLDS` | `outputs.rs:621` | ✅ Removed | — |
| `DYNAMIC_LINK_ACTIONS` | `inputs.rs:59` | ✅ Removed | — |
| `ACTIVE_CONFIG` | `worker.rs:85` | ✅ Removed | — |
| `PERSISTENT_WORKER` | `dataflow.rs:608` | ✅ Allowed | DD infrastructure |

**Fix:** ✅ Completed — removed registries; use `DataflowConfig`.

---

### Category 8: String-Based Dispatch ✅ RESOLVED

| Location | Pattern | Issue |
|----------|---------|-------|
| `inputs.rs:220` | `vec!["Escape".to_string()]` | Magic string |
| `inputs.rs:278-279` | `"Enter:text"` format | String protocol |
| `inputs.rs:304` | `format!("hover_{}", link_id)` | Cell naming convention |
| `inputs.rs:348-384` | Key event string parsing | Not type-safe |

**Fix:** ✅ Complete - IO uses typed EventValue/Key enums; no string parsing remains.

---

### Summary: Work Remaining by Category

| Category | Severity | Estimated Hours | Files |
|----------|----------|-----------------|-------|
| Business logic functions | ✅ Resolved | 0 | — |
| Hardcoded fallbacks | ✅ Resolved | 0 | — |
| Evaluator side effects | ✅ Resolved | 0 | — |
| Global counters | ✅ Resolved | 0 | — |
| O(n) operations | ✅ Resolved | 0 | — |
| DynamicLinkAction | ✅ Resolved | 0 | — |
| Thread-local registries | ✅ Resolved | 0 | — |
| String-based dispatch | ✅ Resolved | 0 | — |
| **TOTAL** | | **0** | ✅ Complete |

---

### Recommended Fix Order

1. **Hardcoded fallbacks → explicit failures** ✅
2. **Remove `is_item_completed_generic()`** ✅
3. **Move evaluator side effects** ✅
4. **Replace `.to_vec()` with diffs** ✅
5. **Remove DynamicLinkAction business logic** ✅
6. **Wire VecDiff adapter to bridge** ✅
7. **Remove thread-local registries** ✅
8. **Type system cleanup** ✅
