//! Output handling for DD values.
//!
//! This module provides the OutputObserver which allows the bridge to
//! observe DD output values as async streams.
//!
//! Also provides global reactive state for HOLD values that the bridge
//! can observe for DOM updates.
//!
//! Persistence: HOLD values are saved to localStorage and restored on re-run.

use std::collections::HashMap;
use super::super::core::DdOutput;
use super::super::dd_value::DdValue;
use zoon::futures_util::stream::Stream;
use zoon::Mutable;
use zoon::signal::MutableSignalCloned;
use zoon::{local_storage, WebStorage};

const DD_HOLD_STORAGE_KEY: &str = "dd_hold_states";

/// Clear all DD persisted hold states.
/// Called when user clicks "Clear saved states" in playground.
pub fn clear_dd_persisted_states() {
    local_storage().remove(DD_HOLD_STORAGE_KEY);
    // Also clear in-memory HOLD_STATES
    HOLD_STATES.with(|states| {
        states.lock_mut().clear();
    });
    zoon::println!("[DD Persist] Cleared all DD states");
}

/// Clear in-memory HOLD_STATES only (not localStorage).
/// Called at the start of each interpretation to prevent state contamination between examples.
pub fn clear_hold_states_memory() {
    HOLD_STATES.with(|states| {
        states.lock_mut().clear();
    });
    CHECKBOX_TOGGLE_HOLDS.with(|holds| {
        holds.lock_mut().clear();
    });
    // Also reset filter and route state to prevent cross-example contamination
    clear_selected_filter();
    clear_current_route();
}

// Global reactive state for HOLD values
// DD collections remain the source of truth; this just mirrors for rendering
thread_local! {
    static HOLD_STATES: Mutable<HashMap<String, DdValue>> = Mutable::new(HashMap::new()); // ALLOWED: view state
}

// Global list of checkbox toggle hold IDs (used for reactive "items left" count)
// Set by interpreter when detecting checkbox toggle pattern
thread_local! {
    static CHECKBOX_TOGGLE_HOLDS: Mutable<Vec<String>> = Mutable::new(Vec::new()); // ALLOWED: view state
}

// Current selected filter (All, Active, Completed)
// Updated by router navigation
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TodoFilter {
    All,
    Active,
    Completed,
}

impl Default for TodoFilter {
    fn default() -> Self {
        Self::All
    }
}

thread_local! {
    static SELECTED_FILTER: Mutable<TodoFilter> = Mutable::new(TodoFilter::All); // ALLOWED: view state
    static CURRENT_ROUTE: Mutable<String> = Mutable::new("/".to_string()); // ALLOWED: route state
}

/// Set the selected filter based on route.
/// Called by router navigation.
pub fn set_filter_from_route(route: &str) {
    let filter = match route {
        "/" => TodoFilter::All,
        "/active" => TodoFilter::Active,
        "/completed" => TodoFilter::Completed,
        _ => TodoFilter::All,
    };
    zoon::println!("[DD Filter] Setting filter to {:?} from route {}", filter, route);
    SELECTED_FILTER.with(|f| f.set(filter));
    // Also update current route for reactive Router/route()
    CURRENT_ROUTE.with(|r| r.set(route.to_string()));
    // Update HOLD_STATES so Router/route() HoldRef is reactive
    update_hold_state_no_persist("current_route", super::super::dd_value::DdValue::text(route));
}

/// Get the current route value.
/// Used by Router/route() when returning a HoldRef.
pub fn get_current_route() -> String {
    CURRENT_ROUTE.with(|r| r.lock_ref().clone())
}

/// Initialize the current route from the browser URL.
pub fn init_current_route() {
    #[cfg(target_arch = "wasm32")]
    {
        use zoon::*;
        let path = window().location().pathname().unwrap_or_else(|_| "/".to_string());
        CURRENT_ROUTE.with(|r| r.set(path.clone()));
        // Also set the filter based on initial route
        let filter = match path.as_str() {
            "/" => TodoFilter::All,
            "/active" => TodoFilter::Active,
            "/completed" => TodoFilter::Completed,
            _ => TodoFilter::All,
        };
        SELECTED_FILTER.with(|f| f.set(filter));
    }
}

/// Clear the current route state.
pub fn clear_current_route() {
    CURRENT_ROUTE.with(|r| r.set("/".to_string()));
}

/// Get a signal for the selected filter.
pub fn selected_filter_signal() -> impl zoon::Signal<Item = TodoFilter> {
    SELECTED_FILTER.with(|f| f.signal_cloned())
}

/// Clear the selected filter (reset to All).
pub fn clear_selected_filter() {
    SELECTED_FILTER.with(|f| f.set(TodoFilter::All));
}

/// Register checkbox toggle hold IDs for reactive count computation.
/// Called by interpreter when detecting the checkbox toggle pattern.
pub fn set_checkbox_toggle_holds(hold_ids: Vec<String>) {
    CHECKBOX_TOGGLE_HOLDS.with(|holds| {
        holds.set(hold_ids);
    });
}

/// Clear checkbox toggle holds.
/// Called when clearing state or starting a new run.
pub fn clear_checkbox_toggle_holds() {
    CHECKBOX_TOGGLE_HOLDS.with(|holds| {
        holds.lock_mut().clear();
    });
}

/// Get a signal for checkbox toggle holds.
/// Returns hold IDs that represent boolean checkbox states.
pub fn checkbox_toggle_holds_signal() -> MutableSignalCloned<Vec<String>> {
    CHECKBOX_TOGGLE_HOLDS.with(|holds| holds.signal_cloned())
}

/// Get current checkbox toggle hold IDs synchronously.
/// Used by the bridge when rendering "N items left" reactively.
pub fn get_checkbox_toggle_holds() -> Vec<String> {
    CHECKBOX_TOGGLE_HOLDS.with(|holds| holds.lock_ref().clone())
}

/// Compute the count of unchecked (false) checkbox toggles.
/// Used for reactive "N items left" rendering.
pub fn get_unchecked_checkbox_count() -> usize {
    CHECKBOX_TOGGLE_HOLDS.with(|holds| {
        let hold_ids = holds.lock_ref();
        HOLD_STATES.with(|states| {
            let states = states.lock_ref();
            hold_ids.iter()
                .filter(|hold_id| {
                    // Count as "active" if NOT completed (false/False)
                    match states.get(*hold_id) {
                        Some(DdValue::Bool(true)) => false,  // completed
                        Some(DdValue::Tagged { tag, .. }) if tag.as_ref() == "True" => false,  // completed
                        _ => true,  // not completed (false, False, or missing)
                    }
                })
                .count()
        })
    })
}

/// Update a HOLD state value and persist to storage.
/// Called by the output listener when DD produces new output.
pub fn update_hold_state(hold_id: &str, value: DdValue) {
    HOLD_STATES.with(|states| {
        states.lock_mut().insert(hold_id.to_string(), value.clone());
    });
    // Persist to localStorage
    persist_hold_value(hold_id, &value);
}

/// Update a HOLD state value WITHOUT persisting.
/// Used for initial value when we want to check persisted value first.
pub fn update_hold_state_no_persist(hold_id: &str, value: DdValue) {
    HOLD_STATES.with(|states| {
        states.lock_mut().insert(hold_id.to_string(), value);
    });
}

/// Load persisted HOLD value from localStorage.
/// Returns None if no persisted value exists.
pub fn load_persisted_hold_value(hold_id: &str) -> Option<DdValue> {
    let storage: HashMap<String, zoon::serde_json::Value> = match local_storage().get::<HashMap<String, zoon::serde_json::Value>>(DD_HOLD_STORAGE_KEY) {
        None => return None,
        Some(Ok(s)) => s,
        Some(Err(_)) => return None, // Ignore deserialization errors
    };

    let json_value = storage.get(hold_id)?;
    json_to_dd_value(json_value)
}

/// Initialize a HOLD with initial value, respecting persisted state.
/// Returns the actual initial value (persisted or provided).
pub fn init_hold_state(hold_id: &str, initial: DdValue) -> DdValue {
    // Try to load persisted value first
    if let Some(persisted) = load_persisted_hold_value(hold_id) {
        zoon::println!("[DD Persist] Restored {} = {:?}", hold_id, persisted);
        update_hold_state_no_persist(hold_id, persisted.clone());
        persisted
    } else {
        zoon::println!("[DD Persist] Using initial {} = {:?}", hold_id, initial);
        update_hold_state_no_persist(hold_id, initial.clone());
        initial
    }
}

/// Persist a HOLD value to localStorage.
fn persist_hold_value(hold_id: &str, value: &DdValue) {
    // Load existing storage
    let mut storage: HashMap<String, zoon::serde_json::Value> = match local_storage().get::<HashMap<String, zoon::serde_json::Value>>(DD_HOLD_STORAGE_KEY) {
        None => HashMap::new(),
        Some(Ok(s)) => s,
        Some(Err(_)) => HashMap::new(), // Start fresh on deserialization error
    };

    // Convert DdValue to JSON and store
    if let Some(json) = dd_value_to_json(value) {
        storage.insert(hold_id.to_string(), json);
        if let Err(e) = local_storage().insert(DD_HOLD_STORAGE_KEY, &storage) {
            zoon::eprintln!("[DD Persist] Failed to save: {:?}", e);
        }
    }
}

/// Convert DdValue to JSON for storage.
fn dd_value_to_json(value: &DdValue) -> Option<zoon::serde_json::Value> {
    use zoon::serde_json::json;
    match value {
        DdValue::Unit => Some(json!(null)),
        DdValue::Bool(b) => Some(json!(b)),
        DdValue::Number(n) => Some(json!(n.0)),
        DdValue::Text(s) => Some(json!(s.as_ref())),
        DdValue::List(items) => {
            let arr: Vec<_> = items.iter().filter_map(|v| dd_value_to_json(v)).collect();
            Some(json!(arr))
        }
        DdValue::Object(fields) => {
            // Persist Objects (like todo items) by recursively converting fields
            let mut obj = zoon::serde_json::Map::new();
            for (key, val) in fields.iter() {
                if let Some(json_val) = dd_value_to_json(val) {
                    obj.insert(key.to_string(), json_val);
                }
            }
            Some(zoon::serde_json::Value::Object(obj))
        }
        // Don't persist complex types - they need code evaluation
        DdValue::Tagged { .. } | DdValue::HoldRef(_) | DdValue::LinkRef(_) | DdValue::TimerRef { .. } | DdValue::WhileRef { .. } | DdValue::ComputedRef { .. } | DdValue::FilteredListRef { .. } | DdValue::ReactiveFilteredList { .. } => None,
    }
}

/// Convert JSON to DdValue.
fn json_to_dd_value(json: &zoon::serde_json::Value) -> Option<DdValue> {
    use zoon::serde_json::Value as JsonValue;
    use std::collections::BTreeMap;
    match json {
        JsonValue::Null => Some(DdValue::Unit),
        JsonValue::Bool(b) => Some(DdValue::Bool(*b)),
        JsonValue::Number(n) => Some(DdValue::float(n.as_f64()?)),
        JsonValue::String(s) => Some(DdValue::text(s.clone())),
        JsonValue::Array(arr) => {
            let items: Vec<_> = arr.iter().filter_map(|v| json_to_dd_value(v)).collect();
            Some(DdValue::List(items.into()))
        }
        JsonValue::Object(obj) => {
            // Restore Objects (like todo items)
            let mut fields = BTreeMap::new();
            for (key, val) in obj.iter() {
                if let Some(dd_val) = json_to_dd_value(val) {
                    fields.insert(std::sync::Arc::from(key.as_str()), dd_val);
                }
            }
            Some(DdValue::Object(std::sync::Arc::new(fields)))
        }
    }
}

/// Get a signal for all HOLD states.
/// The bridge uses this to reactively update the DOM.
pub fn hold_states_signal() -> MutableSignalCloned<HashMap<String, DdValue>> {
    HOLD_STATES.with(|states| states.signal_cloned())
}

/// Get the current value of a specific HOLD.
/// Returns None if the HOLD hasn't been set yet.
pub fn get_hold_value(hold_id: &str) -> Option<DdValue> {
    HOLD_STATES.with(|states| {
        states.lock_ref().get(hold_id).cloned()
    })
}

/// Output observer for receiving values from the DD worker.
///
/// The bridge uses this to observe DD outputs as async streams.
/// All observation is through streams - there's no synchronous access.
pub struct OutputObserver<T> {
    output: DdOutput<T>,
}

impl<T> OutputObserver<T> {
    /// Create a new output observer with the given output channel.
    pub fn new(output: DdOutput<T>) -> Self {
        Self { output }
    }

    /// Convert to an async stream for observation.
    ///
    /// This is the ONLY way to observe DD output values.
    /// The stream emits whenever the DD dataflow produces new output.
    ///
    /// Note: This consumes the observer. Use `stream()` when you're ready
    /// to start observing - you can't call this multiple times.
    pub fn stream(self) -> impl Stream<Item = T> {
        self.output.stream()
    }
}
