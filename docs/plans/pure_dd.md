# Pure DD Engine - Architecture and Status

**Status:** 🟡 7/11 example tests passing — 4 failures remain - 2026-02-12

---

## 1. Status Summary

**Core pure-DD architecture is in place.** DD is the single source of truth, list ops are O(delta), IO is a thin router only, and `engine-dd` is the default build feature. The actors engine (`engine-actors`) is legacy-only.

**7 passing:** counter, counter_hold, variables, function, pipes, text_input, hello_world
**4 failing:** shopping_list, todo_mvc, interval, interval_hold

### 1.1 Remaining Failures

#### shopping_list (HIGHEST PRIORITY)

**Symptom:** After typing "Milk" + Enter, items don't get added (shows "0 items").

**Root cause:** Store-level LINKs share a **single LinkRef** for ALL event types (key_down, change, blur). Both `on_change` and `on_key_down` fire on the same link ID (e.g., `store.elements.item_input`). The `EventFilter::Any` on `ListAppendSimpleWithClear` lets everything through.

**What was done:**
- Fixed `ListAppendSimple`/`WithClear` in `dataflow.rs:950-984` to only accept `EventValue::KeyDown { key: Key::Enter, text: Some(t) }` — blocks per-keystroke appends from `on_change`.
- But now Enter doesn't add items either.
- Fixed `ListState` to use auto-generated keys (`__auto:N`) for plain value items (prevents duplicate key panics for non-Object list items).

**What to investigate:**
1. Enable `LOG_DD_DEBUG=true` in `mod.rs:32` and press Enter in the input — check if a `KeyDown { key: Enter, text: Some("Milk") }` event reaches the transform.
2. Check `get_dd_text_input_value()` in `bridge.rs` — does it return the text from the input element?
3. The `fire_global_key_down` call in `bridge.rs:1227` passes `DdKey::Enter` (which is `Key::Enter`) and the input text — verify this actually fires.
4. **Architectural concern:** The Enter-key check in the transform is business logic (Boon-level `WHEN { Enter => ... }`) leaking into the DD engine. The proper fix may be separate link IDs per event type at store level, or letting the Boon-level WHEN be the gatekeeper.

**Source:** `TODO(shopping_list test)` in `dataflow.rs:933`

#### todo_mvc

**Symptom:** "All" filter button has no visible outline CSS at startup. Test expects `outlineWidth="1px"` but gets `"0px"`.

**What to investigate:**
1. The outline depends on the `current_route` cell matching `"/"` — check if the evaluator's WHILE/WHEN on the route produces an outline Object vs NoOutline.
2. Check if the initial route value resolves to `"/"` before the first render.
3. The button rendering is in `bridge.rs:412+` — trace how `outline_resolved` is computed from `WhileConfig`.

**Source:** `TODO(todo_mvc test)` in `bridge.rs:412`

#### interval

**Symptom:** After clear + re-run, counter shows "3" instead of "1". Timer ticks accumulate during the page refresh cycle.

**What to investigate:**
1. Check if `set_timer_handle()` properly cancels the old timer before starting a new one.
2. Check if `shutdown_persistent_worker()` is called before re-initialization on clear+re-run.
3. The timer loop is in `interpreter.rs:729-737` — the `Task::start_droppable` handle must be dropped to cancel the old timer.

**Source:** `TODO(interval + interval_hold tests)` in `interpreter.rs:718`

#### interval_hold

**Symptom:** Shows "7" instead of "1" after 1 second. Timer fires too many rapid ticks during initialization.

**What to investigate:**
1. Same timer lifecycle issue as `interval` above.
2. Additionally, `Timer::sleep(0)` in the worker event loop may process queued timer events before the first real interval fires.
3. Check the HOLD state accumulation — if multiple timer ticks arrive in one batch, the HOLD may increment multiple times.

**Source:** `TODO(interval + interval_hold tests)` in `interpreter.rs:718`

### 1.2 Other Changes in Working Tree

- **PlaceholderField guard** in `worker.rs` (~lines 2634, 2732): Added `contains_placeholder()` check before syncing template cells to IO boundary. Prevents `List/map` template cells from reaching the render layer.
- **ListState auto-keys**: Plain value items (Text, Number, etc.) now get `__auto:N` keys instead of using the value itself as key. Prevents duplicate key panics for lists like `["Milk", "Milk"]`.
- **`LOG_DD_DEBUG` is currently `true`** in `mod.rs:32` — set to `false` before shipping.

---

## 2. Module Structure

```
engine_dd/
├── mod.rs                    # Re-exports public API
├── core/
│   ├── mod.rs
│   ├── collection_ops.rs     # CollectionOp, CollectionOpConfig (~77 lines)
│   ├── dataflow.rs           # DdCellConfig, DdTransform, persistent DD worker (~3947 lines)
│   ├── guards.rs             # DD context validation
│   ├── operators.rs          # hold, hold_with_output operators (~643 lines)
│   ├── types.rs              # CellId, LinkId, Event, LinkAction, LinkCellMapping (~927 lines)
│   ├── value.rs              # Value, CellUpdate, CollectionHandle, TemplateValue (~1016 lines)
│   └── worker.rs             # Worker event loop, pre-instantiation, templates (~3866 lines)
├── eval/
│   ├── mod.rs
│   ├── evaluator.rs          # AST → DataflowConfig builder (~4872 lines)
│   └── interpreter.rs        # Program entry, persistence, worker spawn (~809 lines)
├── render/
│   ├── mod.rs
│   └── bridge.rs             # Value → Zoon elements, list_signal_vec rendering (~1577 lines)
└── io/
    ├── mod.rs
    ├── inputs.rs             # Browser event → DD event injection
    └── outputs.rs            # DD output → CELL_STATES/ListState/render (~1210 lines)
```

---

## 3. Architecture Overview

### 3.1 Configuration Model

The evaluator does NOT build a dataflow graph directly. Instead, it builds a **`DataflowConfig`** - a declarative description of cells, link mappings, collection operations, and templates. The dataflow worker then consumes this config to execute the reactive program.

**Key config types** (in `core/dataflow.rs` and `core/types.rs`):

```rust
// DataflowConfig - the evaluator's output, the worker's input
pub struct DataflowConfig {
    pub cells: Vec<CellConfig>,                              // HOLD cells with transforms
    pub cell_initializations: HashMap<String, CellInitialization>, // Initial values
    pub link_mappings: Vec<LinkCellMapping>,                  // LINK → Cell action bindings
    pub collection_ops: Vec<CollectionOpConfig>,              // List filter/map/count chains
    pub initial_collections: HashMap<CollectionId, Vec<Value>>, // List initial items
    pub collection_sources: HashMap<CollectionId, String>,    // Collection → cell binding
    pub list_item_templates: HashMap<String, ListItemTemplate>, // Template for dynamic items
    pub list_append_bindings: Vec<ListAppendBinding>,         // Event → list append
    pub bulk_remove_bindings: Vec<BulkRemoveBinding>,         // Event → bulk list remove
    pub route_cells: HashSet<String>,                         // Routing cells
    pub remove_event_paths: HashMap<String, Vec<String>>,     // Per-list remove paths
    pub list_element_templates: HashMap<String, TemplateValue>, // Render templates
}
```

The worker converts `DataflowConfig` into `DdCellConfig` entries for the internal DD dataflow:

```rust
pub struct DdCellConfig {
    pub id: CellId,
    pub initial: Value,
    pub triggers: Vec<LinkId>,
    pub transform: DdTransform,
    pub filter: EventFilter,
}

pub enum DdTransform {
    Increment, Toggle, SetValue(Value), Identity,
    ListAppendPrepared,
    ListAppendPreparedWithClear { clear_link_id: String },
    ListRemove,
    WithLinkMappings { base: Box<DdTransform>, mappings: Vec<LinkCellMapping>, ... },
}
```

### 3.2 Event Flow

```
┌─────────────────────────────────────────────────────────────────┐
│  Browser event (click, keydown, input, hover, timer)            │
└──────────────────────────┬──────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────────┐
│  IO Layer (inputs.rs) - thin, no business logic                 │
│  • Convert DOM event → Event { Link { id, value } }            │
│  • event_input.send(event)                                      │
└──────────────────────────┬──────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────────┐
│  Worker Event Loop (worker.rs)                                  │
│  1. yield_to_browser().await                                    │
│  2. collect_pending_events() from mpsc channel                  │
│  3. maybe_pre_instantiate() - side effects OUTSIDE DD           │
│     (generate IDs, prepare template items)                      │
│  4. execute_directly() - process via DD dataflow                │
│     • Apply DdTransform per cell                                │
│     • Apply LinkCellMappings                                    │
│     • Run collection ops (filter/map/count/...)                 │
│     • Emit CellUpdate outputs via capture()                     │
│  5. sync outputs to IO boundary                                 │
└──────────────────────────┬──────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────────┐
│  IO Layer (outputs.rs) - render cache, not source of truth      │
│  • CellUpdate::SetValue → CELL_STATES (Mutable<HashMap>)       │
│  • CellUpdate::ListPush/Remove → LIST_STATES (ListState)       │
│  • LIST_SIGNAL_VECS drives incremental VecDiff rendering        │
│  • Persistence writes to localStorage                           │
└──────────────────────────┬──────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────────┐
│  Render (bridge.rs)                                             │
│  • cell_signal(cell_id) - per-cell granular signals             │
│  • list_signal_vec(cell_id) - incremental list rendering        │
│  • Static element tree with _signal property bindings           │
└─────────────────────────────────────────────────────────────────┘
```

### 3.3 State Model

**Worker** is the source of truth. It owns cell states and list states internally during DD processing. After each event batch, it emits `CellUpdate` commands to the IO boundary.

**IO Layer** maintains render caches:
- `CELL_STATES: Mutable<HashMap<String, Value>>` - scalar cell values
- `LIST_STATES: Mutex<HashMap<String, ListState>>` - authoritative list state with key index
- `LIST_SIGNAL_VECS: Mutex<HashMap<String, MutableVec<Value>>>` - drives VecDiff rendering
- `CURRENT_ROUTE: Mutable<String>` - browser route

**ListState** tracks items with O(1) key-based access:
```rust
pub struct ListState {
    items: Vec<Value>,
    index_by_key: HashMap<Arc<str>, usize>,
}
```

### 3.4 Key Types

**Values** (`core/value.rs`):
```rust
pub enum Value {
    Unit, Bool(bool), Number(OrderedFloat<f64>), Text(Arc<str>),
    Object(Arc<BTreeMap<Arc<str>, Value>>),
    List(CollectionHandle),           // ID-only, no stored items
    Tagged { tag: Arc<str>, fields: Arc<BTreeMap<Arc<str>, Value>> },
    CellRef(CellId),                  // References HOLD state
    LinkRef(LinkId),                  // References event source
    TimerRef { id: Arc<str>, interval_ms: u64 },
    Placeholder,                      // Template substitution marker
    PlaceholderField(Arc<Vec<Arc<str>>>),
    PlaceholderWhile(Arc<PlaceholderWhileConfig>),
    WhileConfig(Arc<WhileConfig>),
    Flushed(Box<Value>),              // Error propagation (FLUSH)
}
```

**State updates** (`core/value.rs`):
```rust
pub enum CellUpdate {
    SetValue { cell_id, value },
    ListPush { cell_id, item },
    ListInsertAt { cell_id, index, item },
    ListRemoveAt { cell_id, index },
    ListRemoveByKey { cell_id, key },
    ListRemoveBatch { cell_id, keys },
    ListClear { cell_id },
    ListItemUpdate { cell_id, key, field_path, new_value },
    Multi(Vec<CellUpdate>),
    NoOp,
}
```

**Collection handle** (`core/value.rs`):
```rust
pub struct CollectionHandle {
    pub id: CollectionId,              // Unique collection identifier
    pub cell_id: Option<Arc<str>>,     // Where list state lives (None = unbound)
}
```

**Identity types** (`core/types.rs`):
```rust
pub enum CellId { Static(Arc<str>), Dynamic(u32) }
pub enum LinkId { Static(Arc<str>), Dynamic { counter: u32, name: Arc<str> } }
pub struct TimerId(pub String);
pub struct CollectionId(u64);
```

**Events** (`core/types.rs`):
```rust
pub enum Event {
    Link { id: LinkId, value: EventValue },
    Timer { id: TimerId, tick: u64 },
    External { name: String, value: EventValue },
}

pub enum EventValue {
    Unit, Text(String), KeyDown { key: Key, text: Option<String> },
    Bool(bool), Number(OrderedFloat<f64>),
    PreparedItem { item, initializations, source_text, source_key },
}

pub enum Key { Enter, Escape, Other(Arc<str>) }
pub enum EventFilter { Any, TextEquals(String), KeyEquals(Key) }
pub enum BoolTag { True, False }
```

**Link actions** (`core/types.rs`):
```rust
pub enum LinkAction {
    BoolToggle, SetTrue, SetFalse, AddValue(Value),
    HoverState, RemoveListItem { list_cell_id: CellId },
    SetText, SetValue(Value),
}

pub struct LinkCellMapping {
    pub link_id: LinkId,
    pub cell_id: CellId,
    pub action: LinkAction,
    pub key_filter: Option<Vec<Key>>,
}
```

**Collection operations** (`core/collection_ops.rs`):
```rust
pub enum CollectionOp {
    Filter { field_filter: Option<(Arc<str>, Value)>, predicate_template: Option<TemplateValue> },
    Map { element_template: TemplateValue },
    Count,
    CountWhere { filter_field: Arc<str>, filter_value: Value },
    IsEmpty,
    Concat { other_source: CollectionId },
    Subtract { right_source: CollectionId },
    GreaterThanZero,
    Equal { right_source: CollectionId },
}

pub struct CollectionOpConfig {
    pub output_id: CollectionId,
    pub source_id: CollectionId,
    pub op: CollectionOp,
}
```

### 3.5 Boon Construct → DD Mapping

| Boon Construct | DD Representation | Key Type |
|----------------|-------------------|----------|
| `HOLD state { body }` | `CellConfig` + `DdTransform` | `DdCellConfig` |
| `LATEST { ... }` | `CellConfig` + `LinkCellMapping` (scalar) or `CollectionOp::Concat` (collections) | `LinkCellMapping` / `CollectionOpConfig` |
| `WHEN { pattern => body }` | Pre-evaluated `WhileConfig` with arms | `WhileConfig` |
| `WHILE { pattern => body }` | Reactive `WhileConfig` or `PlaceholderWhile` in templates | `WhileConfig` |
| `THEN { body }` | `LinkCellMapping` with `SetValue` action | `LinkCellMapping` |
| `LINK` | `LinkId` + `LinkCellMapping` entries | `LinkId`, `LinkCellMapping` |
| `[a, b, c]` (list literal) | `CollectionHandle` + items in `initial_collections` | `CollectionHandle` |
| `List/filter(pred)` | `CollectionOp::Filter` | `CollectionOpConfig` |
| `List/map(f)` | `CollectionOp::Map` | `CollectionOpConfig` |
| `List/count()` | `CollectionOp::Count` → scalar `CellRef` | `CollectionOpConfig` |
| `List/append(item)` | `ListAppendBinding` + `EventValue::PreparedItem` | `ListAppendBinding` |
| `List/remove(pred)` | `BulkRemoveBinding` or `LinkAction::RemoveListItem` | `BulkRemoveBinding` |
| `FLUSH` | `Value::Flushed(Box<Value>)` propagation | `Value::Flushed` |

### 3.6 Anti-Cheat I/O Design

```rust
pub struct Output<T> { receiver: mpsc::UnboundedReceiver<T> }
// Can ONLY observe via async .stream() - NO .get()

pub struct Input<T> { sender: mpsc::UnboundedSender<T> }
// Can ONLY inject via .send() - NO state access
```

DD outputs are collected via `capture()` (pure message passing) instead of `inspect()` (side-effecting callbacks). No `Mutex` in the DD dataflow.

---

## 4. Improvements Achieved

67 improvements beyond the original plan, implemented across Phases 1-12:

1. **Fail-fast list diffs**: list diffs now panic on missing MutableVec / missing keys / wrong cell IDs (no silent ignores).
2. **Per-list identity**: removal paths are per list (`remove_event_paths`) instead of global heuristics.
3. **Bulk remove is explicit**: bulk list remove uses explicit predicates (no inferred "completed" logic).
4. **Link mappings are DD-native**: IO registries removed; mapping lives in DataflowConfig and DD joins.
5. **Incremental list diffs wired**: list diffs are applied incrementally to collection state (no full list clones on push/remove).
6. **DD collection ops in hot path**: filter/map/count/is_empty/concat/subtract/equal now run inside DD for both batch + persistent workers (no worker recompute).
7. **DD emits initial collection outputs**: startup no longer calls `compute_collection_outputs`; init events seed outputs in dataflow.
8. **No snapshot updates for lists**: snapshots after init panic; initial snapshots are converted into ListDiffs.
9. **List transforms removed**: `DdTransform::ListCount/ListFilter/ListMap` deleted; only collection ops allowed.
10. _(skipped in original numbering)_
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

---

## 5. Remaining Work

### 5.1 Fix 4 Failing Examples (see §1.1)
Fix shopping_list, todo_mvc, interval, interval_hold. Each has a `TODO(...)` comment in the source with debug steps.

### 5.2 Testing
- **Run tests:** `boon-tools exec test-examples` (or use Boon Browser MCP: `boon_select_example` + `boon_run` + `boon_preview`)
- **Expected files:** `playground/frontend/src/examples/*.expected` define pass/fail criteria
- All 11 registered examples must pass without workarounds

### 5.3 Post-Fix Cleanup
- Set `LOG_DD_DEBUG = false` in `mod.rs:32`
- Remove `TODO(...)` comments from the 3 source files once fixed
- Optional: run the comprehensive cleanup plan in `~/.claude/plans/twinkly-snacking-cerf.md`

---

## 6. Phase History

All 12 phases are COMPLETE. Each is summarized below with the key architectural decision.

### Phase 1: Module Restructure
Renamed files from `dd_*` prefix to module-organized structure (`dd_value.rs` → `core/value.rs`, `dd_runtime.rs` → `core/operators.rs`, `dd_evaluator.rs` → `eval/evaluator.rs`, `dd_bridge.rs` → `render/bridge.rs`, `dd_interpreter.rs` → `eval/interpreter.rs`). Created `core/`, `eval/`, `render/`, `io/` subdirectories.

### Phase 2: Hold → Cell Rename
Renamed `HoldId` → `CellId`, `HoldRef` → `CellRef`, `HoldConfig` → `CellConfig`, `HOLD_STATES` → `CELL_STATES`, and all related variables/functions. 474 occurrences across 11 files.

### Phase 3: Type-Safe IDs and Tags
Replaced string-based matching with typed enums: `CellId` (Static/Dynamic), `LinkId` (Static/Dynamic), `BoolTag` (True/False), `Key` (Enter/Escape/Other), `EventFilter` (Any/TextEquals/KeyEquals), `EventValue` (Unit/Text/KeyDown/Bool/Number/PreparedItem). `TimerId` remained a String newtype. Eliminated `starts_with(DYNAMIC_HOLD_PREFIX)` patterns and `tag.as_ref() == "True"` string checks.

### Phase 4: DD Infrastructure
Wired DD collection operators into the main dataflow. Collection operations (`Filter`, `Map`, `Count`, `CountWhere`, `IsEmpty`, `Concat`, `Subtract`, `GreaterThanZero`, `Equal`) defined in `collection_ops.rs` as `CollectionOp` + `CollectionOpConfig`. Operations run inside DD for both batch and persistent workers. Startup seeds outputs via init events (removed `compute_collection_outputs`).

### Phase 5: DD-First Architecture
Made the persistent DD worker the main event loop. The evaluator builds `DataflowConfig` (declarative), the worker converts it to `DdCellConfig` entries and runs the reactive dataflow. Events flow through `Input<Event>` channels; outputs collected via `capture()` (pure message passing, no `inspect()` side effects). `CELL_STATES` became a render cache, not source of truth.

### Phase 6: Single State Authority
Eliminated dual code paths for initialization and runtime updates. The interpreter only builds `DataflowConfig` (no `init_cell()` calls). The DD worker loads persisted values on startup and is the sole writer to cell state. Removed `DocumentUpdate` channel and output listener indirection.

**Key insight**: "Why have both initialization AND reactive updates when we can have ONLY reactive updates?"

### Phase 7: O(delta) List Operations
Moved all side effects OUTSIDE DD via pre-instantiation. Template items are prepared with fresh IDs in `maybe_pre_instantiate()` before injection. Inside DD, `ListAppendPrepared` is a pure append. `EventValue::PreparedItem` carries pre-instantiated data. True O(delta) complexity for list operations.

### Phase 8: Pure LINK Handling
Migrated all business logic from IO layer to DD via `LinkAction` + `LinkCellMapping`. Actions include `BoolToggle`, `SetTrue`, `SetFalse`, `HoverState`, `RemoveListItem`, `SetText`, `SetValue`, `AddValue`. Removed `DYNAMIC_LINK_ACTIONS`, `CHECKBOX_TOGGLE_HOLDS`, `TEXT_CLEAR_CELLS`, `*_EVENT_BINDINGS` registries. IO layer became thin routing only.

### Phase 9: True Incremental Lists
Made `CollectionHandle` ID-only (removed `trace` and snapshot storage). `ListState` with key index became the authoritative list state. All list operations work through `CellUpdate` diffs. Removed `.to_vec()` from hot paths. Nested collection identity and persistence supported.

### Phase 10: Eliminate inspect() Side Effects
Replaced `inspect()` + `Arc<Mutex<Vec<DdOutput>>>` pattern with `capture()` (timely's built-in pure message passing). No Mutex in the DD dataflow. Outputs extracted after dataflow step completes. `CELL_STATES` still exists as render cache but is only written by the IO output observer, not by DD.

### Phase 11: Make DD Engine Generic
Removed example-specific hacks: `ROUTER_MAPPINGS` (Phase 11a), `cell_states_signal()` broadcast anti-pattern (Phase 11b), legacy thread_local registries for event bindings (Phase 11c), `TEXT_CLEAR_CELLS` (Phase 11d). Replaced coarse `cell_states_signal()` with fine-grained `cell_signal(cell_id)` per-cell signals.

**Key insight**: The blur bug was caused by `cell_states_signal()` broadcasting ALL cell changes, triggering spurious list re-renders. Fine-grained signals fixed it.

### Phase 12: Incremental Rendering
List rendering driven by `list_signal_vec` producing `VecDiff` directly from `ListState` diffs. The initial `list_adapter.rs` module (DD-to-VecDiff stream adapter) was created then removed - replaced by direct `MutableVec` manipulation in the IO layer. Element properties bound via `_signal` methods for O(1) updates without element recreation.

**Two complementary approaches**: Granular signals for scalar property updates (O(1)), VecDiff for list structure changes (O(delta)).

---

## 7. Testing Infrastructure

- **`boon-tools exec smoke-examples`**: Discovers examples from `playground/frontend/src/main.rs` via `make_example_data!` macro regex, then for each example: clear state → select → run → verify preview text.
- **`boon-tools exec test-examples`**: Runs curated test suites with `.expected` files and action sequences.
- **Visual regression**: `reference_metadata.json` captures element-level layout snapshots. `boon-tools pixel-diff` provides grid analysis, semantic diff detection, and zoom inspection.

---

## 8. Future Work (Speculative)

> **Note**: The following are research directions, not planned work. Code examples are aspirational.

### Memory Optimizations (GRADES-NDA Research)

From GRADES-NDA 2024 research paper:
- **Read-Friendly Indices**: BTreeMap by key instead of append-only log (19x for UI workloads)
- **Fast Empty Difference Check**: Skip processing when input unchanged since last tick
- **Bounded Diff History**: VecDeque with max_diffs limit (1.7x memory reduction)

### E-Graph Optimization (EGRAPHS 2023 Research)

Automatic semi-naive evaluation discovery using `egg` crate. Rewrite rules:
```
persist(delta(x)) ≡ x
delta(persist(x)) ≡ x
hold(init, body) ≡ latest(prev(hold(init, body)), delta(body))
```

### WebWorker Parallelism (Deferred)

Multi-threaded DD execution using SharedArrayBuffer. Deferred because single-threaded DD is sufficient for current use cases. WebWorker adds complexity (SharedArrayBuffer security restrictions, worker coordination) that should wait for proven performance bottlenecks.

---

## 9. IO Thread-Local Inventory

Remaining thread_locals (infrastructure only, no business logic):

**`inputs.rs`:**
- `GLOBAL_DISPATCHER` - Event channel (browser → DD)
- `TASK_HANDLE` - Async task management
- `OUTPUT_LISTENER_HANDLE` - DD output subscription
- `TIMER_HANDLE` - Interval timer management

**`outputs.rs`:**
- `CELL_STATES` - Scalar cell render cache (`Mutable<HashMap<String, Value>>`)
- `LIST_SIGNAL_VECS` - Per-list MutableVec for VecDiff rendering
- `LIST_STATES` - Authoritative list state with key index
- `CURRENT_ROUTE` - Browser route state
