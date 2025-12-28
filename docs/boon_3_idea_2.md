# Boon v3 — “Instance VM” Architecture (Idea 2)

This document is an implementer-oriented spec for a **Boon v3** runtime that is:

- **Simple** (no graph cloning, no dynamic routing tables, no async tasks)
- **Deterministic** (same inputs → same outputs)
- **Type-safe** (compiler knows shapes; runtime is not “best effort”)
- **Debbuggable** (explain “why did this change?” without spelunking subscriptions)
- **Compatible with existing `.bn` UI code** (e.g. `playground/frontend/src/examples/todo_mvc/todo_mvc.bn`) **without changing the code**

The core move: **stop compiling to a mutable reactive graph**, and instead run Boon as an **incremental interpreter** over a **stable-ID IR**, with all runtime state stored in **cells keyed by (ExprId, ScopeId)**.

This makes “dynamic list item doesn’t subscribe to X” bugs impossible by design, because **new instances do not need to subscribe**: they are evaluated every tick in a deterministic order and read from shared inputs/cells.

---

## 0. Executive Summary

### What changes vs v2

v2 tries to be a classic reactive engine: nodes, routing, dirty propagation, plus **runtime cloning of subgraphs** for list items and template isolation. That combination is where a lot of complexity and “missing wire” bugs come from.

v3 replaces it with:

1. **Stable IDs** for every expression / callsite (`ExprId`, `SourceId`)
2. **Scopes** as the only instantiation mechanism (`ScopeId`)
3. **Cells** for stateful things (`HOLD`, `LIST`, `LINK`) keyed by `(ScopeId, ExprId)`
4. A **tick-based evaluator** that (a) ingests external inputs, (b) evaluates the program in deterministic order, (c) commits state changes/effects.

### What stays the same

- The Boon language surface syntax (including `HOLD`, `LINK`, `LIST`, `LATEST`, `THEN`, `WHEN`, `WHILE`)
- The “no reactive wrapper types” philosophy (values are just `T`, reactivity is runtime)
- Ability to build UI via `Element/*` stdlib and `Document/new(...)`

---

## 1. Design Goals & Non-Goals

### Goals

1. **Make existing UI examples work without code changes**
   - Especially cyclic patterns like:
     - `store.elements.toggle_all_checkbox: LINK`
     - UI binds element into the link: `toggle_all_checkbox() |> LINK { store.elements.toggle_all_checkbox }`
     - store reads events from it: `store.elements.toggle_all_checkbox.event.click`
2. **Determinism**
   - Stable evaluation order
   - Stable identity of list items and UI elements
3. **Debuggability**
   - “Why did value X change?” and “What triggered this?” must be answerable from runtime state.
4. **Type-safety**
   - Field access should be mostly static; dynamic fallback should be explicit and still safe.
5. **Simplicity**
   - No runtime graph cloning.
   - No subscription/routing table correctness hazards.
   - No `Arc` / async task lifecycle issues.

### Non-goals (for the first v3 cut)

- Aggressive micro-optimizations (we optimize after correctness + observability)
- Full cross-platform backends (browser first; CLI runner can come next)
- Advanced static scheduling (the simplest correct tick loop first)

---

## 2. Core Concepts & IDs

### 2.1 `SourceId` (stable callsite identity)

`SourceId` identifies a syntactic origin that should remain stable under small edits (ideally based on a structural hash + a fallback parse order).

Used for:
- Stable UI element identity
- Stable list allocation sites
- Debug spans (“this value comes from file:line”)

### 2.2 `ExprId` (stable expression identity)

`ExprId` is a unique ID assigned to every expression node in the lowered IR.

Properties:
- Unique within a compiled module/program
- Deterministic assignment (preorder numbering, or stable hashing + collision resolution)

### 2.3 `ScopeId` (runtime instantiation identity)

`ScopeId` identifies **where** an expression is evaluated.

Scope is created by:
- Function calls (including implicit context via PASS/PASSED)
- List item instances
- (Optionally) WHILE arms, WHEN arms, etc.

The critical rule:

> Any stateful construct (`HOLD`, `LINK`, `LIST`) stores its state in a cell keyed by **(ScopeId, ExprId)**.

This is what eliminates “cloned instance got wired wrong”: there is nothing to clone. Instantiation is just a new ScopeId.

### 2.4 `SlotKey` = `(ScopeId, ExprId)`

This is the universal address for:
- Cached computed values
- Cells (stateful runtime storage)
- Debugging records (last change, dependencies, triggers)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotKey {
    pub scope: ScopeId,
    pub expr: ExprId,
}
```

### 2.5 `ItemKey` (stable list item identity)

Lists store items by stable key:

- The key must persist across ticks (and ideally across reload via snapshot).
- Keys are allocated from a per-allocation-site counter:
  - allocation site = `SourceId` of the `List/append` callsite (or list literal site)

```rust
pub type ItemKey = u64;

pub struct AllocSite {
    pub site: SourceId,
    pub next: ItemKey,
}
```

---

## 3. Runtime Model (Cells + Incremental Evaluation)

### 3.1 The big picture

Boon v3 runs like an incremental “re-render”:

1. Collect external inputs (DOM events, timers, router changes)
2. Evaluate the program in deterministic order using cached values + cells
3. Commit state updates and effects
4. Render VDOM → DOM diff

This is similar in spirit to React/Elm:
- The whole “program” is conceptually re-evaluated,
- but with persistent state in cells,
- and with caching to avoid recomputing everything.

### 3.2 Stateful constructs are **cells**

Cells are the only mutable runtime state:

- `HOLD` → `HoldCell<T>`
- `LINK` → `LinkCell<T>`
- `LIST` / Bus → `ListCell<T>`
- UI state (hovered/text/focus) → `UiStateCell` keyed by ElementId (renderer-owned)
- Event pulses → `EventPortCell` keyed by EventPortId (renderer-owned)

All user-visible state lives in cells. Everything else is recomputed (or cached) deterministically.

### 3.3 Value caching (optional but recommended)

For simplicity, the first implementation can evaluate “from scratch” each tick and still be correct.

For performance and for `LATEST`/“most recent” semantics, add a cache per SlotKey:

```rust
pub struct CacheEntry {
    pub value: Value,
    pub computed_at: TickId,
    pub last_changed: TickSeq,
    pub deps: smallvec::SmallVec<[SlotKey; 8]>,
}
```

Recompute rules:
- If `computed_at == current_tick`, return cached.
- Else recompute, record `deps`, compare value to previous to update `last_changed`.

This gives:
- Deterministic “what changed when”
- Ability to implement `LATEST` as “pick input with max last_changed”

You can start with cache disabled (always recompute), then add cache + `last_changed`.

---

## 4. Tick Model & Determinism

### 4.1 Ticks and sequencing

Define:
- `TickId`: monotonic integer incremented for each external schedule (microtask/frame)
- `TickSeq`: (tick, seq) pair for ordering events/changes within the tick

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TickSeq {
    pub tick: u64,
    pub seq: u32,
}
```

Within a tick:
- external inputs are assigned increasing `seq` in arrival order
- state updates are committed in a deterministic order (see below)

### 4.2 Deterministic evaluation order

At minimum:
- Evaluate top-level definitions in **source order**
- Within an expression, evaluate children in a deterministic AST order

For committing updates:
- Sort by `(ScopeId, ExprId)` then by `TickSeq` of triggering event
- Within a list, preserve stable item order (append order) and process in that order

This must be explicitly defined so “same inputs → same state”.

---

## 5. Semantics of Key Language Constructs

This section defines the behaviors that matter for `todo_mvc.bn` and similar apps.

### 5.1 `SKIP`

`SKIP` is “no value emitted” (think: no event / no update).

Implementation:
- Make `Value` include `Skip`, or represent `Option<Value>` where `None` = skip.

Rules:
- `THEN` returns `SKIP` when input is `SKIP`.
- `WHEN` returns `SKIP` when input is `SKIP`.
- `LATEST` ignores `SKIP` inputs unless all are `SKIP`.
- Field access on `SKIP` yields `SKIP`.

### 5.2 `LATEST { a; b; c }`

`LATEST` returns the value from the input that changed most recently.

Implementation:
- Evaluate each input to `(value, last_changed)`
- Choose the input with greatest `last_changed`
- If ties, break by input index (stable)
- If all are `SKIP`, output `SKIP`

This is the core primitive for “merge multiple event sources”.

### 5.3 `THEN { body }`

`x |> THEN { body }` is an event transform:
- if `x` is `SKIP` → output `SKIP`
- else evaluate `body` (with any pattern bindings) and output that result

`THEN` is allowed to produce:
- a value (state update, navigation path, etc.)
- `SKIP` (filtered)
- effects (queued, executed after stabilization)

### 5.4 `WHEN { pattern => body; __ => ... }`

`WHEN` is a conditional transform on a value arrival:
- If input is `SKIP`, output `SKIP`
- Else pick first matching pattern and evaluate that arm’s body

Patterns should be fully type-checked at compile time.

### 5.5 `WHILE { pattern => body; __ => ... }`

`WHILE` is continuous selection based on the **current value**:
- Evaluate input (should usually be a behavior, not an event)
- Select matching arm
- Evaluate and return arm output every tick

For v3 MVP, it’s acceptable to define:
- If input is `SKIP`, treat as no match and choose `__` arm if present.

### 5.6 `HOLD` (state)

Syntax (example):
```boon
counter: 0 |> HOLD state {
    button.event.press |> THEN { state + 1 }
}
```

Semantics:

- A `HOLD` expression owns a `HoldCell` keyed by its SlotKey.
- The `HOLD` result value is the cell’s current state.
- The body block is evaluated each tick (in the same scope) to produce **zero or more updates** (usually via `LATEST`).
- When the body produces a non-skip value `new_state`, the cell updates to `new_state`.

Commit semantics (deterministic):
- Collect all updates produced within a tick
- Apply them in increasing `(TickSeq, ScopeId, ExprId)` order
- If a cell receives multiple updates in one tick, apply sequentially

Sequential updates matter for constructs like “pulses”, but TodoMVC is fine even with “last write wins” as long as order is deterministic.

Practical MVP rule (good default):
- Apply updates immediately when encountered during evaluation of top-level `store` before evaluating `document`.
  - This ensures view sees updated state in the same tick.
  - Still keep a deterministic guard against re-entrant infinite loops (max iterations or “no update twice without new input”).

### 5.7 `LINK` (late binding / wiring)

This is the critical cycle-breaker for UI wiring.

In v3, `LINK` is a state cell like `HOLD`, but its purpose is *binding references* (usually UI elements).

#### `LINK` literal
`LINK` creates a `LinkCell` with initial state `Unbound`.

#### `x |> LINK { target }`
This is a *wiring commit*, not a transform:
- Evaluate `target` to a `LinkCellRef`
- Set `target = Bound(x_ref)`
- Return `x` unchanged

#### Reading from an unbound link
If you evaluate the value of a `LINK` that is still unbound:
- It returns `SKIP` (recommended) or a distinguished `Unbound` value.

For compatibility with existing `.bn` patterns, **prefer `SKIP`** because it naturally suppresses event flows until the UI actually binds the element.

This single rule breaks cyclic bootstrapping cleanly:
- store reads `store.elements.toggle_all_checkbox.event.click`
- but until the UI binds `toggle_all_checkbox` into that link, the read yields `SKIP`
- after binding, it yields actual event pulses

### 5.8 `LIST` (stable keys + scoped evaluation)

#### List as “keys + order”
A list value is (conceptually):
- stable ordered keys: `[k0, k1, ...]`
- allocation site state (`AllocSite.next`)
- (optional) per-item stored values, or per-item root value refs

#### List item scopes
For each item key `k`, define a child scope:

```text
item_scope = ScopeId::child(parent_scope, list_alloc_site, k)
```

Everything inside the item (including HOLD/LINK) is keyed by `(item_scope, expr_id)`.

#### `List/append(item: expr)`
- If input event/value is `SKIP`, do nothing.
- Else allocate a new `ItemKey` from the callsite `AllocSite`.
- Create `item_scope`.
- Evaluate `expr` in `item_scope` (lexically capturing outer bindings).
- Append the resulting value to list items.

**No graph cloning, no template rewiring.** The item is just “the same code evaluated in a different scope”.

#### `List/map(item, new: expr)`
Do not allocate new keys.

Instead, treat map as a “view” over the same keys:
- For each source item key `k`, evaluate `expr` in a derived scope:

```text
mapped_scope = ScopeId::child(parent_scope, map_callsite, k)
```

This preserves per-item state inside mapped expressions (if any).

#### `List/retain(item, if: cond)`
Also a view:
- Evaluate `cond` for each item key (in a derived per-item scope if needed)
- Keep keys whose condition is true

#### `List/remove(item on: event)`
Deterministic removal:
- Evaluate `event` per item in stable order
- If event is non-skip for an item, mark it removed
- After pass, remove marked keys in a single commit (so iteration remains stable)

Optional: schedule explicit finalization hooks later, but MVP can simply drop item keys and let cells in that scope be garbage-collected by reachability.

---

## 6. UI: VDOM + Stable Element Identity + Built-in UI State

### 6.1 Why v3 separates “UI state” from user graph

Hover/focus/text are **renderer-owned facts** about the DOM, not application state.

In v2-style engines, treating them as just another reactive node makes debugging styles painful (“why is hover not affecting this text color?” becomes “where is the subscription edge?”).

In v3:
- UI state lives in `UiStore` keyed by `ElementId`
- User code reads it via fields like `element.hovered` or `text_input.text`
- These reads are normal inputs into evaluation, but they are not created by cloning/wiring; they are looked up by stable IDs.

### 6.2 `ElementId`

An element gets an ID derived from:
- the callsite (`SourceId`)
- the scope (`ScopeId`)
- (optionally) position index if the same callsite repeats within the same scope in a loop

Rule of thumb:
- If it is inside a list item scope, the item key already makes it unique.

```rust
pub struct ElementId(u128); // hash of (source_id, scope_id, maybe ordinal)
```

### 6.3 Event ports

Each element exposes event ports (click, press, key_down, change, blur, ...).

Define:
```rust
pub enum EventType { Click, Press, KeyDown, Change, Blur, DoubleClick, /* ... */ }
pub struct EventPortId { pub element: ElementId, pub ty: EventType }
```

`UiStore` holds “last pulse” for each port:
```rust
pub struct EventPortState {
    pub last_pulse: Option<(TickSeq, Value)>,
}
```

When evaluating an event read:
- if `last_pulse.tick == current_tick` (or last_pulse.seq >= last_consumed_seq), return payload
- else return `SKIP`

This makes DOM events naturally integrate with `LATEST`/`THEN`/`HOLD`.

### 6.4 Text inputs (behaviors)

Text inputs often need both:
- the pulse: `event.change` with payload `{ text: ... }`
- the current value: `.text`

Implement `.text` as UI state:
```rust
pub struct UiTextInputState { pub text: String, pub focused: bool, /* ... */ }
```

The renderer updates this state on DOM change/input events, and evaluation reads it by `ElementId`.

### 6.5 VDOM shape

Evaluation of `Document/new(root: ...)` returns a VDOM tree:

```rust
pub enum VNode {
    Element(VElement),
    Text(String),
    None,
}

pub struct VElement {
    pub id: ElementId,
    pub tag: Tag,                  // Div, Button, Input, custom semantics
    pub attrs: Vec<Attr>,
    pub style: StyleObject,        // from Boon style records
    pub children: Vec<VNode>,
    pub event_bindings: Vec<EventType>, // which DOM listeners to attach
}
```

The browser bridge diffs `VNode` trees by `ElementId` (keyed diff).

---

## 7. How This Fixes the TodoMVC “Toggle All + Dynamically Added Todos” Class of Bugs

The common failure mode in graph-cloning engines:
- a new list item is created by cloning a subgraph/template
- some reference inside the clone (often to an external event source) is accidentally not wired
- the item never sees that event

In v3:

- There is no clone.
- A todo item’s `completed` HOLD simply *reads* from:
  - its own checkbox event (bound via item scope’s ElementId)
  - the global “toggle all” event (read through the shared `LINK` cell to the toggle-all element)
- Because evaluation runs every tick and reads from stable inputs, a newly appended todo automatically starts reading the same global event next tick.

No “subscribe step” exists, so it can’t be forgotten.

---

## 8. Debugging & Diagnostics (first-class)

### 8.1 “Explain why” data model

For every SlotKey, store:
- `last_changed: TickSeq`
- `changed_because: Trigger` (optional)
- `deps: Vec<SlotKey>` (from last evaluation)

```rust
pub enum Trigger {
    DomEvent { port: EventPortId, seq: TickSeq },
    HoldUpdate { cell: SlotKey, seq: TickSeq },
    LinkBind { cell: SlotKey, seq: TickSeq },
    ListMutation { cell: SlotKey, seq: TickSeq },
}
```

This enables:
- “why did `all_completed` become True?”
- “why did this style field change?”
- “why didn’t this click do anything?” (answer: event existed but path was unbound → SKIP)

### 8.2 Inspector queries (MVP)

Implement a debug API (even if internal-only first):
- list all cells
- get SlotKey value + last_changed + deps
- trace a dependency chain to a trigger
- dump per-element UI state + event pulses

### 8.3 Deterministic replay (roadmap)

Record:
- external DOM events stream (EventPortId + payload + TickSeq)
- snapshots of all cells (HOLD/LINK/LIST)

Replay is then:
- load snapshot
- feed same event stream
- v3 determinism guarantees the same UI output.

---

## 9. Compilation to IR (what the compiler must provide)

This architecture depends on the compiler producing an IR that makes runtime evaluation straightforward.

### 9.1 Required compiler outputs

1. `ExprId` for every expression
2. `SourceId` + source span for diagnostics
3. A typed expression tree (or bytecode) with resolved:
   - function calls and parameter bindings
   - PASS/PASSED access (either compiled to hidden params or explicit ops)
   - list allocation site IDs
4. Record layout information so field access can be mostly static:
   - field names → field indices / ExprIds

### 9.2 Preferred field access strategy (type-safe, low complexity)

Avoid runtime “object maps” whenever possible:
- If a value is a record with known layout, field access compiles to “evaluate field ExprId in that record’s scope”.
- If a value is a union of record layouts:
  - compile to a checked match on runtime layout tag
  - if field missing, return `Unplugged` / error / SKIP (decide by language rules)

This removes the need for v2-style dynamic Extractor nodes.

---

## 10. Implementation Plan (for an agent)

This is a practical sequence that gets TodoMVC working early.

### Phase A — minimal correct engine (no caching)

1. IR: assign `ExprId`/`SourceId`; keep AST structure
2. Runtime:
   - implement `ScopeId` creation for:
     - root
     - function calls
     - list item instances
   - implement cells:
     - `HoldCell`
     - `LinkCell`
     - `ListCell` with `AllocSite`
3. Evaluator:
   - recursive eval for needed expression variants:
     - literals, objects, lists
     - field access
     - pipes
     - `LATEST`, `THEN`, `WHEN`, `WHILE`
     - `HOLD`, `LINK`
     - list ops used by TodoMVC: append/remove/retain/map/count/is_empty
4. UI:
   - VDOM construction from `Element/*` and `Document/new`
   - ElementId generation = hash(SourceId, ScopeId)
   - Event ports + hovered/text state in UiStore
   - DOM diff keyed by ElementId
5. Tick loop:
   - ingest DOM events into UiStore
   - evaluate top-level `store` then `document`
   - commit state updates/effects
   - render VDOM

TodoMVC should work at this stage, even if slower.

### Phase B — add caching + “latest” timestamps

1. Add `CacheEntry` per SlotKey
2. Implement `last_changed` tracking
3. Implement `LATEST` based on `last_changed`
4. Add debug “why” chain from deps + triggers

### Phase C — correctness hardening

1. Define and enforce update ordering rules
2. Detect infinite loops (state updates without new inputs)
3. Garbage collect unreachable scopes (removed list items)

### Phase D — golden screenshot tests (pixel-perfect)

1. Headless browser render at 700×700
2. Screenshot compare against the reference PNG
3. Run in CI (or local) to keep regression tight

---

## 11. Key Invariants (must hold)

1. **No runtime subgraph cloning**
   - All instantiation is by ScopeId.
2. **All state is in cells keyed by SlotKey**
   - No hidden mutable state in random nodes.
3. **Reading an unbound LINK is safe**
   - Prefer: it yields SKIP, not crash.
4. **Stable identity**
   - list items: stable ItemKey and deterministic ordering
   - UI elements: stable ElementId
5. **Determinism**
   - explicit ordering for:
     - event ingestion
     - state commit
     - list operations

---

## 12. Notes / Open Decisions (explicitly choose later)

1. `HOLD` commit semantics within a tick:
   - immediate vs batched vs iterative stabilization
   - TodoMVC works with simple deterministic immediate updates as long as store is evaluated before document.
2. Behavior of field access on `SKIP` / unbound `LINK`:
   - recommended: propagate SKIP
3. Error handling / FLUSH:
   - v3 can keep “error value union” or explicit `Flushed` payload; decide based on current language direction.

