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
pub enum ListFilter {
    All,
    Active,
    Completed,
}

impl Default for ListFilter {
    fn default() -> Self {
        Self::All
    }
}

thread_local! {
    static SELECTED_FILTER: Mutable<ListFilter> = Mutable::new(ListFilter::All); // ALLOWED: view state
    static CURRENT_ROUTE: Mutable<String> = Mutable::new("/".to_string()); // ALLOWED: route state
    // Detected list variable name from Boon code (e.g., "items", "list_data", or any List variable)
    // Set by interpreter during initialization, used by bridge for HOLD lookups
    static LIST_VAR_NAME: std::cell::RefCell<String> = std::cell::RefCell::new("list_data".to_string()); // ALLOWED: config state
    // Detected elements field name from list item objects (e.g., "todo_elements", "item_elements")
    // This is the Object field containing LinkRefs for item UI interactions
    static ELEMENTS_FIELD_NAME: std::cell::RefCell<String> = std::cell::RefCell::new("item_elements".to_string()); // ALLOWED: config state
    // Remove event path: parsed from List/remove(item, on: item.X.Y.event.press)
    // Stores the path ["X", "Y"] from item to the LinkRef that triggers removal
    // This is PARSED FROM CODE, not guessed from field names
    static REMOVE_EVENT_PATH: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new()); // ALLOWED: config state
}

/// Set the detected list variable name.
/// Called by interpreter after detecting the list variable from the Boon code.
pub fn set_list_var_name(name: String) {
    LIST_VAR_NAME.with(|n| *n.borrow_mut() = name);
}

/// Get the detected list variable name.
/// Used by bridge when looking up the list HOLD.
pub fn get_list_var_name() -> String {
    LIST_VAR_NAME.with(|n| n.borrow().clone())
}

/// Clear the list variable name (reset to default).
/// Called when clearing state between examples.
pub fn clear_list_var_name() {
    LIST_VAR_NAME.with(|n| *n.borrow_mut() = "list_data".to_string());
}

/// Set the detected elements field name.
/// Called by interpreter after detecting the elements field from list item objects.
pub fn set_elements_field_name(name: String) {
    ELEMENTS_FIELD_NAME.with(|n| *n.borrow_mut() = name);
}

/// Get the detected elements field name.
/// Used when looking up LinkRefs in list item objects.
pub fn get_elements_field_name() -> String {
    ELEMENTS_FIELD_NAME.with(|n| n.borrow().clone())
}

/// Clear the elements field name (reset to default).
/// Called when clearing state between examples.
pub fn clear_elements_field_name() {
    ELEMENTS_FIELD_NAME.with(|n| *n.borrow_mut() = "item_elements".to_string());
}

/// Set the remove event path.
/// Parsed from List/remove(item, on: item.X.Y.event.press) → ["X", "Y"]
/// This is the path from item to the LinkRef that triggers removal.
pub fn set_remove_event_path(path: Vec<String>) {
    zoon::println!("[DD Config] Setting remove event path: {:?}", path);
    REMOVE_EVENT_PATH.with(|p| *p.borrow_mut() = path);
}

/// Get the remove event path.
/// Used when cloning templates to wire the correct LinkRef to removal.
pub fn get_remove_event_path() -> Vec<String> {
    REMOVE_EVENT_PATH.with(|p| p.borrow().clone())
}

/// Clear the remove event path.
/// Called when clearing state between examples.
pub fn clear_remove_event_path() {
    REMOVE_EVENT_PATH.with(|p| p.borrow_mut().clear());
}

/// Editing event bindings parsed from HOLD body.
/// Contains paths to LinkRefs that control editing state.
#[derive(Clone, Debug, Default)]
pub struct EditingEventBindings {
    /// Path to LinkRef whose double_click triggers edit mode (e.g., ["todo_elements", "todo_title_element"])
    pub edit_trigger_path: Vec<String>,
    /// Path to LinkRef whose key_down exits edit mode on Enter/Escape (e.g., ["todo_elements", "editing_todo_title_element"])
    pub exit_key_path: Vec<String>,
    /// Path to LinkRef whose blur exits edit mode (e.g., ["todo_elements", "editing_todo_title_element"])
    pub exit_blur_path: Vec<String>,
}

thread_local! {
    // Editing event bindings parsed from HOLD body
    static EDITING_EVENT_BINDINGS: std::cell::RefCell<EditingEventBindings> = std::cell::RefCell::new(EditingEventBindings::default()); // ALLOWED: config state
}

/// Set the editing event bindings.
/// Parsed from HOLD body expressions like:
/// `todo_elements.todo_title_element.event.double_click |> THEN { True }`
pub fn set_editing_event_bindings(bindings: EditingEventBindings) {
    zoon::println!("[DD Config] Setting editing bindings: {:?}", bindings);
    EDITING_EVENT_BINDINGS.with(|b| *b.borrow_mut() = bindings);
}

/// Get the editing event bindings.
pub fn get_editing_event_bindings() -> EditingEventBindings {
    EDITING_EVENT_BINDINGS.with(|b| b.borrow().clone())
}

/// Clear the editing event bindings.
pub fn clear_editing_event_bindings() {
    EDITING_EVENT_BINDINGS.with(|b| *b.borrow_mut() = EditingEventBindings::default());
}

/// Set the selected filter based on route.
/// Called by router navigation.
pub fn set_filter_from_route(route: &str) {
    let filter = match route {
        "/" => ListFilter::All,
        "/active" => ListFilter::Active,
        "/completed" => ListFilter::Completed,
        _ => ListFilter::All,
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
            "/" => ListFilter::All,
            "/active" => ListFilter::Active,
            "/completed" => ListFilter::Completed,
            _ => ListFilter::All,
        };
        SELECTED_FILTER.with(|f| f.set(filter));
    }
}

/// Clear the current route state.
pub fn clear_current_route() {
    CURRENT_ROUTE.with(|r| r.set("/".to_string()));
}

/// Get a signal for the selected filter.
pub fn selected_filter_signal() -> impl zoon::Signal<Item = ListFilter> {
    SELECTED_FILTER.with(|f| f.signal_cloned())
}

/// Clear the selected filter (reset to All).
pub fn clear_selected_filter() {
    SELECTED_FILTER.with(|f| f.set(ListFilter::All));
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
    zoon::println!("[DD HOLD] update_hold_state('{}', {:?})", hold_id, value);
    HOLD_STATES.with(|states| {
        let mut lock = states.lock_mut();
        let old_value = lock.get(hold_id).cloned();
        lock.insert(hold_id.to_string(), value.clone());
        zoon::println!("[DD HOLD] {} changed: {:?} -> {:?}", hold_id, old_value, value);
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

/// Remove a HOLD state (used to clean up temporary editing text holds).
pub fn clear_hold_state(hold_id: &str) {
    HOLD_STATES.with(|states| {
        states.lock_mut().remove(hold_id);
    });
}

/// Toggle a boolean HOLD value (for checkbox interactions).
/// Reads the current value, inverts it, and updates both memory and persistent storage.
pub fn toggle_hold_bool(hold_id: &str) {
    HOLD_STATES.with(|states| {
        let mut lock = states.lock_mut();
        let current = lock.get(hold_id).cloned();
        let new_value = match current {
            Some(DdValue::Bool(b)) => DdValue::Bool(!b),
            Some(DdValue::Tagged { tag, .. }) if tag.as_ref() == "True" => DdValue::Bool(false),
            Some(DdValue::Tagged { tag, .. }) if tag.as_ref() == "False" => DdValue::Bool(true),
            _ => DdValue::Bool(true), // Default to true if no current value
        };
        lock.insert(hold_id.to_string(), new_value.clone());
        // Persist the new value
        drop(lock);
        persist_hold_value(hold_id, &new_value);
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
            // Persist Objects (like list items) by recursively converting fields
            let mut obj = zoon::serde_json::Map::new();
            for (key, val) in fields.iter() {
                if let Some(json_val) = dd_value_to_json(val) {
                    obj.insert(key.to_string(), json_val);
                }
            }
            Some(zoon::serde_json::Value::Object(obj))
        }
        // Dereference HoldRefs to persist their actual values
        DdValue::HoldRef(hold_id) => {
            // Look up the actual value in HOLD_STATES and persist that
            HOLD_STATES.with(|cell| {
                let states = cell.lock_ref(); // ALLOWED: IO layer
                if let Some(value) = states.get(hold_id.as_ref()) {
                    zoon::println!("[DD Persist] HoldRef {} -> {:?}", hold_id, value);
                    dd_value_to_json(value)
                } else {
                    zoon::println!("[DD Persist] HoldRef {} NOT FOUND in HOLD_STATES", hold_id);
                    None
                }
            })
        }
        // Don't persist complex types - they need code evaluation
        DdValue::Tagged { .. } | DdValue::LinkRef(_) | DdValue::TimerRef { .. } | DdValue::WhileRef { .. } | DdValue::ComputedRef { .. } | DdValue::FilteredListRef { .. } | DdValue::ReactiveFilteredList { .. } => None,
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
            // Restore Objects (like list items)
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

/// Get a snapshot of all current HOLD states.
/// This reads the current state without subscribing to changes.
pub fn get_all_hold_states() -> HashMap<String, DdValue> {
    HOLD_STATES.with(|states| {
        states.lock_ref().clone()
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
