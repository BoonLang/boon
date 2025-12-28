# Boon v3 (Chronicle Reactor) — Implementation-Oriented Notes

Status: Draft
Date: 2025-12-28
Audience: Engine/Compiler implementers
Goal: A deterministic, type-safe, easily debuggable runtime that runs existing Boon UI examples (esp. `todo_mvc.bn`) unchanged and can match pixel‑perfect reference rendering.

---

## 0) Problem Summary (What v3 fixes)

**v2 pain points (from current repo behavior):**
- Dynamic list items don’t reliably subscribe to external dependencies (e.g., TodoMVC “Toggle All” doesn’t affect newly added items).
- LINK targets and event wiring are hard to reason about (subgraphs clone IOPads, late binding, hidden rewires).
- Debugging is opaque (implicit graph rewrites, async scheduling, implicit subscriptions).

**v3 goals:**
- Deterministic synchronous tick.
- Compile‑time dependency capture (no runtime “discovery”).
- Stable list item identity and subscriptions.
- Explicit event ports and typed payloads.
- Inspectable “delta ledger” per tick.

---

## 1) The Core Concept: Graph Templates + Instances

### 1.1 Graph Template (Compile-time)
A **template** is a frozen subgraph compiled from a Boon expression or function body. It has:

- **Inputs**: named ports (typed).
- **Outputs**: named ports (typed).
- **Capture list**: *all external dependencies* referenced inside.
- **Event ports**: explicit event streams (from LINK) used in the template.
- **Field paths**: LINK paths recorded as explicit field paths.
- **Internal nodes**: “pure” node graph with local SlotIds.

A template must be **self‑contained** except for declared captures.

### 1.2 Template Instance (Runtime)
An **instance** is a template specialized with:
- **ScopeId** (unique per item or WHILE arm).
- **Bindings** for inputs (item payloads, etc.).
- **Capture bindings** to actual runtime slots.

Instances **do not recompile**; they only clone a template’s internal nodes and substitute bindings. External references are never “discovered” at runtime.

---

## 2) Deterministic Tick Reactor

The runtime executes in a single deterministic tick, without async tasks:

1) **Collect external events** (DOM events, timers).  
2) **Propagate changes** through dirty nodes in stable order.  
3) **Produce a Delta Ledger** (debug log of all changes).  
4) **Render diff** through Bridge.  
5) **Commit** updated node values.

**Ordering**: (SourceId, ScopeId, Port) to ensure determinism.

---

## 3) Delta Ledger (Debugging Backbone)

Every node emits **deltas**, not just values. The ledger is a per‑tick log:

- `Set { value }`
- `ListInsert { key, value }`
- `ListRemove { key }`
- `Event { kind, payload }`
- `LinkBind { element_id, port }`
- `LinkUnbind { element_id, port }`

**Why**: It makes every tick reproducible and explainable. You can replay a tick and see exactly why a UI change happened.

---

## 4) Collections / Lists

### 4.1 Identity Rules
- Each `List/append` site has an **AllocSite** (SourceId + counter).
- `ItemKey = AllocSiteId + counter` (monotonic).
- The key *never* changes, even if the list is filtered.

### 4.2 List/map Template Instantiation (Fixes Toggle All)
The List/map body is compiled once as a template.

- The `item` parameter is a template input.
- External dependencies (e.g., `store.elements.toggle_all_checkbox.event.click`) are captured.
- Each list item instance binds `item` and all captures.

**This guarantees new items subscribe to all external dependencies.**

---

## 5) LINK: Explicit Event and State Ports

LINK is split into:
- **Event ports** (discrete): click, press, key_down, blur, change
- **State ports** (continuous): hovered, focused, checked, text

Example schemas:
- click: `Unit`
- key_down: `{ key: Tag }`
- change: `{ text: Text }`
- hovered: `Bool`
- checked: `Bool`

LINK resolution occurs once, at instantiation, via recorded field paths. No runtime inference.

---

## 6) Type System Integration

- Field paths are validated at compile time.
- Tagged objects (`Hidden[...]`, `Reference[...]`, `Oklch[...]`) have known schemas.
- Runtime uses typed payloads; no untyped JSON walking.

If a field is missing:
- **Strict mode**: compile error.
- **Dev mode**: typed `Missing` value + ledger warning.

---

## 7) Bridge v3 (Pixel‑Perfect Rendering)

Bridge receives:
- A typed element tree.
- A style object with resolved values.
- Diff operations (create/update/remove).

Rules:
- **No implicit layout logic** in runtime.
- **Single CSS converter** for all style mapping.
- Deterministic style output per tick.

This ensures pixel‑perfect rendering for `todo_mvc.bn`.

---

## 8) Runtime Data Structures (Sketch)

### 8.1 Node Kinds (minimal)
- Producer
- Wire
- Register (HOLD)
- Combiner (LATEST)
- Transformer (THEN)
- PatternMux (WHEN)
- SwitchedWire (WHILE)
- Bus (List)
- ListMap, ListFilter, ListReduce
- IOPad (events)
- StatePort (hovered, text, checked)
- Effect (Router/go_to)

### 8.2 Template Spec (conceptual)

```
Template {
  id: TemplateId,
  inputs: [PortSpec],
  outputs: [PortSpec],
  captures: [CaptureSpec],
  events: [EventPortSpec],
  field_paths: [PathSpec],
  nodes: [NodeDef],
  routes: [RouteDef]
}
```

### 8.3 Instance Spec

```
Instance {
  template_id: TemplateId,
  scope_id: ScopeId,
  bindings: { input_port -> SlotId },
  captures: { capture_id -> SlotId }
}
```

---

## 9) Key Algorithms

### 9.1 Compile Time — Build Template
1) Walk expression → build node graph.
2) Collect **external dependencies** (anything not created inside template scope).
3) Record captures.
4) Export Template.

### 9.2 Runtime — Instantiate Template
1) Allocate new slots for all internal nodes.
2) Remap internal slots.
3) Bind `item` and captures to existing runtime slots.
4) Resolve LINK paths and event ports.
5) Mark entry nodes dirty to evaluate.

---

## 10) TodoMVC Success Criteria

TodoMVC must work unchanged (`todo_mvc.bn`):
- Toggle‑all affects newly added items.
- Filter buttons update lists properly.
- Hover styling is correct and deterministic.
- Pixel match to `reference_700x700.png` is achievable.

---

## 11) Implementation Plan (Minimal)

**Phase 1**: Template compiler
- Add template builder in compiler pipeline.
- Capture external dependencies.

**Phase 2**: Template instantiation
- Instantiate per List/append and List/map.
- Bind captures explicitly.

**Phase 3**: Deterministic tick + ledger
- Replace async scheduling with synchronous tick.
- Emit delta ledger per tick.

**Phase 4**: Bridge v3
- Typed element tree input.
- Single, deterministic CSS converter.

**Phase 5**: Validation
- Run `todo_mvc.bn`, `filter_checkbox_bug.bn`, `list_map_external_dep.bn`.

---

## 12) Notes for Another Agent

- **Focus on templates and captures**. This is the core fix for dynamic list issues.
- The runtime should **never infer dependencies** at runtime.
- Every node should emit deltas into the ledger; that ledger is the primary debug tool.
- Keep the tick loop **synchronous**. Determinism beats concurrency here.
- The Bridge is intentionally simple: apply diffs, no hidden logic.

---

## 13) Appendix: Quick Glossary

- **Template**: compiled static subgraph with explicit captures.
- **Instance**: runtime clone of template with bindings.
- **Capture**: external dependency referenced by template.
- **Delta ledger**: per‑tick log of changes.
- **AllocSite**: monotonic item key generator for lists.

---

End of document.

## 14) Human-Oriented Diagrams (Mental Models)

### 14.1 Compile + Runtime Flow (Big Picture)

```
Boon Source (.bn)
    |
    v
Parser -> AST
    |
    v
Template Compiler
    |
    +--> Graph Template(s)
    |
    v
Runtime Instantiation
    |
    v
Deterministic Tick Reactor
    |
    +--> Delta Ledger (debug)
    |
    v
Bridge v3 (diff -> DOM)
```

### 14.2 Template vs Instance

```
Template T (compile-time)
  inputs:   [item]
  captures: [store.toggle_all.click, store.all_completed]
  nodes:    (subgraph)
  output:   [todo_item_element]

Instance T#42 (runtime)
  item   -> slot_9001 (specific todo item)
  capture[0] -> slot_120 (toggle_all.click IOPad)
  capture[1] -> slot_121 (store.all_completed Register)
  cloned nodes -> slots 15000..15080
```

### 14.3 Tick Reactor (Deterministic)

```
Tick start
  1) Collect external events
  2) Process dirty nodes in stable order
  3) Append deltas to ledger
  4) Apply UI diff
  5) Commit node values
Tick end
```

### 14.4 TodoMVC Toggle-All Path

```
Click ToggleAll
  -> event.click delta
  -> item.completed HOLD registers (all items)
  -> list counts update
  -> UI checkbox updates

Key: new items are subscribed because captures were bound at instantiation.
```

---

## 15) AI-Oriented Algorithms (Precise Steps)

### 15.1 Compile Template

Pseudo-code (compiler stage):

```
compile_template(expr, ctx):
  template = new Template()
  enter template scope
  internal_slots = set()
  local_bindings = {}

  slot = compile_expr(expr):
    when creating a node -> add to internal_slots
    when reading a slot:
      if slot not in internal_slots and not a local binding:
        cap_id = template.add_capture(slot)
        return CaptureRef(cap_id)
      else return slot

  template.output = slot
  template.nodes = collected nodes
  template.routes = collected routes
  return template
```

**Rule**: captures are detected by “slot not created inside template scope.”

### 15.2 Instantiate Template

```
instantiate(template, bindings, capture_bindings, scope_id):
  slot_map = {}

  // map captures and inputs directly
  for input in template.inputs:
    slot_map[input.slot] = bindings[input.name]

  for capture in template.captures:
    slot_map[capture.slot] = capture_bindings[capture.id]

  // clone internal nodes
  for node in template.nodes:
    new_slot = arena.alloc(scope_id)
    slot_map[node.slot] = new_slot

  // clone node kinds with remapped slots
  for node in template.nodes:
    new_kind = remap(node.kind, slot_map)
    arena.set_kind(slot_map[node.slot], new_kind)

  // add routes
  for route in template.routes:
    add_route(remap(route.src), remap(route.dst), route.port)

  // mark entry nodes dirty
  mark_dirty(slot_map[template.output])

  return slot_map[template.output]
```

### 15.3 List/map Runtime Update

```
on_bus_change(bus_slot):
  items = bus.items

  for each item in items:
    if item not in mapped:
      output = instantiate(template, {item: item.slot}, captures, scope_id=item.key)
      mapped[item.slot] = output
      output_bus.insert(item.key, output)

  for each mapped_item not in items:
    finalize_scope(mapped_item.scope_id)
    output_bus.remove(mapped_item.key)
```

### 15.4 LINK Resolution

```
resolve_link(instance, path):
  current = instance.capture_root_or_input
  for field in path:
    current = arena.get_field(current, field)
  return current  // should be an IOPad or StatePort
```

**Never** search or infer links at runtime beyond the recorded path.

---

## 16) Runtime Contracts (What Must Always Be True)

1) **No runtime dependency discovery**. All external references are captures.
2) **Deterministic tick order**. Sorting key is (SourceId, ScopeId, Port).
3) **List identity is stable**. Keys never change for an item.
4) **LINK binding is single-pass**. Once resolved, it is not re-resolved unless the template instance is destroyed.
5) **Every mutation emits a delta**. The ledger must be complete.

---

## 17) Invariants + Assertions (Suggested)

- Every template instance must have all captures bound.
- No route points to a freed slot.
- Each List/append site has its own AllocSite.
- List/map output keys match source keys (unless explicitly transformed).
- Every LINK target resolves to a concrete port or fails with a clear error.

Add debug assertions in dev builds and fail fast with a ledger dump.

---

## 18) Minimal Instrumentation for Debugging

**Ledger export**
- Expose `ledger.json` per tick in dev mode.
- Include node ids, source locations, and payloads.

**Trace helpers**
- `trace_node(slot_id)` logs all deltas for that node.
- `trace_scope(scope_id)` logs all node changes within that scope.
- `trace_capture(template_id, capture_id)` shows binding + activity.

---

## 19) TodoMVC Dependency Map (Concrete Example)

For each todo item instance, the `completed` HOLD depends on:
- `todo_elements.todo_checkbox.event.click`
- `store.elements.toggle_all_checkbox.event.click`
- `store.all_completed` (used to compute next state)

In v3:
- These dependencies are **captures** of the item template.
- Each item instance binds them explicitly.
- Newly appended items automatically subscribe to toggle_all.

---

## 20) Implementation Checklist for Another Agent

1) Build template compiler with capture detection.
2) Add capture placeholders to the node graph.
3) Implement template instantiation (clone + remap + route).
4) Replace List/map runtime cloning with template instantiation.
5) Add synchronous tick reactor.
6) Add delta ledger output.
7) Implement Bridge v3 diff pipeline.
8) Validate TodoMVC + filter_checkbox_bug + list_map_external_dep.

---

## 21) Optional: Data Structures for Templates (More Explicit)

```
struct Template {
  id: TemplateId,
  inputs: Vec<PortSpec>,
  outputs: Vec<PortSpec>,
  captures: Vec<CaptureSpec>,
  nodes: Vec<NodeDef>,
  routes: Vec<RouteDef>,
  link_paths: Vec<LinkPathSpec>,
}

struct CaptureSpec {
  id: CaptureId,
  source_slot: SlotId,   // compile-time slot
  type_id: TypeId,
}
```

---

## 22) Optional: Glossary Addendum

- **CaptureRef**: placeholder slot in template that is bound to a runtime slot on instantiation.
- **LinkPathSpec**: field path description used to resolve LINK targets.
- **Ledger**: complete ordered list of all deltas in a tick.

---

End of document.
