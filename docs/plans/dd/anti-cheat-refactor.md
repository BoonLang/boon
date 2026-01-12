# Plan: Refactor DD Engine with Anti-Cheat Enforcement

**Status: In Progress** - Legacy modules removed, anti-cheat verification passes.

## Goal
Remove `trigger_render()` hack and make events flow naturally through Differential Dataflow.
**Zero Mutable/RefCell** - DD collections are the ONLY source of truth.

## Current Progress

- [x] Phase 1: Anti-cheat infrastructure (verify-dd-no-cheats task)
- [x] Phase 1: DdOutput/DdInput wrapper types with no sync access
- [x] Phase 1: Runtime assertion guards (DdContextGuard)
- [x] Phase 1: Module restructure (core/io/bridge)
- [x] Phase 2: DdWorker with spawn_local
- [x] Legacy modules removed (dd_stream, dd_reactive_eval, dd_link, dd_bridge, dd_interpreter)
- [x] Phase 3: HOLD operator integrated into DdWorker (uses hold() from dd_runtime)
- [x] Phase 3: DataflowConfig for configurable HOLD operators
- [x] Phase 3: DdEventValue with Ord/Hash traits for DD collections
- [x] Stub modules for frontend compatibility (dd_bridge, dd_interpreter, dd_reactive_eval)
- [ ] Phase 3: LATEST as collection concat (pending)
- [ ] Phase 4: Bridge wiring to Zoon
- [ ] Phase 4: New interpreter for DD dataflow
- [ ] Phase 5: Verification

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

Before calling implementation complete:

- [ ] `grep -r "Mutable<" engine_dd/` returns nothing
- [ ] `grep -r "RefCell<" engine_dd/` returns nothing
- [ ] `grep -r "trigger_render" engine_dd/` returns nothing
- [ ] `grep -r "handle_link_fire" engine_dd/` returns nothing
- [ ] `grep -r "\.get()" engine_dd/` returns only HashMap/BTreeMap access
- [ ] `makers verify-dd-no-cheats` passes
- [ ] `makers verify-playground-dd` passes
- [ ] counter.bn works (button → HOLD)
- [ ] interval.bn works (timer → HOLD)
- [ ] todo_mvc.bn works (CRUD operations)
- [ ] shopping_list.bn works (persistence)
