# DD Engine Problem List

**Created:** 2026-01-18
**Status:** Active - Problems being fixed one by one

This document lists ALL problems found in the DD engine that violate pure DD architecture.
Each problem includes exact location, how to find it, why it's wrong, and how to fix it.

---

## Table of Contents

1. [Critical: Business Logic Functions](#1-critical-business-logic-functions)
2. [Critical: Hardcoded Field Fallbacks](#2-critical-hardcoded-field-fallbacks)
3. [Critical: Evaluator Side Effects](#3-critical-evaluator-side-effects)
4. [High: O(n) List Operations](#4-high-on-list-operations)
5. [High: DynamicLinkAction Business Logic](#5-high-dynamiclinkaction-business-logic)
6. [Medium: Thread-Local Registries](#6-medium-thread-local-registries)
7. [Medium: Global Atomic Counters](#7-medium-global-atomic-counters)
8. [Medium: String-Based Dispatch](#8-medium-string-based-dispatch)
9. [Low: Unused Infrastructure](#9-low-unused-infrastructure)

---

## 1. Critical: Business Logic Functions

### Problem 1.1: `is_item_completed_generic()`

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/core/worker.rs`
**Lines:** 215-249

**How to find:**
```bash
grep -n "fn is_item_completed_generic" crates/boon/src/platform/browser/engine_dd/core/worker.rs
```

**What it does:**
- Determines if a list item is "completed" by scanning object fields
- Uses string heuristic `field_name.contains("edit")` to skip editing fields
- Reads from IO layer via `get_cell_value()` inside DD transform
- Called from `ListRemoveCompleted` transform

**Why it's wrong:**
1. Encodes domain knowledge (todo app "completion" concept) in engine
2. IO read inside DD transform breaks purity
3. String heuristics are brittle (what if field is "editable"?)
4. Other apps might use "done", "checked", "finished" for same concept

**The code:**
```rust
fn is_item_completed_generic(item: &Value) -> bool {
    match item {
        Value::Object(obj) => {
            if let Some((_, is_completed)) = find_checkbox_toggle_in_item(obj) {
                return is_completed;
            }
            for (field_name, field_value) in obj.iter() {
                if field_name.contains("edit") {  // ❌ String heuristic
                    continue;
                }
                if let Value::CellRef(cell_id) = field_value {
                    if let Some(cell_value) = super::super::io::get_cell_value(&cell_id.name()) {  // ❌ IO read
                        match cell_value {
                            Value::Bool(b) => return b,
                            // ...
                        }
                    }
                }
            }
            false
        }
        _ => false,
    }
}
```

**How to fix:**
1. Remove this function entirely
2. Make completion field explicit in `ListItemTemplate`
3. Add `completion_field: Option<String>` to template config
4. `ListRemoveCompleted` should use configured field, not inference

**Status:** ⬜ TODO

---

### Problem 1.2: `find_checkbox_toggle_in_item()`

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/core/worker.rs`
**Lines:** 176-210

**How to find:**
```bash
grep -n "fn find_checkbox_toggle_in_item" crates/boon/src/platform/browser/engine_dd/core/worker.rs
```

**What it does:**
- Searches object fields for a "checkbox toggle" cell
- Uses `CHECKBOX_TOGGLE_HOLDS` registry to identify toggles
- Reads from IO layer via `get_cell_value()`

**Why it's wrong:**
1. Registry-based field detection is implicit
2. IO read inside worker function
3. Depends on external registration that bypasses DD

**The code:**
```rust
fn find_checkbox_toggle_in_item(obj: &BTreeMap<Arc<str>, Value>) -> Option<(String, bool)> {
    let toggle_holds = get_checkbox_toggle_holds();  // ❌ Registry lookup
    for (_, value) in obj.iter() {
        match value {
            Value::CellRef(cell_id) => {
                let cell_name = cell_id.name();
                if toggle_holds.contains(&cell_name) || cell_id.is_dynamic() {
                    let is_completed = super::super::io::get_cell_value(&cell_name)  // ❌ IO read
                        .map(|v| /* ... */)
                        .unwrap_or(false);
                    return Some((cell_name, is_completed));
                }
            }
            // ...
        }
    }
    None
}
```

**How to fix:**
1. Remove this function
2. Remove `CHECKBOX_TOGGLE_HOLDS` registry
3. Make toggle fields explicit in template config

**Status:** ⬜ TODO

---

### Problem 1.3: `find_boolean_field_in_template()`

**Severity:** 🟠 HIGH
**File:** `crates/boon/src/platform/browser/engine_dd/eval/interpreter.rs`
**Lines:** 1581-1614

**How to find:**
```bash
grep -n "fn find_boolean_field_in_template" crates/boon/src/platform/browser/engine_dd/eval/interpreter.rs
```

**What it does:**
- Scans template to find "the boolean field"
- Assumes first boolean field is the "completed" field
- Reads from IO layer

**Why it's wrong:**
1. Heuristic assumption ("first boolean = completed")
2. What if template has multiple boolean fields?
3. Comment says "This is likely the 'completed' field" - speculation, not design

**How to fix:**
1. Remove this function
2. Require explicit field declaration in Boon code
3. Parse field role from Boon syntax, not infer from type

**Status:** ⬜ TODO

---

### Problem 1.4: `extract_item_key_for_removal()`

**Severity:** 🟠 HIGH
**File:** `crates/boon/src/platform/browser/engine_dd/core/worker.rs`
**Lines:** 609-637

**How to find:**
```bash
grep -n "fn extract_item_key_for_removal" crates/boon/src/platform/browser/engine_dd/core/worker.rs
```

**What it does:**
- Extracts a "key" from an item for identification
- Deep-searches nested objects for CellRef/LinkRef
- Falls back to `.to_display_string()` if nothing found

**Why it's wrong:**
1. Deep nested search is O(depth * fields)
2. Fallback to display string is unreliable for identity
3. Duplicate implementation exists at line 1220

**How to fix:**
1. Items should carry explicit `__key` field
2. Key assigned at instantiation time
3. Remove deep search logic

**Status:** ⬜ TODO

---

## 2. Critical: Hardcoded Field Fallbacks

These violate the **NO FALLBACKS IN IDENTITY/KEY LOGIC** rule from CLAUDE.md.

### Problem 2.1: "completed" fallback at worker.rs:750

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/core/worker.rs`
**Line:** 750

**How to find:**
```bash
grep -n "completed" crates/boon/src/platform/browser/engine_dd/core/worker.rs | grep unwrap_or
```

**The code:**
```rust
let completed_field_name = data_template.as_ref()
    .and_then(|tmpl| find_boolean_field_in_template(tmpl))
    .unwrap_or_else(|| "completed".to_string());  // ❌ FALLBACK
```

**Why it's wrong:**
- If field detection fails, silently uses "completed"
- Shopping list might use "checked", task app might use "done"
- Bug is hidden until runtime in a different app

**How to fix:**
```rust
let completed_field_name = data_template.as_ref()
    .and_then(|tmpl| find_boolean_field_in_template(tmpl))
    .expect("Bug: template must declare completion field explicitly");
```

**Status:** ⬜ TODO

---

### Problem 2.2: "completed" fallback at interpreter.rs:1424

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/eval/interpreter.rs`
**Line:** 1424

**How to find:**
```bash
grep -n "completed" crates/boon/src/platform/browser/engine_dd/eval/interpreter.rs | grep unwrap_or
```

**The code:**
```rust
let completed_field_name = data_template.as_ref()
    .and_then(|tmpl| find_boolean_field_in_template(tmpl))
    .unwrap_or_else(|| "completed".to_string()); // Fallback for legacy compatibility
```

**How to fix:** Same as 2.1 - fail explicitly.

**Status:** ⬜ TODO

---

### Problem 2.3: "title" fallback at worker.rs:2354

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/core/worker.rs`
**Line:** 2354

**How to find:**
```bash
grep -n '"title"' crates/boon/src/platform/browser/engine_dd/core/worker.rs | grep unwrap_or
```

**The code:**
```rust
let title_field = template.field_initializers.iter()
    .find(|(_, init)| matches!(init, FieldInitializer::FromEventText))
    .map(|(path, _)| path.first().cloned().unwrap_or_default())
    .unwrap_or_else(|| "title".to_string());  // ❌ FALLBACK
```

**How to fix:** Fail explicitly if field not found.

**Status:** ⬜ TODO

---

### Problem 2.4: "remove_button" hardcoded identity path

**Severity:** 🟠 HIGH
**File:** `crates/boon/src/platform/browser/engine_dd/core/worker.rs`
**Line:** 2424

**How to find:**
```bash
grep -n "remove_button" crates/boon/src/platform/browser/engine_dd/core/worker.rs
```

**The code:**
```rust
identity: ItemIdentitySpec {
    link_ref_path: vec!["remove_button".to_string()],  // ❌ HARDCODED
},
```

**Why it's wrong:**
- Assumes all apps name their remove button "remove_button"
- Other apps might use "delete", "trash", "x_button"

**How to fix:**
- Parse identity path from Boon code
- Make it part of template configuration

**Status:** ⬜ TODO

---

## 3. Critical: Evaluator Side Effects

The evaluator should ONLY build `DataflowConfig`. Any state mutation is a violation.

### Problem 3.1: init_cell() at evaluator.rs:784

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs`
**Line:** 784

**How to find:**
```bash
grep -n "init_cell" crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs
```

**Context:** Called when evaluating objects with list fields.

**How to fix:** Store initial values in `DataflowConfig.cell_initializations`, let worker init.

**Status:** ⬜ TODO

---

### Problem 3.2: init_cell() at evaluator.rs:1222

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs`
**Line:** 1222

**How to find:**
```bash
grep -n "current_route" crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs
```

**The code:**
```rust
"route" => {
    super::super::io::init_current_route();
    let path = super::super::io::get_current_route();
    init_cell("current_route", Value::text(path));  // ❌ Side effect
    Value::CellRef(CellId::new("current_route"))
}
```

**How to fix:** Router initialization should be in interpreter, not evaluator.

**Status:** ⬜ TODO

---

### Problem 3.3: init_cell() at evaluator.rs:1953

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs`
**Line:** 1953

**Context:** HOLD with link trigger.

**Status:** ⬜ TODO

---

### Problem 3.4: init_cell() at evaluator.rs:3052

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs`
**Line:** 3052

**Context:** LATEST with events.

**Status:** ⬜ TODO

---

### Problem 3.5: init_cell() at evaluator.rs:3329

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs`
**Line:** 3329

**Context:** WHILE with LinkRef.

**Status:** ⬜ TODO

---

### Problem 3.6: add_dynamic_link_action() at evaluator.rs:1972

**Severity:** 🔴 CRITICAL
**File:** `crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs`
**Lines:** 1972-1977

**How to find:**
```bash
grep -n "add_dynamic_link_action" crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs
```

**The code:**
```rust
add_dynamic_link_action(exit_key_link_id.clone(), DynamicLinkAction::SetFalseOnKeys {
    cell_id: cell_id.clone(),
    keys: vec!["Enter".to_string(), "Escape".to_string()],
});
```

**Why it's wrong:** Mutates global IO state during evaluation.

**How to fix:** Store in DataflowConfig, let interpreter register.

**Status:** ⬜ TODO

---

## 4. High: O(n) List Operations

Every `.to_vec()` call defeats O(delta) complexity.

### Problem 4.1-4.8: List Append .to_vec() calls

**Severity:** 🟠 HIGH
**File:** `crates/boon/src/platform/browser/engine_dd/core/worker.rs`

**How to find all:**
```bash
grep -n "\.to_vec()" crates/boon/src/platform/browser/engine_dd/core/worker.rs
```

**Locations:**
| Line | Context |
|------|---------|
| 2281 | List/append text fallback |
| 2479 | List/append elements |
| 2487 | List/append data items |
| 2506 | List/append text items |
| 2671 | List/append elements (dup) |
| 2677 | List/append data items (dup) |
| 3016 | List/append instantiated |
| 3022 | List/append element |

**Why it's wrong:**
- Adding 1 item to 1000-item list costs O(1000)
- Should cost O(1)

**How to fix:**
- Return only the appended item as diff
- Let outputs.rs use `VecDiff::Push`

**Status:** ⬜ TODO

---

### Problem 4.9-4.14: List Remove .to_vec() calls

**Severity:** 🟠 HIGH
**File:** `crates/boon/src/platform/browser/engine_dd/core/worker.rs`

**Locations:**
| Line | Context |
|------|---------|
| 2714 | Filter elements |
| 2789 | RemoveListItem |
| 2796 | Remove element |
| 2850 | Remove matched element |
| 2929 | Toggle remove |
| 2979 | Toggle remove element |

**How to fix:**
- Track indices in DD
- Return removal index, not full list
- Use `VecDiff::RemoveAt`

**Status:** ⬜ TODO

---

## 5. High: DynamicLinkAction Business Logic

### Problem 5.1: EditingHandler variant

**Severity:** 🟠 HIGH
**File:** `crates/boon/src/platform/browser/engine_dd/io/inputs.rs`
**Line:** 22

**How to find:**
```bash
grep -n "EditingHandler" crates/boon/src/platform/browser/engine_dd/io/inputs.rs
```

**The code:**
```rust
EditingHandler { editing_cell: String, title_cell: String },
```

**Why it's wrong:**
- Encodes todo_mvc editing workflow (double-click → edit mode)
- Field names "editing_cell", "title_cell" are app-specific
- Should be generic DD operators

**How to fix:**
- Remove this variant
- Use generic `LinkAction::SetFalse` with key filter
- Let Boon code define editing behavior

**Status:** ⬜ TODO

---

### Problem 5.2: ListToggleAllCompleted variant

**Severity:** 🟠 HIGH
**File:** `crates/boon/src/platform/browser/engine_dd/io/inputs.rs`
**Line:** 30

**How to find:**
```bash
grep -n "ListToggleAllCompleted" crates/boon/src/platform/browser/engine_dd/io/inputs.rs
```

**The code:**
```rust
ListToggleAllCompleted {
    list_cell_id: String,
    completed_field: String,  // ❌ App-specific
},
```

**Why it's wrong:**
- "Toggle all completed" is todo_mvc feature
- Other apps don't have this concept
- `completed_field` hardcodes the name

**How to fix:**
- Remove this variant
- Implement as generic "map all items" DD operator
- Let Boon code define the transformation

**Status:** ⬜ TODO

---

## 6. Medium: Thread-Local Registries

### Problem 6.1: CHECKBOX_TOGGLE_HOLDS

**Severity:** 🟡 MEDIUM
**File:** `crates/boon/src/platform/browser/engine_dd/io/outputs.rs`
**Line:** 621

**How to find:**
```bash
grep -n "CHECKBOX_TOGGLE_HOLDS" crates/boon/src/platform/browser/engine_dd/io/outputs.rs
```

**The code:**
```rust
thread_local! {
    static CHECKBOX_TOGGLE_HOLDS: Mutable<Vec<String>> = Mutable::new(Vec::new());
}
```

**Why it's wrong:**
- "Checkbox toggle" is app-specific concept
- Used by `find_checkbox_toggle_in_item()` (Problem 1.2)
- Bypasses DD configuration

**How to fix:**
- Remove this registry
- Add toggle field info to `DataflowConfig`
- Remove `find_checkbox_toggle_in_item()`

**Status:** ⬜ TODO

---

### Problem 6.2: DYNAMIC_LINK_ACTIONS

**Severity:** 🟡 MEDIUM
**File:** `crates/boon/src/platform/browser/engine_dd/io/inputs.rs`
**Line:** 59

**How to find:**
```bash
grep -n "DYNAMIC_LINK_ACTIONS" crates/boon/src/platform/browser/engine_dd/io/inputs.rs
```

**Why it's wrong:**
- Bypasses DD input channel
- Registration happens outside DD visibility
- `get_all_link_mappings()` bridge perpetuates the pattern

**How to fix:**
- Remove this registry
- All link mappings in `DataflowConfig`
- Remove `add_dynamic_link_action()` function

**Status:** ⬜ TODO

---

## 7. Medium: Global Atomic Counters

### Problem 7.1: GLOBAL_CELL_COUNTER

**Severity:** 🟡 MEDIUM
**File:** `crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs`
**Line:** 17

**How to find:**
```bash
grep -n "GLOBAL_CELL_COUNTER" crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs
```

**The code:**
```rust
static GLOBAL_CELL_COUNTER: AtomicU32 = AtomicU32::new(0);
```

**Why it's wrong:**
- Non-deterministic across runs (flaky tests)
- `BoonDdRuntime` has instance counters that aren't used
- Global state persists across evaluations

**How to fix:**
- Use `BoonDdRuntime.cell_counter` instead
- Remove global atomic
- Counter resets with each runtime instance

**Status:** ⬜ TODO

---

### Problem 7.2: GLOBAL_LINK_COUNTER

**Severity:** 🟡 MEDIUM
**File:** `crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs`
**Line:** 20

**How to find:**
```bash
grep -n "GLOBAL_LINK_COUNTER" crates/boon/src/platform/browser/engine_dd/eval/evaluator.rs
```

**Same issue as 7.1.**

**Status:** ⬜ TODO

---

## 8. Medium: String-Based Dispatch

### Problem 8.1: "Escape" string in key filter

**Severity:** 🟡 MEDIUM
**File:** `crates/boon/src/platform/browser/engine_dd/io/inputs.rs`
**Line:** 220

**How to find:**
```bash
grep -n '"Escape"' crates/boon/src/platform/browser/engine_dd/io/inputs.rs
```

**The code:**
```rust
vec!["Escape".to_string()]
```

**Why it's wrong:**
- Magic string, no compile-time check
- Typo "Esacpe" would silently fail

**How to fix:**
- Use enum `Key::Escape`
- Or at least const: `const KEY_ESCAPE: &str = "Escape";`

**Status:** ⬜ TODO

---

### Problem 8.2: "Enter:text" protocol

**Severity:** 🟡 MEDIUM
**File:** `crates/boon/src/platform/browser/engine_dd/io/inputs.rs`
**Lines:** 278-279

**How to find:**
```bash
grep -n "Enter:" crates/boon/src/platform/browser/engine_dd/io/inputs.rs
```

**Why it's wrong:**
- Text encoding protocol in IO layer
- Should use `EventPayload::Enter(String)` which already exists

**Status:** ⬜ TODO

---

### Problem 8.3: "hover_" prefix convention

**Severity:** 🟡 MEDIUM
**File:** `crates/boon/src/platform/browser/engine_dd/io/inputs.rs`
**Line:** 304

**How to find:**
```bash
grep -n 'hover_' crates/boon/src/platform/browser/engine_dd/io/inputs.rs
```

**The code:**
```rust
let hover_cell_id = format!("hover_{}", link_id);
```

**Why it's wrong:**
- IO layer invents DD cell naming convention
- Should be explicit in DataflowConfig

**Status:** ⬜ TODO

---

## 9. Low: Unused Infrastructure

### Problem 9.1: VecDiff adapter not wired to bridge

**Severity:** 🟢 LOW
**File:** `crates/boon/src/platform/browser/engine_dd/render/list_adapter.rs`

**How to find:**
```bash
grep -rn "dd_diffs_to_vec_diff_stream" crates/boon/src/platform/browser/engine_dd/render/
```

**Issue:**
- `dd_diffs_to_vec_diff_stream()` exists
- `bridge.rs` never imports or uses it
- Bridge still does `.children(items.collect())`

**How to fix:**
- Import adapter in bridge.rs
- Use `children_signal_vec()` with adapter

**Status:** ⬜ TODO

---

## Summary

| Category | Count | Fully Fixed | Partial | Remaining | Severity |
|----------|-------|-------------|---------|-----------|----------|
| Business Logic Functions | 4 | 2 | 1 | 1 | 🔴 CRITICAL |
| Hardcoded Fallbacks | 4 | 3 | 1 | 0 | 🟢 MOSTLY DONE |
| Evaluator Side Effects | 6 | 0 | 0 | 6 | 🔴 CRITICAL |
| O(n) List Operations | 14 | 0 | 0 | 14 | 🟠 HIGH |
| DynamicLinkAction | 2 | 0 | 0 | 2 | 🟠 HIGH |
| Thread-Local Registries | 2 | 1 | 0 | 1 | 🟡 MEDIUM |
| Global Counters | 2 | 0 | 0 | 2 | 🟡 MEDIUM (deferred) |
| String-Based Dispatch | 3 | 0 | 0 | 3 | 🟡 MEDIUM |
| Unused Infrastructure | 1 | 0 | 0 | 1 | 🟢 LOW |
| **TOTAL** | **38** | **6** | **2** | **30** | |

---

## Progress Tracking

### ✅ FULLY COMPLETED (2026-01-18)

- [x] Problem 1.1: `is_item_completed_generic()` - **DELETED** (replaced by `is_item_field_true()` with explicit field name)
- [x] Problem 1.2: `find_checkbox_toggle_in_item()` - **DELETED** (was unused after 1.1 removal)
- [x] Problem 2.1: "completed" fallback worker.rs - **FIXED** (now Option pattern, skips if not found)
- [x] Problem 2.2: "completed" fallback interpreter.rs - **FIXED** (now Option pattern, skips if not found)
- [x] Problem 2.3: "title" fallback worker.rs - **FIXED** (returns None instead of fallback)
- [x] Problem 6.1: CHECKBOX_TOGGLE_HOLDS - **DELETED** (registry was dead code - set but never read)

### 🟡 PARTIALLY FIXED (2026-01-18)

- [~] Problem 1.3: `find_boolean_field_in_template()` - **FALLBACKS REMOVED** but function still has IO reads
- [~] Problem 2.4: "remove_button" hardcoded - **WARNING ADDED** but string still hardcoded

### ⏸️ DEFERRED (need larger refactors)

- [ ] Problem 1.4: `extract_item_key_for_removal()` - needs template-based key extraction
- [ ] Problem 3.1-3.6: init_cell() in evaluator - needs DataflowConfig.cell_initializations
- [ ] Problem 7.1-7.2: Global counters - needs counter propagation through sub-runtimes

### ⬜ TODO

- [ ] Problem 4.1-4.8: List Append .to_vec() (8 locations)
- [ ] Problem 4.9-4.14: List Remove .to_vec() (6 locations)
- [ ] Problem 5.1: EditingHandler variant
- [ ] Problem 5.2: ListToggleAllCompleted variant
- [ ] Problem 6.2: DYNAMIC_LINK_ACTIONS registry
- [ ] Problem 8.1: "Escape" string dispatch
- [ ] Problem 8.2: "Enter:text" protocol
- [ ] Problem 8.3: "hover_" prefix
- [ ] Problem 9.1: VecDiff adapter not wired
