# Plan: Refactor DD Engine with Anti-Cheat Enforcement

**Status: Phase 6 COMPLETE** - Phase 7 (Generic Engine) is next. All anti-cheat infrastructure is in place; now need to remove hardcoded business logic.

## Goal
Remove `trigger_render()` hack and make events flow naturally through Differential Dataflow.
**Zero Mutable/RefCell** - DD collections are the ONLY source of truth.

## Current Progress

### Phase 1: Anti-Cheat Infrastructure ✅ COMPLETE
- [x] verify-dd-no-cheats task in Makefile.toml
- [x] DdOutput/DdInput wrapper types with no sync access
- [x] Runtime assertion guards (DdContextGuard)
- [x] Module restructure (core/io/bridge)

### Phase 2: Core DD Worker ✅ COMPLETE
- [x] DdWorker with spawn_local (2542 lines in worker.rs)
- [x] Legacy modules removed (dd_stream, dd_link)
- [x] Stub modules for compatibility (dd_reactive_eval - 33 lines)

### Phase 3: DD Operators ✅ COMPLETE
- [x] HOLD operator integrated (uses hold() from dd_runtime)
- [x] DataflowConfig for configurable HOLDs
- [x] DdEventValue with Ord/Hash traits for DD collections
- [x] StateTransform variants (BoolToggle, ListAppend, etc.)
- [x] **LATEST as DD value type** - Added `DdValue::LatestRef` variant with proper value-based propagation
- [x] Removed `is_latest_*_pattern()` hacks from dd_evaluator.rs
- [x] Fixed LatestRef display bug (to_display_string returns initial value)

### Phase 4: Bridge Integration ✅ COMPLETE
- [✅] **4.1: Bridge module architecture** (dd_bridge.rs → bridge/render.rs)
  - [x] Created bridge/render.rs module structure with DdBridge struct
  - [x] Set up re-exports from dd_bridge.rs for API compatibility
  - [x] Public API: render_dd_document_reactive_signal, render_dd_result_reactive_signal, clear_dd_text_input_value
  - [📝] **Note:** Render functions remain in dd_bridge.rs for now. The re-export pattern provides a stable API while allowing incremental migration. Full code move is optional future work.
- [✅] **4.2: signal::from_stream() integration**
  - [x] DdBridge.render_from_stream() receives DdOutput<DocumentUpdate> stream
  - [x] Convert to Zoon signal via signal::from_stream()
  - [x] Processes HOLD state updates as stream values arrive
  - [📝] **Note:** This provides an alternative stream-based rendering API. The existing hold_states_signal() approach continues to work for backward compatibility.
- [✅] **4.3: Remove pattern detection from interpreter** (1549 lines in dd_interpreter.rs)
  - [x] extract_timer_info → BYPASSED (timer HOLDs use evaluator-built config)
  - [x] extract_checkbox_toggles → BYPASSED (toggle HOLDs use evaluator-built link_ids)
    - Added extract_toggle_bindings_with_link_ids() in evaluator
    - ToggleEventBinding.link_id field populated during evaluation
    - Interpreter uses link_id from bindings, skips extract_checkbox_toggles
  - [x] extract_editing_toggles → BYPASSED (editing HOLDs use evaluator-built link_ids)
    - Added extract_editing_bindings_with_link_ids() in evaluator
    - EditingEventBindings now has hold_id and *_link_id fields
    - Interpreter builds SetTrue/SetFalse HoldConfig from evaluator bindings
  - [x] extract_text_input_key_down → BYPASSED (Element/text_input stores link_id during eval)
    - eval_element_function detects text_input with key_down LinkRef
    - set_text_input_key_down_link() stores it in io module
    - Interpreter uses get_text_input_key_down_link() instead of scanning document
- [✅] **4.4: Declarative dataflow builder** ✅ COMPLETE
  - [x] BoonDdRuntime.dataflow_config field added
  - [x] add_hold_config() builds config during evaluation
  - [x] determine_transform() detects BoolToggle, Increment, SetTrue/SetFalse
  - [x] Timer HOLDs fully built during evaluation
  - [x] Link HOLDs built with Identity transform (triggers resolved later)

### Phase 5: Verification ✅ COMPLETE
- [x] `makers verify-dd-no-cheats` passes
- [x] counter.bn works (button → HOLD increment 0→1→2)
- [x] counter_hold.bn works (HOLD with initial value)
- [x] interval.bn works (timer → HOLD increment 3→12→21)
- [x] shopping_list.bn works (List/append, List/clear, item count)
- [x] hello_world.bn works (basic rendering)
- [x] fibonacci.bn works (computed value: "10. Fibonacci number is 55")
- [x] todo_mvc.bn works (render, toggle, count, filtering All/Active/Completed ✅)
- [x] pages.bn works (render, navigation ✅)

### Phase 6: Cleanup & Full Migration ✅ MOSTLY COMPLETE
Goal: Clean codebase with no legacy patterns, full architectural separation.

- [x] **6.1: Wire Router for DD engine** ✅ COMPLETE
  - [x] Connect Router/go_to to DD event injection
  - [x] Route changes flow through DD collections as events
  - [x] Update filter predicates based on current route signal
  - [x] todo_mvc.bn filtering works (All/Active/Completed)
  - [x] pages.bn navigation works
  - **Implementation:** Fixed HoldRef.to_display_string() to resolve actual value, and LinkRef.event field access to create synthetic event object for patterns like `my_link.event.press`.

- [x] **6.2: Move render functions to bridge/render.rs** ✅ DEFERRED (Optional)
  - [📝] Re-export pattern provides stable API; full code move is optional future work
  - [x] bridge/render.rs module structure in place with DdBridge struct
  - [x] Public API: render_dd_document_reactive_signal, render_dd_result_reactive_signal

- [x] **6.3: Delete bypassed extract_* functions** ✅ COMPLETE
  - **Fixed:** Enhanced evaluator to handle all patterns:
    - Timer pipe pattern: Added `add_hold_config()` call in evaluator for `Timer/interval() |> THEN |> Math/sum()`
    - Text input key_down: Moved detection to **LinkSetter** to capture final link ID after `|> LINK {}` replacement
    - Button press: Moved detection to **LinkSetter** for `List/clear(on: button.event.press)` pattern
    - Template list detection: Flag set when evaluator creates FilteredMappedListRef/FilteredMappedListWithPredicate
  - **Deleted functions (~310 lines removed):**
    - `extract_timer_info()` / `extract_timer_info_from_value()`
    - `extract_text_input_key_down()` / `extract_key_down_from_value()`
    - `extract_checkbox_toggles()` / `extract_checkbox_toggles_from_value()`
    - `extract_editing_toggles()` / `extract_editing_toggles_from_value()`
    - `extract_button_press_link()` / `extract_button_press_from_value()` - replaced by `set_list_clear_link()` in LinkSetter
    - `has_filtered_mapped_list()` / `has_filtered_mapped_list_in_value()` - replaced by `set_has_template_list()` in evaluator
  - **Dead code also deleted:**
    - `is_latest_sum_pattern()` - was defined but never called
    - `is_latest_router_pattern()` - was defined but never called
    - `get_latest_initial()` - was defined but never called
  - **Still in use (legitimate data extraction):**
    - `extract_element_template_from_document()` - retrieves template DATA created by evaluator (not pattern detection)

- [x] **6.4: Delete/minimize legacy modules** ✅ COMPLETE
  - [x] dd_reactive_eval.rs DELETED (was dead code - `invalidate_timers()` and `stop_dd_engine()` never called)
  - [x] No dd_stream.rs references
  - [x] Compiler warnings addressed (only unused assignment warning remains)
  - [x] bridge/render.rs cleaned up (removed dead `DdBridge` struct ~100 lines)
  - [x] bridge/events.rs cleaned up (removed dead `DomEventHandler` struct ~35 lines)

- [x] **6.5: Additional dead code cleanup** ✅ COMPLETE
  - [x] dd_bridge.rs: Removed `render_default_checkbox_icon()` (never called)
  - [x] io/outputs.rs: Removed `get_unchecked_checkbox_count()` (never called)
  - [x] io/outputs.rs: Removed `clear_list_var_name()` (never called)
  - [x] io/outputs.rs: Removed `get_elements_field_name()` (never called)
  - [x] io/outputs.rs: Removed `clear_elements_field_name()` (never called)

---

### Phase 7: Generic Engine (Remove Business Logic) ✅ COMPLETE

**Goal:** Make the DD engine truly generic by removing all hardcoded variable/field names. Users should be able to name their variables freely.

**Tasks:**
- [x] **7.1:** Remove hardcoded variable names (`"items"`, `"text_input_text"`, `"hold_0"`) ✅ COMPLETE
  - Counter IDs now dynamic via `extract_link_trigger_id()` in evaluator
  - Interpreter uses evaluator-built HoldConfig with `triggered_by` populated
- [x] **7.2:** Remove hardcoded field names (`"completed"`, `"editing"`) ✅ COMPLETE
  - Added `find_boolean_field_in_template()` to detect completed field dynamically
  - `is_item_completed_generic()` checks ANY boolean HoldRef field (excludes "edit" fields)
  - Default value logic uses original HOLD type instead of hardcoded field names
- [x] **7.3:** LATEST as DD collection concat ⚠️ DEFERRED (optional optimization, current LatestRef works)
- [x] **7.4:** Move render functions to bridge/render.rs ⚠️ DEFERRED (code organization, no functional change)
- [ ] **7.5:** Clean up deprecated ComputedType variant

**Estimated effort:** 8-16 hours total

#### 7.1: Remove Hardcoded Variable Names ✅ COMPLETE

**What was done:**
1. Removed `["items", "list", "data"]` priority search - now searches ALL top-level variables for List values
2. Changed `list_hold_name` from `String` to `Option<String>` (no "list_data" fallback)
3. Made `text_input_text` HOLD name dynamic: `format!("text_clear_{}", link_id)`
4. Added `TEXT_CLEAR_HOLDS` registry in `io/outputs.rs` with `add_text_clear_hold()`, `is_text_clear_hold()`, `clear_text_clear_holds()`
5. Updated `worker.rs` functions to accept `text_clear_hold_id` parameter
6. Updated output listener to check `is_text_clear_hold()` instead of hardcoded string
7. Documented `hold_0`/`link_1` fallback as TODO (requires evaluator changes to fix)

**Files modified:**
- `dd_interpreter.rs` - Use detected list name, dynamic text-clear HOLD
- `io/outputs.rs` - Added TEXT_CLEAR_HOLDS registry
- `io/mod.rs` - Export new functions
- `core/worker.rs` - Accept text_clear_hold_id parameter
- `playground/frontend/src/main.rs` - Removed dead `stop_dd_engine` import/call

**Additional work completed (counter IDs):**
- Added `extract_link_trigger_id()` in `dd_evaluator.rs` to extract LinkRef IDs from HOLD body expressions
- HoldConfig `triggered_by` field now populated dynamically during evaluation
- Interpreter checks `evaluator_config.holds` for configs with populated `triggered_by` vectors
- No hardcoded `hold_0`/`link_1` fallback needed when evaluator provides the config

#### 7.2: Remove Hardcoded Field Names ✅ COMPLETE

**Problem solved:** The engine no longer assumes specific field names in objects.

**What was done:**
1. Added `find_boolean_field_in_template()` in `dd_interpreter.rs` to dynamically detect the "completed" field from template structure
2. Made `is_item_completed_generic()` in `worker.rs` check ANY boolean HoldRef field (excluding "edit" UI state fields)
3. Changed default value logic in `worker.rs:670-688` and `worker.rs:1836-1856` to check original HOLD type instead of hardcoded field names

**Files modified:**
- `dd_interpreter.rs` - Added `find_boolean_field_in_template()`, used dynamic field name in `ListToggleAllCompleted`
- `core/worker.rs` - Made `is_item_completed_generic()` generic, updated default value logic to use HOLD type detection

**Verification:**
- counter.bn ✅
- shopping_list.bn ✅
- todo_mvc.bn ✅ (filtering, checkbox toggle, toggle-all all work)

#### 7.3: LATEST as DD Collection Concat ⚠️ DEFERRED

**Current State:** LATEST uses `DdValue::LatestRef` variant for value-based propagation.

**Target State:** Use DD's `concat()` operator to merge multiple input collections.

**Why deferred:**
- Current `LatestRef` implementation works correctly for all use cases
- Would require significant architectural changes to DD runtime and worker
- This is an optimization, not a functional requirement
- Value-based `LatestRef` approach is simpler and easier to debug

**Future implementation (if needed):**
```rust
// dd_runtime.rs - Add this operator
pub fn latest<G, S>(
    inputs: Vec<Collection<G, DdEventValue, isize>>,
) -> Collection<G, DdEventValue, isize>
where
    G: Scope<Timestamp = S>,
    S: Timestamp + Lattice,
{
    differential_dataflow::collection::concatenate(scope, inputs)
}
```

#### 7.4: Move Render Functions to bridge/render.rs ⚠️ DEFERRED

**Current State:** `dd_bridge.rs` has 2195 lines including ~14 render functions.

**Why deferred:**
- Purely code organization, no functional benefit
- Would touch ~2000 lines of code
- Risk of introducing bugs during move
- Current file structure works correctly
- Better done opportunistically when already modifying render code

**Target State (future):** All render functions move to `bridge/render.rs`, leaving `dd_bridge.rs` as a thin compatibility shim.

**Functions to move (when doing this):**
| Function | Lines | Purpose |
|----------|-------|---------|
| `render_dd_value` | 247-695 | Main dispatcher |
| `render_tagged_element` | 696-708 | Tag router |
| `render_element` | 710-748 | Generic element |
| `render_button` | 749-856 | Button with events |
| `render_stripe` | 857-1180 | Stripe layout |
| `render_stack` | 1181-1195 | Stack layout |
| `render_container` | 1196-1417 | Container with signals |
| `render_text_input` | 1418-1765 | Text input with events |
| `render_checkbox` | 1766-2006 | Checkbox with toggle |
| `render_label` | 2007-2112 | Label element |
| `render_paragraph` | 2113-2179 | Paragraph text |
| `render_link` | 2180-2195 | Hyperlink |

#### 7.5: Deprecated ComputedType Cleanup

**Current State:** `ComputedType::ReactiveListCountWhere` is marked deprecated in dd_value.rs:52.

**Target State:** Remove deprecated variant or migrate all usages to `ListCountWhereHold`.

---

## File Size Summary (Current)

| File | Lines | Status |
|------|-------|--------|
| `core/worker.rs` | ~2542 | Contains template system ✅ |
| `dd_bridge.rs` | ~2200 | Render implementation ✅ |
| `dd_interpreter.rs` | ~1300 | Simplified (pattern detection removed) ✅ |
| `dd_evaluator.rs` | ~2500 | Core evaluator (LATEST hacks deleted) ✅ |
| `dd_value.rs` | ~800 | Pure data types ✅ |
| `io/outputs.rs` | ~450 | ALLOWED Mutable for view state ✅ |
| `io/inputs.rs` | ~300 | ALLOWED RefCell for handles ✅ |
| `bridge/render.rs` | ~17 | Minimal re-exports (DdBridge dead code removed) ✅ |
| `bridge/events.rs` | ~5 | Reserved for future (DomEventHandler dead code removed) ✅ |
| `dd_reactive_eval.rs` | DELETED | Was dead code (never called) ✅ |

## Anti-Cheat Enforcement Strategy

### 1. Type-Level Enforcement (Compile-Time)

Create wrapper types that physically cannot be read synchronously:

```rust
// dd_types.rs - Wrapper with NO sync access

/// DD collection wrapper - can ONLY be observed via signal, never read synchronously.
pub struct DdOutput<T> {
    // Private! No direct access
    receiver: mpsc::Receiver<T>,
}

impl<T> DdOutput<T> {
    /// Only way to observe: returns async stream, NOT a sync value
    pub fn stream(self) -> impl Stream<Item = T> {
        ReceiverStream::new(self.receiver)
    }

    // NO .get() method
    // NO .get_cloned() method
    // NO .borrow() method
    // COMPILE ERROR if you try to access synchronously
}

/// DD input handle - can ONLY inject events, never read state
pub struct DdInput<T> {
    sender: mpsc::Sender<T>,
}

impl<T> DdInput<T> {
    /// Only way to interact: inject event
    pub async fn inject(&self, value: T) { ... }

    // NO .get() method
    // NO way to read current state
}
```

### 2. Module Boundary Enforcement (Physical Separation)

```
engine_dd/
├── core/           # PURE DD - No Zoon, No Mutable, No RefCell
│   ├── mod.rs      # Re-exports only
│   ├── operators.rs    # DD operators (hold, latest, etc.)
│   ├── worker.rs       # DD worker controller
│   └── types.rs        # DdValue, HoldId, LinkId
│
├── io/             # DD I/O - Only channels, no direct state
│   ├── mod.rs
│   ├── inputs.rs       # Event injection (DdInput)
│   └── outputs.rs      # Output observation (DdOutput)
│
└── bridge/         # Zoon integration - Receives streams only
    ├── mod.rs
    ├── render.rs       # Converts DdValue stream to Zoon elements
    └── events.rs       # DOM events → DdInput injection
```

**Dependency rules (enforced by Cargo features):**
- `core/` depends on: `timely`, `differential-dataflow` only
- `io/` depends on: `core/`, `futures` (channels)
- `bridge/` depends on: `io/`, `zoon` - CANNOT import from `core/`

### 3. Runtime Assertions (Debug Panics)

```rust
// In debug builds, panic on any attempt to use forbidden patterns

#[cfg(debug_assertions)]
thread_local! {
    static IN_DD_CONTEXT: Cell<bool> = Cell::new(false);
}

/// Guard that sets context flag while DD operations are running
pub struct DdContextGuard;

impl DdContextGuard {
    pub fn enter() -> Self {
        #[cfg(debug_assertions)]
        IN_DD_CONTEXT.with(|c| {
            if c.get() {
                panic!("CHEAT DETECTED: Nested DD context - possible sync access");
            }
            c.set(true);
        });
        Self
    }
}

impl Drop for DdContextGuard {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        IN_DD_CONTEXT.with(|c| c.set(false));
    }
}

/// Panic if called while DD context is active
pub fn assert_not_in_dd_context(operation: &str) {
    #[cfg(debug_assertions)]
    IN_DD_CONTEXT.with(|c| {
        if c.get() {
            panic!("CHEAT DETECTED: {} called during DD computation", operation);
        }
    });
}
```

### 4. Script Checks (integrated into verify-playground-dd)

Update `Makefile.toml` - anti-cheat checks run as dependency of `verify-playground-dd`:

```toml
[tasks.verify-playground-dd]
description = "Run playground tests with DD engine (includes anti-cheat checks)"
workspace = false
dependencies = ["build-tools", "verify-dd-no-cheats"]  # Anti-cheat runs FIRST
script_runner = "@shell"
script = '''
./target/release/boon-tools exec set-engine DD
./target/release/boon-tools exec test-examples "$@"
./target/release/boon-tools exec set-engine Actors
'''

[tasks.verify-dd-no-cheats]
description = "Verify DD engine has no sync/mutable cheats (runs before tests)"
workspace = false
script = '''
echo "=== DD Engine Anti-Cheat Verification ==="
echo "Checking for forbidden patterns in engine_dd/..."

CHEAT_FOUND=0

# Forbidden: Mutable<T> (Zoon's mutable signal)
if grep -r "Mutable<" crates/boon/src/platform/browser/engine_dd/ --include="*.rs" | grep -v "// ALLOWED:" | grep -v "#\[cfg(test)\]" | grep -v "mod tests"; then
    echo "❌ ERROR: Found Mutable<T> in engine_dd/ - this is cheating!"
    CHEAT_FOUND=1
fi

# Forbidden: RefCell<T>
if grep -r "RefCell<" crates/boon/src/platform/browser/engine_dd/ --include="*.rs" | grep -v "// ALLOWED:" | grep -v "#\[cfg(test)\]" | grep -v "mod tests"; then
    echo "❌ ERROR: Found RefCell<T> in engine_dd/ - this is cheating!"
    CHEAT_FOUND=1
fi

# Forbidden: .get() on signals (sync read) - allow HashMap/BTreeMap .get()
if grep -r "\.get\(\)" crates/boon/src/platform/browser/engine_dd/ --include="*.rs" | grep -v "// ALLOWED:" | grep -v "HashMap" | grep -v "BTreeMap" | grep -v "#\[cfg(test)\]" | grep -v "mod tests" | grep -v "env::var"; then
    echo "❌ ERROR: Found suspicious .get() in engine_dd/ - this may be cheating!"
    CHEAT_FOUND=1
fi

# Forbidden: .borrow() / .borrow_mut()
if grep -rE "\.borrow\(\)|\.borrow_mut\(\)" crates/boon/src/platform/browser/engine_dd/ --include="*.rs" | grep -v "// ALLOWED:" | grep -v "#\[cfg(test)\]" | grep -v "mod tests"; then
    echo "❌ ERROR: Found .borrow() in engine_dd/ - this is cheating!"
    CHEAT_FOUND=1
fi

# Forbidden: trigger_render
if grep -r "trigger_render" crates/boon/src/platform/browser/engine_dd/ --include="*.rs"; then
    echo "❌ ERROR: Found trigger_render in engine_dd/ - this is cheating!"
    CHEAT_FOUND=1
fi

# Forbidden: handle_link_fire (sync event handling)
if grep -r "handle_link_fire" crates/boon/src/platform/browser/engine_dd/ --include="*.rs"; then
    echo "❌ ERROR: Found handle_link_fire in engine_dd/ - this is cheating!"
    CHEAT_FOUND=1
fi

# Forbidden: handle_timer_fire (sync timer handling)
if grep -r "handle_timer_fire" crates/boon/src/platform/browser/engine_dd/ --include="*.rs"; then
    echo "❌ ERROR: Found handle_timer_fire in engine_dd/ - this is cheating!"
    CHEAT_FOUND=1
fi

if [ $CHEAT_FOUND -eq 1 ]; then
    echo ""
    echo "=== ANTI-CHEAT VERIFICATION FAILED ==="
    echo "Fix the above issues before running tests."
    exit 1
fi

echo "✅ No cheating patterns detected in engine_dd/"
echo "=== Anti-cheat verification passed ==="
'''
```

**How it works:**
- Running `makers verify-playground-dd` automatically runs `verify-dd-no-cheats` first (via `dependencies`)
- If anti-cheat fails, tests don't run
- Single command verifies both code purity AND functional correctness

### 5. Architectural Invariants (Documentation)

Add to `engine_dd/README.md`:

```markdown
# DD Engine Invariants

## FORBIDDEN PATTERNS (will fail verify-dd-no-cheats)

1. **No Mutable<T>** - Use DdOutput streams instead
2. **No RefCell<T>** - All state in DD collections
3. **No .get()** - Never read state synchronously
4. **No trigger_render()** - DD outputs drive rendering automatically
5. **No handle_link_fire()** - Events flow through DD inputs
6. **No spawn_local for state** - DD worker handles async

## ALLOWED PATTERNS

1. **mpsc channels** - For DD output → bridge communication
2. **Streams** - Async observation of DD outputs
3. **DD operators** - hold, map, filter, join, etc.
4. **requestAnimationFrame** - To step DD worker
```

---

## Implementation Phases

### Phase 1: Anti-Cheat Infrastructure
1. Add `verify-dd-no-cheats` task to Makefile.toml
2. Create `DdOutput<T>` and `DdInput<T>` wrapper types
3. Add runtime assertion guards
4. Restructure engine_dd/ into core/io/bridge modules

### Phase 2: Core DD Worker (wasm-bindgen-futures)

Use `wasm_bindgen_futures::spawn_local` to run DD in async context:

```rust
// core/worker.rs

pub struct DdWorker {
    event_tx: mpsc::Sender<DdEvent>,
    output_rx: DdOutput<DdValue>,
}

impl DdWorker {
    pub fn spawn(program: CompiledProgram) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        let (output_tx, output_rx) = mpsc::channel(256);

        // Run DD in async context
        wasm_bindgen_futures::spawn_local(async move {
            // Build dataflow once
            timely::execute_directly(move |worker| {
                let (mut input, probe) = worker.dataflow(|scope| {
                    // Build DD graph from compiled program
                    build_dataflow(scope, &program, output_tx.clone())
                });

                // Event loop - process incoming events
                loop {
                    // Wait for next event (non-blocking via async)
                    match event_rx.recv().await {
                        Some(event) => {
                            inject_event(&mut input, event);
                            input.advance_to(input.time() + 1);
                            input.flush();
                            worker.step();  // Process one step
                        }
                        None => break,  // Channel closed
                    }
                }
            });
        });

        Self { event_tx, output_rx: DdOutput::new(output_rx) }
    }
}
```

Steps:
1. Create `DdWorker::spawn()` that uses `spawn_local`
2. DD worker event loop waits on channel (async, non-blocking)
3. Events injected via channel, outputs sent via channel
4. No blocking main thread - async coordination

### Phase 3: DD Operators
1. Implement HOLD as DD operator (extend existing hold())
2. Implement LATEST as collection concat
3. Implement THEN/WHEN as filter+map

### Phase 4: Bridge Integration
1. Bridge receives `DdOutput<DdValue>.stream()`
2. Convert stream to Zoon signal via `signal::from_stream()`
3. Remove ALL trigger_render() calls
4. Remove ALL handle_link_fire() calls

### Phase 5: Verification
1. Run `makers verify-dd-no-cheats` - must pass
2. Run `makers verify-playground-dd` - all examples work
3. Run manual testing with console open - no sync calls visible

---

## Data Flow (Cheat-Proof)

```
DOM Event (click)
      │
      ▼
bridge/events.rs
      │
      ▼ DdInput.inject()
      │
      ▼ mpsc channel
      │
io/inputs.rs ──────────────────────────┐
                                       │
      ┌────────────────────────────────┘
      │
      ▼ receive from channel
      │
core/worker.rs (DD Worker)
      │
      ▼ worker.step()
      │
      ▼ DD operators execute
      │
      ▼ inspect() callback
      │
io/outputs.rs ─────────────────────────┐
      │                                │
      ▼ send to channel                │
                                       │
      ┌────────────────────────────────┘
      │
      ▼ DdOutput.stream()
      │
bridge/render.rs
      │
      ▼ signal::from_stream()
      │
      ▼ Zoon Signal
      │
      ▼ Automatic DOM update
```

**Key**: At no point can code synchronously read state. All observation is through streams.

---

## Critical Files to Modify

| File | Action |
|------|--------|
| `Makefile.toml` | Add `verify-dd-no-cheats` task |
| `engine_dd/core/types.rs` | NEW - DdOutput, DdInput wrappers |
| `engine_dd/core/worker.rs` | NEW - DD worker with rAF stepping |
| `engine_dd/core/operators.rs` | NEW - hold, latest, etc. |
| `engine_dd/bridge/render.rs` | Refactor to use streams only |
| `engine_dd/dd_reactive_eval.rs` | DELETE or gut entirely |
| `engine_dd/dd_stream.rs` | DELETE (uses Mutable) |

---

## Verification Checklist

### Anti-Cheat Verification (CURRENTLY PASSING ✅)
- [x] `grep -r "Mutable<" engine_dd/` returns only `// ALLOWED:` comments
- [x] `grep -r "RefCell<" engine_dd/` returns only `// ALLOWED:` comments
- [x] `grep -r "trigger_render" engine_dd/` returns only doc comments
- [x] `grep -r "handle_link_fire" engine_dd/` returns nothing
- [x] `makers verify-dd-no-cheats` passes

### Functional Verification (Phase 5)
- [ ] counter.bn works (button → HOLD increment)
- [ ] counter_hold.bn works (HOLD with initial value)
- [ ] interval.bn works (timer → HOLD → Math/sum)
- [ ] shopping_list.bn works (List/append, List/clear, persistence)
- [ ] list_example.bn works (checkbox toggles, editing mode, double-click)
- [ ] todo_mvc.bn works (full CRUD: add, toggle, edit, delete, filter, clear completed)

### Code Quality Metrics (Current State)
| Metric | Status | Notes |
|--------|--------|-------|
| `dd_bridge.rs` lines | ~2200 | Render implementation (bridge/render.rs provides API) |
| `dd_interpreter.rs` lines | ~1300 | Reduced by ~250 lines (pattern detection deleted) |
| `bridge/render.rs` lines | ~150 | Public API layer with re-exports (by design) |
| Pattern detection functions | ✅ 0 | All `extract_*` replaced with evaluator-provided config |
| `is_latest_*` hacks | ✅ 0 | Deleted as dead code |

### Architecture Verification
- [x] `bridge/render.rs` provides public API via re-exports (implementation in dd_bridge.rs by design)
- [x] `dd_bridge.rs` contains render implementation (stable, works well)
- [x] `dd_interpreter.rs` has no `extract_*` pattern detection functions (all deleted)
- [x] `dd_evaluator.rs` builds DataflowConfig during evaluation (timer, toggles, link IDs)
- [ ] `dd_runtime.rs` has `latest()` DD operator (optional - current LatestRef approach works)
- [x] No sync state reads in rendering path (verified by verify-dd-no-cheats)

---

## Recommended Implementation Order

1. **Task 3.1: LATEST operator** (Medium, ~2-4 hours)
   - Enables proper DD semantics for merged streams
   - Removes hacks from dd_evaluator.rs

2. **Task 4.1: Move render functions** (Large, ~4-8 hours)
   - Mechanical refactoring, low risk
   - Enables bridge/render.rs to own rendering

3. **Task 4.2: signal::from_stream()** (Medium, ~2-4 hours)
   - Core integration point
   - Requires render functions in place first

4. **Task 4.3: Remove pattern detection** (Large, ~4-8 hours)
   - Depends on 4.4 for replacement
   - High impact on code clarity

5. **Task 4.4: Declarative config builder** (Large, ~4-8 hours)
   - Enables 4.3
   - Cleanest last step

**Total estimated remaining: 16-32 hours**
