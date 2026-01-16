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
use super::super::core::Output;
use super::super::core::types::BoolTag;
use super::super::core::value::Value;
use super::super::LOG_DD_DEBUG;
use zoon::futures_util::stream::Stream;
use zoon::Mutable;
use zoon::signal::MutableSignalCloned;
use zoon::{local_storage, WebStorage};

// Phase 6: Import bridge function for text-clear side effect
#[cfg(target_arch = "wasm32")]
use super::super::render::bridge::clear_dd_text_input_value;

const DD_HOLD_STORAGE_KEY: &str = "dd_hold_states";

/// Clear all DD persisted hold states.
/// Called when user clicks "Clear saved states" in playground.
pub fn clear_dd_persisted_states() {
    local_storage().remove(DD_HOLD_STORAGE_KEY);
    // Also clear in-memory CELL_STATES
    CELL_STATES.with(|states| {
        states.lock_mut().clear();
    });
    if LOG_DD_DEBUG { zoon::println!("[DD Persist] Cleared all DD states"); }
}

/// Clear in-memory CELL_STATES only (not localStorage).
/// Called at the start of each interpretation to prevent state contamination between examples.
pub fn clear_cells_memory() {
    CELL_STATES.with(|states| {
        states.lock_mut().clear();
    });
    CHECKBOX_TOGGLE_HOLDS.with(|holds| {
        holds.lock_mut().clear();
    });
    // Also reset route state to prevent cross-example contamination
    clear_current_route();
}

// ═══════════════════════════════════════════════════════════════════════════
// INITIALIZATION FUNCTIONS (Phase 7)
// These functions are ONLY for setting up initial state BEFORE DD starts.
// They are NOT for reactive updates - all runtime updates must flow through DD.
// ═══════════════════════════════════════════════════════════════════════════

/// Initialize a cell with its initial value (for startup only, NOT reactive updates).
///
/// # Phase 7 Architecture
/// This is called ONLY during interpreter initialization, before the DD worker starts.
/// All subsequent updates to this cell MUST flow through DD events.
///
/// DO NOT call this function in response to user events or runtime changes.
pub fn init_cell(cell_id: impl Into<String>, value: Value) {
    let cell_id = cell_id.into();
    if LOG_DD_DEBUG { zoon::println!("[DD Init] {} = {:?}", cell_id, value); }
    CELL_STATES.with(|states| {
        states.lock_mut().insert(cell_id, value);
    });
}

/// Initialize a cell and also persist it to localStorage.
///
/// # Phase 7 Architecture
/// Same as init_cell, but also persists the value. Used for cells that need
/// to survive page reloads.
pub fn init_cell_with_persist(cell_id: impl Into<String>, value: Value) {
    let cell_id = cell_id.into();
    if LOG_DD_DEBUG { zoon::println!("[DD Init+Persist] {} = {:?}", cell_id, value); }
    CELL_STATES.with(|states| {
        states.lock_mut().insert(cell_id.clone(), value.clone());
    });
    persist_hold_value(&cell_id, &value);
}

/// Sync a cell value from DD output (called by DD worker after processing).
///
/// # Phase 6 Architecture
/// This function is called ONLY by the DD worker to update CELL_STATES.
/// The worker is the single state authority.
///
/// Side effects:
/// - If the cell is a text-clear cell, triggers DOM input clearing
pub fn sync_cell_from_dd(cell_id: impl Into<String>, value: Value) {
    let cell_id = cell_id.into();
    if LOG_DD_DEBUG { zoon::println!("[DD Sync] {} = {:?}", cell_id, value); }

    // Check if this is a text-clear cell BEFORE updating (for side effect)
    let should_clear_text = is_text_clear_cell(&cell_id);

    CELL_STATES.with(|states| {
        states.lock_mut().insert(cell_id, value);
    });

    // Phase 6: Trigger text input clearing as side effect
    #[cfg(target_arch = "wasm32")]
    if should_clear_text {
        clear_dd_text_input_value();
    }
}

/// Sync a cell value from DD output and persist it.
pub fn sync_cell_from_dd_with_persist(cell_id: impl Into<String>, value: Value) {
    let cell_id = cell_id.into();
    if LOG_DD_DEBUG { zoon::println!("[DD Sync+Persist] {} = {:?}", cell_id, value); }
    CELL_STATES.with(|states| {
        states.lock_mut().insert(cell_id.clone(), value.clone());
    });
    persist_hold_value(&cell_id, &value);
}

/// Update a cell value without persistence.
///
/// # Phase 8 Note
/// This function is for internal use by the IO layer when updating
/// cells that don't need to be persisted (e.g., current_route).
/// Runtime updates from DD events should use `sync_cell_from_dd`.
pub fn update_cell_no_persist(cell_id: impl Into<String>, value: Value) {
    let cell_id = cell_id.into();
    if LOG_DD_DEBUG { zoon::println!("[DD Update] {} = {:?}", cell_id, value); }
    CELL_STATES.with(|states| {
        states.lock_mut().insert(cell_id, value);
    });
}

// ═══════════════════════════════════════════════════════════════════════════

// Global reactive state for HOLD values
// DD collections remain the source of truth; this just mirrors for rendering
thread_local! {
    static CELL_STATES: Mutable<HashMap<String, Value>> = Mutable::new(HashMap::new()); // ALLOWED: view state
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
    /// The Cell ID that these bindings control (e.g., "cell_5")
    pub cell_id: Option<String>,
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
    /// The Cell ID that this toggle affects
    pub cell_id: String,
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
    /// The Cell ID that this toggle affects (the list cell like "todos")
    pub list_cell_id: String,
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
    // Flag to skip key_down detection during WHILE pre-evaluation for reactive rendering
    // When this is > 0, we're inside eval_pattern_match pre-evaluating WHILE arms
    static WHILE_PREEVAL_DEPTH: std::cell::Cell<u32> = std::cell::Cell::new(0); // ALLOWED: config state
}

/// Enter WHILE pre-evaluation context.
/// Called at the start of eval_pattern_match for CellRef inputs.
pub fn enter_while_preeval() {
    WHILE_PREEVAL_DEPTH.with(|d| {
        let new_depth = d.get() + 1;
        d.set(new_depth);
        zoon::println!("[DD WHILE preeval] ENTER depth={}", new_depth);
    }); // ALLOWED: IO layer
}

/// Exit WHILE pre-evaluation context.
/// Called at the end of eval_pattern_match for CellRef inputs.
pub fn exit_while_preeval() {
    WHILE_PREEVAL_DEPTH.with(|d| {
        let current = d.get();
        if current > 0 {
            let new_depth = current - 1;
            d.set(new_depth);
            zoon::println!("[DD WHILE preeval] EXIT depth={}", new_depth);
        }
    }); // ALLOWED: IO layer
}

/// Check if we're inside WHILE pre-evaluation context.
fn in_while_preeval() -> bool {
    WHILE_PREEVAL_DEPTH.with(|d| d.get() > 0) // ALLOWED: IO layer
}

/// Set the text_input key_down LinkRef ID.
/// Called by eval_element_function when Element/text_input has a key_down event.
/// Task 4.3: Eliminates extract_text_input_key_down() document scanning.
///
/// NOTE: Skips setting during WHILE pre-evaluation to prevent editing text_inputs
/// inside WHILE branches from overwriting the main document's text_input link.
pub fn set_text_input_key_down_link(link_id: String) {
    let depth = WHILE_PREEVAL_DEPTH.with(|d| d.get());
    if in_while_preeval() {
        zoon::println!("[DD Config] Skipping text_input key_down link in WHILE preeval (depth={}): {}", depth, link_id);
        return;
    }
    zoon::println!("[DD Config] Setting text_input key_down link (depth={}): {}", depth, link_id);
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
    // Update CELL_STATES so Router/route() CellRef is reactive
    update_cell_no_persist("current_route", super::super::core::value::Value::text(route));
}

/// Get the current route value.
/// Used by Router/route() when returning a CellRef.
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
pub fn set_checkbox_toggle_holds(cell_ids: Vec<String>) {
    CHECKBOX_TOGGLE_HOLDS.with(|holds| {
        holds.set(cell_ids);
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
pub fn add_text_clear_cell(cell_id: String) {
    TEXT_CLEAR_HOLDS.with(|holds| holds.borrow_mut().insert(cell_id)); // ALLOWED: IO layer
}

/// Check if a HOLD ID is a text-clear HOLD.
/// Used by output listener to know when to clear text input DOM.
pub fn is_text_clear_cell(cell_id: &str) -> bool {
    TEXT_CLEAR_HOLDS.with(|holds| holds.borrow().contains(cell_id)) // ALLOWED: IO layer
}

/// Clear text-clear hold registry.
/// Called when clearing state or starting a new run.
pub fn clear_text_clear_cells() {
    TEXT_CLEAR_HOLDS.with(|holds| holds.borrow_mut().clear()); // ALLOWED: IO layer
}

// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED (Phase 6.1):
//   - update_cell()
//   - update_cell_no_persist()
//   - clear_cell()
//   - toggle_cell_bool()
//   - toggle_all_list_items_completed()
//
// These functions directly mutated CELL_STATES HashMap, bypassing DD.
//
// Phase 7 TODO: Replace with DD InputHandle injection:
//   - Events flow through DD dataflow graph
//   - DD operators process state transitions
//   - DD output observers update CELL_STATES
// ═══════════════════════════════════════════════════════════════════════════

/// Load persisted HOLD value from localStorage.
/// Returns None if no persisted value exists.
pub fn load_persisted_cell_value(cell_id: &str) -> Option<Value> {
    let storage: HashMap<String, zoon::serde_json::Value> = match local_storage().get::<HashMap<String, zoon::serde_json::Value>>(DD_HOLD_STORAGE_KEY) {
        None => return None,
        Some(Ok(s)) => s,
        Some(Err(_)) => return None, // Ignore deserialization errors
    };

    let json_value = storage.get(cell_id)?;
    json_to_dd_value(json_value)
}

// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED (Phase 6.1): init_cell()
// This function called update_cell_no_persist(), bypassing DD.
//
// Phase 7 TODO: Cell initialization should flow through DD:
//   - Load persisted values via DD InputHandle at startup
//   - DD operators compute initial state
//   - DD output observers populate CELL_STATES
// ═══════════════════════════════════════════════════════════════════════════

/// Persist a HOLD value to localStorage.
fn persist_hold_value(cell_id: &str, value: &Value) {
    // Load existing storage
    let mut storage: HashMap<String, zoon::serde_json::Value> = match local_storage().get::<HashMap<String, zoon::serde_json::Value>>(DD_HOLD_STORAGE_KEY) {
        None => HashMap::new(),
        Some(Ok(s)) => s,
        Some(Err(_)) => HashMap::new(), // Start fresh on deserialization error
    };

    // Convert Value to JSON and store
    if let Some(json) = dd_value_to_json(value) {
        storage.insert(cell_id.to_string(), json);
        if let Err(e) = local_storage().insert(DD_HOLD_STORAGE_KEY, &storage) {
            zoon::eprintln!("[DD Persist] Failed to save: {:?}", e);
        }
    }
}

/// Convert Value to JSON for storage.
fn dd_value_to_json(value: &Value) -> Option<zoon::serde_json::Value> {
    use zoon::serde_json::json;
    use super::super::core::types::BoolTag;
    match value {
        Value::Unit => Some(json!(null)),
        Value::Bool(b) => Some(json!(b)),
        // Handle Tagged booleans (True/False) - serialize as JSON booleans
        Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => {
            Some(json!(BoolTag::is_true(tag.as_ref())))
        }
        Value::Number(n) => Some(json!(n.0)),
        Value::Text(s) => Some(json!(s.as_ref())),
        Value::List(items) => {
            let arr: Vec<_> = items.iter().filter_map(|v| dd_value_to_json(v)).collect();
            Some(json!(arr))
        }
        Value::Collection(handle) => {
            // Persist Collection by converting its snapshot to JSON (same as List)
            let arr: Vec<_> = handle.iter().filter_map(|v| dd_value_to_json(v)).collect();
            Some(json!(arr))
        }
        Value::Object(fields) => {
            // Persist Objects (like list items) by recursively converting fields
            let mut obj = zoon::serde_json::Map::new();
            for (key, val) in fields.iter() {
                if let Some(json_val) = dd_value_to_json(val) {
                    obj.insert(key.to_string(), json_val);
                }
            }
            Some(zoon::serde_json::Value::Object(obj))
        }
        // Dereference CellRefs to persist their actual values
        Value::CellRef(cell_id) => {
            // Look up the actual value in CELL_STATES and persist that
            CELL_STATES.with(|cell| {
                let states = cell.lock_ref(); // ALLOWED: IO layer
                if let Some(value) = states.get(&cell_id.name()) {
                    if LOG_DD_DEBUG { zoon::println!("[DD Persist] CellRef {} -> {:?}", cell_id, value); }
                    dd_value_to_json(value)
                } else {
                    if LOG_DD_DEBUG { zoon::println!("[DD Persist] CellRef {} NOT FOUND in CELL_STATES", cell_id); }
                    None
                }
            })
        }
        // Don't persist complex types - they need code evaluation
        Value::Tagged { .. } | Value::LinkRef(_) | Value::TimerRef { .. } | Value::WhileRef { .. } | Value::ComputedRef { .. } | Value::FilteredListRef { .. } | Value::FilteredListRefWithPredicate { .. } | Value::ReactiveFilteredList { .. } | Value::ReactiveText { .. } | Value::Placeholder | Value::PlaceholderField { .. } | Value::PlaceholderWhileRef { .. } | Value::NegatedPlaceholderField { .. } | Value::MappedListRef { .. } | Value::FilteredMappedListRef { .. } | Value::FilteredMappedListWithPredicate { .. } | Value::LatestRef { .. } | Value::Flushed(_) => None,
    }
}

/// Convert JSON to Value.
fn json_to_dd_value(json: &zoon::serde_json::Value) -> Option<Value> {
    use zoon::serde_json::Value as JsonValue;
    use std::collections::BTreeMap;
    match json {
        JsonValue::Null => Some(Value::Unit),
        // IMPORTANT: Boon uses Tagged booleans (Tagged { tag: "True/False" }), not Rust bools
        // Deserialize JSON booleans as Tagged to maintain type consistency
        JsonValue::Bool(b) => Some(Value::Tagged {
            tag: std::sync::Arc::from(if *b { "True" } else { "False" }),
            fields: std::sync::Arc::new(BTreeMap::new()),
        }),
        JsonValue::Number(n) => Some(Value::float(n.as_f64()?)),
        JsonValue::String(s) => Some(Value::text(s.clone())),
        JsonValue::Array(arr) => {
            let items: Vec<_> = arr.iter().filter_map(|v| json_to_dd_value(v)).collect();
            Some(Value::List(items.into()))
        }
        JsonValue::Object(obj) => {
            // Restore Objects (like list items)
            let mut fields = BTreeMap::new();
            for (key, val) in obj.iter() {
                if let Some(dd_val) = json_to_dd_value(val) {
                    fields.insert(std::sync::Arc::from(key.as_str()), dd_val);
                }
            }
            Some(Value::Object(std::sync::Arc::new(fields)))
        }
    }
}

/// Get a signal for all HOLD states.
/// The bridge uses this to reactively update the DOM.
pub fn cell_states_signal() -> MutableSignalCloned<HashMap<String, Value>> {
    CELL_STATES.with(|states| states.signal_cloned())
}

/// Get the current value of a specific HOLD.
/// Returns None if the HOLD hasn't been set yet.
pub fn get_cell_value(cell_id: &str) -> Option<Value> {
    CELL_STATES.with(|states| {
        states.lock_ref().get(cell_id).cloned()
    })
}

/// Get the current value of a specific HOLD by CellId.
/// Returns None if the HOLD hasn't been set yet.
pub fn get_cell_value_by_id(cell_id: &crate::platform::browser::engine_dd::core::types::CellId) -> Option<Value> {
    get_cell_value(&cell_id.name())
}

/// Get a snapshot of all current HOLD states.
/// This reads the current state without subscribing to changes.
pub fn get_all_cell_states() -> HashMap<String, Value> {
    CELL_STATES.with(|states| {
        states.lock_ref().clone()
    })
}

/// Output observer for receiving values from the DD worker.
///
/// The bridge uses this to observe DD outputs as async streams.
/// All observation is through streams - there's no synchronous access.
pub struct OutputObserver<T> {
    output: Output<T>,
}

impl<T> OutputObserver<T> {
    /// Create a new output observer with the given output channel.
    pub fn new(output: Output<T>) -> Self {
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
