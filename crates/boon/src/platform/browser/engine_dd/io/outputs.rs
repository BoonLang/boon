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
use super::super::LOG_DD_DEBUG;
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
    if LOG_DD_DEBUG { zoon::println!("[DD Persist] Cleared all DD states"); }
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
    // Also reset route state to prevent cross-example contamination
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

// Global set of text-clear HOLD IDs (derived from link IDs, not hardcoded)
// When these HOLDs are updated, the corresponding text input DOM element is cleared
// Task 7.1: Replaces hardcoded "text_input_text" with dynamic link-derived names
thread_local! {
    static TEXT_CLEAR_HOLDS: std::cell::RefCell<std::collections::HashSet<String>> = std::cell::RefCell::new(std::collections::HashSet::new()); // ALLOWED: config state
}

thread_local! {
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
    // Bulk remove event path: parsed from List/remove(item, on: elements.X.event.press |> THEN {...})
    // Stores the full path ["elements", "X"] to the global LinkRef that triggers bulk removal
    // This replaces label-based pattern matching for "Clear completed" button
    static BULK_REMOVE_EVENT_PATH: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new()); // ALLOWED: config state
}

/// Set the detected list variable name.
/// Called by interpreter after detecting the list variable from the Boon code.
pub fn set_list_var_name(name: String) {
    LIST_VAR_NAME.with(|n| *n.borrow_mut() = name); // ALLOWED: IO layer
}

/// Get the detected list variable name.
/// Used by bridge when looking up the list HOLD.
pub fn get_list_var_name() -> String {
    LIST_VAR_NAME.with(|n| n.borrow().clone()) // ALLOWED: IO layer
}

// DEAD CODE DELETED: clear_list_var_name() - never called

/// Set the detected elements field name.
/// Called by interpreter after detecting the elements field from list item objects.
pub fn set_elements_field_name(name: String) {
    ELEMENTS_FIELD_NAME.with(|n| *n.borrow_mut() = name); // ALLOWED: IO layer
}

// DEAD CODE DELETED: get_elements_field_name() - never called
// DEAD CODE DELETED: clear_elements_field_name() - never called

/// Set the remove event path.
/// Parsed from List/remove(item, on: item.X.Y.event.press) → ["X", "Y"]
/// This is the path from item to the LinkRef that triggers removal.
pub fn set_remove_event_path(path: Vec<String>) {
    zoon::println!("[DD Config] Setting remove event path: {:?}", path);
    REMOVE_EVENT_PATH.with(|p| *p.borrow_mut() = path); // ALLOWED: IO layer
}

/// Get the remove event path.
/// Used when cloning templates to wire the correct LinkRef to removal.
pub fn get_remove_event_path() -> Vec<String> {
    REMOVE_EVENT_PATH.with(|p| p.borrow().clone()) // ALLOWED: IO layer
}

/// Clear the remove event path.
/// Called when clearing state between examples.
pub fn clear_remove_event_path() {
    REMOVE_EVENT_PATH.with(|p| p.borrow_mut().clear()); // ALLOWED: IO layer
}

/// Set the bulk remove event path.
/// Parsed from List/remove(item, on: elements.X.event.press |> THEN {...}) → ["elements", "X"]
/// This is the path to the global LinkRef that triggers bulk removal (e.g., "Clear completed" button).
pub fn set_bulk_remove_event_path(path: Vec<String>) {
    zoon::println!("[DD Config] Setting bulk remove event path: {:?}", path);
    BULK_REMOVE_EVENT_PATH.with(|p| *p.borrow_mut() = path); // ALLOWED: IO layer
}

/// Get the bulk remove event path.
/// Used by interpreter to wire the correct LinkRef to bulk removal.
pub fn get_bulk_remove_event_path() -> Vec<String> {
    BULK_REMOVE_EVENT_PATH.with(|p| p.borrow().clone()) // ALLOWED: IO layer
}

/// Clear the bulk remove event path.
/// Called when clearing state between examples.
pub fn clear_bulk_remove_event_path() {
    BULK_REMOVE_EVENT_PATH.with(|p| p.borrow_mut().clear()); // ALLOWED: IO layer
}

/// Editing event bindings parsed from HOLD body.
/// Contains paths to LinkRefs that control editing state.
#[derive(Clone, Debug, Default)]
pub struct EditingEventBindings {
    /// The HOLD ID that these bindings control (e.g., "hold_5")
    pub hold_id: Option<String>,
    /// Path to LinkRef whose double_click triggers edit mode (e.g., ["todo_elements", "todo_title_element"])
    pub edit_trigger_path: Vec<String>,
    /// Actual LinkRef ID for edit trigger (resolved during evaluation)
    pub edit_trigger_link_id: Option<String>,
    /// Path to LinkRef whose key_down exits edit mode on Enter/Escape (e.g., ["todo_elements", "editing_todo_title_element"])
    pub exit_key_path: Vec<String>,
    /// Actual LinkRef ID for exit key (resolved during evaluation)
    pub exit_key_link_id: Option<String>,
    /// Path to LinkRef whose blur exits edit mode (e.g., ["todo_elements", "editing_todo_title_element"])
    pub exit_blur_path: Vec<String>,
    /// Actual LinkRef ID for exit blur (resolved during evaluation)
    pub exit_blur_link_id: Option<String>,
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
    EDITING_EVENT_BINDINGS.with(|b| *b.borrow_mut() = bindings); // ALLOWED: IO layer
}

/// Get the editing event bindings.
pub fn get_editing_event_bindings() -> EditingEventBindings {
    EDITING_EVENT_BINDINGS.with(|b| b.borrow().clone()) // ALLOWED: IO layer
}

/// Clear the editing event bindings.
pub fn clear_editing_event_bindings() {
    EDITING_EVENT_BINDINGS.with(|b| *b.borrow_mut() = EditingEventBindings::default()); // ALLOWED: IO layer
}

/// Toggle event binding parsed from HOLD body.
/// Contains the path to a LinkRef whose click event toggles a boolean HOLD.
#[derive(Clone, Debug)]
pub struct ToggleEventBinding {
    /// The HOLD ID that this toggle affects
    pub hold_id: String,
    /// Path to LinkRef whose click triggers toggle (e.g., ["todo_elements", "todo_checkbox"])
    pub event_path: Vec<String>,
    /// Event type (usually "click")
    pub event_type: String,
    /// Actual LinkRef ID if available (resolved during evaluation)
    /// When present, this takes precedence over event_path resolution
    pub link_id: Option<String>,
}

thread_local! {
    // Toggle event bindings parsed from HOLD bodies
    static TOGGLE_EVENT_BINDINGS: std::cell::RefCell<Vec<ToggleEventBinding>> = std::cell::RefCell::new(Vec::new()); // ALLOWED: config state
}

/// Add a toggle event binding.
/// Parsed from HOLD body expressions like:
/// `todo_elements.todo_checkbox.event.click |> THEN { state |> Bool/not() }`
pub fn add_toggle_event_binding(binding: ToggleEventBinding) {
    zoon::println!("[DD Config] Adding toggle binding: {:?}", binding);
    TOGGLE_EVENT_BINDINGS.with(|b| b.borrow_mut().push(binding)); // ALLOWED: IO layer
}

/// Get all toggle event bindings.
pub fn get_toggle_event_bindings() -> Vec<ToggleEventBinding> {
    TOGGLE_EVENT_BINDINGS.with(|b| b.borrow().clone()) // ALLOWED: IO layer
}

/// Clear all toggle event bindings.
pub fn clear_toggle_event_bindings() {
    TOGGLE_EVENT_BINDINGS.with(|b| b.borrow_mut().clear()); // ALLOWED: IO layer
}

/// Global toggle event binding for "toggle all" patterns.
/// Contains the path to a LinkRef whose click toggles ALL items in a list.
/// Pattern: `store.elements.toggle_all.event.click |> THEN { store.all_completed |> Bool/not() }`
#[derive(Clone, Debug)]
pub struct GlobalToggleEventBinding {
    /// The HOLD ID that this toggle affects (the list HOLD like "todos")
    pub list_hold_id: String,
    /// Path to LinkRef whose click triggers toggle (e.g., ["store", "elements", "toggle_all_checkbox"])
    pub event_path: Vec<String>,
    /// Event type (usually "click")
    pub event_type: String,
    /// Path to the global computed value (e.g., ["store", "all_completed"])
    pub value_path: Vec<String>,
}

thread_local! {
    // Global toggle event bindings parsed from HOLD bodies
    static GLOBAL_TOGGLE_BINDINGS: std::cell::RefCell<Vec<GlobalToggleEventBinding>> = std::cell::RefCell::new(Vec::new()); // ALLOWED: config state
}

/// Add a global toggle event binding.
pub fn add_global_toggle_binding(binding: GlobalToggleEventBinding) {
    zoon::println!("[DD Config] Adding global toggle binding: {:?}", binding);
    GLOBAL_TOGGLE_BINDINGS.with(|b| b.borrow_mut().push(binding)); // ALLOWED: IO layer
}

/// Get all global toggle event bindings.
pub fn get_global_toggle_bindings() -> Vec<GlobalToggleEventBinding> {
    GLOBAL_TOGGLE_BINDINGS.with(|b| b.borrow().clone()) // ALLOWED: IO layer
}

/// Clear all global toggle event bindings.
pub fn clear_global_toggle_bindings() {
    GLOBAL_TOGGLE_BINDINGS.with(|b| b.borrow_mut().clear()); // ALLOWED: IO layer
}

// Text input key_down LinkRef extracted from Element/text_input during evaluation
thread_local! {
    static TEXT_INPUT_KEY_DOWN_LINK: std::cell::Cell<Option<String>> = std::cell::Cell::new(None); // ALLOWED: config state
}

/// Set the text_input key_down LinkRef ID.
/// Called by eval_element_function when Element/text_input has a key_down event.
/// Task 4.3: Eliminates extract_text_input_key_down() document scanning.
pub fn set_text_input_key_down_link(link_id: String) {
    zoon::println!("[DD Config] Setting text_input key_down link: {}", link_id);
    TEXT_INPUT_KEY_DOWN_LINK.with(|l| l.set(Some(link_id))); // ALLOWED: IO layer
}

/// Get the text_input key_down LinkRef ID.
/// Returns the LinkRef ID from Element/text_input's key_down event, if set.
pub fn get_text_input_key_down_link() -> Option<String> {
    TEXT_INPUT_KEY_DOWN_LINK.with(|l| l.take()) // ALLOWED: IO layer
}

/// Clear the text_input key_down LinkRef.
pub fn clear_text_input_key_down_link() {
    TEXT_INPUT_KEY_DOWN_LINK.with(|l| l.set(None)); // ALLOWED: IO layer
}

// List/clear LinkRef extracted from List/clear(on: ...) during evaluation
thread_local! {
    static LIST_CLEAR_LINK: std::cell::Cell<Option<String>> = std::cell::Cell::new(None); // ALLOWED: config state
}

/// Set the List/clear event LinkRef ID.
/// Called by evaluator when List/clear(on: ...) evaluates the on: argument to a LinkRef.
/// Task 6.3: Eliminates extract_button_press_link() document scanning.
pub fn set_list_clear_link(link_id: String) {
    zoon::println!("[DD Config] Setting List/clear link: {}", link_id);
    LIST_CLEAR_LINK.with(|l| l.set(Some(link_id))); // ALLOWED: IO layer
}

/// Get the List/clear event LinkRef ID.
/// Returns the LinkRef ID from List/clear's on: argument, if set.
pub fn get_list_clear_link() -> Option<String> {
    LIST_CLEAR_LINK.with(|l| l.take()) // ALLOWED: IO layer
}

/// Clear the List/clear event LinkRef.
pub fn clear_list_clear_link() {
    LIST_CLEAR_LINK.with(|l| l.set(None)); // ALLOWED: IO layer
}

// Flag for template-based lists (FilteredMappedListWithPredicate/FilteredMappedListRef)
thread_local! {
    static HAS_TEMPLATE_LIST: std::cell::Cell<bool> = std::cell::Cell::new(false); // ALLOWED: config state
}

/// Set the has_template_list flag.
/// Called by evaluator when creating FilteredMappedListWithPredicate or FilteredMappedListRef.
/// Task 6.3: Eliminates has_filtered_mapped_list() document scanning.
pub fn set_has_template_list(value: bool) {
    zoon::println!("[DD Config] Setting has_template_list: {}", value);
    HAS_TEMPLATE_LIST.with(|l| l.set(value)); // ALLOWED: IO layer
}

/// Get the has_template_list flag.
/// Returns true if the document contains template-based lists.
pub fn get_has_template_list() -> bool {
    HAS_TEMPLATE_LIST.with(|l| l.get()) // ALLOWED: IO layer
}

/// Clear the has_template_list flag.
pub fn clear_has_template_list() {
    HAS_TEMPLATE_LIST.with(|l| l.set(false)); // ALLOWED: IO layer
}

/// Set the current route.
/// Called by router navigation. Updates the "current_route" HOLD for reactive filtering.
pub fn set_filter_from_route(route: &str) {
    zoon::println!("[DD Route] Setting route to {}", route);
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
    }
}

/// Clear the current route state.
pub fn clear_current_route() {
    CURRENT_ROUTE.with(|r| r.set("/".to_string()));
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

// DEAD CODE DELETED: get_unchecked_checkbox_count() - never called

/// Register a text-clear HOLD ID (derived from link ID).
/// When this HOLD is updated, the text input DOM will be cleared.
/// Task 7.1: Replaces hardcoded "text_input_text" with dynamic names.
pub fn add_text_clear_hold(hold_id: String) {
    TEXT_CLEAR_HOLDS.with(|holds| {
        holds.borrow_mut().insert(hold_id);
    });
}

/// Check if a HOLD ID is a text-clear HOLD.
/// Used by output listener to know when to clear text input DOM.
pub fn is_text_clear_hold(hold_id: &str) -> bool {
    TEXT_CLEAR_HOLDS.with(|holds| holds.borrow().contains(hold_id))
}

/// Clear text-clear hold registry.
/// Called when clearing state or starting a new run.
pub fn clear_text_clear_holds() {
    TEXT_CLEAR_HOLDS.with(|holds| holds.borrow_mut().clear());
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
    zoon::println!("[DD toggle_hold_bool] CALLED with hold_id={}", hold_id);
    HOLD_STATES.with(|states| {
        let mut lock = states.lock_mut();
        let current = lock.get(hold_id).cloned();
        zoon::println!("[DD toggle_hold_bool] Current value for {}: {:?}", hold_id, current);
        let new_value = match current {
            Some(DdValue::Bool(b)) => DdValue::Bool(!b),
            Some(DdValue::Tagged { tag, .. }) if tag.as_ref() == "True" => DdValue::Bool(false),
            Some(DdValue::Tagged { tag, .. }) if tag.as_ref() == "False" => DdValue::Bool(true),
            _ => DdValue::Bool(true), // Default to true if no current value
        };
        zoon::println!("[DD toggle_hold_bool] New value for {}: {:?}", hold_id, new_value);
        lock.insert(hold_id.to_string(), new_value.clone());
        // Persist the new value
        drop(lock);
        persist_hold_value(hold_id, &new_value);
    });
}

/// Toggle ALL items' completed field in a list HOLD.
/// Used for "toggle all" checkbox that sets all items to completed or not completed.
/// The new value is: NOT(all_completed), i.e., if all are completed -> make all not completed.
pub fn toggle_all_list_items_completed(list_hold_id: &str, completed_field: &str) {
    use std::sync::Arc;

    // Phase 1: Collect data and compute new state (with lock held)
    let (items, all_completed, hold_ids_to_update) = HOLD_STATES.with(|states| {
        let lock = states.lock_mut();
        let current_list = lock.get(list_hold_id).cloned();

        let items = match current_list {
            Some(DdValue::List(items)) => items,
            _ => {
                zoon::println!("[DD ToggleAll] No list found at {}", list_hold_id);
                return (None, false, Vec::new());
            }
        };

        if items.is_empty() {
            zoon::println!("[DD ToggleAll] List is empty, nothing to toggle");
            return (None, false, Vec::new());
        }

        // First collect all hold IDs and their completion states
        // (Don't use .all() because it short-circuits and won't collect all hold IDs!)
        let mut hold_ids: Vec<String> = Vec::new();
        let mut completed_states: Vec<bool> = Vec::new();

        for item in items.iter() {
            if let DdValue::Object(fields) = item {
                match fields.get(completed_field) {
                    Some(DdValue::Bool(b)) => {
                        completed_states.push(*b);
                    }
                    Some(DdValue::HoldRef(hold_id)) => {
                        hold_ids.push(hold_id.to_string());
                        // Look up the HoldRef value
                        let is_completed = lock.get(hold_id.as_ref())
                            .map(|v| matches!(v, DdValue::Bool(true)))
                            .unwrap_or(false);
                        completed_states.push(is_completed);
                    }
                    _ => {
                        completed_states.push(false);
                    }
                }
            } else {
                completed_states.push(false);
            }
        }

        // Now check if all items are completed
        let all_completed = !completed_states.is_empty() && completed_states.iter().all(|&c| c);

        (Some(items), all_completed, hold_ids)
    });

    // Early return if no items
    let items = match items {
        Some(items) => items,
        None => return,
    };

    // New value is the opposite of all_completed
    let new_completed = !all_completed;
    zoon::println!("[DD ToggleAll] all_completed={}, setting all to {}", all_completed, new_completed);

    // Phase 2: Update all HOLDs (with lock held)
    HOLD_STATES.with(|states| {
        let mut lock = states.lock_mut();

        // Update all referenced HOLDs
        for hold_id in &hold_ids_to_update {
            lock.insert(hold_id.clone(), DdValue::Bool(new_completed));
        }

        // Build new items list (fields don't change, only HoldRef values)
        let new_items: Vec<DdValue> = items.iter().map(|item| {
            if let DdValue::Object(fields) = item {
                // If completed is a direct Bool field (not HoldRef), update it
                if fields.get(completed_field).map(|v| matches!(v, DdValue::Bool(_))).unwrap_or(false) {
                    let mut new_fields = (**fields).clone();
                    new_fields.insert(Arc::from(completed_field), DdValue::Bool(new_completed));
                    DdValue::Object(Arc::new(new_fields))
                } else {
                    // HoldRef - no need to clone, the reference stays the same
                    item.clone()
                }
            } else {
                item.clone()
            }
        }).collect();

        // Update the list in HOLD_STATES
        lock.insert(list_hold_id.to_string(), DdValue::List(Arc::new(new_items)));
    });

    // Phase 3: Persist all updated HOLDs (without lock)
    for hold_id in &hold_ids_to_update {
        persist_hold_value(hold_id, &DdValue::Bool(new_completed));
    }

    zoon::println!("[DD ToggleAll] Updated {} items in {}", items.len(), list_hold_id);
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
        if LOG_DD_DEBUG { zoon::println!("[DD Persist] Restored {} = {:?}", hold_id, persisted); }
        update_hold_state_no_persist(hold_id, persisted.clone());
        persisted
    } else {
        if LOG_DD_DEBUG { zoon::println!("[DD Persist] Using initial {} = {:?}", hold_id, initial); }
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
                    if LOG_DD_DEBUG { zoon::println!("[DD Persist] HoldRef {} -> {:?}", hold_id, value); }
                    dd_value_to_json(value)
                } else {
                    if LOG_DD_DEBUG { zoon::println!("[DD Persist] HoldRef {} NOT FOUND in HOLD_STATES", hold_id); }
                    None
                }
            })
        }
        // Don't persist complex types - they need code evaluation
        DdValue::Tagged { .. } | DdValue::LinkRef(_) | DdValue::TimerRef { .. } | DdValue::WhileRef { .. } | DdValue::ComputedRef { .. } | DdValue::FilteredListRef { .. } | DdValue::FilteredListRefWithPredicate { .. } | DdValue::ReactiveFilteredList { .. } | DdValue::ReactiveText { .. } | DdValue::Placeholder | DdValue::PlaceholderField { .. } | DdValue::PlaceholderWhileRef { .. } | DdValue::NegatedPlaceholderField { .. } | DdValue::MappedListRef { .. } | DdValue::FilteredMappedListRef { .. } | DdValue::FilteredMappedListWithPredicate { .. } | DdValue::LatestRef { .. } => None,
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
