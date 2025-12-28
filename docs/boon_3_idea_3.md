# Boon v3 — "Instance VM" Architecture (Idea 3)

This document specifies a **simple, deterministic runtime** for Boon that keeps **existing Boon syntax unchanged** while removing the complexity of mutable reactive graphs, dynamic wiring, and actor lifetimes. It is written to help another agent **understand and implement** the architecture.

The core move: **evaluate Boon as an incremental interpreter over a stable-ID IR**, with all runtime state stored in **cells keyed by (ScopeId, ExprId)**. There is **no runtime graph cloning**. Dynamic instances are just different scopes.

---

## 0. Executive Summary

**Replace** the current reactive graph + cloning approach with:

1. **Stable IDs** for every expression (`ExprId`) and callsite (`SourceId`)
2. **Scopes** as the sole instantiation mechanism (`ScopeId`)
3. **Cells** for stateful constructs (`HOLD`, `LINK`, `LIST`) keyed by `(ScopeId, ExprId)`
4. A **tick-based evaluator** that (a) ingests external inputs, (b) evaluates the program in deterministic order, (c) commits state updates and effects

This removes "missing wire" classes of bugs for dynamic lists because **new items do not subscribe to anything**. They are evaluated every tick and read shared inputs/cells.

---

## 1. Goals & Non-Goals

### Goals
- **Keep Boon syntax unchanged** (`HOLD`, `LINK`, `LIST`, `LATEST`, `THEN`, `WHEN`, `WHILE`)
- **Determinism**: same inputs => same outputs, same state
- **Simplicity**: no runtime graph cloning, no subscription routing tables, no actor lifetimes
- **Debuggability**: answer "why did X change?"
- **Correct dynamic lists** without extra wiring steps

### Non-Goals (initial cut)
- Aggressive micro-optimizations
- Distributed/cluster runtime
- Full static scheduling or incremental dependency pruning (can be added later)

---

## 2. Compiler Outputs (Required)

The compiler must lower Boon source to a stable IR with the following properties.

### 2.1 Stable IDs
- **ExprId**: unique, deterministic id for every expression node.
- **SourceId**: stable id for callsites / syntactic origins (used for element identity and list allocation sites).

Stability rule: small edits should not reshuffle ids for unrelated code. Use structural hashing with a deterministic fallback if needed.

### 2.2 Lowered IR
The runtime should receive:
- A tree (or DAG) of IR nodes with **ExprId** for each node
- Source spans for error reporting and diagnostics
- Metadata for constructs that allocate scopes or items (function calls, list alloc sites, UI element creation)

### 2.3 Scope construction metadata
The compiler must indicate where scopes are created, including:
- Function calls (explicit or implicit context via PASS/PASSED)
- List item instantiation
- (Optional) WHEN/WHILE arm scopes if needed for per-arm state isolation

---

## 3. Core Runtime Concepts

### 3.1 Tick & Ordering
- **TickId**: monotonic integer incremented each external scheduling step (frame/microtask)
- **TickSeq**: `(tick, seq)` used to order events and updates within a tick

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TickSeq {
    pub tick: u64,
    pub seq: u32,
}
```

### 3.2 Scopes & Slot Keys
- **ScopeId**: identifies a runtime instantiation of code
- **SlotKey** = `(ScopeId, ExprId)`; universal address for:
  - cached computed values
  - state cells (HOLD/LINK/LIST)
  - diagnostics

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotKey {
    pub scope: ScopeId,
    pub expr: ExprId,
}
```

### 3.3 Values
Include a representation for **Skip** (no value emitted), e.g.

```rust
enum Value { /* ... */, Skip }
```

Rules:
- Field access on `Skip` yields `Skip`
- `THEN`, `WHEN`, `LATEST` treat `Skip` as "no event"

### 3.4 Cells
State is stored only in cells keyed by SlotKey:
- `HoldCell<T>` for HOLD
- `LinkCell<T>` for LINK
- `ListCell<T>` for LIST

Cells are the only mutable runtime state besides the UI store.

### 3.5 Cache (Optional but recommended)
Per SlotKey cache entry:

```rust
pub struct CacheEntry {
    pub value: Value,
    pub computed_at: TickId,
    pub last_changed: TickSeq,
    pub deps: smallvec::SmallVec<[SlotKey; 8]>,
}
```

Rules:
- If `computed_at == current_tick`, return cached
- Else recompute and update `last_changed` if value differs

This enables `LATEST` as "pick input with max last_changed" and supports diagnostics.

---

## 4. Evaluation Model

### 4.1 Deterministic Order
- Evaluate top-level definitions in source order
- Within each expression, evaluate children in a deterministic AST order
- Commit updates in deterministic order (see HOLD semantics)

### 4.2 Evaluator signature

```rust
fn eval(expr: ExprId, scope: ScopeId, ctx: &mut EvalCtx) -> (Value, TickSeq)
```

Where `TickSeq` represents the **last_changed** of the returned value.

### 4.3 Cache Integration (Pseudo)

```rust
fn eval_slot(key: SlotKey, ctx: &mut EvalCtx) -> (Value, TickSeq) {
    if let Some(cache) = ctx.cache.get(&key) {
        if cache.computed_at == ctx.current_tick {
            return (cache.value.clone(), cache.last_changed);
        }
    }

    let (value, last_changed, deps) = eval_uncached(key, ctx);

    ctx.cache.insert(key, CacheEntry {
        value: value.clone(),
        computed_at: ctx.current_tick,
        last_changed,
        deps,
    });

    (value, last_changed)
}
```

---

## 5. Semantics of Key Constructs

### 5.1 SKIP
- `SKIP` means "no value emitted" (no event / no update)
- `THEN`, `WHEN` return `SKIP` if input is `SKIP`
- `LATEST` ignores SKIP inputs unless all are SKIP
- Field access on SKIP yields SKIP

### 5.2 LATEST
- Evaluate all inputs to `(value, last_changed)`
- Choose the value with max `last_changed`
- Ties resolved by input index (stable)
- If all SKIP -> SKIP

### 5.3 THEN
`x |> THEN { body }`:
- If `x` is SKIP => SKIP
- Else evaluate body, return body result

### 5.4 WHEN
- If input SKIP => SKIP
- Else pick first matching pattern arm, evaluate and return it

### 5.5 WHILE
- Continuous selection based on current value
- Evaluate input (behavior-like), pick matching arm each tick

### 5.6 HOLD (state)
- A HOLD owns a `HoldCell` keyed by SlotKey
- The HOLD expression returns the cell's current state
- The body is evaluated every tick, producing zero or more updates

**Commit semantics** (deterministic):
- Collect all updates in a tick
- Apply in order of `(TickSeq, ScopeId, ExprId)`
- If multiple updates target the same cell in a tick, apply sequentially

MVP rule (acceptable):
- Evaluate store roots first and apply HOLD updates immediately so UI sees new state in the same tick

### 5.7 LINK (late binding)
- `LINK` literal creates a LinkCell with state `Unbound`
- `x |> LINK { target }` binds the target cell to `x`, and returns `x`
- Reading an unbound LINK returns **SKIP** (recommended for compatibility)

### 5.8 LIST (stable keys + scopes)
A list consists of stable ordered keys and allocation-site state.

#### Item keys
- Keys allocated per list allocation site (SourceId)
- `AllocSite { site: SourceId, next: ItemKey }`

#### Scopes
For item key `k`:

```
item_scope = ScopeId::child(parent_scope, list_alloc_site, k)
```

Everything inside the item (including HOLD/LINK) is keyed by `(item_scope, expr_id)`.

#### List/append
- If input is SKIP: no-op
- Else allocate new ItemKey, evaluate item expr in `item_scope`, append

#### List/map
- Does not allocate new keys
- For each key `k`, evaluate `expr` in `mapped_scope = ScopeId::child(parent, map_callsite, k)`

#### List/retain
- Evaluate predicate per item and keep keys that pass

#### List/remove
- Evaluate removal event per item
- Remove marked keys after evaluation in stable order

---

## 6. UI Integration

### 6.1 ElementId
Element identity is derived from:
- `SourceId` of the element construction callsite
- `ScopeId` of the current evaluation
- (Optional) ordinal if multiple elements from same callsite in same scope

```rust
pub struct ElementId(u128); // hash(source_id, scope_id, ordinal)
```

### 6.2 UiStore
Renderer-owned state, not part of user graph:
- hover/focus
- text input state
- last event pulses

Event ports:

```rust
pub enum EventType { Click, Press, KeyDown, Change, Blur, DoubleClick }

pub struct EventPortId {
    pub element: ElementId,
    pub ty: EventType,
}

pub struct EventPortState {
    pub last_pulse: Option<(TickSeq, Value)>,
}
```

Reading an event port returns:
- The pulse payload if it occurred this tick and not yet consumed
- SKIP otherwise

### 6.3 VDOM
Evaluation returns a VDOM tree:

```rust
pub enum VNode { Element(VElement), Text(String), None }

pub struct VElement {
    pub id: ElementId,
    pub tag: Tag,
    pub attrs: Vec<Attr>,
    pub style: StyleObject,
    pub children: Vec<VNode>,
    pub event_bindings: Vec<EventType>,
}
```

Renderer diffs by ElementId (keyed diff).

---

## 7. Tick Loop (Runtime)

Pseudo:

```rust
loop {
    let tick = next_tick();
    ingest_external_inputs(tick); // events -> UiStore with TickSeq

    // deterministic evaluation
    eval_top_level_definitions(tick);

    // commit updates / effects in deterministic order
    commit_hold_updates(tick);
    commit_link_bindings(tick);
    commit_list_mutations(tick);
    run_effects(tick);

    render_vdom();
}
```

Order matters for determinism. Keep it explicit and documented.

---

## 8. Diagnostics (First-Class)

For every SlotKey, store:
- `last_changed: TickSeq`
- `deps: Vec<SlotKey>` (last evaluation dependencies)
- `changed_because: Trigger` (optional)

```rust
pub enum Trigger {
    DomEvent { port: EventPortId, seq: TickSeq },
    HoldUpdate { cell: SlotKey, seq: TickSeq },
    LinkBind { cell: SlotKey, seq: TickSeq },
    ListMutation { cell: SlotKey, seq: TickSeq },
}
```

This enables "why did X change?" queries.

---

## 9. Persistence & Snapshot (Optional Early)

Because all mutable state is in cells + UiStore, snapshot is straightforward:
- Serialize all cells
- Serialize list allocation site counters
- Serialize UiStore event state if needed

On restore:
- Set `last_execution_tick` to restored tick to prevent re-running effects

---

## 10. Implementation Plan (Suggested)

1. **Compiler: IR with ExprId + SourceId**
2. **Runtime: basic evaluator** (no cache, recompute every tick)
3. **Cells for HOLD/LINK/LIST**
4. **List scopes + stable item keys**
5. **LATEST via last_changed** (add cache)
6. **UiStore + ElementId + VDOM diff**
7. **Diagnostics** (deps + last_changed)

---

## 11. Testing Checklist

- Determinism: same inputs => same outputs and cell states
- Dynamic list items receive global events (toggle-all test)
- LINK unbound -> SKIP; bound -> event flow works
- HOLD sequential updates within a tick
- LATEST picks most recent input deterministically

---

## 12. Invariants (Do Not Break)

- **No runtime graph cloning**
- **All state is in cells keyed by (ScopeId, ExprId)**
- **Evaluation order is deterministic and documented**
- **Unbound LINK reads as SKIP**
- **List item identity is stable across ticks**

---

## 13. Appendix: Minimal Data Structures

```rust
struct Runtime {
    tick: TickId,
    cache: HashMap<SlotKey, CacheEntry>,
    holds: HashMap<SlotKey, HoldCell>,
    links: HashMap<SlotKey, LinkCell>,
    lists: HashMap<SlotKey, ListCell>,
    ui: UiStore,
}

struct HoldCell { value: Value }
struct LinkCell { bound: Option<ValueRef> }
struct ListCell {
    keys: Vec<ItemKey>,
    alloc: AllocSite,
}

struct AllocSite { site: SourceId, next: ItemKey }
```

---

This architecture is intentionally simple: **deterministic evaluation, stable IDs, and cell-based state**. It is designed to preserve Boon syntax while removing runtime wiring complexity.
