# Plan: TodoMVC Wasm Parity (Compile to Wasm, Run in Preview)

**Status:** Draft (implementation deferred)
**Date:** 2026-02-18
**Depends on:** `wasm_engine_direct_compilation_plan.md`

---

## 1. Goal

Compile `todo_mvc.bn` directly to Wasm and pass the existing behavioral expectations in `todo_mvc.expected` with no fallback engines.

This document focuses on parity strategy for the most complex interactive example.

---

## 2. Source of Truth

- Program: `playground/frontend/src/examples/todo_mvc/todo_mvc.bn`
- Expected behavior: `playground/frontend/src/examples/todo_mvc/todo_mvc.expected`

The expected file is already comprehensive and should remain the reference contract.

---

## 3. Behavior Surface to Match

TodoMVC parity includes:

- startup input focus and typeability,
- add flow (Enter key, trim, empty rejection),
- dynamic checkbox behavior and counts,
- filter routing (`All`, `Active`, `Completed`),
- row toggle and toggle-all semantics,
- edit lifecycle (double-click, Escape, Enter, blur),
- title save correctness,
- row delete and clear-completed correctness,
- persistence across refresh/re-run,
- footer text correctness.

---

## 4. Compile-Time Decomposition of `todo_mvc.bn`

## 4.1 Global state and links

Compile the `store` root object into:

- global link refs:
  - filter buttons (`all`, `active`, `completed`)
  - `remove_completed_button`
  - `toggle_all_checkbox`
  - `new_todo_title_text_input`
- global cells:
  - `navigation_result`
  - `selected_filter`
  - `title_to_add`
  - `todos_count`
  - `completed_todos_count`
  - `active_todos_count`
  - `all_completed`
- global list store:
  - `todos`

## 4.2 Row template (`new_todo_with_completed`)

Each todo row compiles to item-local state:

- row links:
  - `remove_todo_button`
  - `editing_todo_title_element`
  - `todo_title_element`
  - `todo_checkbox`
- row cells:
  - `title`
  - `editing`
  - `completed`

Each row must have stable identity (`__key`) for event routing and persistence.

---

## 5. Operator Mapping in TodoMVC Context

- `LATEST` in navigation and row state updates:
  - compile as shared-target, multi-arm transitions.

- `WHILE` for selected filter and styling conditions:
  - compile as dependency-flowing match nodes,
  - trigger recompute on source/dependency changes.

- `WHEN` for key matching and simple mapping:
  - compile as frozen match nodes triggered by source updates.

- `HOLD` for title/editing/completed states:
  - compile as mutable cell update transitions.

- `THEN` for event-triggered transformations:
  - compile as trigger-bound expression nodes.

- `FLUSH`:
  - not central in todo_mvc path, but runtime semantics remain enabled globally.

---

## 6. TodoMVC Event Model (Wasm Runtime)

Required event classes:

1. global press events:
   - filter buttons,
   - clear completed button.

2. global click events:
   - toggle all checkbox.

3. new-todo input events:
   - key_down,
   - change.

4. row events:
   - checkbox click,
   - title double_click,
   - edit input key_down,
   - edit input blur,
   - remove press,
   - row hover-related visibility toggles (host/UI side + link action).

5. route change event:
   - recompute selected filter and affected views.

All event routes must be deterministic and keyed for row-local actions.

---

## 7. Runtime Update Model for TodoMVC

Each incoming event executes:

1. event decode (`EventId` + payload + optional row key),
2. dispatch to precompiled node list,
3. mutate cells/lists/views,
4. emit patch stream,
5. host applies patches and updates preview.

Patch stream must preserve event order and include sufficient detail for incremental UI updates.

---

## 8. Parity Phases Mapped to `todo_mvc.expected`

## Phase A: startup and early add checks (Sections 1, 1b)

- initial focus and typeability assertions,
- initial outline state,
- add todo and verify count/checkbox growth.

## Phase B: filter correctness (Section 2)

- route updates,
- Active/Completed/All visibility,
- filter outline behavior.

## Phase C: multi-add and count growth (Section 3)

- sequential appends,
- correct aggregate recompute.

## Phase D: toggle flows (Sections 4, 5, 6, 6b, 7)

- per-row toggle isolation,
- toggle-all transitions,
- clear completed correctness,
- post-clear add/toggle correctness.

## Phase E: editing flows (Sections 8, 8b, 9)

- enter edit mode with double-click,
- focus persistence in edit mode,
- Escape cancel,
- Enter save with trimmed text semantics.

## Phase F: delete/clear/footer rendering (Sections 10, 11, 13)

- hover reveal + delete action,
- remove completed item correctness,
- footer text remains plain text and semantically correct.

## Phase G: persistence (Section 12)

- refresh + rerun restores expected state and visible text.

---

## 9. Key Correctness Invariants

1. **Row identity invariant**
   - each item has stable unique key,
   - row events never leak to other rows.

2. **Aggregate invariant**
   - counts always match list state after each event.

3. **Filter-view invariant**
   - `selected_filter` drives visible list membership correctly.

4. **Edit invariant**
   - Escape exits without save,
   - Enter/blur save only valid trimmed non-empty content.

5. **Persistence invariant**
   - list rows and row-local states survive full refresh.

---

## 10. Planned Debug Instrumentation (Implementation Phase)

To speed parity debugging during implementation:

- optional event trace ring buffer (`event_id`, `row_key`, `node_id`),
- optional aggregate trace before/after events,
- optional visible-membership trace for filtered views,
- host-side trace dump on failed expected step.

All instrumentation must be feature- or debug-gated.

---

## 11. Acceptance Criteria

TodoMVC parity is complete when:

1. `todo_mvc.expected` passes in Wasm mode,
2. no fallback execution occurs,
3. repeated runs produce deterministic results,
4. persistence checks pass after full refresh,
5. row identity and action isolation remain stable.

---

## 12. Deferred Execution Note

This document defines the parity plan only. Implementation is deferred.
