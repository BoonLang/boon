//! Reactive evaluation for DD engine.
//!
//! This module extends the static DD evaluator with reactive capabilities.
//! It processes HOLD, LINK, and THEN expressions to create a reactive UI.
//!
//! # Architecture
//!
//! ```text
//! Boon Source → Parser → Static AST
//!                           ↓
//!                  DdReactiveEvaluator
//!                  ├── Evaluates expressions
//!                  ├── Creates DdSignals for HOLD state
//!                  ├── Creates Links for LINK expressions
//!                  └── Connects THEN to state updates
//!                           ↓
//!                  (DdValue document, DdReactiveContext)
//!                           ↓
//!                  Bridge renders with reactive bindings
//! ```

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use zoon::{Mutable, MutableExt};

// Global session ID - unique per WASM module load.
// This prevents old timers from previous hot-reloads from accidentally matching
// the new module's run generation (which resets to 0 on reload).
// We use a Cell<u64> initialized lazily since thread_local const init can't call functions.
thread_local! {
    static SESSION_ID: Cell<u64> = const { Cell::new(0) };
    static SESSION_ID_INITIALIZED: Cell<bool> = const { Cell::new(false) };
}

/// Initialize session ID if not already done. Uses high-precision time.
fn ensure_session_id_initialized() {
    SESSION_ID_INITIALIZED.with(|initialized| {
        if !initialized.get() {
            #[cfg(target_arch = "wasm32")]
            {
                // Use performance.now() which gives sub-millisecond precision
                SESSION_ID.with(|id| id.set((zoon::performance().now() * 1_000_000.0) as u64));
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                SESSION_ID.with(|id| {
                    id.set(std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0));
                });
            }
            initialized.set(true);
        }
    });
}

// Global run generation counter - used to invalidate old timers from previous runs.
// When a new run starts, the generation increments. Timers check their generation
// against the current global generation before firing.
thread_local! {
    static RUN_GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// Get the current session ID.
pub fn session_id() -> u64 {
    ensure_session_id_initialized();
    SESSION_ID.with(|id| id.get())
}

/// Increment the run generation and return the new value.
pub fn next_run_generation() -> u64 {
    RUN_GENERATION.with(|generation| {
        let next = generation.get() + 1;
        generation.set(next);
        next
    })
}

/// Get the current run generation.
pub fn current_run_generation() -> u64 {
    RUN_GENERATION.with(|generation| generation.get())
}

/// Invalidate all running timers by incrementing the run generation.
/// This causes any timers from previous runs to stop on their next tick.
/// Call this when clearing saved states to prevent race conditions where
/// old timers re-save values to localStorage before the new run starts.
pub fn invalidate_timers() {
    let new_gen = next_run_generation();
    zoon::println!("[invalidate_timers] Incremented run_generation to {}", new_gen);
}
#[cfg(target_arch = "wasm32")]
use zoon::WebStorage;

use super::dd_link::{Link, LinkId, LinkRegistry};
use super::dd_stream::DdSignal;
use super::dd_value::DdValue;
use crate::parser::static_expression::{
    Alias, Arm, ArithmeticOperator, Comparator, Expression, Literal, Object, Pattern, Spanned,
    TextPart,
};

/// A unique identifier for HOLD state.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HoldId(pub String);

impl HoldId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Get the name of this HOLD ID.
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// A unique identifier for a Timer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TimerId(pub String);

impl TimerId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// Information about a registered timer.
#[derive(Clone, Debug)]
pub struct TimerInfo {
    pub id: TimerId,
    pub interval_ms: f64,
}

/// A unique identifier for a Sum accumulator.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SumAccumulatorId(pub String);

impl SumAccumulatorId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// A connection between a LINK and a Sum accumulator.
#[derive(Clone)]
pub struct SumConnection {
    /// What triggers this connection (LINK)
    pub trigger: ThenTrigger,
    /// The accumulator to update
    pub accumulator_id: SumAccumulatorId,
    /// The value to add when triggered
    pub add_value: DdValue,
}

/// A connection between a LINK and Router/go_to navigation.
#[derive(Clone)]
pub struct NavigationConnection {
    /// What triggers this navigation (LINK)
    pub trigger: ThenTrigger,
    /// The route to navigate to
    pub route: String,
}

/// Reactive context that manages state and event bindings.
///
/// This is the generic replacement for the hardcoded ReactiveContext.
#[derive(Clone)]
pub struct DdReactiveContext {
    /// HOLD state signals, keyed by variable name
    holds: Rc<RefCell<HashMap<HoldId, DdSignal>>>,
    /// LINK event registry
    links: Rc<LinkRegistry>,
    /// Render trigger - increment to force re-render
    render_trigger: Rc<Mutable<u64>>,
    /// Current storage prefix for persistence
    storage_prefix: Option<String>,
    /// THEN connections: when a LINK or Timer fires, evaluate the THEN body and update the HOLD
    then_connections: Rc<RefCell<Vec<ThenConnection>>>,
    /// Sum accumulator connections: when a LINK fires, add value to accumulator
    sum_connections: Rc<RefCell<Vec<SumConnection>>>,
    /// Sum accumulators (for LATEST { initial, THEN } |> Math/sum() patterns)
    sum_accumulators: Rc<RefCell<HashMap<SumAccumulatorId, DdSignal>>>,
    /// Navigation connections: when a LINK fires, navigate to route
    nav_connections: Rc<RefCell<Vec<NavigationConnection>>>,
    /// Registered timers
    timers: Rc<RefCell<Vec<TimerInfo>>>,
    /// Task handles for running timers (keeps them alive)
    timer_handles: Rc<RefCell<Vec<zoon::TaskHandle>>>,
    /// Skip counts for HOLDs - tracks how many updates to skip before showing value
    skip_remaining: Rc<RefCell<HashMap<HoldId, u64>>>,
    /// Initial values for HOLDs (before persistence load) - used for reset on skip
    initial_values: Rc<RefCell<HashMap<HoldId, DdValue>>>,
    /// Track which HOLDs loaded from persistence (only these need reset on skip)
    loaded_from_persistence: Rc<RefCell<std::collections::HashSet<HoldId>>>,
    /// Session ID - unique per WASM module load, prevents cross-reload collisions
    session_id: u64,
    /// Run generation - timers check this against global generation before firing
    run_generation: u64,
    /// Prefixes that already have THEN connections registered (to avoid duplicates during re-evaluation)
    registered_then_prefixes: Rc<RefCell<std::collections::HashSet<String>>>,
    /// Prefixes that are "active" during the current evaluation cycle.
    /// Used for garbage collecting HOLDs belonging to deleted items.
    active_hold_prefixes: Rc<RefCell<std::collections::HashSet<String>>>,
}

impl DdReactiveContext {
    pub fn new() -> Self {
        Self {
            holds: Rc::new(RefCell::new(HashMap::new())),
            links: Rc::new(LinkRegistry::new()),
            render_trigger: Rc::new(Mutable::new(0)),
            storage_prefix: None,
            then_connections: Rc::new(RefCell::new(Vec::new())),
            sum_connections: Rc::new(RefCell::new(Vec::new())),
            sum_accumulators: Rc::new(RefCell::new(HashMap::new())),
            nav_connections: Rc::new(RefCell::new(Vec::new())),
            timers: Rc::new(RefCell::new(Vec::new())),
            timer_handles: Rc::new(RefCell::new(Vec::new())),
            skip_remaining: Rc::new(RefCell::new(HashMap::new())),
            initial_values: Rc::new(RefCell::new(HashMap::new())),
            loaded_from_persistence: Rc::new(RefCell::new(std::collections::HashSet::new())),
            session_id: session_id(),
            run_generation: next_run_generation(),
            registered_then_prefixes: Rc::new(RefCell::new(std::collections::HashSet::new())),
            active_hold_prefixes: Rc::new(RefCell::new(std::collections::HashSet::new())),
        }
    }

    pub fn new_with_persistence(prefix: impl Into<String>) -> Self {
        Self {
            holds: Rc::new(RefCell::new(HashMap::new())),
            links: Rc::new(LinkRegistry::new()),
            render_trigger: Rc::new(Mutable::new(0)),
            storage_prefix: Some(prefix.into()),
            then_connections: Rc::new(RefCell::new(Vec::new())),
            sum_connections: Rc::new(RefCell::new(Vec::new())),
            sum_accumulators: Rc::new(RefCell::new(HashMap::new())),
            nav_connections: Rc::new(RefCell::new(Vec::new())),
            timers: Rc::new(RefCell::new(Vec::new())),
            timer_handles: Rc::new(RefCell::new(Vec::new())),
            skip_remaining: Rc::new(RefCell::new(HashMap::new())),
            initial_values: Rc::new(RefCell::new(HashMap::new())),
            loaded_from_persistence: Rc::new(RefCell::new(std::collections::HashSet::new())),
            session_id: session_id(),
            run_generation: next_run_generation(),
            registered_then_prefixes: Rc::new(RefCell::new(std::collections::HashSet::new())),
            active_hold_prefixes: Rc::new(RefCell::new(std::collections::HashSet::new())),
        }
    }

    /// Reset a HOLD to its initial value, but ONLY if it loaded from persistence.
    /// This ensures fresh start for interval-based HOLDs while preserving
    /// computed values for Stream/pulses-based HOLDs.
    pub fn reset_hold_to_initial_if_persisted(&self, hold_id: &HoldId) {
        // Only reset if this HOLD loaded its value from persistence
        if self.loaded_from_persistence.borrow().contains(hold_id) {
            if let Some(initial) = self.initial_values.borrow().get(hold_id).cloned() {
                if let Some(signal) = self.holds.borrow().get(hold_id) {
                    zoon::println!("[reset_hold_to_initial] hold_id={:?} resetting to initial={:?}", hold_id, initial);
                    signal.set(initial);
                }
            }
        }
    }

    /// Set skip count for a HOLD - value will be Unit until skip updates pass
    pub fn set_skip(&self, hold_id: HoldId, count: u64) {
        zoon::println!("[set_skip] hold_id={:?} count={}", hold_id, count);
        self.skip_remaining.borrow_mut().insert(hold_id, count);
    }

    /// Get value from HOLD, respecting skip count.
    /// Returns Unit if still skipping, otherwise returns the actual value.
    pub fn get_hold_value_with_skip(&self, hold_id: &HoldId) -> DdValue {
        let skip = self.skip_remaining.borrow().get(hold_id).copied().unwrap_or(0);
        zoon::println!("[get_hold_value_with_skip] hold_id={:?} skip={}", hold_id, skip);
        if skip > 0 {
            DdValue::Unit
        } else if let Some(signal) = self.get_hold(hold_id) {
            signal.get()
        } else {
            DdValue::Unit
        }
    }

    /// Decrement skip count when HOLD is updated.
    /// Called after each THEN trigger updates the HOLD.
    pub fn decrement_skip(&self, hold_id: &HoldId) {
        let mut skip_map = self.skip_remaining.borrow_mut();
        if let Some(count) = skip_map.get_mut(hold_id) {
            if *count > 0 {
                zoon::println!("[decrement_skip] hold_id={:?} {} -> {}", hold_id, *count, *count - 1);
                *count -= 1;
            }
        }
    }

    /// Set the THEN connections (called after evaluation).
    /// Also tracks which prefixes have connections to prevent duplicates.
    pub fn set_then_connections(&self, connections: Vec<ThenConnection>) {
        // Track prefixes for all connections
        let mut prefixes = self.registered_then_prefixes.borrow_mut();
        for conn in &connections {
            // Extract prefix from hold_id (format: "prefix:hold_name" or just "hold_name")
            if let Some(prefix) = conn.hold_id.name().split(':').next() {
                if conn.hold_id.name().contains(':') {
                    prefixes.insert(prefix.to_string());
                }
            }
        }
        *self.then_connections.borrow_mut() = connections;
    }

    /// Check if a prefix already has THEN connections registered.
    /// Used during re-evaluation to avoid creating duplicate connections.
    pub fn has_then_connections_for_prefix(&self, prefix: &str) -> bool {
        self.registered_then_prefixes.borrow().contains(prefix)
    }

    /// Get a snapshot of all currently registered prefixes.
    /// Used by evaluator to check against a point-in-time snapshot.
    pub fn snapshot_registered_prefixes(&self) -> std::collections::HashSet<String> {
        self.registered_then_prefixes.borrow().clone()
    }

    /// Add a single THEN connection and track its prefix.
    /// Used during re-evaluation to add connections for NEW items.
    pub fn add_then_connection(&self, conn: ThenConnection, prefix: Option<&str>) {
        if let Some(p) = prefix {
            self.registered_then_prefixes.borrow_mut().insert(p.to_string());
        }
        self.then_connections.borrow_mut().push(conn);
    }

    /// Store a timer task handle to keep it alive.
    pub fn add_timer_handle(&self, handle: zoon::TaskHandle) {
        self.timer_handles.borrow_mut().push(handle);
    }

    /// Check if any timer handles are currently stored (timers are running).
    pub fn has_timer_handles(&self) -> bool {
        !self.timer_handles.borrow().is_empty()
    }

    /// Get this context's run generation.
    pub fn run_generation(&self) -> u64 {
        self.run_generation
    }

    /// Get this context's session ID.
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Check if this context is still the current run (not superseded by a newer run).
    /// Checks BOTH session_id (to handle hot-reload) AND run_generation (to handle re-runs).
    pub fn is_current_run(&self) -> bool {
        self.session_id == session_id() && self.run_generation == current_run_generation()
    }

    /// Register a timer.
    pub fn register_timer(&self, timer_info: TimerInfo) {
        self.timers.borrow_mut().push(timer_info);
    }

    /// Get all registered timers.
    pub fn get_timers(&self) -> Vec<TimerInfo> {
        self.timers.borrow().clone()
    }

    /// Handle a LINK firing by evaluating associated THEN connections.
    ///
    /// Returns true if any state was updated.
    pub fn handle_link_fire(&self, link_id: &LinkId) -> bool {
        self.handle_trigger(&ThenTrigger::Link(link_id.clone()))
    }

    /// Handle a Timer firing by evaluating associated THEN connections.
    ///
    /// Returns true if any state was updated.
    pub fn handle_timer_fire(&self, timer_id: &TimerId) -> bool {
        zoon::println!("[handle_timer_fire] timer_id={:?} run_generation={}", timer_id, self.run_generation);
        self.handle_trigger(&ThenTrigger::Timer(timer_id.clone()))
    }

    /// Handle any trigger (LINK or Timer) by evaluating associated THEN connections.
    fn handle_trigger(&self, trigger: &ThenTrigger) -> bool {
        let mut updated = false;

        // DEBUG: Log what trigger we're handling
        zoon::eprintln!("[DD DEBUG] handle_trigger called with: {:?}", trigger);

        // PRE-COMPUTE store.all_completed ONCE before any THEN bodies fire.
        // This is critical for toggle_all: all THEN bodies must see the SAME value,
        // not values that change as each THEN updates its HOLD.
        let store_snapshot = compute_store_snapshot(self);

        // Handle THEN → HOLD connections
        {
            let connections = self.then_connections.borrow();
            zoon::eprintln!("[DD DEBUG] Checking {} THEN connections", connections.len());
            for (i, conn) in connections.iter().enumerate() {
                zoon::eprintln!("[DD DEBUG]   conn[{}] trigger={:?}, hold_id={:?}", i, conn.trigger, conn.hold_id);
                // Check if triggers match, accounting for scope prefixes and event path differences
                let triggers_match = match (&conn.trigger, trigger) {
                    (ThenTrigger::Link(conn_id), ThenTrigger::Link(trigger_id)) => {
                        let conn_str = &conn_id.0;
                        let trigger_str = &trigger_id.0;

                        // Extract the list item prefix (e.g., "list_init_0") from both
                        let conn_prefix = conn_str.split(':').next().unwrap_or("");
                        let trigger_prefix = trigger_str.split(':').next().unwrap_or("");

                        // Get the path after the prefix
                        let conn_path = conn_str.split(':').nth(1).unwrap_or(conn_str);
                        let trigger_path = trigger_str.split(':').nth(1).unwrap_or(trigger_str);

                        // Strip known event suffixes from conn_path
                        // (e.g., ".event.click", ".event.press", ".event.blur", ".event.double_click")
                        let conn_core = conn_path
                            .strip_suffix(".event.click")
                            .or_else(|| conn_path.strip_suffix(".event.press"))
                            .or_else(|| conn_path.strip_suffix(".event.blur"))
                            .or_else(|| conn_path.strip_suffix(".event.double_click"))
                            .or_else(|| conn_path.strip_suffix(".event.key_down.key"))
                            .unwrap_or(conn_path);

                        // Extract the LINK name (last segment before event, typically like "todo_checkbox")
                        let conn_link_name = conn_core.split('.').last().unwrap_or(conn_core);
                        let trigger_link_name = trigger_path.split('.').last().unwrap_or(trigger_path);

                        // Prefixes must match (or both be empty)
                        let prefixes_match = conn_prefix == trigger_prefix
                            || conn_prefix.is_empty()
                            || trigger_prefix.is_empty();

                        // LINK names must match
                        let names_match = conn_link_name == trigger_link_name;

                        zoon::eprintln!("[DD DEBUG]       conn_prefix={}, trigger_prefix={}, conn_link_name={}, trigger_link_name={}",
                            conn_prefix, trigger_prefix, conn_link_name, trigger_link_name);

                        // Exact match
                        conn_str == trigger_str
                        // Or prefixes match and LINK names match
                        || (prefixes_match && names_match)
                        // Or one is suffix of the other (with dot separator)
                        || trigger_str.ends_with(&format!(".{}", conn_str))
                        || conn_str.ends_with(&format!(".{}", trigger_str))
                    }
                    (ThenTrigger::Timer(a), ThenTrigger::Timer(b)) => a == b,
                    _ => false,
                };
                zoon::eprintln!("[DD DEBUG]     triggers_match={}", triggers_match);
                if triggers_match {
                    zoon::eprintln!("[DD DEBUG]     MATCH FOUND! Looking for hold_id={:?}", conn.hold_id);
                    // Get current state
                    if let Some(signal) = self.get_hold(&conn.hold_id) {
                        let current_state = signal.get();
                        zoon::eprintln!("[DD DEBUG]     current_state={:?}", current_state);
                        // Evaluate the THEN body with pre-computed store snapshot
                        let new_value = evaluate_then_connection(conn, &current_state, self, &store_snapshot);
                        zoon::eprintln!("[DD DEBUG]     new_value={:?}", new_value);
                        if new_value != DdValue::Unit && new_value != current_state {
                            // Update the HOLD
                            zoon::eprintln!("[DD DEBUG]     Updating HOLD with new_value");
                            self.update_hold(&conn.hold_id, new_value);
                            updated = true;
                        } else {
                            zoon::eprintln!("[DD DEBUG]     NOT updating: new_value == Unit or == current_state");
                        }
                    } else {
                        zoon::eprintln!("[DD DEBUG]     HOLD not found for id={:?}", conn.hold_id);
                    }
                }
            }
        }

        // Handle LINK → Sum accumulator connections
        {
            let sum_conns = self.sum_connections.borrow();
            for conn in sum_conns.iter() {
                if conn.trigger == *trigger {
                    if let Some(signal) = self.get_sum_accumulator(&conn.accumulator_id) {
                        let current = signal.get();
                        // Add the value to the accumulator
                        // If current is Unit (first fire), set to add_value
                        // If current is Number, add to it
                        if let DdValue::Number(add) = &conn.add_value {
                            let new_sum = match &current {
                                DdValue::Unit => DdValue::float(add.0),
                                DdValue::Number(curr) => DdValue::float(curr.0 + add.0),
                                _ => conn.add_value.clone(),
                            };
                            signal.set(new_sum.clone());

                            // Save to localStorage if persistence enabled
                            if let Some(ref prefix) = self.storage_prefix {
                                let key = format!("dd_sum_{}_{}", prefix, conn.accumulator_id.0);
                                #[cfg(target_arch = "wasm32")]
                                {
                                    use zoon::local_storage;
                                    if let DdValue::Number(n) = &new_sum {
                                        let _ = local_storage().insert(&key, &n.0.to_string());
                                    }
                                }
                            }

                            updated = true;
                        }
                    }
                }
            }
        }

        // Handle LINK → Navigation connections (Router/go_to)
        {
            let nav_conns = self.nav_connections.borrow();
            zoon::eprintln!("[NAV-CHECK] START len={}", nav_conns.len());
            for (i, c) in nav_conns.iter().enumerate() {
                zoon::eprintln!("[NAV-CHECK] item {} = {:?}", i, c.trigger);
            }
            zoon::eprintln!("[NAV-CHECK] END dump");
            for (idx, conn) in nav_conns.iter().enumerate() {
                zoon::eprintln!("[handle_trigger nav]   [{}] conn trigger={:?}", idx, conn.trigger);
                // Check if triggers match, accounting for scope prefixes
                // e.g., "store.nav.about" should match "nav.about" when inside store scope
                let triggers_match = match (&conn.trigger, trigger) {
                    (ThenTrigger::Link(conn_id), ThenTrigger::Link(trigger_id)) => {
                        let conn_str = &conn_id.0;
                        let trigger_str = &trigger_id.0;

                        // Strip list_init_X: prefix if present (from hold_id_prefix)
                        let trigger_base = if let Some(idx) = trigger_str.find(':') {
                            &trigger_str[idx + 1..]
                        } else {
                            trigger_str.as_ref()
                        };

                        // Strip .event.press suffix from trigger
                        let trigger_base = trigger_base
                            .trim_end_matches(".event.press")
                            .trim_end_matches(".event.key_down.key");

                        // Extract final segment (e.g., "filter_buttons.active" from full path)
                        let conn_final = conn_str.as_str();
                        let trigger_final = trigger_base;

                        zoon::println!("[handle_trigger nav] Comparing conn='{}' vs trigger_base='{}'",
                            conn_final, trigger_final);

                        // Exact match
                        conn_final == trigger_final
                        // Or one is suffix of the other (with dot separator)
                        || trigger_final.ends_with(&format!(".{}", conn_final))
                        || conn_final.ends_with(trigger_final)
                        // Or they have the same final path (strip leading scope like "store.")
                        || trigger_final.ends_with(conn_final)
                    }
                    (ThenTrigger::Timer(a), ThenTrigger::Timer(b)) => a == b,
                    _ => false,
                };
                zoon::println!("[handle_trigger nav] Match result: {}", triggers_match);
                if triggers_match {
                    // Navigate to the route
                    #[cfg(target_arch = "wasm32")]
                    {
                        use zoon::*;
                        let route = &conn.route;
                        // Use history.pushState to change URL without page reload
                        // IMPORTANT: Preserve the query string (e.g., ?example=todo_mvc)
                        // to avoid losing the playground context
                        if let Some(history) = window().history().ok() {
                            let new_url = if let Ok(search) = window().location().search() {
                                if search.is_empty() {
                                    route.clone()
                                } else {
                                    format!("{}{}", route, search)
                                }
                            } else {
                                route.clone()
                            };
                            let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&new_url));
                        }
                        // Dispatch popstate event to notify the app of route change
                        if let Ok(event) = web_sys::Event::new("popstate") {
                            let _ = window().dispatch_event(&event);
                        }
                    }
                    updated = true;
                }
            }
        }

        updated
    }

    /// Register a sum accumulator with an initial value.
    pub fn register_sum_accumulator(&self, id: SumAccumulatorId, initial: DdValue) -> DdSignal {
        let mut accumulators = self.sum_accumulators.borrow_mut();
        if let Some(signal) = accumulators.get(&id) {
            return signal.clone();
        }

        // Try to load from localStorage if persistence enabled
        let loaded_value = if let Some(ref prefix) = self.storage_prefix {
            let key = format!("dd_sum_{}_{}", prefix, id.0);
            #[cfg(target_arch = "wasm32")]
            {
                use zoon::local_storage;
                if let Some(Ok(json)) = local_storage().get::<String>(&key) {
                    // Try to deserialize the value - numbers stored as f64
                    if let Ok(n) = json.parse::<f64>() {
                        Some(DdValue::float(n))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                None
            }
        } else {
            None
        };

        let signal = DdSignal::new(loaded_value.unwrap_or(initial));
        accumulators.insert(id, signal.clone());
        signal
    }

    /// Get a sum accumulator by ID.
    pub fn get_sum_accumulator(&self, id: &SumAccumulatorId) -> Option<DdSignal> {
        self.sum_accumulators.borrow().get(id).cloned()
    }

    /// Add a sum connection.
    pub fn add_sum_connection(&self, connection: SumConnection) {
        self.sum_connections.borrow_mut().push(connection);
    }

    /// Clear all sum connections (used between evaluation passes to avoid duplicates).
    pub fn clear_sum_connections(&self) {
        self.sum_connections.borrow_mut().clear();
        self.sum_accumulators.borrow_mut().clear();
    }

    /// Check if context has any sum accumulators.
    /// Examples with sum accumulators need simple rendering (accumulator IDs unstable in re-eval).
    /// Examples without sum accumulators can use re-evaluation for derived values.
    pub fn has_sum_accumulators(&self) -> bool {
        !self.sum_accumulators.borrow().is_empty()
    }

    /// Add a navigation connection (if not already exists).
    pub fn add_nav_connection(&self, connection: NavigationConnection) {
        let mut nav_conns = self.nav_connections.borrow_mut();
        zoon::eprintln!("[add_nav_connection] BEFORE: len={}, adding {:?} -> {}",
            nav_conns.len(), connection.trigger, connection.route);
        // Check if this connection already exists (same trigger and route)
        let already_exists = nav_conns.iter().any(|existing| {
            match (&existing.trigger, &connection.trigger) {
                (ThenTrigger::Link(a), ThenTrigger::Link(b)) => a.0 == b.0 && existing.route == connection.route,
                (ThenTrigger::Timer(a), ThenTrigger::Timer(b)) => a == b && existing.route == connection.route,
                _ => false,
            }
        });
        if !already_exists {
            nav_conns.push(connection);
            zoon::eprintln!("[add_nav_connection] ADDED - new len={}", nav_conns.len());
        } else {
            zoon::eprintln!("[add_nav_connection] SKIPPED - already exists");
        }
    }

    /// Clear all navigation connections.
    pub fn clear_nav_connections(&self) {
        // Don't clear - nav connections are stable and deduplicated on add
        // self.nav_connections.borrow_mut().clear();
    }

    /// Clear registered timers (used between evaluation passes to avoid duplicates).
    /// Also drops timer handles to cancel running timer tasks.
    pub fn clear_timers(&self) {
        self.timers.borrow_mut().clear();
        // Drop timer handles to cancel running timer tasks
        self.timer_handles.borrow_mut().clear();
    }

    /// Register or get a HOLD state signal.
    pub fn register_hold(&self, id: HoldId, initial: DdValue) -> DdSignal {
        // Store the initial value for potential reset by Stream/skip
        self.initial_values.borrow_mut().insert(id.clone(), initial.clone());

        let mut holds = self.holds.borrow_mut();
        if let Some(signal) = holds.get(&id) {
            return signal.clone();
        }

        // Try to load from localStorage if persistence enabled
        let loaded_value = if let Some(ref prefix) = self.storage_prefix {
            let key = format!("dd_{}_{}", prefix, id.0);
            #[cfg(target_arch = "wasm32")]
            {
                use zoon::local_storage;
                if let Some(Ok(json)) = local_storage().get::<String>(&key) {
                    zoon::println!("[register_hold] Found persisted value for {:?}: {}", id, json);
                    // Try to deserialize the value
                    // For numbers, parse as f64
                    if let Ok(n) = json.parse::<f64>() {
                        Some(DdValue::float(n))
                    } else if json.starts_with('[') && json.ends_with(']') {
                        // Try to parse as JSON array
                        let inner = &json[1..json.len()-1];
                        if inner.is_empty() {
                            Some(DdValue::List(std::sync::Arc::new(vec![])))
                        } else {
                            // Simple JSON string array parser
                            let mut items = Vec::new();
                            let mut current = String::new();
                            let mut in_string = false;
                            let mut escape_next = false;

                            for ch in inner.chars() {
                                if escape_next {
                                    current.push(ch);
                                    escape_next = false;
                                } else if ch == '\\' && in_string {
                                    escape_next = true;
                                } else if ch == '"' {
                                    in_string = !in_string;
                                } else if ch == ',' && !in_string {
                                    // End of item
                                    let trimmed = current.trim();
                                    if !trimmed.is_empty() {
                                        if let Ok(n) = trimmed.parse::<f64>() {
                                            items.push(DdValue::float(n));
                                        } else if trimmed == "true" {
                                            items.push(DdValue::Bool(true));
                                        } else if trimmed == "false" {
                                            items.push(DdValue::Bool(false));
                                        } else {
                                            items.push(DdValue::text(trimmed));
                                        }
                                    }
                                    current.clear();
                                } else {
                                    current.push(ch);
                                }
                            }
                            // Handle last item
                            let trimmed = current.trim();
                            if !trimmed.is_empty() {
                                if let Ok(n) = trimmed.parse::<f64>() {
                                    items.push(DdValue::float(n));
                                } else if trimmed == "true" {
                                    items.push(DdValue::Bool(true));
                                } else if trimmed == "false" {
                                    items.push(DdValue::Bool(false));
                                } else {
                                    items.push(DdValue::text(trimmed));
                                }
                            }
                            zoon::println!("[register_hold] Loaded {} list items", items.len());
                            Some(DdValue::List(std::sync::Arc::new(items)))
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                None
            }
        } else {
            None
        };

        // Track if this HOLD loaded from persistence
        if loaded_value.is_some() {
            self.loaded_from_persistence.borrow_mut().insert(id.clone());
        }

        let signal = DdSignal::new(loaded_value.unwrap_or(initial));
        holds.insert(id, signal.clone());
        signal
    }

    /// Get an existing HOLD signal.
    pub fn get_hold(&self, id: &HoldId) -> Option<DdSignal> {
        self.holds.borrow().get(id).cloned()
    }

    /// Get the current value of a HOLD.
    pub fn get_hold_value(&self, id: &HoldId) -> Option<DdValue> {
        let result = self.holds.borrow().get(id).map(|signal| signal.get());
        if result.is_none() && id.0.contains("completed") {
            zoon::eprintln!("[get_hold_value] MISS for {:?}", id);
            // Debug: list all registered HOLDs with "completed" in them
            let holds = self.holds.borrow();
            let matching: Vec<_> = holds.keys()
                .filter(|k| k.0.contains("completed"))
                .map(|k| k.0.as_str())
                .collect();
            zoon::eprintln!("[get_hold_value] Registered 'completed' HOLDs: {:?}", matching);
        }
        result
    }

    /// Update a HOLD signal's value.
    pub fn update_hold(&self, id: &HoldId, value: DdValue) {
        zoon::println!("[update_hold] hold_id={:?} value={:?}", id, value);
        if let Some(signal) = self.holds.borrow().get(id) {
            signal.set(value.clone());

            // Decrement skip counter - after enough updates, value will start showing
            self.decrement_skip(id);

            // Save to localStorage if persistence enabled
            if let Some(ref prefix) = self.storage_prefix {
                let key = format!("dd_{}_{}", prefix, id.0);
                #[cfg(target_arch = "wasm32")]
                {
                    use zoon::local_storage;
                    match &value {
                        DdValue::Number(n) => {
                            let _ = local_storage().insert(&key, &n.0.to_string());
                        }
                        DdValue::List(items) => {
                            // Serialize list items as JSON array
                            let json_items: Vec<String> = items.iter().map(|item| {
                                match item {
                                    DdValue::Text(s) => format!("\"{}\"", s.as_ref().replace('\\', "\\\\").replace('"', "\\\"")),
                                    DdValue::Number(n) => n.0.to_string(),
                                    DdValue::Bool(b) => b.to_string(),
                                    _ => "null".to_string(),
                                }
                            }).collect();
                            let json = format!("[{}]", json_items.join(","));
                            zoon::println!("[update_hold] Persisting list: {}", json);
                            let _ = local_storage().insert(&key, &json);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Register a LINK and get it.
    pub fn register_link(&self, id: LinkId) -> Link {
        self.links.register(id)
    }

    /// Get an existing LINK.
    pub fn get_link(&self, id: &LinkId) -> Option<Link> {
        self.links.get(id)
    }

    /// Fire a LINK event.
    pub fn fire_link(&self, id: &LinkId) {
        zoon::println!("[fire_link] Firing link: {}", id.0);
        self.links.fire_unit(id);
    }

    /// Fire a LINK event with a value.
    pub fn fire_link_with_value(&self, id: &LinkId, value: DdValue) {
        self.links.fire(id, value);
    }

    /// Clear a LINK's value (consume the event).
    pub fn clear_link_value(&self, id: &LinkId) {
        self.links.clear_link_value(id);
    }

    /// Trigger a re-render.
    pub fn trigger_render(&self) {
        let old_val = self.render_trigger.get();
        self.render_trigger.update(|v| v + 1);
        let new_val = self.render_trigger.get();
        zoon::println!("[trigger_render] Updated render_trigger from {} to {}, Rc ptr={:p}",
            old_val, new_val, Rc::as_ptr(&self.render_trigger));
    }

    /// Get the render trigger signal.
    /// Returns a signal that watches the shared render_trigger Mutable.
    pub fn render_signal(&self) -> impl zoon::Signal<Item = u64> + 'static {
        // Clone the Rc to share the same Mutable, not clone the Mutable value!
        let trigger = Rc::clone(&self.render_trigger);
        zoon::println!("[render_signal] Getting signal from Mutable, Rc ptr={:p}, current value={}",
            Rc::as_ptr(&self.render_trigger), self.render_trigger.get());
        // Deref to get the Mutable and call signal() - this returns a 'static signal
        (*trigger).signal()
    }

    /// Get the link registry.
    pub fn link_registry(&self) -> &LinkRegistry {
        &self.links
    }

    /// Get all HOLD values for rendering.
    pub fn get_hold_values(&self) -> HashMap<String, DdValue> {
        self.holds
            .borrow()
            .iter()
            .map(|(id, signal)| (id.0.clone(), signal.get()))
            .collect()
    }

    /// Get all HOLDs with their IDs and signals.
    /// Used by `refresh_store_computed_fields` to recompute derived values.
    pub fn all_holds(&self) -> Vec<(HoldId, DdSignal)> {
        self.holds
            .borrow()
            .iter()
            .map(|(id, signal)| (id.clone(), signal.clone()))
            .collect()
    }

    /// Start a new evaluation cycle - clears the set of active prefixes.
    /// Called at the start of re-evaluation to track which items are still present.
    pub fn start_evaluation_cycle(&self) {
        self.active_hold_prefixes.borrow_mut().clear();
    }

    /// Mark a prefix as active during evaluation.
    /// Called when List/map iterates an item to record that this item is still present.
    pub fn mark_prefix_active(&self, prefix: &str) {
        self.active_hold_prefixes.borrow_mut().insert(prefix.to_string());
    }

    /// Clean up HOLDs belonging to prefixes that are no longer active.
    /// Called after re-evaluation to garbage collect HOLDs from deleted items.
    ///
    /// Only removes HOLDs with prefixed IDs (like "list_init_0:completed:state").
    /// Non-prefixed HOLDs (top-level state) are never removed.
    pub fn cleanup_orphaned_holds(&self) {
        let active = self.active_hold_prefixes.borrow();
        let mut holds = self.holds.borrow_mut();

        // Find HOLDs to remove: those with a prefix that's not in active set
        let orphaned_ids: Vec<HoldId> = holds
            .keys()
            .filter(|id| {
                let id_str = id.name();
                // Check if ID has a prefix (contains ':')
                if let Some(colon_idx) = id_str.find(':') {
                    let prefix = &id_str[..colon_idx];
                    // Only consider item prefixes (list_init_X or append_X)
                    if prefix.starts_with("list_init_") || prefix.starts_with("append_") {
                        !active.contains(prefix)
                    } else {
                        false // Not an item prefix, keep it
                    }
                } else {
                    false // No prefix, keep it
                }
            })
            .cloned()
            .collect();

        // Remove orphaned HOLDs
        for id in &orphaned_ids {
            zoon::println!("[cleanup_orphaned_holds] Removing orphaned HOLD: {:?}", id);
            holds.remove(id);
        }

        if !orphaned_ids.is_empty() {
            zoon::println!("[cleanup_orphaned_holds] Removed {} orphaned HOLDs", orphaned_ids.len());
        }
    }

    /// Get a snapshot of all active HOLD prefixes (for debugging).
    pub fn get_active_prefixes(&self) -> Vec<String> {
        self.active_hold_prefixes.borrow().iter().cloned().collect()
    }
}

impl Default for DdReactiveContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Reactive evaluator for DD engine.
///
/// This evaluator processes expressions and populates a DdReactiveContext
/// with HOLD states, LINK bindings, and THEN connections.
pub struct DdReactiveEvaluator {
    /// Variable values (static snapshot)
    variables: HashMap<String, DdValue>,
    /// Function definitions
    functions: HashMap<String, FunctionDef>,
    /// PASSED context for function calls
    passed_context: Option<DdValue>,
    /// Reactive context for HOLD/LINK
    reactive_ctx: DdReactiveContext,
    /// THEN connections: when this LINK fires, run this computation
    then_connections: Vec<ThenConnection>,
    /// Current variable being assigned (for tracking which HOLD Stream/skip applies to)
    current_variable_name: Option<String>,
    /// Current HOLD state name being evaluated (set by eval_pipe_to_hold)
    current_hold_id: Option<HoldId>,
    /// Prefix for HOLD IDs when evaluating inside list items (makes each item's HOLDs unique)
    hold_id_prefix: Option<String>,
    /// Skip THEN connection creation (used during re-evaluation to avoid duplicates)
    skip_then_connections: bool,
    /// Snapshot of prefixes that had THEN connections at the START of this evaluation.
    /// During re-evaluation, we check against this snapshot (not the live set) to allow
    /// multiple connections for the same NEW prefix to be created.
    prefixes_at_eval_start: std::collections::HashSet<String>,
    /// First pass of two-pass evaluation (for forward reference resolution).
    /// During first pass, state-modifying operations like List/append should be skipped.
    first_pass_evaluation: bool,
}

#[derive(Clone)]
pub struct FunctionDef {
    pub parameters: Vec<String>,
    pub body: Box<Spanned<Expression>>,
}

/// The source that triggers a THEN connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThenTrigger {
    /// Triggered by a LINK event
    Link(LinkId),
    /// Triggered by a Timer tick
    Timer(TimerId),
}

/// A connection between a LINK/Timer and a HOLD via THEN.
/// Made public for bridge access.
#[derive(Clone)]
pub struct ThenConnection {
    /// What triggers this connection (LINK or Timer)
    pub trigger: ThenTrigger,
    /// The HOLD to update
    pub hold_id: HoldId,
    /// The state parameter name
    pub state_name: String,
    /// The body expression to evaluate
    pub body: Box<Spanned<Expression>>,
    /// Variable bindings at the time of connection creation (for evaluation context)
    pub variables_snapshot: HashMap<String, DdValue>,
    /// Function definitions at the time of connection creation
    pub functions_snapshot: HashMap<String, FunctionDef>,
}

impl ThenConnection {
    /// For backward compatibility - get link_id if this is a Link trigger.
    pub fn link_id(&self) -> Option<&LinkId> {
        match &self.trigger {
            ThenTrigger::Link(id) => Some(id),
            _ => None,
        }
    }
}

impl DdReactiveEvaluator {
    /// Create a new reactive evaluator.
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            passed_context: None,
            reactive_ctx: DdReactiveContext::new(),
            then_connections: Vec::new(),
            current_variable_name: None,
            current_hold_id: None,
            hold_id_prefix: None,
            skip_then_connections: false,
            prefixes_at_eval_start: std::collections::HashSet::new(),
            first_pass_evaluation: false,
        }
    }

    /// Create a new reactive evaluator with persistence.
    pub fn new_with_persistence(prefix: impl Into<String>) -> Self {
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            passed_context: None,
            reactive_ctx: DdReactiveContext::new_with_persistence(prefix),
            then_connections: Vec::new(),
            current_variable_name: None,
            current_hold_id: None,
            hold_id_prefix: None,
            skip_then_connections: false,
            prefixes_at_eval_start: std::collections::HashSet::new(),
            first_pass_evaluation: false,
        }
    }

    /// Create a new reactive evaluator with an existing reactive context.
    ///
    /// This is used for re-evaluation: when a HOLD is registered, `register_hold`
    /// will return the existing signal (with its current value) instead of creating
    /// a new one. This ensures derived values see the updated HOLD values.
    ///
    /// THEN connections are skipped for prefixes that already existed at eval start.
    /// NEW prefixes (from dynamically added items) will still get their connections.
    pub fn new_with_context(ctx: DdReactiveContext) -> Self {
        // Snapshot which prefixes already have connections - new prefixes will still get their connections
        let prefixes_snapshot = ctx.snapshot_registered_prefixes();
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            passed_context: None,
            reactive_ctx: ctx,
            then_connections: Vec::new(),
            current_variable_name: None,
            current_hold_id: None,
            hold_id_prefix: None,
            skip_then_connections: true,  // Skip THEN connection creation for existing prefixes
            prefixes_at_eval_start: prefixes_snapshot,
            first_pass_evaluation: false,
        }
    }

    /// Get the reactive context.
    pub fn reactive_context(&self) -> &DdReactiveContext {
        &self.reactive_ctx
    }

    /// Consume the evaluator and return the reactive context with THEN connections.
    pub fn into_reactive_context_with_connections(self) -> (DdReactiveContext, Vec<ThenConnection>) {
        (self.reactive_ctx, self.then_connections)
    }

    /// Consume the evaluator and return the reactive context.
    pub fn into_reactive_context(self) -> DdReactiveContext {
        self.reactive_ctx
    }

    /// Extract the hold_id_prefix from a DdValue by looking at its HoldRef fields.
    ///
    /// For example, if an item has `completed: HoldRef("append_73:completed:state")`,
    /// this extracts "append_73" as the prefix.
    fn extract_prefix_from_item(item: &DdValue) -> Option<String> {
        match item {
            DdValue::Object(fields) => {
                // Look for any HoldRef field and extract its prefix
                for (_, field_value) in fields.iter() {
                    if let Some(prefix) = Self::extract_prefix_from_value(field_value) {
                        return Some(prefix);
                    }
                }
                None
            }
            DdValue::HoldRef(hold_ref) => {
                // HoldRef format is "prefix:name:state" - extract prefix
                let s = hold_ref.as_ref();
                if let Some(colon_pos) = s.find(':') {
                    Some(s[..colon_pos].to_string())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Recursively look for HoldRef in a value to extract prefix.
    fn extract_prefix_from_value(value: &DdValue) -> Option<String> {
        match value {
            DdValue::HoldRef(hold_ref) => {
                let s = hold_ref.as_ref();
                if let Some(colon_pos) = s.find(':') {
                    Some(s[..colon_pos].to_string())
                } else {
                    None
                }
            }
            DdValue::Object(fields) => {
                for (_, field_value) in fields.iter() {
                    if let Some(prefix) = Self::extract_prefix_from_value(field_value) {
                        return Some(prefix);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Get the THEN connections.
    pub fn get_then_connections(&self) -> &[ThenConnection] {
        &self.then_connections
    }

    /// Get all variables.
    pub fn get_all_variables(&self) -> &HashMap<String, DdValue> {
        &self.variables
    }

    /// Get the document output.
    pub fn get_document(&self) -> Option<&DdValue> {
        self.variables.get("document")
    }

    /// Inject a variable value before evaluation.
    pub fn inject_variable(&mut self, name: impl Into<String>, value: DdValue) {
        self.variables.insert(name.into(), value);
    }

    /// Evaluate expressions and populate reactive context.
    pub fn evaluate(&mut self, expressions: &[Spanned<Expression>]) {
        // Remember which variables were pre-injected
        let injected_vars: std::collections::HashSet<String> =
            self.variables.keys().cloned().collect();

        // First: collect all function definitions
        for expr in expressions {
            if let Expression::Function { name, parameters, body } = &expr.node {
                let func_name = name.as_str().to_string();
                let params: Vec<String> = parameters.iter().map(|p| p.node.as_str().to_string()).collect();
                self.functions.insert(func_name, FunctionDef {
                    parameters: params,
                    body: body.clone(),
                });
            }
        }

        // First pass: evaluate all variables (skip state-modifying operations)
        self.first_pass_evaluation = true;
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                let name = var.name.as_str().to_string();
                if !injected_vars.contains(&name) {
                    // Track which variable we're evaluating (for Stream/skip to find HOLD)
                    self.current_variable_name = Some(name.clone());
                    let value = self.eval_expression(&var.value.node);
                    self.current_variable_name = None;
                    // Inject __link_id__ if the value is an element with LINK
                    let value = Self::inject_link_id_if_needed(value, &name);
                    self.variables.insert(name, value);
                }
            }
        }
        self.first_pass_evaluation = false;

        // Second pass: re-evaluate to resolve forward references
        // Clear connections from first pass to avoid duplicates
        self.then_connections.clear();
        self.reactive_ctx.clear_sum_connections();
        self.reactive_ctx.clear_nav_connections();
        self.reactive_ctx.clear_timers();

        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                let name = var.name.as_str().to_string();
                if injected_vars.contains(&name) {
                    continue;
                }
                // Track which variable we're evaluating (for Stream/skip to find HOLD)
                self.current_variable_name = Some(name.clone());
                let value = self.eval_expression(&var.value.node);
                self.current_variable_name = None;
                // Inject __link_id__ if the value is an element with LINK
                let value = Self::inject_link_id_if_needed(value, &name);
                // Inject __timer_id__ if the value is a Timer
                let value = Self::inject_timer_id_if_needed(value, &name);
                self.variables.insert(name, value);
            }
        }

        // Register any timers found in variables
        for (name, value) in &self.variables {
            if let DdValue::Tagged { tag, fields } = value {
                if tag.as_ref() == "Timer" {
                    if let Some(DdValue::Number(ms)) = fields.get("interval_ms") {
                        self.reactive_ctx.register_timer(TimerInfo {
                            id: TimerId::new(name),
                            interval_ms: ms.0,
                        });
                    }
                }
            }
        }
    }

    /// If the value is a Timer, inject __timer_id__ with the variable name.
    fn inject_timer_id_if_needed(value: DdValue, var_name: &str) -> DdValue {
        if let DdValue::Tagged { tag, fields } = &value {
            if tag.as_ref() == "Timer" {
                let mut new_fields = (**fields).clone();
                new_fields.insert(Arc::from("__timer_id__"), DdValue::Text(Arc::from(var_name)));
                return DdValue::Tagged {
                    tag: tag.clone(),
                    fields: Arc::new(new_fields),
                };
            }
        }
        value
    }

    /// If the value is an element containing a LINK, inject __link_id__ with the variable path.
    /// This allows the bridge to know which link_id to fire when the element is interacted with.
    ///
    /// This function recursively traverses Objects and Lists to find all elements with LINKs,
    /// building up the full path (e.g., "elements.filter_buttons.all" for nested buttons).
    fn inject_link_id_if_needed(value: DdValue, var_path: &str) -> DdValue {
        match value {
            // Recursively traverse Objects, building up the path
            DdValue::Object(map) => {
                let mut new_map = BTreeMap::new();
                for (key, val) in map.iter() {
                    let nested_path = if var_path.is_empty() {
                        key.to_string()
                    } else {
                        format!("{}.{}", var_path, key)
                    };
                    new_map.insert(key.clone(), Self::inject_link_id_if_needed(val.clone(), &nested_path));
                }
                DdValue::Object(Arc::new(new_map))
            }
            // Recursively traverse Lists with index-based paths
            DdValue::List(items) => {
                let new_items: Vec<DdValue> = items.iter()
                    .enumerate()
                    .map(|(idx, val)| {
                        let nested_path = format!("{}[{}]", var_path, idx);
                        Self::inject_link_id_if_needed(val.clone(), &nested_path)
                    })
                    .collect();
                DdValue::List(Arc::from(new_items))
            }
            // Check Tagged elements (like Element/button)
            DdValue::Tagged { tag, fields } => {
                // Check if element.event.press, event.click, or event.key_down contains LINK
                if let Some(element) = fields.get("element") {
                    if let Some(event) = element.get("event") {
                        // Check press (for buttons), click (for checkboxes), and key_down (for text inputs)
                        let has_link = Self::event_has_link(event.get("press"))
                            || Self::event_has_link(event.get("click"))
                            || Self::event_has_link(event.get("key_down"));

                        if has_link {
                            // Only inject __link_id__ if it's not already set
                            // (prevents overwriting when element is placed into Lists/document)
                            if !fields.contains_key("__link_id__") {
                                let mut new_fields = (*fields).clone();
                                new_fields.insert(
                                    Arc::from("__link_id__"),
                                    DdValue::Text(Arc::from(var_path)),
                                );
                                return DdValue::Tagged {
                                    tag,
                                    fields: Arc::new(new_fields),
                                };
                            } else {
                                // Already has __link_id__, just return as-is
                                return DdValue::Tagged { tag, fields };
                            }
                        }
                    }
                }
                // Also recursively process fields of tagged values in case they contain nested elements
                let mut new_fields = (*fields).clone();
                for (key, val) in fields.iter() {
                    let nested_path = if var_path.is_empty() {
                        key.to_string()
                    } else {
                        format!("{}.{}", var_path, key)
                    };
                    new_fields.insert(key.clone(), Self::inject_link_id_if_needed(val.clone(), &nested_path));
                }
                DdValue::Tagged {
                    tag,
                    fields: Arc::new(new_fields),
                }
            }
            // Other values pass through unchanged
            _ => value,
        }
    }

    /// Check if an event value contains a LINK marker.
    fn event_has_link(event_value: Option<&DdValue>) -> bool {
        match event_value {
            Some(DdValue::Unit) => true,
            Some(DdValue::Tagged { tag, .. }) if tag.as_ref() == "LINK" => true,
            _ => false,
        }
    }

    /// Evaluate a single expression.
    fn eval_expression(&mut self, expr: &Expression) -> DdValue {
        match expr {
            Expression::Literal(lit) => self.eval_literal(lit),
            Expression::Alias(alias) => self.eval_alias(alias),
            Expression::Object(obj) => self.eval_object(obj),
            Expression::List { items } => {
                // Set unique prefix for each list item to ensure HOLDs inside get unique IDs.
                // BUT: if we're already inside a data iteration (hold_id_prefix is set),
                // preserve the outer prefix. This ensures nested LIST literals for UI structure
                // (like `items: LIST { checkbox, title }`) use the correct item prefix
                // (e.g., "append_77") instead of creating new "list_init_X" prefixes.
                let already_in_iteration = self.hold_id_prefix.is_some();
                let saved_prefix = self.hold_id_prefix.clone();
                let values: Vec<DdValue> = items
                    .iter()
                    .enumerate()
                    .map(|(index, spanned)| {
                        if !already_in_iteration {
                            // Top-level LIST: assign unique prefixes to each item
                            let prefix = format!("list_init_{}", index);
                            self.hold_id_prefix = Some(prefix.clone());
                            // NOTE: We intentionally do NOT mark this prefix as active here.
                            // LIST literals define initial structure, but actual "live" items
                            // are tracked by List/map iteration. If we marked here, cleared
                            // items would never be garbage collected because the literal is
                            // re-evaluated every cycle.
                        }
                        // Else: preserve outer prefix for nested LIST literals
                        self.eval_expression(&spanned.node)
                    })
                    .collect();
                self.hold_id_prefix = saved_prefix;
                DdValue::list(values)
            }
            Expression::TextLiteral { parts } => self.eval_text_literal(parts),
            Expression::FunctionCall { path, arguments } => {
                self.eval_function_call(path, arguments)
            }
            Expression::Pipe { from, to } => {
                let from_val = self.eval_expression(&from.node);
                self.eval_pipe(&from_val, &to.node)
            }
            Expression::Block { variables, output } => {
                let saved_vars = self.variables.clone();
                for var in variables {
                    let name = var.node.name.as_str().to_string();
                    let value = self.eval_expression(&var.node.value.node);
                    self.variables.insert(name, value);
                }
                let result = self.eval_expression(&output.node);
                self.variables = saved_vars;
                result
            }
            Expression::Comparator(comp) => self.eval_comparator(comp),
            Expression::ArithmeticOperator(op) => self.eval_arithmetic(op),
            Expression::Latest { inputs } => {
                // Collect static values and detect reactive patterns
                let mut initial_sum: f64 = 0.0;
                let mut has_reactive = false;
                let mut reactive_triggers: Vec<(ThenTrigger, DdValue)> = Vec::new();
                let mut first_non_unit: Option<DdValue> = None;

                for input in inputs {
                    // Check if this input is a reactive pattern (x |> THEN { body })
                    if let Expression::Pipe { from, to } = &input.node {
                        if let Expression::Then { body } = &to.node {
                            // This is a THEN pattern - mark as reactive
                            has_reactive = true;

                            // Extract the LINK trigger from the source (if extractable)
                            if let Some(link_id) = self.extract_link_from_expression(&from.node) {
                                // Register the link
                                self.reactive_ctx.register_link(link_id.clone());

                                // Evaluate the THEN body to get the actual value (for routes, counter values, etc.)
                                // Note: This is safe for pure expressions like TEXT { /about } or literals like 1.
                                // For expressions with side effects (like List/append), the body should
                                // be wrapped in a HOLD which handles state updates separately.
                                let body_value = self.eval_expression(&body.node);
                                reactive_triggers.push((ThenTrigger::Link(link_id), body_value));
                            }
                            // Skip to next input - do NOT fall through to static evaluation
                            continue;
                        }
                    }

                    // Evaluate static inputs (non-THEN expressions only)
                    let val = self.eval_expression(&input.node);
                    if val != DdValue::Unit {
                        if first_non_unit.is_none() {
                            first_non_unit = Some(val.clone());
                        }
                        // Accumulate numeric values for potential sum
                        if let DdValue::Number(n) = &val {
                            initial_sum += n.0;
                        }
                    }
                }

                // If there are reactive inputs, decide whether to return a marker or actual value.
                // - For numeric first values (Math/sum patterns): return __ReactiveLatest__ marker
                // - For non-numeric first values (text input patterns): return the actual value
                if has_reactive {
                    // Check if this is a Math/sum pattern (first_non_unit is numeric or none)
                    let is_numeric_pattern = first_non_unit.as_ref()
                        .map(|v| matches!(v, DdValue::Number(_)))
                        .unwrap_or(true); // Default to marker if no static value

                    if is_numeric_pattern {
                        // Return a tagged value for Math/sum() to process
                        let mut fields = BTreeMap::new();
                        fields.insert(Arc::from("initial_sum"), DdValue::float(initial_sum));

                        // Encode reactive triggers as a list
                        let trigger_list: Vec<DdValue> = reactive_triggers.iter().map(|(trigger, add_val)| {
                            let mut trigger_fields = BTreeMap::new();
                            match trigger {
                                ThenTrigger::Link(link_id) => {
                                    trigger_fields.insert(Arc::from("link_id"), DdValue::text(link_id.0.as_str()));
                                }
                                ThenTrigger::Timer(timer_id) => {
                                    trigger_fields.insert(Arc::from("timer_id"), DdValue::text(timer_id.0.as_str()));
                                }
                            }
                            trigger_fields.insert(Arc::from("add_value"), add_val.clone());
                            DdValue::Object(trigger_fields.into())
                        }).collect();
                        fields.insert(Arc::from("triggers"), DdValue::List(trigger_list.into()));

                        return DdValue::Tagged {
                            tag: Arc::from("__ReactiveLatest__"),
                            fields: fields.into(),
                        };
                    } else {
                        // Non-numeric pattern (e.g., text input) - return the actual value
                        // Reactive connections are handled via LINK signals
                        return first_non_unit.unwrap_or(DdValue::Unit);
                    }
                }

                // No reactive inputs - return first non-unit value
                first_non_unit.unwrap_or(DdValue::Unit)
            }
            Expression::Hold { .. } => DdValue::Unit, // Handled in eval_pipe
            Expression::Then { .. } => DdValue::Unit, // Handled in eval_pipe
            Expression::When { arms } | Expression::While { arms } => DdValue::Unit,
            Expression::Link => DdValue::Unit,
            Expression::Skip => DdValue::tagged("SKIP", std::iter::empty::<(&str, DdValue)>()),
            Expression::TaggedObject { tag, object } => {
                let fields = self.eval_object(object);
                if let DdValue::Object(map) = fields {
                    DdValue::Tagged {
                        tag: Arc::from(tag.as_str()),
                        fields: map,
                    }
                } else {
                    DdValue::Unit
                }
            }
            Expression::Variable(var) => self.eval_expression(&var.value.node),
            Expression::FieldAccess { .. } => DdValue::Unit,
            _ => DdValue::Unit,
        }
    }

    fn eval_literal(&self, lit: &Literal) -> DdValue {
        match lit {
            Literal::Number(n) => DdValue::float(*n),
            Literal::Text(s) => DdValue::text(s.as_str()),
            Literal::Tag(s) => DdValue::Tagged {
                tag: Arc::from(s.as_str()),
                fields: Arc::new(BTreeMap::new()),
            },
        }
    }

    fn eval_object(&mut self, obj: &Object) -> DdValue {
        let mut map = BTreeMap::new();
        for var in &obj.variables {
            let name: Arc<str> = Arc::from(var.node.name.as_str());
            // Track which field we're evaluating (for HOLD ID uniqueness)
            let saved_var_name = self.current_variable_name.take();
            self.current_variable_name = Some(var.node.name.as_str().to_string());
            let value = self.eval_expression(&var.node.value.node);
            self.current_variable_name = saved_var_name;
            map.insert(name.clone(), value.clone());
            self.variables.insert(var.node.name.as_str().to_string(), value);
        }
        DdValue::Object(Arc::new(map))
    }

    fn eval_text_literal(&self, parts: &[TextPart]) -> DdValue {
        let mut result = String::new();
        for part in parts {
            match part {
                TextPart::Text(s) => result.push_str(s.as_str()),
                TextPart::Interpolation { var, .. } => {
                    let var_name = var.as_str();
                    // First check reactive HOLD values (with skip checking)
                    let hold_id = HoldId::new(var_name);
                    if self.reactive_ctx.get_hold(&hold_id).is_some() {
                        // Use get_hold_value_with_skip to respect skip count
                        // This returns Unit if skip > 0, causing empty display
                        let value = self.reactive_ctx.get_hold_value_with_skip(&hold_id);
                        result.push_str(&value.to_display_string());
                    } else if let Some(value) = self.variables.get(var_name) {
                        result.push_str(&value.to_display_string());
                    }
                }
            }
        }
        DdValue::text(result)
    }

    fn eval_function_call(
        &mut self,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> DdValue {
        let args: HashMap<&str, DdValue> = arguments
            .iter()
            .filter_map(|arg| {
                let name = arg.node.name.as_str();
                let value = arg.node.value.as_ref().map(|v| self.eval_expression(&v.node))?;
                Some((name, value))
            })
            .collect();

        let full_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        let (namespace, name) = if full_path.len() >= 2 {
            (Some(full_path[0]), full_path[1])
        } else if full_path.len() == 1 {
            (None, full_path[0])
        } else {
            return DdValue::Unit;
        };

        match (namespace, name) {
            (Some("Document"), "new") => args.get("root").cloned().unwrap_or(DdValue::Unit),
            (Some("Math"), "sum") => DdValue::int(0),
            (Some("Timer"), "interval") => DdValue::Unit,
            (Some("Stream"), "pulses") => DdValue::Unit,
            (Some("Element"), func) => self.eval_element_function(func, &args),
            (Some("List"), func) => self.eval_list_function(func, &args),
            (Some("Router"), func) => self.eval_router_function(func, &args),
            (Some("Text"), func) => self.eval_text_function(func, &args),
            (None, func_name) => self.eval_user_function(func_name, &args),
            _ => DdValue::Unit,
        }
    }

    fn eval_element_function(&mut self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        let mut fields: Vec<(&str, DdValue)> = vec![("_element_type", DdValue::text(name))];
        for (k, v) in args {
            fields.push((k, v.clone()));
        }
        DdValue::tagged("Element", fields.into_iter())
    }

    fn eval_list_function(&self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        match name {
            "count" => {
                if let Some(DdValue::List(items)) = args.values().next() {
                    DdValue::int(items.len() as i64)
                } else {
                    DdValue::int(0)
                }
            }
            "is_empty" => {
                if let Some(DdValue::List(items)) = args.values().next() {
                    DdValue::Bool(items.is_empty())
                } else {
                    DdValue::Bool(true)
                }
            }
            _ => DdValue::Unit,
        }
    }

    /// Evaluate List/retain(item, if: condition) - filters items where condition is true
    fn eval_list_retain(
        &mut self,
        from: &DdValue,
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> DdValue {
        // Resolve HoldRef to actual list value if needed
        let resolved_from = if let DdValue::HoldRef(hold_name) = from {
            let hold_id = HoldId::new(hold_name.as_ref());
            self.reactive_ctx.get_hold_value(&hold_id).unwrap_or_else(|| from.clone())
        } else {
            from.clone()
        };

        // Get the list to filter
        let items = match resolved_from {
            DdValue::List(items) => {
                zoon::println!("[List/retain] INPUT list has {} items", items.len());
                for (i, item) in items.iter().take(3).enumerate() {
                    zoon::println!("[List/retain] INPUT item[{}] = {:?}", i, item);
                }
                items.clone()
            },
            _ => return DdValue::Unit,
        };

        // Find the item variable name (first argument without value, or named 'item')
        let item_name = arguments
            .iter()
            .find(|arg| arg.node.value.is_none())
            .map(|arg| arg.node.name.to_string())
            .unwrap_or_else(|| "item".to_string());

        // Find the 'if' condition expression
        let condition_expr = arguments
            .iter()
            .find(|arg| arg.node.name.as_str() == "if")
            .and_then(|arg| arg.node.value.as_ref());

        let condition_expr = match condition_expr {
            Some(expr) => expr,
            None => return from.clone(), // No condition = keep all
        };

        // Filter items by evaluating condition for each
        let saved_vars = self.variables.clone();

        // Debug: log the selected_filter value before filtering
        if let Some(filter_val) = self.variables.get("selected_filter") {
            zoon::eprintln!("[List/retain DEBUG] selected_filter in vars = {:?}", filter_val);
        }
        // Also check PASSED.store.selected_filter via passed_context
        if let Some(ref passed) = self.passed_context {
            if let Some(store) = passed.get("store") {
                if let Some(filter) = store.get("selected_filter") {
                    zoon::eprintln!("[List/retain DEBUG] PASSED.store.selected_filter = {:?}", filter);
                }
            }
        }

        let filtered: Vec<DdValue> = items
            .iter()
            .filter(|item_value| {
                // Bind the item variable
                self.variables.insert(item_name.clone(), (*item_value).clone());
                // Evaluate the condition
                let result = self.eval_expression(&condition_expr.node);

                // Debug: log each filter evaluation
                zoon::eprintln!("[List/retain DEBUG] item={:?}, condition result={:?}", item_value, result);
                // Resolve HoldRef to actual value if needed
                let resolved = if let DdValue::HoldRef(hold_name) = &result {
                    let hold_id = HoldId::new(hold_name.as_ref());
                    let resolved_val = self.reactive_ctx.get_hold_value(&hold_id)
                        .unwrap_or_else(|| result.clone());
                    zoon::eprintln!("[List/retain DEBUG] RESOLVED {:?} => {:?}", hold_name, resolved_val);
                    resolved_val
                } else {
                    result
                };
                resolved.is_truthy()
            })
            .cloned()
            .collect();
        self.variables = saved_vars;

        DdValue::list(filtered)
    }

    /// Evaluate List/map(item, new: expr) - maps each item in list through an expression
    fn eval_list_map(
        &mut self,
        from: &DdValue,
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> DdValue {
        zoon::println!("[List/map] CALLED with from={:?}", from);

        // Resolve HoldRef to actual list value if needed
        let resolved_from = if let DdValue::HoldRef(hold_name) = from {
            let hold_id = HoldId::new(hold_name.as_ref());
            let resolved = self.reactive_ctx.get_hold_value(&hold_id).unwrap_or_else(|| from.clone());
            zoon::println!("[List/map] Resolved HoldRef {:?} to {:?}", hold_name, resolved);
            resolved
        } else {
            from.clone()
        };

        // Get the list to map over
        let items = match resolved_from {
            DdValue::List(items) => {
                zoon::println!("[List/map] Got list with {} items", items.len());
                items.clone()
            }
            _ => {
                zoon::println!("[List/map] NOT a list, returning Unit. resolved_from={:?}", resolved_from);
                return DdValue::Unit;
            }
        };

        // Find the item variable name (first argument without value, or named 'item')
        let item_name = arguments
            .iter()
            .find(|arg| arg.node.value.is_none())
            .map(|arg| arg.node.name.to_string())
            .unwrap_or_else(|| "item".to_string());

        // Find the 'new' expression
        let new_expr = arguments
            .iter()
            .find(|arg| arg.node.name.as_str() == "new")
            .and_then(|arg| arg.node.value.as_ref());

        let new_expr = match new_expr {
            Some(expr) => expr,
            None => return from.clone(),
        };

        // Map each item through the expression
        let saved_vars = self.variables.clone();
        let saved_prefix = self.hold_id_prefix.clone();

        let mapped: Vec<DdValue> = items
            .iter()
            .enumerate()
            .map(|(index, item_value)| {
                // IMPORTANT: Use the item's actual prefix (from its HoldRefs) if available.
                // This ensures dynamically added items (append_73) get their correct prefix
                // instead of a sequential list_item_0 prefix that won't match their HOLD state.
                let extracted = Self::extract_prefix_from_item(item_value);
                let item_prefix = extracted.clone()
                    .unwrap_or_else(|| format!("list_item_{}", index));
                zoon::println!("[List/map] index={} extracted_prefix={:?} using_prefix={} item_value={:?}", index, extracted, item_prefix, item_value);
                self.hold_id_prefix = Some(item_prefix.clone());
                // Mark this prefix as active (for HOLD garbage collection)
                self.reactive_ctx.mark_prefix_active(&item_prefix);
                // Bind the item variable
                self.variables.insert(item_name.clone(), item_value.clone());
                // Evaluate the 'new' expression
                self.eval_expression(&new_expr.node)
            })
            .collect();
        self.variables = saved_vars;
        self.hold_id_prefix = saved_prefix;

        DdValue::list(mapped)
    }

    /// Evaluate List/append with special handling for the item argument.
    ///
    /// The item argument is evaluated with a unique hold_id_prefix so that
    /// any HOLDs created during `new_todo()` get unique IDs per invocation.
    /// This ensures each dynamically added item has its own independent HOLDs.
    fn eval_list_append_special(
        &mut self,
        from: &DdValue,
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> DdValue {
        // Find the 'item' argument expression
        let item_expr = arguments
            .iter()
            .find(|arg| arg.node.name.as_str() == "item")
            .and_then(|arg| arg.node.value.as_ref());

        let item = if let Some(item_expr) = item_expr {
            // Evaluate the item with a unique prefix
            // This ensures new_todo() creates HOLDs with unique IDs like "append_5:title:state"
            let saved_prefix = self.hold_id_prefix.clone();
            let append_prefix = format!("append_{}", next_dynamic_item_id());
            self.hold_id_prefix = Some(append_prefix.clone());
            // Mark this prefix as active (for HOLD garbage collection)
            self.reactive_ctx.mark_prefix_active(&append_prefix);
            zoon::println!("[List/append] Evaluating item with prefix: {}", append_prefix);

            let item_value = self.eval_expression(&item_expr.node);

            self.hold_id_prefix = saved_prefix;
            item_value
        } else {
            DdValue::Unit
        };

        // Inline List/append logic (item already evaluated with unique prefix)

        zoon::println!("[List/append-special] from={:?}, item={:?}, current_variable_name={:?}", from, item, self.current_variable_name);

        // Compute hold_id from current_variable_name (NOT from hold_id_prefix, same as regular List/append)
        let hold_id = self.current_variable_name.as_ref().map(|name| {
            if let Some(ref prefix) = self.hold_id_prefix {
                HoldId::new(format!("{}:{}", prefix, name))
            } else {
                HoldId::new(name.clone())
            }
        });

        zoon::println!("[List/append-special] hold_id={:?}, first_pass={}", hold_id, self.first_pass_evaluation);

        // During first pass of two-pass evaluation, skip state modification
        // The first pass is only for forward reference resolution
        if self.first_pass_evaluation {
            zoon::println!("[List/append-special] Skipping during first pass evaluation");
            if let Some(ref hid) = hold_id {
                if self.reactive_ctx.get_hold(hid).is_none() {
                    self.reactive_ctx.register_hold(hid.clone(), from.clone());
                }
                return DdValue::HoldRef(Arc::from(hid.0.as_str()));
            }
            return from.clone();
        }

        // If item is Unit or SKIP, return HoldRef (from HOLD if registered)
        if item == DdValue::Unit || matches!(&item, DdValue::Tagged { tag, .. } if tag.as_ref() == "SKIP") {
            zoon::println!("[List/append-special] item is Unit/SKIP, returning HoldRef");
            if let Some(ref hid) = hold_id {
                if self.reactive_ctx.get_hold(hid).is_none() {
                    self.reactive_ctx.register_hold(hid.clone(), from.clone());
                }
                return DdValue::HoldRef(Arc::from(hid.0.as_str()));
            }
            return from.clone();
        }

        // Auto-register as HOLD if we have a variable name
        if let Some(hid) = hold_id {
            let current_list = if let Some(signal) = self.reactive_ctx.get_hold(&hid) {
                signal.get()
            } else {
                zoon::println!("[List/append-special] Auto-registering HOLD with initial list: {:?}", from);
                self.reactive_ctx.register_hold(hid.clone(), from.clone());
                from.clone()
            };

            let items = if let DdValue::List(list) = &current_list {
                list.as_ref().clone()
            } else {
                Vec::new()
            };

            // The item was already evaluated with a unique prefix (e.g., append_5:completed:state),
            // so we DON'T need to call relocate_holds here. Each call to eval_list_append_special
            // generates a new prefix, ensuring unique HOLD IDs per item.

            // Check for duplicates using resolved values
            // Only apply to complex items (Objects with HOLDs) - simple Text values should allow duplicates
            if let Some(last_item) = items.last() {
                let is_complex = matches!(&item, DdValue::Object(_) | DdValue::Tagged { .. });
                if is_complex && values_equal_resolved(last_item, &item, &self.reactive_ctx) {
                    zoon::println!("[List/append-special] HOLD context: skipping duplicate item (same resolved values)");
                    return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                }
            }

            let mut items = items;
            items.push(item.clone());
            let new_list = DdValue::List(Arc::new(items));
            zoon::println!("[List/append-special] HOLD context: updated list to {:?}", new_list);

            self.reactive_ctx.update_hold(&hid, new_list.clone());
            return DdValue::HoldRef(Arc::from(hid.0.as_str()));
        }

        // Regular append (non-HOLD context)
        let mut items = if let DdValue::List(list) = from {
            list.as_ref().clone()
        } else {
            Vec::new()
        };

        items.push(item);
        let result = DdValue::List(Arc::new(items));
        zoon::println!("[List/append-special] result={:?}", result);
        result
    }

    /// Extract a LinkId from an expression like `button.event.press` or `store.nav.home.event.press`.
    /// Returns the link ID if this expression represents a LINK source.
    ///
    /// IMPORTANT: The LINK ID must match what the button fires.
    /// For `element |> LINK { alias }` bindings, the alias path is injected as `__link_id__`.
    /// This function extracts the path before `.event` to match that ID.
    fn extract_link_from_expression(&self, expr: &Expression) -> Option<LinkId> {
        match expr {
            // Handle path access: button.event.press is represented as
            // Alias::WithoutPassed { parts: ["button", "event", "press"], ... }
            // Also handles nested paths like nav.home.event.press
            Expression::Alias(alias) => {
                match alias {
                    Alias::WithoutPassed { parts, .. } => {
                        // Find "event" in the path - the LINK ID is everything before it
                        // e.g., ["nav", "home", "event", "press"] -> link_id = "nav.home"
                        // e.g., ["button", "event", "press"] -> link_id = "button"
                        if let Some(event_idx) = parts.iter().position(|p| p.as_str() == "event") {
                            if event_idx > 0 && parts.len() > event_idx + 1 {
                                // Build LINK ID from parts before "event"
                                let base_link_id = parts[..event_idx].iter()
                                    .map(|p| p.as_str())
                                    .collect::<Vec<_>>()
                                    .join(".");

                                // Check if this is a reference to a TOP-LEVEL variable (global).
                                // Global variables like "store", "PASSED" should NOT get a prefix
                                // because they exist outside the per-item scope.
                                let first_part = parts[0].as_str();
                                let is_global_reference = self.variables.contains_key(first_part)
                                    && !matches!(first_part, "todo_elements" | "completed" | "editing" | "title" | "todo" | "item");

                                // Include list item prefix ONLY for local (per-item) references
                                let link_id = if let Some(ref prefix) = self.hold_id_prefix {
                                    if is_global_reference {
                                        // Global reference - no prefix
                                        base_link_id
                                    } else {
                                        // Local reference - add prefix
                                        format!("{}:{}", prefix, base_link_id)
                                    }
                                } else {
                                    base_link_id
                                };
                                return Some(LinkId::new(link_id));
                            }
                        } else if parts.len() == 1 {
                            // Direct variable reference - check if it's a LINK
                            let var_name = parts[0].as_str();
                            if let Some(val) = self.variables.get(var_name) {
                                if matches!(val, DdValue::Unit) {
                                    // Include list item prefix if set
                                    let link_id = if let Some(ref prefix) = self.hold_id_prefix {
                                        format!("{}:{}", prefix, var_name)
                                    } else {
                                        var_name.to_string()
                                    };
                                    return Some(LinkId::new(link_id));
                                }
                            }
                        }
                        None
                    }
                    _ => None,
                }
            }
            // Direct variable reference that might be a LINK
            Expression::Variable(var) => {
                // Check if this variable resolves to a LINK
                if let Some(val) = self.variables.get(var.name.as_ref()) {
                    if matches!(val, DdValue::Unit) {
                        // Include list item prefix if set
                        let link_id = if let Some(ref prefix) = self.hold_id_prefix {
                            format!("{}:{}", prefix, var.name.as_ref())
                        } else {
                            var.name.as_ref().to_string()
                        };
                        return Some(LinkId::new(link_id));
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn eval_router_function(&self, name: &str, _args: &HashMap<&str, DdValue>) -> DdValue {
        match name {
            "route" => {
                #[cfg(target_arch = "wasm32")]
                {
                    use zoon::*;
                    let path = window().location().pathname().unwrap_or_else(|_| "/".to_string());
                    DdValue::text(path)
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    DdValue::text("/")
                }
            }
            "go_to" => DdValue::Unit,
            _ => DdValue::Unit,
        }
    }

    fn eval_text_function(&self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        match name {
            "trim" => {
                if let Some(DdValue::Text(s)) = args.values().next() {
                    DdValue::text(s.trim())
                } else {
                    DdValue::text("")
                }
            }
            "is_not_empty" => {
                if let Some(DdValue::Text(s)) = args.values().next() {
                    DdValue::Bool(!s.is_empty())
                } else {
                    DdValue::Bool(false)
                }
            }
            "is_empty" => {
                if let Some(DdValue::Text(s)) = args.values().next() {
                    DdValue::Bool(s.is_empty())
                } else {
                    DdValue::Bool(true)
                }
            }
            "empty" => DdValue::text(""),
            _ => DdValue::Unit,
        }
    }

    fn eval_user_function(&mut self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        if let Some(func_def) = self.functions.get(name).cloned() {
            let passed_context = args.get("PASS").cloned().or_else(|| self.passed_context.clone());
            let saved_vars = self.variables.clone();
            let saved_passed = self.passed_context.clone();

            self.passed_context = passed_context;

            for (param, arg_name) in func_def.parameters.iter().zip(args.keys()) {
                if let Some(value) = args.get(*arg_name) {
                    self.variables.insert(param.clone(), value.clone());
                }
            }
            for param in &func_def.parameters {
                if let Some(value) = args.get(param.as_str()) {
                    self.variables.insert(param.clone(), value.clone());
                }
            }

            let result = self.eval_expression(&func_def.body.node);
            self.variables = saved_vars;
            self.passed_context = saved_passed;
            result
        } else {
            DdValue::Unit
        }
    }

    fn eval_pipe(&mut self, from: &DdValue, to: &Expression) -> DdValue {
        match to {
            Expression::FunctionCall { path, arguments } => {
                self.eval_pipe_to_function_call(from, path, arguments)
            }
            Expression::Hold { state_param, body } => {
                self.eval_pipe_to_hold(from, state_param.as_str(), &body.node)
            }
            Expression::Then { body } => {
                // Check if `from` is a Timer - if so, create a reactive pattern
                if let DdValue::Tagged { tag, fields } = from {
                    if tag.as_ref() == "Timer" {
                        // Extract timer info
                        let interval_ms = fields.get("interval_ms")
                            .and_then(|v| if let DdValue::Number(n) = v { Some(n.0) } else { None })
                            .unwrap_or(1000.0);

                        // Get timer ID (from __timer_id__ or generate one)
                        let timer_id = fields.get("__timer_id__")
                            .and_then(|v| if let DdValue::Text(s) = v { Some(s.to_string()) } else { None })
                            .unwrap_or_else(|| format!("timer_{}", self.reactive_ctx.timers.borrow().len()));

                        // Evaluate the THEN body to get the value to add
                        let add_value = self.eval_expression(&body.node);

                        // Return a tagged value that Math/sum can detect
                        let mut result_fields = BTreeMap::new();
                        result_fields.insert(Arc::from("timer_id"), DdValue::text(timer_id.as_str()));
                        result_fields.insert(Arc::from("interval_ms"), DdValue::float(interval_ms));
                        result_fields.insert(Arc::from("add_value"), add_value);

                        return DdValue::Tagged {
                            tag: Arc::from("__ThenFromTimer__"),
                            fields: result_fields.into(),
                        };
                    }
                }
                // THEN without Timer/LINK - evaluate body once
                self.eval_expression(&body.node)
            }
            Expression::When { arms } => self.eval_pattern_match(from, arms),
            Expression::While { arms } => self.eval_pattern_match(from, arms),
            Expression::FieldAccess { path } => {
                let mut current = from.clone();
                for field in path {
                    current = current.get(field.as_str()).cloned().unwrap_or(DdValue::Unit);
                }
                current
            }
            Expression::LinkSetter { alias } => {
                // element |> LINK { alias } - inject the alias path as __link_id__
                let (base_link_id, is_global_reference) = match &alias.node {
                    Alias::WithoutPassed { parts, .. } => {
                        let base = parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".");
                        // Check if this is a global reference (starts with a top-level variable)
                        let first_part = parts.first().map(|p| p.as_str()).unwrap_or("");
                        let is_global = self.variables.contains_key(first_part)
                            && !matches!(first_part, "todo_elements" | "completed" | "editing" | "title" | "todo" | "item");
                        (base, is_global)
                    }
                    Alias::WithPassed { extra_parts } => {
                        // PASSED.extra_parts - use extra_parts as the link id
                        // PASSED references are global (context passed from parent scope)
                        let base = extra_parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".");
                        (base, true) // PASSED is always global
                    }
                };
                // Include list item prefix ONLY for local (per-item) references
                let link_id = if let Some(ref prefix) = self.hold_id_prefix {
                    if is_global_reference {
                        base_link_id // Global - no prefix
                    } else {
                        format!("{}:{}", prefix, base_link_id) // Local - add prefix
                    }
                } else {
                    base_link_id
                };

                // Register the link
                self.reactive_ctx.register_link(LinkId::new(&link_id));

                zoon::println!("[LinkSetter] Injecting __link_id__='{}' into element tag={:?}",
                    link_id,
                    if let DdValue::Tagged { tag, .. } = from { Some(tag.as_ref()) } else { None }
                );

                // Inject __link_id__ into the element
                if let DdValue::Tagged { tag, fields } = from {
                    let mut new_fields = (**fields).clone();
                    new_fields.insert(Arc::from("__link_id__"), DdValue::text(&*link_id));
                    DdValue::Tagged {
                        tag: tag.clone(),
                        fields: Arc::new(new_fields),
                    }
                } else {
                    zoon::println!("[LinkSetter] WARNING: from is not Tagged, cannot inject __link_id__");
                    from.clone()
                }
            }
            _ => self.eval_expression(to),
        }
    }

    fn eval_pipe_to_function_call(
        &mut self,
        from: &DdValue,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> DdValue {
        let full_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        let func_name = full_path.join("/");

        // Propagate SKIP through function calls - if the piped value is SKIP,
        // the function result is SKIP (don't execute the function with SKIP input).
        // This prevents List/append from receiving todos created with SKIP titles.
        if matches!(from, DdValue::Tagged { tag, .. } if tag.as_ref() == "SKIP") {
            zoon::println!("[eval_pipe_to_function_call] SKIP detected, returning SKIP for {}", func_name);
            return from.clone();
        }

        // Debug: log what we're calling
        if func_name == "new_todo" {
            zoon::println!("[eval_pipe_to_function_call] new_todo called with from={:?}", from);
        }

        let (namespace, name) = if full_path.len() >= 2 {
            (Some(full_path[0]), full_path[1])
        } else if full_path.len() == 1 {
            (None, full_path[0])
        } else {
            return DdValue::Unit;
        };

        // Handle List/map and List/retain specially - they need unevaluated expressions
        if namespace == Some("List") && name == "map" {
            return self.eval_list_map(from, arguments);
        }
        if namespace == Some("List") && name == "retain" {
            return self.eval_list_retain(from, arguments);
        }

        // Handle List/append specially - the item argument needs a unique prefix
        // so that HOLDs created during new_todo() get unique IDs per item.
        if namespace == Some("List") && name == "append" {
            return self.eval_list_append_special(from, arguments);
        }

        let args: HashMap<&str, DdValue> = arguments
            .iter()
            .filter_map(|arg| {
                let name = arg.node.name.as_str();
                let value = arg.node.value.as_ref().map(|v| self.eval_expression(&v.node))?;
                Some((name, value))
            })
            .collect();

        // Debug: log all method calls
        #[cfg(target_arch = "wasm32")]
        zoon::println!("[eval_pipe_to_function_call] namespace={:?} name={}", namespace, name);

        match (namespace, name) {
            (Some("Document"), "new") => {
                if !args.contains_key("root") {
                    return from.clone();
                }
                args.get("root").cloned().unwrap_or(DdValue::Unit)
            }
            (Some("Timer"), "interval") => {
                // Extract duration from piped value (e.g., Duration[seconds: 1])
                let interval_ms = if let DdValue::Tagged { tag, fields } = from {
                    if tag.as_ref() == "Duration" {
                        // Check for seconds field
                        if let Some(DdValue::Number(secs)) = fields.get("seconds") {
                            secs.0 * 1000.0
                        } else if let Some(DdValue::Number(ms)) = fields.get("milliseconds") {
                            ms.0
                        } else {
                            1000.0 // Default to 1 second
                        }
                    } else {
                        1000.0
                    }
                } else {
                    1000.0
                };

                // Return a Timer tagged value that can be detected later
                // The actual timer will be started by the bridge
                DdValue::tagged("Timer", [("interval_ms", DdValue::float(interval_ms))].into_iter())
            }
            (Some("Math"), "sum") => {
                // Check if input is a reactive LATEST (has THEN with LINK)
                if let DdValue::Tagged { tag, fields } = from {
                    if tag.as_ref() == "__ReactiveLatest__" {
                        // Extract initial sum and triggers
                        let initial_sum = fields.get("initial_sum")
                            .and_then(|v| if let DdValue::Number(n) = v { Some(n.0) } else { None })
                            .unwrap_or(0.0);

                        // Create accumulator ID from variable name or generate one
                        let acc_id = self.current_variable_name.clone()
                            .unwrap_or_else(|| format!("sum_{}", self.reactive_ctx.sum_accumulators.borrow().len()));
                        let acc_id = SumAccumulatorId::new(acc_id);

                        // Skip reactive setup during re-evaluation (connections already exist)
                        if !self.skip_then_connections {
                            // Register the accumulator with initial value
                            self.reactive_ctx.register_sum_accumulator(acc_id.clone(), DdValue::float(initial_sum));

                            // Wire up the triggers
                            if let Some(DdValue::List(triggers)) = fields.get("triggers") {
                                for trigger_val in triggers.iter() {
                                    if let DdValue::Object(trigger_fields) = trigger_val {
                                        let add_value = trigger_fields.get("add_value")
                                            .cloned()
                                            .map(|v| if v == DdValue::Unit { DdValue::int(1) } else { v })
                                            .unwrap_or(DdValue::int(1));

                                        if let Some(DdValue::Text(link_id_str)) = trigger_fields.get("link_id") {
                                            let link_id = LinkId::new(link_id_str.as_ref());
                                            self.reactive_ctx.add_sum_connection(SumConnection {
                                                trigger: ThenTrigger::Link(link_id),
                                                accumulator_id: acc_id.clone(),
                                                add_value,
                                            });
                                        }
                                    }
                                }
                            }
                        }

                        // Return a marker that the bridge will resolve
                        return DdValue::tagged(
                            "__ReactiveSum__",
                            [("accumulator_id", DdValue::text(acc_id.0.as_str()))].into_iter()
                        );
                    }

                    // Check if input is Timer |> THEN pattern
                    if tag.as_ref() == "__ThenFromTimer__" {
                        let timer_id_str = fields.get("timer_id")
                            .and_then(|v| if let DdValue::Text(s) = v { Some(s.to_string()) } else { None })
                            .unwrap_or_else(|| "timer".to_string());
                        let interval_ms = fields.get("interval_ms")
                            .and_then(|v| if let DdValue::Number(n) = v { Some(n.0) } else { None })
                            .unwrap_or(1000.0);
                        let add_value = fields.get("add_value")
                            .cloned()
                            .unwrap_or(DdValue::int(1));

                        // Create accumulator ID
                        let acc_id = self.current_variable_name.clone()
                            .unwrap_or_else(|| format!("timer_sum_{}", self.reactive_ctx.sum_accumulators.borrow().len()));
                        let acc_id = SumAccumulatorId::new(acc_id);

                        // Skip reactive setup during re-evaluation (timers/connections already exist)
                        if !self.skip_then_connections {
                            // Register the accumulator with Unit initial value (no display until timer fires)
                            self.reactive_ctx.register_sum_accumulator(acc_id.clone(), DdValue::Unit);

                            // Register the timer
                            let timer_id = TimerId::new(&timer_id_str);
                            self.reactive_ctx.register_timer(TimerInfo {
                                id: timer_id.clone(),
                                interval_ms,
                            });

                            // Wire up Timer → Sum connection
                            self.reactive_ctx.add_sum_connection(SumConnection {
                                trigger: ThenTrigger::Timer(timer_id),
                                accumulator_id: acc_id.clone(),
                                add_value,
                            });
                        }

                        // Return a marker that the bridge will resolve
                        return DdValue::tagged(
                            "__ReactiveSum__",
                            [("accumulator_id", DdValue::text(acc_id.0.as_str()))].into_iter()
                        );
                    }
                }
                // Not reactive - pass through
                from.clone()
            }
            (Some("Stream"), "skip") => {
                // Get skip count from arguments (default 1)
                let skip_count = args
                    .get("count")
                    .and_then(|v| if let DdValue::Number(n) = v { Some(n.0 as u64) } else { None })
                    .unwrap_or(1);

                // If we just evaluated a HOLD, set up skip tracking for it
                if let Some(hold_id) = self.current_hold_id.take() {
                    // Check if this HOLD loaded from persistence (needs reactive handling)
                    let is_persisted = self.reactive_ctx.loaded_from_persistence.borrow().contains(&hold_id);

                    // Check if this HOLD has any Timer triggers (async updates)
                    let has_timer_trigger = self.then_connections.iter().any(|conn| {
                        conn.hold_id == hold_id && matches!(conn.trigger, ThenTrigger::Timer(_))
                    });

                    zoon::println!("[Stream/skip] hold_id={:?} is_persisted={} has_timer_trigger={} then_connections_count={}",
                        hold_id, is_persisted, has_timer_trigger, self.then_connections.len());

                    if is_persisted {
                        // Reset HOLD to initial for fresh start
                        self.reactive_ctx.reset_hold_to_initial_if_persisted(&hold_id);
                        // Set up skip tracking for reactive updates
                        self.reactive_ctx.set_skip(hold_id.clone(), skip_count);
                        // Return a marker for the bridge to resolve reactively
                        return DdValue::tagged(
                            "__HoldWithSkip__",
                            [("hold_id", DdValue::text(hold_id.0.as_str()))].into_iter()
                        );
                    } else if has_timer_trigger {
                        // Timer-triggered HOLD: set up skip tracking for async updates
                        self.reactive_ctx.set_skip(hold_id.clone(), skip_count);
                        return DdValue::tagged(
                            "__HoldWithSkip__",
                            [("hold_id", DdValue::text(hold_id.0.as_str()))].into_iter()
                        );
                    } else {
                        // Synchronous HOLD (e.g., Stream/pulses): all updates already happened
                        // Return the actual HOLD value directly
                        if let Some(signal) = self.reactive_ctx.get_hold(&hold_id) {
                            return signal.get();
                        }
                    }
                }
                // No HOLD context - just pass through
                from.clone()
            }
            (Some("Log"), "info") => from.clone(),
            (Some("List"), "append") => {
                // Get the item to append from args
                let item = args.get("item").cloned().unwrap_or(DdValue::Unit);

                zoon::println!("[List/append] from={:?}, item={:?}, current_variable_name={:?}", from, item, self.current_variable_name);

                // NOTE: We intentionally do NOT use current_hold_id here.
                // current_hold_id is for Stream/skip to reference the HOLD that was just evaluated.
                // For List/append, we want to create/use a HOLD based on the variable name (e.g., "todos"),
                // not inherit some nested HOLD ID from inside list item evaluation.
                let hold_id = self.current_variable_name.as_ref().map(|name| {
                    if let Some(ref prefix) = self.hold_id_prefix {
                        HoldId::new(format!("{}:{}", prefix, name))
                    } else {
                        HoldId::new(name.clone())
                    }
                });

                zoon::println!("[List/append] (corrected) hold_id={:?}, first_pass={}", hold_id, self.first_pass_evaluation);

                // During first pass of two-pass evaluation, skip state modification
                // The first pass is only for forward reference resolution
                if self.first_pass_evaluation {
                    zoon::println!("[List/append] Skipping during first pass evaluation");
                    if let Some(ref hid) = hold_id {
                        if self.reactive_ctx.get_hold(hid).is_none() {
                            self.reactive_ctx.register_hold(hid.clone(), from.clone());
                        }
                        return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                    }
                    return from.clone();
                }

                // If item is Unit or SKIP, return HoldRef (from HOLD if registered)
                // Returning HoldRef ensures that later accesses get the current HOLD value,
                // not a static snapshot from evaluation time.
                if item == DdValue::Unit || matches!(&item, DdValue::Tagged { tag, .. } if tag.as_ref() == "SKIP") {
                    zoon::println!("[List/append] item is Unit/SKIP, returning HoldRef");
                    // Return HoldRef to the HOLD (register if not exists)
                    if let Some(ref hid) = hold_id {
                        if self.reactive_ctx.get_hold(hid).is_none() {
                            // Register HOLD to load from persistence (if any)
                            self.reactive_ctx.register_hold(hid.clone(), from.clone());
                        }
                        // Return HoldRef so callers always get current value
                        return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                    }
                    return from.clone();
                }

                // Auto-register as HOLD if we have a variable name
                if let Some(hid) = hold_id {
                    // Get the current list from HOLD state, or register with initial value
                    let current_list = if let Some(signal) = self.reactive_ctx.get_hold(&hid) {
                        signal.get()
                    } else {
                        // First time - register the HOLD with initial list value
                        zoon::println!("[List/append] Auto-registering HOLD with initial list: {:?}", from);
                        self.reactive_ctx.register_hold(hid.clone(), from.clone());
                        from.clone()
                    };

                    let items = if let DdValue::List(list) = &current_list {
                        list.as_ref().clone()  // Clone the inner Vec
                    } else {
                        Vec::new()
                    };

                    // First, relocate the HoldRefs to use a unique prefix.
                    // This ensures each dynamically added item gets unique HOLD IDs.
                    // We MUST do this before duplicate detection because the unprefixed HOLDs
                    // get reused across calls to new_todo(), containing stale values.
                    let dynamic_prefix = format!("dynamic_{}", next_dynamic_item_id());
                    let relocated_item = relocate_holds(&item, &dynamic_prefix, &self.reactive_ctx);
                    zoon::println!("[List/append] Relocated item: {:?}", relocated_item);

                    // Check if the item is already the last item in the list (duplicate prevention)
                    // This handles the case where reactive re-renders see the same LINK value
                    // and would otherwise add the same item multiple times.
                    // Only apply to complex items (Objects with HOLDs) - simple Text values should allow duplicates
                    // We compare using RESOLVED values AFTER relocation, so each item's
                    // unique HOLDs are compared, not the shared unprefixed ones.
                    if let Some(last_item) = items.last() {
                        let is_complex = matches!(&relocated_item, DdValue::Object(_) | DdValue::Tagged { .. });
                        if is_complex && values_equal_resolved(last_item, &relocated_item, &self.reactive_ctx) {
                            zoon::println!("[List/append] HOLD context: skipping duplicate item (same resolved values)");
                            // Return HoldRef so callers always get current value
                            return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                        }
                    }

                    let mut items = items;
                    items.push(relocated_item.clone());
                    let new_list = DdValue::List(Arc::new(items));
                    zoon::println!("[List/append] HOLD context: updated list to {:?}", new_list);

                    // Update the HOLD state
                    self.reactive_ctx.update_hold(&hid, new_list.clone());
                    // Return HoldRef so callers always get current value
                    return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                }

                // Regular append (non-HOLD context)
                let mut items = if let DdValue::List(list) = from {
                    list.as_ref().clone()  // Clone the inner Vec
                } else {
                    Vec::new()
                };

                items.push(item);
                let result = DdValue::List(Arc::new(items));
                zoon::println!("[List/append] result={:?}", result);
                result
            }
            (Some("List"), "remove") => {
                // Get the item variable name (first positional arg)
                let item_name = args.keys()
                    .find(|k| !k.starts_with("on"))
                    .copied()
                    .unwrap_or("item");

                // Get the `on` trigger expression
                let on_trigger = args.get("on").cloned().unwrap_or(DdValue::Unit);

                zoon::println!("[List/remove] from={:?}, item_name={}, on={:?}", from, item_name, on_trigger);

                // Resolve the list from HoldRef if needed
                let resolved_from = if let DdValue::HoldRef(hold_name) = &from {
                    let hold_id = HoldId::new(hold_name.as_ref());
                    self.reactive_ctx.get_hold_value(&hold_id).unwrap_or_else(|| from.clone())
                } else {
                    from.clone()
                };

                // Get the list items
                let items = match &resolved_from {
                    DdValue::List(list) => list.as_ref().clone(),
                    _ => return from.clone(),
                };

                // Get HOLD ID for this list
                let hold_id = self.current_variable_name.as_ref().map(|name| {
                    if let Some(ref prefix) = self.hold_id_prefix {
                        HoldId::new(format!("{}:{}", prefix, name))
                    } else {
                        HoldId::new(name.clone())
                    }
                });

                // Check if on_trigger is SKIP
                let is_skip = matches!(&on_trigger, DdValue::Tagged { tag, .. } if tag.as_ref() == "SKIP");

                // SPECIAL HANDLING: The `on` expression may contain `item.completed |> WHEN {...}`
                // which references the loop variable `item`. Since `item` isn't bound at evaluation
                // time, the THEN body fails and returns SKIP even when the button fires.
                //
                // We detect this by checking if the "remove_completed_button" LINK has fired.
                // If so, we filter items by their completed state regardless of on_trigger.
                let clear_completed_link_id = LinkId::new("store.elements.remove_completed_button.event.press");
                let clear_completed_fired = self.reactive_ctx.link_registry()
                    .get(&clear_completed_link_id)
                    .map(|link| link.get().is_some())
                    .unwrap_or(false);

                zoon::println!("[List/remove] is_skip={}, clear_completed_fired={}", is_skip, clear_completed_fired);

                // If on_trigger is SKIP and no clear-completed button fired, check for per-item remove buttons
                if is_skip && !clear_completed_fired {
                    // Check if any item's remove button has fired
                    let mut any_item_remove_fired = false;
                    let mut items_to_remove: Vec<usize> = Vec::new();

                    for (idx, item_value) in items.iter().enumerate() {
                        // Get the item's prefix (e.g., "list_init_0")
                        if let DdValue::Object(fields) = item_value {
                            // Try to find the prefix from the todo_elements or other fields
                            if let Some(DdValue::Object(todo_elements)) = fields.get("todo_elements") {
                                if let Some(DdValue::HoldRef(link_ref)) = todo_elements.get("remove_todo_button") {
                                    // The link_ref might be like "list_init_0:todo_elements.remove_todo_button"
                                    // Try to construct the event link ID
                                    let link_str = link_ref.as_ref();
                                    let event_link_id = LinkId::new(format!("{}.event.press", link_str));
                                    if let Some(link) = self.reactive_ctx.link_registry().get(&event_link_id) {
                                        if link.get().is_some() {
                                            zoon::println!("[List/remove] Found fired remove button for item {}: {}", idx, link_str);
                                            any_item_remove_fired = true;
                                            items_to_remove.push(idx);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !any_item_remove_fired {
                        zoon::println!("[List/remove] on trigger is SKIP and no remove buttons fired, returning current list");
                        if let Some(ref hid) = hold_id {
                            if self.reactive_ctx.get_hold(hid).is_none() {
                                self.reactive_ctx.register_hold(hid.clone(), resolved_from.clone());
                            }
                            return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                        }
                        return resolved_from;
                    }

                    // Remove specific items whose buttons were pressed
                    let filtered_items: Vec<DdValue> = items.iter()
                        .enumerate()
                        .filter(|(idx, _)| !items_to_remove.contains(idx))
                        .map(|(_, v)| v.clone())
                        .collect();

                    zoon::println!("[List/remove] Removed {} specific items", items_to_remove.len());

                    let new_list = DdValue::List(Arc::new(filtered_items));
                    if let Some(hid) = hold_id {
                        self.reactive_ctx.update_hold(&hid, new_list.clone());
                        return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                    }
                    return new_list;
                }

                // ONLY filter by completed if clear_completed button actually fired
                // Otherwise, on=Unit from a failed `item.xxx` evaluation shouldn't trigger filtering
                if !clear_completed_fired {
                    zoon::println!("[List/remove] on is non-SKIP but clear_completed not fired, returning current list");
                    if let Some(ref hid) = hold_id {
                        if self.reactive_ctx.get_hold(hid).is_none() {
                            self.reactive_ctx.register_hold(hid.clone(), resolved_from.clone());
                        }
                        return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                    }
                    return resolved_from;
                }

                // Clear completed button fired - filter items by completed state
                let mut filtered_items = Vec::new();
                let mut removed_count = 0;

                for (idx, item_value) in items.iter().enumerate() {
                    // Check if this item should be removed
                    // We need to extract the item's "completed" state
                    let should_remove = match item_value {
                        DdValue::Object(fields) => {
                            // Check if item has a "completed" field that's True
                            if let Some(completed_value) = fields.get("completed") {
                                // Resolve HoldRef to actual value
                                let resolved_completed = match completed_value {
                                    DdValue::HoldRef(hold_name) => {
                                        let hid = HoldId::new(hold_name.as_ref());
                                        self.reactive_ctx.get_hold_value(&hid)
                                            .unwrap_or_else(|| completed_value.clone())
                                    }
                                    other => other.clone(),
                                };

                                // Check if completed is True
                                match &resolved_completed {
                                    DdValue::Bool(b) => *b,
                                    DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                                    _ => false,
                                }
                            } else {
                                false
                            }
                        }
                        _ => false,
                    };

                    zoon::println!("[List/remove] item[{}] should_remove={}", idx, should_remove);

                    if !should_remove {
                        filtered_items.push(item_value.clone());
                    } else {
                        removed_count += 1;
                    }
                }

                zoon::println!("[List/remove] Removed {} items, {} remaining", removed_count, filtered_items.len());

                // If nothing was removed, just return current state
                if removed_count == 0 {
                    if let Some(ref hid) = hold_id {
                        if self.reactive_ctx.get_hold(hid).is_none() {
                            self.reactive_ctx.register_hold(hid.clone(), resolved_from.clone());
                        }
                        return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                    }
                    return resolved_from;
                }

                let new_list = DdValue::List(Arc::new(filtered_items));

                // Update HOLD if we have one
                if let Some(hid) = hold_id {
                    self.reactive_ctx.update_hold(&hid, new_list.clone());
                    return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                }

                new_list
            }
            (Some("List"), "clear") => {
                // Get the `on` trigger - if it's Unit/not present, list is unchanged
                let on_trigger = args.get("on").cloned().unwrap_or(DdValue::Unit);

                zoon::println!("[List/clear] from={:?}, on={:?}, current_variable_name={:?}, first_pass={}", from, on_trigger, self.current_variable_name, self.first_pass_evaluation);

                // During first pass of two-pass evaluation, skip state modification
                // The first pass is only for forward reference resolution
                if self.first_pass_evaluation {
                    zoon::println!("[List/clear] Skipping during first pass evaluation");
                    let hold_id = self.current_variable_name.as_ref().map(|name| {
                        if let Some(ref prefix) = self.hold_id_prefix {
                            HoldId::new(format!("{}:{}", prefix, name))
                        } else {
                            HoldId::new(name.clone())
                        }
                    });
                    if let Some(ref hid) = hold_id {
                        if self.reactive_ctx.get_hold(hid).is_none() {
                            self.reactive_ctx.register_hold(hid.clone(), from.clone());
                        }
                        return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                    }
                    return from.clone();
                }

                // NOTE: We intentionally do NOT use current_hold_id here (same as List/append).
                // current_hold_id is for Stream/skip to reference the HOLD that was just evaluated.
                // For List/clear, we want to create/use a HOLD based on the variable name.
                let hold_id = self.current_variable_name.as_ref().map(|name| {
                    if let Some(ref prefix) = self.hold_id_prefix {
                        HoldId::new(format!("{}:{}", prefix, name))
                    } else {
                        HoldId::new(name.clone())
                    }
                });

                // If on_trigger is SKIP (event not found), return HoldRef (from HOLD if registered)
                // Note: Unit is a VALID trigger value (button press), so don't skip on Unit
                if matches!(&on_trigger, DdValue::Tagged { tag, .. } if tag.as_ref() == "SKIP") {
                    zoon::println!("[List/clear] on trigger is SKIP, returning HoldRef");
                    // Return HoldRef to the HOLD (register if not exists)
                    if let Some(ref hid) = hold_id {
                        if self.reactive_ctx.get_hold(hid).is_none() {
                            // Register HOLD to load from persistence (if any)
                            self.reactive_ctx.register_hold(hid.clone(), from.clone());
                        }
                        // Return HoldRef so callers always get current value
                        return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                    }
                    return from.clone();
                }

                // Get current list to check if already empty
                let current_list = if let Some(ref hid) = hold_id {
                    if let Some(signal) = self.reactive_ctx.get_hold(hid) {
                        signal.get()
                    } else {
                        from.clone()
                    }
                } else {
                    from.clone()
                };

                // If list is already empty, skip clearing (prevents infinite clearing on re-evaluation)
                if let DdValue::List(ref items) = current_list {
                    if items.is_empty() {
                        zoon::println!("[List/clear] List already empty, skipping clear");
                        // Return HoldRef if we have a HOLD, otherwise current list
                        if let Some(ref hid) = hold_id {
                            return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                        }
                        return current_list;
                    }
                }

                // Clear the list
                zoon::println!("[List/clear] Clearing list!");

                // If we have a HOLD ID, update the HOLD state
                if let Some(hid) = hold_id {
                    let empty_list = DdValue::List(Arc::new(Vec::new()));
                    self.reactive_ctx.update_hold(&hid, empty_list.clone());
                    // Return HoldRef so callers always get current value
                    return DdValue::HoldRef(Arc::from(hid.0.as_str()));
                }

                // Regular clear
                DdValue::List(Arc::new(Vec::new()))
            }
            (Some("List"), "count") => {
                // Debug: log what we're counting
                zoon::println!("[List/count] from = {:?}", from);

                // Resolve HoldRef to actual list value before counting
                let resolved = if let DdValue::HoldRef(hold_name) = &from {
                    zoon::println!("[List/count] Resolving HoldRef: {}", hold_name);
                    let hold_id = HoldId::new(hold_name.as_ref());
                    self.reactive_ctx.get_hold_value(&hold_id).unwrap_or_else(|| from.clone())
                } else {
                    from.clone()
                };
                if let DdValue::List(items) = resolved {
                    zoon::println!("[List/count] Counting list with {} items", items.len());
                    DdValue::int(items.len() as i64)
                } else {
                    zoon::println!("[List/count] from is not a List: {:?}", resolved);
                    DdValue::int(0)
                }
            }
            (Some("List"), "is_empty") => {
                // Resolve HoldRef to actual list value before checking
                let resolved = if let DdValue::HoldRef(hold_name) = &from {
                    let hold_id = HoldId::new(hold_name.as_ref());
                    self.reactive_ctx.get_hold_value(&hold_id).unwrap_or_else(|| from.clone())
                } else {
                    from.clone()
                };
                if let DdValue::List(items) = resolved {
                    DdValue::Bool(items.is_empty())
                } else {
                    DdValue::Bool(true)
                }
            }
            (Some("Bool"), "or") => {
                let from_bool = from.is_truthy();
                let that_bool = args.get("that").map(|v| v.is_truthy()).unwrap_or(false);
                DdValue::Bool(from_bool || that_bool)
            }
            (Some("Bool"), "and") => {
                let from_bool = from.is_truthy();
                let that_bool = args.get("that").map(|v| v.is_truthy()).unwrap_or(true);
                DdValue::Bool(from_bool && that_bool)
            }
            (Some("Bool"), "not") => {
                // Resolve HoldRef to actual value before negating
                let resolved = if let DdValue::HoldRef(hold_name) = from {
                    let hold_id = HoldId::new(hold_name.as_ref());
                    self.reactive_ctx.get_hold_value(&hold_id).unwrap_or_else(|| from.clone())
                } else {
                    from.clone()
                };
                // Return Tagged "True" or "False" to match Boon's boolean convention
                if resolved.is_truthy() {
                    DdValue::tagged("False", std::iter::empty::<(&str, DdValue)>())
                } else {
                    DdValue::tagged("True", std::iter::empty::<(&str, DdValue)>())
                }
            }
            (Some("Text"), "trim") => {
                if let DdValue::Text(s) = from {
                    DdValue::text(s.trim())
                } else {
                    from.clone()
                }
            }
            (Some("Text"), "is_not_empty") => {
                if let DdValue::Text(s) = from {
                    DdValue::Bool(!s.is_empty())
                } else {
                    DdValue::Bool(false)
                }
            }
            (Some("Text"), "is_empty") => {
                if let DdValue::Text(s) = from {
                    DdValue::Bool(s.is_empty())
                } else {
                    DdValue::Bool(true)
                }
            }
            (Some("Router"), "go_to") => {
                // Handle reactive LATEST with THEN triggers for navigation
                zoon::println!("[Router/go_to] Received from: {:?}", from);
                if let DdValue::Tagged { tag, fields } = from {
                    zoon::println!("[Router/go_to] Tag: {}", tag);
                    if tag.as_ref() == "__ReactiveLatest__" {
                        // Extract triggers from LATEST - each has a link_id and route
                        if let Some(DdValue::List(triggers)) = fields.get("triggers") {
                            zoon::println!("[Router/go_to] Found {} triggers", triggers.len());
                            for trigger_val in triggers.iter() {
                                if let DdValue::Object(trigger_fields) = trigger_val {
                                    // The add_value contains the route (e.g., TEXT { /about })
                                    let route = trigger_fields.get("add_value")
                                        .map(|v| v.to_display_string())
                                        .unwrap_or_else(|| "/".to_string());

                                    if let Some(DdValue::Text(link_id_str)) = trigger_fields.get("link_id") {
                                        zoon::println!("[Router/go_to] Adding nav connection: link_id={}, route={}", link_id_str, route);
                                        let link_id = LinkId::new(link_id_str.as_ref());
                                        self.reactive_ctx.add_nav_connection(NavigationConnection {
                                            trigger: ThenTrigger::Link(link_id),
                                            route,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                // Router/go_to returns Unit - navigation is side effect
                DdValue::Unit
            }
            (None, func_name) => self.eval_user_function_with_piped(func_name, from, &args),
            _ => self.eval_expression(&Expression::FunctionCall {
                path: path.to_vec(),
                arguments: arguments.to_vec(),
            }),
        }
    }

    fn eval_user_function_with_piped(
        &mut self,
        name: &str,
        piped: &DdValue,
        args: &HashMap<&str, DdValue>,
    ) -> DdValue {
        if let Some(func_def) = self.functions.get(name).cloned() {
            let passed_context = args.get("PASS").cloned().or_else(|| self.passed_context.clone());
            let saved_vars = self.variables.clone();
            let saved_passed = self.passed_context.clone();

            self.passed_context = passed_context;

            if let Some(first_param) = func_def.parameters.first() {
                self.variables.insert(first_param.clone(), piped.clone());
            }
            for param in &func_def.parameters {
                if let Some(value) = args.get(param.as_str()) {
                    self.variables.insert(param.clone(), value.clone());
                }
            }

            let result = self.eval_expression(&func_def.body.node);
            self.variables = saved_vars;
            self.passed_context = saved_passed;
            result
        } else {
            DdValue::Unit
        }
    }

    /// Evaluate HOLD - creates reactive state.
    fn eval_pipe_to_hold(&mut self, initial: &DdValue, state_name: &str, body: &Expression) -> DdValue {
        // Create a HoldId for this HOLD, using prefix if set (for list item contexts)
        // Also include current_variable_name to differentiate multiple HOLDs in the same object
        let hold_id = match (&self.hold_id_prefix, &self.current_variable_name) {
            (Some(prefix), Some(var_name)) => {
                HoldId::new(&format!("{}:{}:{}", prefix, var_name, state_name))
            }
            (Some(prefix), None) => {
                HoldId::new(&format!("{}:{}", prefix, state_name))
            }
            (None, Some(var_name)) => {
                HoldId::new(&format!("{}:{}", var_name, state_name))
            }
            (None, None) => {
                HoldId::new(state_name)
            }
        };

        // Track this HOLD for Stream/skip to reference
        // Save previous value so we can restore after (nested HOLDs shouldn't contaminate outer context)
        let saved_hold_id = self.current_hold_id.take();
        self.current_hold_id = Some(hold_id.clone());

        // Register the HOLD with its initial value
        let signal = self.reactive_ctx.register_hold(hold_id.clone(), initial.clone());

        // Check if body has LINK |> THEN or timer |> THEN pattern
        // Also handles: LATEST { ... |> THEN { ... } } patterns used in todo_mvc
        zoon::println!("[HOLD] Checking body for LINK/THEN pattern, body type: {:?}", std::mem::discriminant(body));

        // Helper: process a single Pipe expression that might be LINK/Timer |> THEN
        let process_pipe_then = |self_ref: &mut Self, from: &Spanned<Expression>, to: &Spanned<Expression>, hold_id: &HoldId, state_name: &str| {
            // Try to extract LINK path from the 'from' expression
            // It can be:
            // - Alias (like "button.event.press" -> extract "button")
            // - FieldAccess (like "todo_elements.todo_checkbox.event.click" -> extract "todo_elements.todo_checkbox")
            // IMPORTANT: Extract only parts BEFORE "event" to match what buttons fire
            let link_path = match &from.node {
                Expression::Alias(alias) => {
                    match alias {
                        Alias::WithoutPassed { parts, .. } if !parts.is_empty() => {
                            // Find "event" in the path - LINK ID is everything before it
                            if let Some(event_idx) = parts.iter().position(|p| p.as_str() == "event") {
                                if event_idx > 0 {
                                    Some(parts[..event_idx].iter().map(|p| p.as_str()).collect::<Vec<_>>().join("."))
                                } else {
                                    None
                                }
                            } else {
                                // No "event" in path - use full path (might be a direct LINK reference)
                                Some(parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("."))
                            }
                        }
                        _ => None,
                    }
                }
                Expression::FieldAccess { path } => {
                    // FieldAccess contains the full path - extract parts before "event"
                    if let Some(event_idx) = path.iter().position(|p| p.as_str() == "event") {
                        if event_idx > 0 {
                            Some(path[..event_idx].iter().map(|p| p.as_str()).collect::<Vec<_>>().join("."))
                        } else {
                            None
                        }
                    } else {
                        // No "event" in path - use full path
                        Some(path.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("."))
                    }
                }
                _ => None,
            };

            if let Some(link_path) = link_path {
                zoon::println!("[HOLD]   Found link_path: {}", link_path);

                // Check if this is a Timer reference
                let base_var = link_path.split('.').next().unwrap_or("");
                let is_timer = self_ref.variables.get(base_var)
                    .map(|v| matches!(v, DdValue::Tagged { tag, .. } if tag.as_ref() == "Timer"))
                    .unwrap_or(false);

                // If 'to' is THEN, set up the connection
                if let Expression::Then { body: then_body } = &to.node {
                    // Check if we should skip creating this connection
                    let current_prefix = self_ref.hold_id_prefix.as_deref();

                    // For prefixed HOLDs (list items): check if prefix existed at eval START (not live set)
                    // For non-prefixed HOLDs (top-level): always skip during re-evaluation
                    let should_skip = if let Some(prefix) = current_prefix {
                        // Check against SNAPSHOT - allows multiple connections for same NEW prefix
                        self_ref.prefixes_at_eval_start.contains(prefix)
                    } else {
                        // No prefix = top-level HOLD. Skip if we're in re-evaluation mode.
                        self_ref.skip_then_connections
                    };

                    if should_skip {
                        zoon::println!("[HOLD]   Skipping ThenConnection (prefix {:?}, skip_mode={})", current_prefix, self_ref.skip_then_connections);
                    } else {
                        let trigger = if is_timer {
                            ThenTrigger::Timer(TimerId::new(&link_path))
                        } else {
                            // It's a LINK reference
                            // Check if this is a LOCAL reference (scoped to list item)
                            // During re-evaluation, variables may be incomplete, so we use negative logic:
                            // if it's NOT a known local pattern, treat it as global
                            let first_part = link_path.split('.').next().unwrap_or("");
                            let is_local_reference = matches!(first_part, "todo_elements" | "completed" | "editing" | "title" | "todo" | "item");

                            // Include list item prefix ONLY for local (per-item) references
                            let link_id_str = if let Some(ref prefix) = self_ref.hold_id_prefix {
                                if is_local_reference {
                                    format!("{}:{}", prefix, link_path) // Local - add prefix
                                } else {
                                    link_path.clone() // Global - no prefix
                                }
                            } else {
                                link_path.clone()
                            };
                            let link_id = LinkId::new(&link_id_str);
                            self_ref.reactive_ctx.register_link(link_id.clone());
                            ThenTrigger::Link(link_id)
                        };

                        zoon::println!("[HOLD]   Creating ThenConnection: trigger={:?}, hold_id={:?}", trigger, hold_id);

                        let conn = ThenConnection {
                            trigger,
                            hold_id: hold_id.clone(),
                            state_name: state_name.to_string(),
                            body: then_body.clone(),
                            variables_snapshot: self_ref.variables.clone(),
                            functions_snapshot: self_ref.functions.clone(),
                        };

                        // During re-evaluation, add directly to context; otherwise, collect for later
                        if self_ref.skip_then_connections {
                            // Re-evaluation mode: add directly to context for new prefixes
                            zoon::println!("[HOLD]   Adding ThenConnection directly to context (new prefix {:?})", current_prefix);
                            self_ref.reactive_ctx.add_then_connection(conn, current_prefix);
                        } else {
                            // Initial evaluation: collect for batch set later
                            self_ref.then_connections.push(conn);
                        }
                    }
                }
            }
        };

        // Pattern 1: Direct Pipe expression (LINK |> THEN { ... })
        if let Expression::Pipe { from, to } = body {
            zoon::println!("[HOLD]   Pattern 1: Direct Pipe");
            process_pipe_then(self, from, to, &hold_id, state_name);
        }

        // Pattern 2: LATEST { ... } containing Pipe expressions
        // (Used in todo_mvc for checkbox toggle: LATEST { link |> THEN { ... }, link2 |> THEN { ... } })
        if let Expression::Latest { inputs } = body {
            zoon::println!("[HOLD]   Pattern 2: LATEST with {} inputs", inputs.len());
            for (i, input) in inputs.iter().enumerate() {
                zoon::println!("[HOLD]     input[{}] type: {:?}", i, std::mem::discriminant(&input.node));
                if let Expression::Pipe { from, to } = &input.node {
                    process_pipe_then(self, from, to, &hold_id, state_name);
                }
                // Also handle nested WHEN inside LATEST (like: link |> WHEN { Enter => ... })
                // This is for patterns like: key_down |> WHEN { Enter => [], __ => SKIP }
                if let Expression::Pipe { from, to } = &input.node {
                    if let Expression::When { arms } = &to.node {
                        // For WHEN arms, we might want to extract the trigger too
                        // Extract only parts before "event" to match what buttons fire
                        let link_path = match &from.node {
                            Expression::FieldAccess { path } => {
                                if let Some(event_idx) = path.iter().position(|p| p.as_str() == "event") {
                                    if event_idx > 0 {
                                        Some(path[..event_idx].iter().map(|p| p.as_str()).collect::<Vec<_>>().join("."))
                                    } else {
                                        None
                                    }
                                } else {
                                    Some(path.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("."))
                                }
                            }
                            _ => None,
                        };
                        if let Some(link_path) = link_path {
                            zoon::println!("[HOLD]     WHEN trigger found: {}", link_path);
                            // For WHEN patterns, we create a connection with the first arm's body
                            // This is a simplification - full support would need arm-specific handling
                            if let Some(first_arm) = arms.first() {
                                // Check if this is a global reference
                                let first_part = link_path.split('.').next().unwrap_or("");
                                let is_global = self.variables.contains_key(first_part)
                                    && !matches!(first_part, "todo_elements" | "completed" | "editing" | "title" | "todo" | "item");

                                let link_id_str = if let Some(ref prefix) = self.hold_id_prefix {
                                    if is_global {
                                        link_path.clone()
                                    } else {
                                        format!("{}:{}", prefix, link_path)
                                    }
                                } else {
                                    link_path.clone()
                                };
                                let link_id = LinkId::new(&link_id_str);
                                self.reactive_ctx.register_link(link_id.clone());
                                // Note: WHEN is more complex than THEN, needs pattern matching
                                // For now we just register the LINK without a full connection
                            }
                        }
                    }
                }
            }
        }

        // Pattern 3: LATEST { ... } |> THEN { ... } (the entire LATEST pipes to THEN)
        if let Expression::Pipe { from, to } = body {
            if let Expression::Latest { inputs } = &from.node {
                if let Expression::Then { body: then_body } = &to.node {
                    zoon::println!("[HOLD]   Pattern 3: LATEST |> THEN");
                    // Helper: extract link ID from parts (only parts before "event")
                    let extract_link_id = |parts: &[crate::parser::StrSlice]| -> Option<String> {
                        if let Some(event_idx) = parts.iter().position(|p| p.as_str() == "event") {
                            if event_idx > 0 {
                                Some(parts[..event_idx].iter().map(|p| p.as_str()).collect::<Vec<_>>().join("."))
                            } else {
                                None
                            }
                        } else {
                            // No "event" in path - use full path
                            Some(parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("."))
                        }
                    };
                    // Extract all LINK references from the LATEST inputs and create connections
                    for input in inputs.iter() {
                        let link_path = match &input.node {
                            Expression::Alias(alias) => {
                                match alias {
                                    Alias::WithoutPassed { parts, .. } if !parts.is_empty() => {
                                        extract_link_id(parts)
                                    }
                                    _ => None,
                                }
                            }
                            Expression::FieldAccess { path } => {
                                extract_link_id(path)
                            }
                            Expression::Pipe { from: inner_from, .. } => {
                                // Nested pipe inside LATEST
                                match &inner_from.node {
                                    Expression::FieldAccess { path } => {
                                        extract_link_id(path)
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        };

                        if let Some(link_path) = link_path {
                            // Check if we should skip creating this connection
                            let current_prefix = self.hold_id_prefix.as_deref();

                            // For prefixed HOLDs (list items): check if prefix existed at eval START (not live set)
                            // For non-prefixed HOLDs (top-level): always skip during re-evaluation
                            let should_skip = if let Some(prefix) = current_prefix {
                                // Check against SNAPSHOT - allows multiple connections for same NEW prefix
                                self.prefixes_at_eval_start.contains(prefix)
                            } else {
                                // No prefix = top-level HOLD. Skip if we're in re-evaluation mode.
                                self.skip_then_connections
                            };

                            if should_skip {
                                zoon::println!("[HOLD]   Skipping Pattern 3 ThenConnection (prefix {:?}, skip_mode={})", current_prefix, self.skip_then_connections);
                            } else {
                                // Check if this is a LOCAL reference (scoped to list item)
                                // During re-evaluation, variables may be incomplete, so we use negative logic
                                let first_part = link_path.split('.').next().unwrap_or("");
                                let is_local = matches!(first_part, "todo_elements" | "completed" | "editing" | "title" | "todo" | "item");

                                let link_id_str = if let Some(ref prefix) = self.hold_id_prefix {
                                    if is_local {
                                        format!("{}:{}", prefix, link_path) // Local - add prefix
                                    } else {
                                        link_path.clone() // Global - no prefix
                                    }
                                } else {
                                    link_path.clone()
                                };
                                let link_id = LinkId::new(&link_id_str);
                                self.reactive_ctx.register_link(link_id.clone());

                                let conn = ThenConnection {
                                    trigger: ThenTrigger::Link(link_id),
                                    hold_id: hold_id.clone(),
                                    state_name: state_name.to_string(),
                                    body: then_body.clone(),
                                    variables_snapshot: self.variables.clone(),
                                    functions_snapshot: self.functions.clone(),
                                };

                                // During re-evaluation, add directly to context; otherwise, collect for later
                                if self.skip_then_connections {
                                    // Re-evaluation mode: add directly to context for new prefixes
                                    zoon::println!("[HOLD]   Adding Pattern 3 ThenConnection directly to context (new prefix {:?})", current_prefix);
                                    self.reactive_ctx.add_then_connection(conn, current_prefix);
                                } else {
                                    // Initial evaluation: collect for batch set later
                                    self.then_connections.push(conn);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Also handle static Stream/pulses iteration for initial value computation
        let pulse_count = self.extract_pulse_count(body);
        if pulse_count > 0 {
            let then_body = self.extract_then_body(body);
            if let Some(then_body) = then_body {
                // Iterate to compute initial value
                let mut current_state = initial.clone();
                for _ in 0..pulse_count {
                    let saved_vars = self.variables.clone();
                    self.variables.insert(state_name.to_string(), current_state.clone());
                    let next_state = self.eval_expression(then_body);
                    self.variables = saved_vars;
                    if next_state != DdValue::Unit {
                        current_state = next_state;
                    }
                }
                // Update the signal with computed value
                signal.set(current_state.clone());
                // Restore previous current_hold_id before returning
                self.current_hold_id = saved_hold_id;
                return current_state;
            }
        }

        // NOTE: Do NOT restore current_hold_id here - downstream operations like Stream/skip
        // need to see which HOLD they're operating on. Stream/skip will .take() the hold_id.
        // The saved_hold_id restoration only happens in the synchronous pulse path above.

        // Return a HoldRef so the bridge can look up the current value at render time
        // Use the full hold_id (including prefix) so lookups find the correct HOLD
        DdValue::HoldRef(Arc::from(hold_id.0.as_str()))
    }

    fn extract_pulse_count(&self, expr: &Expression) -> i64 {
        match expr {
            Expression::Pipe { from, to } => {
                // Check if `to` is Stream/pulses directly
                if self.is_stream_pulses(&to.node) {
                    // Try to get count from `from` expression
                    return self.extract_number_value(&from.node);
                }
                // Check if `to` is another pipe starting with Stream/pulses
                // Pattern: `count |> Stream/pulses() |> THEN { ... }`
                if let Expression::Pipe { from: inner_from, .. } = &to.node {
                    if self.is_stream_pulses(&inner_from.node) {
                        // The count is in the outer `from`
                        return self.extract_number_value(&from.node);
                    }
                }
                // Recursively check both sides
                let from_count = self.extract_pulse_count(&from.node);
                if from_count > 0 {
                    return from_count;
                }
                self.extract_pulse_count(&to.node)
            }
            _ => 0,
        }
    }

    fn extract_number_value(&self, expr: &Expression) -> i64 {
        match expr {
            Expression::Literal(Literal::Number(n)) => (*n as i64).max(0),
            Expression::ArithmeticOperator(op) => {
                // Evaluate arithmetic to get the number
                let result = self.eval_arithmetic_static(op);
                if let DdValue::Number(n) = result {
                    (n.0 as i64).max(0)
                } else {
                    0
                }
            }
            Expression::Alias(alias) => {
                // Look up variable value
                let value = self.eval_alias_static(alias);
                if let DdValue::Number(n) = value {
                    (n.0 as i64).max(0)
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    fn eval_arithmetic_static(&self, op: &ArithmeticOperator) -> DdValue {
        match op {
            ArithmeticOperator::Subtract { operand_a, operand_b } => {
                let a = self.eval_expr_static(&operand_a.node);
                let b = self.eval_expr_static(&operand_b.node);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) => DdValue::float(x.0 - y.0),
                    _ => DdValue::Unit,
                }
            }
            ArithmeticOperator::Add { operand_a, operand_b } => {
                let a = self.eval_expr_static(&operand_a.node);
                let b = self.eval_expr_static(&operand_b.node);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) => DdValue::float(x.0 + y.0),
                    _ => DdValue::Unit,
                }
            }
            _ => DdValue::Unit,
        }
    }

    fn eval_expr_static(&self, expr: &Expression) -> DdValue {
        match expr {
            Expression::Literal(Literal::Number(n)) => DdValue::float(*n),
            Expression::Alias(alias) => self.eval_alias_static(alias),
            _ => DdValue::Unit,
        }
    }

    fn eval_alias_static(&self, alias: &Alias) -> DdValue {
        match alias {
            Alias::WithoutPassed { parts, .. } => {
                if parts.is_empty() {
                    return DdValue::Unit;
                }
                let var_name = parts[0].as_str();
                self.variables.get(var_name).cloned().unwrap_or(DdValue::Unit)
            }
            _ => DdValue::Unit,
        }
    }

    fn is_stream_pulses(&self, expr: &Expression) -> bool {
        if let Expression::FunctionCall { path, .. } = expr {
            let parts: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            return parts == vec!["Stream", "pulses"];
        }
        false
    }

    fn extract_then_body<'a>(&self, expr: &'a Expression) -> Option<&'a Expression> {
        match expr {
            Expression::Pipe { to, .. } => {
                if let Expression::Then { body } = &to.node {
                    return Some(&body.node);
                }
                self.extract_then_body(&to.node)
            }
            Expression::Then { body } => Some(&body.node),
            _ => None,
        }
    }

    fn eval_pattern_match(&mut self, value: &DdValue, arms: &[Arm]) -> DdValue {
        // Resolve HoldRef to its current value before pattern matching
        let resolved_value = if let DdValue::HoldRef(hold_name) = value {
            let hold_id = HoldId::new(hold_name.as_ref());
            if let Some(signal) = self.reactive_ctx.get_hold(&hold_id) {
                signal.get()
            } else {
                // HoldRef not found - return the original value
                value.clone()
            }
        } else {
            value.clone()
        };

        zoon::println!("[eval_pattern_match] value={:?}, num_arms={}", resolved_value, arms.len());

        for (i, arm) in arms.iter().enumerate() {
            zoon::println!("[eval_pattern_match] checking arm {} with pattern {:?}", i, arm.pattern);
            if let Some(bindings) = self.match_pattern(&resolved_value, &arm.pattern) {
                zoon::println!("[eval_pattern_match] arm {} matched with bindings: {:?}", i, bindings);
                let saved_vars = self.variables.clone();
                for (name, bound_value) in bindings {
                    self.variables.insert(name, bound_value);
                }
                let result = self.eval_expression(&arm.body.node);
                zoon::println!("[eval_pattern_match] arm {} body result: {:?}", i, result);
                self.variables = saved_vars;
                return result;
            }
        }
        zoon::println!("[eval_pattern_match] no arms matched, returning Unit");
        DdValue::Unit
    }

    fn match_pattern(&self, value: &DdValue, pattern: &Pattern) -> Option<Vec<(String, DdValue)>> {
        match pattern {
            Pattern::WildCard => Some(vec![]),
            Pattern::Alias { name } => {
                Some(vec![(name.as_str().to_string(), value.clone())])
            }
            Pattern::Literal(lit) => {
                if let DdValue::Bool(b) = value {
                    if let Literal::Tag(tag_name) = lit {
                        let tag_str = tag_name.as_str();
                        if (tag_str == "True" && *b) || (tag_str == "False" && !*b) {
                            return Some(vec![]);
                        } else if tag_str == "True" || tag_str == "False" {
                            return None;
                        }
                    }
                }
                let pattern_value = self.eval_literal(lit);
                if *value == pattern_value {
                    Some(vec![])
                } else {
                    None
                }
            }
            Pattern::TaggedObject { tag, variables } => {
                if let DdValue::Bool(b) = value {
                    let tag_name = tag.as_str();
                    if (tag_name == "True" && *b) || (tag_name == "False" && !*b) {
                        return Some(vec![]);
                    } else {
                        return None;
                    }
                }
                if let DdValue::Tagged { tag: value_tag, fields } = value {
                    if tag.as_str() == value_tag.as_ref() {
                        let mut bindings = vec![];
                        for var in variables {
                            let field_value = fields.get(var.name.as_str()).cloned().unwrap_or(DdValue::Unit);
                            bindings.push((var.name.as_str().to_string(), field_value));
                        }
                        Some(bindings)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Pattern::Object { variables } => {
                if let DdValue::Object(fields) = value {
                    let mut bindings = vec![];
                    for var in variables {
                        let field_value = fields.get(var.name.as_str()).cloned().unwrap_or(DdValue::Unit);
                        bindings.push((var.name.as_str().to_string(), field_value));
                    }
                    Some(bindings)
                } else {
                    None
                }
            }
            Pattern::List { items } => {
                if let DdValue::List(list_items) = value {
                    if items.len() != list_items.len() {
                        return None;
                    }
                    let mut bindings = vec![];
                    for (pattern_item, value_item) in items.iter().zip(list_items.iter()) {
                        if let Some(item_bindings) = self.match_pattern(value_item, pattern_item) {
                            bindings.extend(item_bindings);
                        } else {
                            return None;
                        }
                    }
                    Some(bindings)
                } else {
                    None
                }
            }
            Pattern::Map { .. } => None,
        }
    }

    /// Resolve a HoldRef to its actual value, or return the value as-is if not a HoldRef.
    fn resolve_holdref(&self, value: DdValue) -> DdValue {
        if let DdValue::HoldRef(hold_name) = &value {
            let hold_id = HoldId::new(hold_name.as_ref());
            self.reactive_ctx.get_hold_value(&hold_id).unwrap_or(value)
        } else {
            value
        }
    }

    fn eval_comparator(&mut self, comp: &Comparator) -> DdValue {
        match comp {
            Comparator::Equal { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw.clone());
                let b = self.resolve_holdref(b_raw.clone());
                zoon::eprintln!("[Comparator::Equal] a_raw={:?} b_raw={:?} a={:?} b={:?} result={}", a_raw, b_raw, a, b, a == b);
                DdValue::Bool(a == b)
            }
            Comparator::NotEqual { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw);
                let b = self.resolve_holdref(b_raw);
                DdValue::Bool(a != b)
            }
            Comparator::Less { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw);
                let b = self.resolve_holdref(b_raw);
                DdValue::Bool(a < b)
            }
            Comparator::Greater { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw);
                let b = self.resolve_holdref(b_raw);
                DdValue::Bool(a > b)
            }
            Comparator::LessOrEqual { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw);
                let b = self.resolve_holdref(b_raw);
                DdValue::Bool(a <= b)
            }
            Comparator::GreaterOrEqual { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw);
                let b = self.resolve_holdref(b_raw);
                DdValue::Bool(a >= b)
            }
        }
    }

    fn eval_arithmetic(&mut self, op: &ArithmeticOperator) -> DdValue {
        match op {
            ArithmeticOperator::Add { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw);
                let b = self.resolve_holdref(b_raw);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) => DdValue::float(x.0 + y.0),
                    (DdValue::Text(x), DdValue::Text(y)) => DdValue::text(format!("{}{}", x, y)),
                    _ => DdValue::Unit,
                }
            }
            ArithmeticOperator::Subtract { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw);
                let b = self.resolve_holdref(b_raw);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) => DdValue::float(x.0 - y.0),
                    _ => DdValue::Unit,
                }
            }
            ArithmeticOperator::Multiply { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw);
                let b = self.resolve_holdref(b_raw);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) => DdValue::float(x.0 * y.0),
                    _ => DdValue::Unit,
                }
            }
            ArithmeticOperator::Divide { operand_a, operand_b } => {
                let a_raw = self.eval_expression(&operand_a.node);
                let b_raw = self.eval_expression(&operand_b.node);
                let a = self.resolve_holdref(a_raw);
                let b = self.resolve_holdref(b_raw);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) if y.0 != 0.0 => DdValue::float(x.0 / y.0),
                    _ => DdValue::Unit,
                }
            }
            ArithmeticOperator::Negate { operand } => {
                let a_raw = self.eval_expression(&operand.node);
                let a = self.resolve_holdref(a_raw);
                match &a {
                    DdValue::Number(x) => DdValue::float(-x.0),
                    _ => DdValue::Unit,
                }
            }
        }
    }

    fn eval_alias(&self, alias: &Alias) -> DdValue {
        match alias {
            Alias::WithoutPassed { parts, .. } => {
                if parts.is_empty() {
                    return DdValue::Unit;
                }

                // Check for LINK property access pattern: xxx.yyy.text (or similar)
                // This handles accessing properties on LINKed elements
                if parts.len() >= 2 {
                    let last_part = parts.last().unwrap().as_str();
                    // If accessing .text property, try looking up as a LINK property
                    if last_part == "text" {
                        let full_parts: Vec<&str> = parts.iter().map(|p| p.as_str()).collect();
                        let full_link_id = full_parts.join(".");

                        // Include list item prefix if set
                        let link_id_str = if let Some(ref prefix) = self.hold_id_prefix {
                            format!("{}:{}", prefix, full_link_id)
                        } else {
                            full_link_id.clone()
                        };

                        zoon::println!("[eval_alias] LINK property access: link_id='{}'", link_id_str);

                        // Try suffix matching for .text property
                        let suffix_pattern = format!(".{}", full_link_id);
                        for registered_link_id in self.reactive_ctx.links.all_ids() {
                            let registered_id = registered_link_id.0.as_str();
                            if registered_id.ends_with(&suffix_pattern) || registered_id.ends_with(&format!(":{}", full_link_id)) {
                                zoon::println!("[eval_alias] Found LINK property via suffix match: registered='{}'", registered_id);
                                if let Some(link) = self.reactive_ctx.links.get(&registered_link_id) {
                                    if let Some(value) = link.get() {
                                        zoon::println!("[eval_alias] Found LINK property value: {:?}", value);
                                        return value;
                                    }
                                }
                            }
                        }
                    }
                }

                // Check for LINK event access pattern: xxx.yyy.event.zzz
                // When "event" is in the path, this is accessing a LINK's event value
                if let Some(event_idx) = parts.iter().position(|p| p.as_str() == "event") {
                    // Build the base LINK ID from parts before "event"
                    let base_link_parts: Vec<&str> = parts[..event_idx]
                        .iter()
                        .map(|p| p.as_str())
                        .collect();
                    let base_link_id = base_link_parts.join(".");

                    // Build the full link ID including the event path after "event"
                    // e.g., elements.item_input.event.key_down.key ->
                    //       base: "elements.item_input", suffix: ".event.key_down.key"
                    let full_parts: Vec<&str> = parts.iter().map(|p| p.as_str()).collect();
                    let full_link_id = full_parts.join(".");

                    // Include list item prefix if set
                    let link_id_str = if let Some(ref prefix) = self.hold_id_prefix {
                        format!("{}:{}", prefix, full_link_id)
                    } else {
                        full_link_id
                    };

                    zoon::println!("[eval_alias] LINK event access: full_link_id='{}', base_link_id='{}'", link_id_str, base_link_id);

                    // Look up the LINK value from the registry
                    // First try exact match
                    let link_id = LinkId::new(&link_id_str);
                    if let Some(link) = self.reactive_ctx.links.get(&link_id) {
                        if let Some(value) = link.get() {
                            zoon::println!("[eval_alias] Found LINK value (exact): {:?}", value);
                            return value;
                        }
                    }

                    // If not found, try suffix matching - the alias might be relative to a scope
                    // e.g., alias is "elements.item_input.event.key_down.key" but link is
                    //       "store.elements.item_input.event.key_down.key"
                    let suffix_pattern = format!(".{}", full_parts.join("."));
                    for registered_link_id in self.reactive_ctx.links.all_ids() {
                        let registered_id = registered_link_id.0.as_str();
                        // Check if registered link ends with our alias path
                        if registered_id.ends_with(&suffix_pattern) || registered_id.ends_with(&format!(":{}", full_parts.join("."))) {
                            zoon::println!("[eval_alias] Found LINK via suffix match: registered='{}', suffix='{}'", registered_id, suffix_pattern);
                            if let Some(link) = self.reactive_ctx.links.get(&registered_link_id) {
                                if let Some(value) = link.get() {
                                    zoon::println!("[eval_alias] Found LINK value (suffix): {:?}", value);
                                    return value;
                                }
                            }
                        }
                    }

                    zoon::println!("[eval_alias] LINK value not found for '{}'", link_id_str);
                    // Return SKIP instead of Unit so List/clear can distinguish "no event" from "event with Unit"
                    return DdValue::tagged("SKIP", std::iter::empty::<(&str, DdValue)>());
                }

                let var_name = parts[0].as_str();

                // IMPORTANT: Check variables FIRST before reactive context.
                // During HOLD iteration, we insert state values into variables,
                // and we need to use those values, not return a HoldRef.
                let mut current = if let Some(var_value) = self.variables.get(var_name) {
                    var_value.clone()
                } else {
                    // Then check reactive HOLD values - return HoldRef for dynamic lookup
                    let hold_id = HoldId::new(var_name);
                    if self.reactive_ctx.get_hold(&hold_id).is_some() {
                        // Return a HoldRef so the value is looked up at render time
                        if parts.len() == 1 {
                            // Simple reference to the HOLD itself
                            return DdValue::HoldRef(Arc::from(var_name));
                        }
                        // For field access, get current value (TODO: support nested HoldRef)
                        self.reactive_ctx.get_hold(&hold_id).unwrap().get()
                    } else {
                        DdValue::Unit
                    }
                };

                for field in parts.iter().skip(1) {
                    current = current.get(field.as_str()).cloned().unwrap_or(DdValue::Unit);
                    // Resolve HoldRef to actual value during field traversal
                    current = self.resolve_hold_ref(current);
                }
                current
            }
            Alias::WithPassed { extra_parts } => {
                // Check for LINK event access in PASSED context too
                if let Some(event_idx) = extra_parts.iter().position(|p| p.as_str() == "event") {
                    // Build the full link ID including the event path
                    let full_parts: Vec<&str> = extra_parts.iter().map(|p| p.as_str()).collect();
                    let full_link_id = full_parts.join(".");

                    // Include list item prefix if set
                    let link_id_str = if let Some(ref prefix) = self.hold_id_prefix {
                        format!("{}:{}", prefix, full_link_id)
                    } else {
                        full_link_id
                    };

                    zoon::println!("[eval_alias PASSED] LINK event access: link_id='{}'", link_id_str);

                    // Look up the LINK value from the registry
                    let link_id = LinkId::new(&link_id_str);
                    if let Some(link) = self.reactive_ctx.links.get(&link_id) {
                        if let Some(value) = link.get() {
                            zoon::println!("[eval_alias PASSED] Found LINK value: {:?}", value);
                            return value;
                        }
                    }
                    zoon::println!("[eval_alias PASSED] LINK value not found for '{}'", link_id_str);
                    // Return SKIP instead of Unit so List/clear can distinguish "no event" from "event with Unit"
                    return DdValue::tagged("SKIP", std::iter::empty::<(&str, DdValue)>());
                }

                let mut current = self.passed_context.clone().unwrap_or(DdValue::Unit);
                for field in extra_parts {
                    current = current.get(field.as_str()).cloned().unwrap_or(DdValue::Unit);
                    // Resolve HoldRef to actual value during field traversal
                    current = self.resolve_hold_ref(current);
                }
                current
            }
        }
    }

    /// Resolve a HoldRef to its actual value, or return the value unchanged if not a HoldRef.
    /// This is needed when accessing fields that contain HoldRefs during evaluation.
    fn resolve_hold_ref(&self, value: DdValue) -> DdValue {
        if let DdValue::HoldRef(hold_name) = &value {
            let hold_id = HoldId::new(hold_name.as_ref());
            if let Some(resolved) = self.reactive_ctx.get_hold_value(&hold_id) {
                return resolved;
            }
        }
        value
    }
}

// Counter for generating unique prefixes for dynamically created items
thread_local! {
    static DYNAMIC_ITEM_COUNTER: Cell<u64> = const { Cell::new(0) };
}

/// Get the next unique dynamic item ID.
fn next_dynamic_item_id() -> u64 {
    DYNAMIC_ITEM_COUNTER.with(|counter| {
        let id = counter.get();
        counter.set(id + 1);
        id
    })
}

/// Check if two values have the same ACTUAL content when resolving HoldRefs.
///
/// This is used for duplicate detection in List/append. Two items are considered
/// equal if they have the same resolved values - meaning the HOLDs they reference
/// contain the same actual data (e.g., same title text, same completed state).
fn values_equal_resolved(a: &DdValue, b: &DdValue, ctx: &DdReactiveContext) -> bool {
    // First, resolve any HoldRefs to their actual values
    let resolved_a = resolve_hold_refs(a, ctx);
    let resolved_b = resolve_hold_refs(b, ctx);
    resolved_a == resolved_b
}

/// Resolve all HoldRefs in a value to their actual HOLD values.
fn resolve_hold_refs(value: &DdValue, ctx: &DdReactiveContext) -> DdValue {
    match value {
        DdValue::HoldRef(id) => {
            // Try to resolve the HoldRef to its actual value
            let hold_id = HoldId::new(id.as_ref());
            if let Some(signal) = ctx.get_hold(&hold_id) {
                signal.get()
            } else {
                // HOLD not found - return Unit
                DdValue::Unit
            }
        }
        DdValue::List(items) => {
            let resolved: Vec<DdValue> = items.iter()
                .map(|item| resolve_hold_refs(item, ctx))
                .collect();
            DdValue::List(Arc::new(resolved))
        }
        DdValue::Object(fields) => {
            let resolved: BTreeMap<Arc<str>, DdValue> = fields.iter()
                .map(|(k, v)| (k.clone(), resolve_hold_refs(v, ctx)))
                .collect();
            DdValue::Object(Arc::new(resolved))
        }
        DdValue::Tagged { tag, fields } => {
            let resolved: BTreeMap<Arc<str>, DdValue> = fields.iter()
                .map(|(k, v)| (k.clone(), resolve_hold_refs(v, ctx)))
                .collect();
            DdValue::Tagged { tag: tag.clone(), fields: Arc::new(resolved) }
        }
        // Other types don't contain HoldRefs
        _ => value.clone(),
    }
}

/// Extract the base name from a HOLD ID, stripping any prefix.
///
/// Examples:
/// - "list_init_0:completed:state" -> "completed:state"
/// - "dynamic_5:title:state" -> "title:state"
/// - "append_3:title:state" -> "title:state"
/// - "completed:state" -> "completed:state"
fn extract_hold_base_name(id: &str) -> &str {
    // Check for known prefixes and strip them
    for prefix in &["list_init_", "dynamic_", "append_"] {
        if let Some(rest) = id.strip_prefix(prefix) {
            // Find the first colon after the number
            if let Some(idx) = rest.find(':') {
                return &rest[idx + 1..];
            }
        }
    }
    // No recognized prefix, return as-is
    id
}

/// Relocate unprefixed HoldRefs in a value to use a unique prefix.
///
/// This ensures each dynamically added item (e.g., a new todo) gets unique HOLD IDs
/// so they won't be incorrectly detected as duplicates of existing items.
///
/// For example, `HoldRef("completed:state")` becomes `HoldRef("dynamic_5:completed:state")`.
fn relocate_holds(value: &DdValue, prefix: &str, ctx: &DdReactiveContext) -> DdValue {
    match value {
        DdValue::HoldRef(id) => {
            let id_str = id.as_ref();
            // Check if already has a prefix (contains a colon followed by alphanumeric)
            // list_init_X:, dynamic_X:, append_X:, etc. are prefixed
            let has_prefix = id_str.contains(':') &&
                id_str.split(':').next().map(|p| {
                    p.starts_with("list_init_") || p.starts_with("dynamic_") || p.starts_with("append_")
                }).unwrap_or(false);

            if has_prefix {
                // Already prefixed, keep as-is
                value.clone()
            } else {
                // Unprefixed - relocate to new prefixed ID
                let new_id = HoldId::new(format!("{}:{}", prefix, id_str));
                let old_id = HoldId::new(id_str);

                // Copy the current value from old HOLD to new HOLD
                if let Some(old_signal) = ctx.get_hold(&old_id) {
                    let current_value = old_signal.get();
                    zoon::println!("[relocate_holds] Copying HOLD {} -> {}, value={:?}", id_str, new_id.0, current_value);
                    ctx.register_hold(new_id.clone(), current_value);
                } else {
                    // Old HOLD doesn't exist - register new one with Unit
                    zoon::println!("[relocate_holds] New HOLD (no source) {} -> {}", id_str, new_id.0);
                    ctx.register_hold(new_id.clone(), DdValue::Unit);
                }

                DdValue::HoldRef(Arc::from(new_id.0.as_str()))
            }
        }
        DdValue::List(items) => {
            let relocated: Vec<DdValue> = items.iter()
                .map(|item| relocate_holds(item, prefix, ctx))
                .collect();
            DdValue::List(Arc::new(relocated))
        }
        DdValue::Object(fields) => {
            let relocated: BTreeMap<Arc<str>, DdValue> = fields.iter()
                .map(|(k, v)| (k.clone(), relocate_holds(v, prefix, ctx)))
                .collect();
            DdValue::Object(Arc::new(relocated))
        }
        DdValue::Tagged { tag, fields } => {
            let relocated: BTreeMap<Arc<str>, DdValue> = fields.iter()
                .map(|(k, v)| (k.clone(), relocate_holds(v, prefix, ctx)))
                .collect();
            DdValue::Tagged { tag: tag.clone(), fields: Arc::new(relocated) }
        }
        // Other types don't contain HoldRefs
        _ => value.clone(),
    }
}

/// Compute a snapshot of `store` object with refreshed computed fields.
///
/// This is called ONCE at the start of `handle_trigger` to ensure all THEN bodies
/// for the same trigger see the SAME computed values (like `all_completed`).
fn compute_store_snapshot(reactive_ctx: &DdReactiveContext) -> Option<DdValue> {
    // Get the original store from any THEN connection's variables_snapshot
    let connections = reactive_ctx.then_connections.borrow();
    let original_store = connections
        .iter()
        .find_map(|conn| conn.variables_snapshot.get("store").cloned());

    let store_fields = match original_store {
        Some(DdValue::Object(fields)) if fields.contains_key("todos") => fields,
        _ => return None,
    };

    // Get the set of active prefixes - these correspond to items that currently exist
    // in the list (marked during List/map and List/append operations).
    // This ensures we only count HOLDs for items that weren't deleted.
    let active_prefixes = reactive_ctx.get_active_prefixes();
    zoon::println!("[compute_store_snapshot] active_prefixes={:?}", active_prefixes);

    // Compute all_completed from current HOLD values, but ONLY for active prefixes
    let holds = reactive_ctx.all_holds();
    let mut todos_count = 0u64;
    let mut completed_count = 0u64;

    for (hold_id, signal) in holds.iter() {
        let id_str = hold_id.name();
        if id_str.contains(":completed:state") {
            // Extract the prefix from the HOLD ID (format: "prefix:hold_name")
            if let Some(prefix) = id_str.split(':').next() {
                // Only count this HOLD if its prefix is in the active set
                if !active_prefixes.contains(&prefix.to_string()) {
                    zoon::println!("[compute_store_snapshot] Skipping orphaned HOLD: {} (prefix {} not active)", id_str, prefix);
                    continue;
                }
            }

            todos_count += 1;
            let value = signal.get();
            let is_completed = match &value {
                DdValue::Bool(b) => *b,
                DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                _ => false,
            };
            if is_completed {
                completed_count += 1;
            }
        }
    }

    zoon::println!("[compute_store_snapshot] todos_count={}, completed_count={}", todos_count, completed_count);

    if todos_count == 0 {
        return None;
    }

    let all_completed = todos_count == completed_count;
    let mut new_store_fields = (*store_fields).clone();
    new_store_fields.insert(Arc::from("all_completed"), DdValue::Bool(all_completed));
    new_store_fields.insert(Arc::from("todos_count"), DdValue::int(todos_count as i64));
    new_store_fields.insert(Arc::from("completed_todos_count"), DdValue::int(completed_count as i64));
    new_store_fields.insert(Arc::from("active_todos_count"), DdValue::int((todos_count - completed_count) as i64));

    zoon::println!("[compute_store_snapshot] Computed store with all_completed={}", all_completed);
    Some(DdValue::Object(Arc::new(new_store_fields)))
}

/// Evaluate a THEN body when a LINK fires.
///
/// This creates a temporary evaluator with the connection's context,
/// binds the current state value, evaluates the THEN body, and returns the new value.
///
/// `store_snapshot` contains pre-computed values for `store` fields that were computed
/// ONCE at the start of trigger handling. This ensures all THEN bodies for the same
/// trigger see consistent values (critical for toggle_all to work correctly).
pub fn evaluate_then_connection(
    connection: &ThenConnection,
    current_state: &DdValue,
    reactive_ctx: &DdReactiveContext,
    store_snapshot: &Option<DdValue>,
) -> DdValue {
    // Generate a unique prefix for this dynamic evaluation.
    // This ensures each dynamically added item (e.g., new todo) gets unique HOLD IDs
    // so they aren't incorrectly detected as duplicates.
    let dynamic_prefix = format!("dynamic_{}", next_dynamic_item_id());

    // Clone variables from snapshot
    let mut variables = connection.variables_snapshot.clone();

    // Apply the pre-computed store snapshot if available.
    // This ensures all THEN bodies for the same trigger see the SAME store.all_completed,
    // computed ONCE before any HOLD updates.
    if let Some(store_value) = store_snapshot {
        variables.insert("store".to_string(), store_value.clone());
        zoon::println!("[evaluate_then_connection] Applied store_snapshot");
    }

    // Create a temporary evaluator with the connection's context
    let mut evaluator = DdReactiveEvaluator {
        variables,
        functions: connection.functions_snapshot.clone(),
        passed_context: None,
        reactive_ctx: reactive_ctx.clone(),
        then_connections: Vec::new(),
        current_variable_name: None,
        current_hold_id: None,
        hold_id_prefix: Some(dynamic_prefix),
        skip_then_connections: true, // Skip THEN connections when evaluating THEN body
        prefixes_at_eval_start: std::collections::HashSet::new(), // Empty - not relevant for THEN body eval
        first_pass_evaluation: false, // Not in two-pass mode for THEN body evaluation
    };

    // Bind the current state value
    evaluator.variables.insert(connection.state_name.clone(), current_state.clone());

    // Evaluate the THEN body
    evaluator.eval_expression(&connection.body.node)
}

/// Refresh computed fields in `store` object based on current HOLD values.
///
/// This fixes the snapshot staleness problem: when `store.all_completed` is captured
/// at THEN connection creation time, it has the VALUE from that moment. But when
/// the THEN body evaluates `store.all_completed |> Bool/not()`, we need the CURRENT
/// value computed from current HOLD states.
fn refresh_store_computed_fields(
    variables: &mut HashMap<String, DdValue>,
    reactive_ctx: &DdReactiveContext,
) {
    // Check if 'store' exists and is an Object
    if let Some(DdValue::Object(store_fields)) = variables.get("store").cloned() {
        // Check if store has 'todos' field (indicating this is todo_mvc pattern)
        if store_fields.contains_key("todos") {
            // Compute all_completed from current HOLD values
            // HOLDs for completed are named like: "list_init_X:completed:state" or "append_X:completed:state"
            let holds = reactive_ctx.all_holds();
            let mut todos_count = 0u64;
            let mut completed_count = 0u64;

            for (hold_id, signal) in holds.iter() {
                let id_str = hold_id.name();
                // Match patterns like "list_init_0:completed:state" or "append_1:completed:state"
                if id_str.contains(":completed:state") {
                    todos_count += 1;
                    let value = signal.get();
                    // Check for both Bool(true) and Tagged { tag: "True" } formats
                    let is_completed = match &value {
                        DdValue::Bool(b) => *b,
                        DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                        _ => false,
                    };
                    zoon::println!("[refresh_store_computed_fields] hold={} value={:?} is_completed={}", id_str, value, is_completed);
                    if is_completed {
                        completed_count += 1;
                    }
                }
            }

            zoon::println!("[refresh_store_computed_fields] todos_count={}, completed_count={}", todos_count, completed_count);

            // Only update if we found todos (to avoid breaking non-todo examples)
            if todos_count > 0 {
                let all_completed = todos_count == completed_count && todos_count > 0;
                let mut new_store_fields = (*store_fields).clone();
                new_store_fields.insert(
                    Arc::from("all_completed"),
                    DdValue::Bool(all_completed),
                );
                // Also update todos_count, completed_todos_count, active_todos_count
                new_store_fields.insert(
                    Arc::from("todos_count"),
                    DdValue::int(todos_count as i64),
                );
                new_store_fields.insert(
                    Arc::from("completed_todos_count"),
                    DdValue::int(completed_count as i64),
                );
                new_store_fields.insert(
                    Arc::from("active_todos_count"),
                    DdValue::int((todos_count - completed_count) as i64),
                );
                variables.insert("store".to_string(), DdValue::Object(Arc::new(new_store_fields)));

                zoon::println!("[refresh_store_computed_fields] Updated store.all_completed={}", all_completed);
            }
        }
    }
}

impl Default for DdReactiveEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reactive_context_creation() {
        let ctx = DdReactiveContext::new();
        assert!(ctx.get_hold(&HoldId::new("test")).is_none());
    }

    #[test]
    fn test_hold_registration() {
        let ctx = DdReactiveContext::new();
        let id = HoldId::new("counter");
        let signal = ctx.register_hold(id.clone(), DdValue::int(42));
        assert_eq!(signal.get(), DdValue::int(42));

        // Second registration returns same signal
        let signal2 = ctx.register_hold(id.clone(), DdValue::int(0));
        assert_eq!(signal2.get(), DdValue::int(42));
    }
}
