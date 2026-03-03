# todo_mvc_physical — Remaining Issues & Status

This document tracks the known issues and current state of `todo_mvc_physical`
across all three engines after the multi-session implementation effort.

## Current State (2026-03-03)

### Actors Engine — Primary, Most Complete

| Feature              | Status | Notes |
|----------------------|--------|-------|
| Initial render       | ✅     | Header, footer, theme buttons, dark mode toggle |
| Add items            | ✅     | Counter updates ("1 item", "2 items") |
| Checkbox toggle      | ✅     | Fixed: `Bool/toggle` rewritten in `api.rs` |
| Toggle-all (">")    | ✅     | Checks/unchecks all items, counter updates |
| Active filter        | ✅     | Hides completed items correctly |
| Completed filter     | ❌     | Shows ALL items — pre-existing engine bug (see §1) |
| Clear completed      | ✅     | Removes completed items |
| Dark mode toggle     | ✅     | Light↔Dark switching, all elements update |
| Theme switching      | ✅     | Professional/Glass/Brutalist/Neumorphic |
| Physical CSS         | ✅     | Materials, depth shadows, gloss, transitions |

### DD Engine — Functional for Static UI

| Feature              | Status | Notes |
|----------------------|--------|-------|
| Initial render       | ✅     | Full static UI with physical CSS |
| Add items            | ✅     | Items appear, counter updates |
| Checkbox toggle      | ✅     | Native `compile_bool_toggle` (HoldState desugaring) |
| Filters              | ❓     | Not fully tested; may share Completed filter bug |
| Physical CSS         | ✅     | Material colors, depth shadows implemented |
| Keyed stripe         | ⚠️     | Detection works via ULID keys (plan in progress) |

### WASM Engine — Renders but Limited

| Feature              | Status | Notes |
|----------------------|--------|-------|
| Initial render       | ✅     | Static UI renders (header, footer, themes) |
| Add items            | ⚠️     | Items render but checkbox icon shows as "0" text (see §2) |
| Checkbox toggle      | ❌     | Click doesn't toggle — related to icon rendering |
| Multi-file support   | ⚠️     | Was blocked by 50K node limit; now renders via guards |
| Physical CSS         | ⚠️     | Partial — some styles not applied |

---

## Issue §1: Completed Filter Shows All Items (Actors Engine)

**Severity:** Medium — Active filter works; only Completed is broken.

**Symptom:** When clicking the "Completed" filter, all items remain visible
instead of showing only completed items.

**Code location:** `RUN.bn` line 439-443:
```boon
PASSED.store.selected_filter |> WHILE {
    All => True
    Active => item.completed |> Bool/not()
    Completed => item.completed
}
```

**Root cause hypothesis:** The `WHILE` evaluator handles direct value
passthrough (`Completed => item.completed`) differently from piped values
(`Active => item.completed |> Bool/not()`). When the WHILE arm switches to
`Completed`, it subscribes to `item.completed` directly. The subscription
may return a stale or non-reactive reference rather than forwarding the
current boolean value.

Evidence:
- `Active => item.completed |> Bool/not()` works correctly (creates a new
  actor via the pipe, which properly subscribes to `item.completed`)
- `Completed => item.completed` doesn't filter (direct reference may not
  properly emit the current value to `List/retain`)
- `All => True` works (static value, no subscription needed)

**Same pattern exists** in regular `todo_mvc` (`todo_mvc.bn` line 392-396),
suggesting this is a pre-existing engine bug, not specific to physical.

**Investigation path:**
1. Check how `WHILE` in the Actors evaluator handles direct actor references
   vs piped expressions in body arms
2. Check if `List/retain`'s predicate subscription correctly receives
   initial values when WHILE switches arms
3. Look at `stream_from_now()` vs `value()` semantics for re-subscribed actors

**Workaround:** Could pipe through identity function:
`Completed => item.completed |> Bool/not() |> Bool/not()` — untested.

---

## Issue §2: WASM Checkbox Icon Renders as "0" Text

**Severity:** Low — WASM engine has broader limitations for multi-file examples.

**Symptom:** Individual checkbox renders as 9×22px element with text "0"
instead of the circle icon background image.

**Code location:** The checkbox icon in `RUN.bn` uses:
```boon
Scene/Element/block(
    style: [
        size: Theme/sizing(of: TouchTarget)
        background: [url: ...]
    ]
)
```

**Root cause:** The WASM bridge's `build_item_checkbox` doesn't properly
handle `background: [url: ...]` on `Scene/Element/block`. It appears to
render the checkbox's boolean value (0.0 for false) as text content instead
of using the background-url CSS property for the icon.

**Files to investigate:**
- `engine_wasm/bridge.rs` — `build_item_checkbox` function
- `engine_wasm/bridge.rs` — `build_element` handling of `background` style
- `engine_wasm/runtime.rs` — how block elements handle nested style objects

---

## Issue §3: DD `shopping_list` Compilation Error (Pre-existing)

**Severity:** Low — not related to `todo_mvc_physical`.

**Symptom:** `DD compilation error: Variable 'elements' not found`

**Root cause:** The DD compiler cannot resolve sibling field references
within the store object. In `shopping_list.bn`, the store uses:
```boon
store: [
    elements: [item_input: LINK, ...]
    text_to_add: elements.item_input.event.key_down.key |> ...
]
```
The reference to `elements` (a sibling field) fails because the DD compiler
doesn't have that field in its variable scope during compilation.

---

## Completed Fixes (This Session)

### Fix: `Bool/toggle` Accumulating Subscriptions (Actors)

**File:** `crates/boon/src/platform/browser/api.rs` — `function_bool_toggle`

**Problem:** The old implementation used `.then().flatten()` which created
a new stream subscription on every toggle event. These subscriptions
accumulated infinitely, causing:
- Multiple redundant subscriptions to the same value source
- Each toggle re-subscribing and doubling the event flow
- Eventual performance degradation

**Solution:** Replaced with `stream::select(value_stream, when_stream).scan(...)`
pattern:
- `value_stream`: filters `SetValue(bool)` messages from the pipe input
- `when_stream`: maps each toggle trigger to a `Toggle` message
- `scan`: maintains a single `Option<bool>` state, toggling on `Toggle`
  messages and resetting on `SetValue` messages

This correctly handles:
- Initial value from `LATEST { False, toggle_all_click |> THEN { ... } }`
- Toggle trigger from `Bool/toggle(when: checkbox.event.click)`
- Value updates from toggle-all overriding individual state

### Cleanup: Removed `pointer-events: auto` (Actors Bridge)

**File:** `crates/boon/src/platform/browser/engine_actors/bridge.rs`

Removed unnecessary `pointer-events: auto` from 4 locations (checkbox,
button, text_input, label). These were added as a workaround attempt for
click interception but were unnecessary since no parent sets
`pointer-events: none`.

---

## Regression Test Results

All existing examples tested across all 3 engines — **no regressions found**.

| Example          | Actors | DD              | WASM |
|------------------|--------|-----------------|------|
| counter          | ✅     | ✅              | ✅   |
| interval         | ✅     | ✅              | ✅   |
| fibonacci        | ✅     | ✅              | ✅   |
| hello_world      | ✅     | ✅              | ✅   |
| shopping_list    | ✅     | ❌ pre-existing  | ✅   |
| todo_mvc         | ✅     | ✅              | ✅   |
| todo_mvc_physical| ✅*    | ✅ (static)     | ✅ (static) |

\* Actors `todo_mvc_physical`: all features work except Completed filter (§1).

---

## Priority for Future Work

1. **§1 Completed filter** — Medium priority. Investigate WHILE evaluator
   subscription semantics. Affects both `todo_mvc` and `todo_mvc_physical`.
2. **§2 WASM checkbox** — Low priority. WASM engine has broader multi-file
   limitations; fixing this one symptom won't make the full example work.
3. **§3 DD shopping_list** — Low priority. Separate from todo_mvc_physical.
