//! Output handling for DD values.
//! Build marker: 2026-01-18-v2 (VecDiff diff detection)
//!
//! This module provides the OutputObserver which allows the bridge to
//! observe DD output values as async streams.
//!
//! Also provides global reactive state for HOLD values that the bridge
//! can observe for DOM updates.
//!
//! Persistence: HOLD values are saved to localStorage and restored on re-run.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use super::super::core::Output;
use super::super::core::value::Value;
// Phase 7.3: Import config accessor functions
use super::super::core::{
    is_text_clear_cell as core_is_text_clear_cell,
    get_remove_event_path as core_get_remove_event_path,
    get_bulk_remove_event_path as core_get_bulk_remove_event_path,
    get_editing_bindings as core_get_editing_bindings,
    get_toggle_bindings as core_get_toggle_bindings,
    get_global_toggle_bindings as core_get_global_toggle_bindings,
    EditingBinding, ToggleBinding, GlobalToggleBinding,
};
use super::super::LOG_DD_DEBUG;
use zoon::futures_util::stream::Stream;
use zoon::Mutable;
use zoon::signal::MutableSignalCloned;
use zoon::futures_signals::signal_vec::{MutableVec, SignalVec};
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
    // Phase 12: Clear list signal vecs
    clear_list_signal_vecs();
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
/// - If the value is a list, updates the MutableVec for incremental rendering (Phase 12)
pub fn sync_cell_from_dd(cell_id: impl Into<String>, value: Value) {
    let cell_id = cell_id.into();
    if LOG_DD_DEBUG { zoon::println!("[DD Sync] {} = {:?}", cell_id, value); }

    // Check if this is a text-clear cell BEFORE updating (for side effect)
    let should_clear_text = is_text_clear_cell(&cell_id);

    // Phase 2: Handle MultiCellUpdate - atomic batch of updates
    // This eliminates side effects in transforms by returning multiple updates
    if let Value::MultiCellUpdate(updates) = &value {
        if LOG_DD_DEBUG { zoon::println!("[DD MultiCellUpdate] {} updates", updates.len()); }
        for (cell_id, cell_value) in updates {
            // Recursively apply each update (may include ListDiff variants)
            sync_cell_from_dd(cell_id.as_ref(), (**cell_value).clone());
        }
        return; // All updates applied
    }

    // Phase 2.1: Handle ListDiff variants directly - O(delta) operations
    match &value {
        // ListPush: O(1) append to MutableVec and CELL_STATES
        Value::ListPush { cell_id: diff_cell_id, item } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} Push: {:?}", diff_cell_id, item); }
            apply_list_push(diff_cell_id, item);
            return; // Don't store the diff itself, it's already applied
        }
        // ListRemoveAt: O(n) shift but no clone
        Value::ListRemoveAt { cell_id: diff_cell_id, index } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} RemoveAt({})", diff_cell_id, index); }
            apply_list_remove_at(diff_cell_id, *index);
            return;
        }
        // ListRemoveByKey: O(1) key lookup
        Value::ListRemoveByKey { cell_id: diff_cell_id, key } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} RemoveByKey({})", diff_cell_id, key); }
            apply_list_remove_by_key(diff_cell_id, key);
            return;
        }
        // ListRemoveBatch: O(k) batch removal
        Value::ListRemoveBatch { cell_id: diff_cell_id, keys } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} RemoveBatch({} keys)", diff_cell_id, keys.len()); }
            apply_list_remove_batch(diff_cell_id, &keys);
            return;
        }
        // ListClear: O(1) clear
        Value::ListClear { cell_id: diff_cell_id } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} Clear", diff_cell_id); }
            apply_list_clear(diff_cell_id);
            return;
        }
        // ListItemUpdate: O(1) lookup + O(1) update
        Value::ListItemUpdate { cell_id: diff_cell_id, key, field_path, new_value } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} ItemUpdate({}, {:?})", diff_cell_id, key, field_path); }
            apply_list_item_update(diff_cell_id, key, field_path, new_value);
            return;
        }
        // Phase 12: Update MutableVec for list values (incremental rendering)
        Value::List(items) => {
            update_list_signal_vec(&cell_id, items);
        }
        Value::Collection(handle) => {
            let items: Vec<Value> = handle.iter().cloned().collect();
            update_list_signal_vec(&cell_id, &items);
        }
        _ => {}
    }

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

    // Phase 2: Handle MultiCellUpdate - atomic batch of updates with persistence
    if let Value::MultiCellUpdate(updates) = &value {
        if LOG_DD_DEBUG { zoon::println!("[DD MultiCellUpdate+Persist] {} updates", updates.len()); }
        for (update_cell_id, cell_value) in updates {
            // Recursively apply each update with persistence
            sync_cell_from_dd_with_persist(update_cell_id.as_ref(), (**cell_value).clone());
        }
        return; // All updates applied
    }

    // Phase 2.1: Handle ListDiff variants directly - O(delta) operations
    match &value {
        // ListPush: O(1) append, then persist
        Value::ListPush { cell_id: diff_cell_id, item } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff+Persist] {} Push", diff_cell_id); }
            apply_list_push(diff_cell_id, item);
            // Persist the updated list
            if let Some(updated) = get_cell_value(diff_cell_id) {
                persist_hold_value(diff_cell_id, &updated);
            }
            return;
        }
        // ListRemoveByKey: O(1) lookup, then persist
        Value::ListRemoveByKey { cell_id: diff_cell_id, key } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff+Persist] {} RemoveByKey({})", diff_cell_id, key); }
            apply_list_remove_by_key(diff_cell_id, key);
            // Persist the updated list
            if let Some(updated) = get_cell_value(diff_cell_id) {
                persist_hold_value(diff_cell_id, &updated);
            }
            return;
        }
        // ListRemoveBatch: O(k) batch removal, then persist
        Value::ListRemoveBatch { cell_id: diff_cell_id, keys } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff+Persist] {} RemoveBatch({} keys)", diff_cell_id, keys.len()); }
            apply_list_remove_batch(diff_cell_id, &keys);
            // Persist the updated list
            if let Some(updated) = get_cell_value(diff_cell_id) {
                persist_hold_value(diff_cell_id, &updated);
            }
            return;
        }
        // ListClear: O(1) clear, then persist
        Value::ListClear { cell_id: diff_cell_id } => {
            if LOG_DD_DEBUG { zoon::println!("[DD ListDiff+Persist] {} Clear", diff_cell_id); }
            apply_list_clear(diff_cell_id);
            // Persist the cleared list
            if let Some(updated) = get_cell_value(diff_cell_id) {
                persist_hold_value(diff_cell_id, &updated);
            }
            return;
        }
        // ListRemoveAt and ListItemUpdate - apply and persist
        Value::ListRemoveAt { cell_id: diff_cell_id, index } => {
            apply_list_remove_at(diff_cell_id, *index);
            if let Some(updated) = get_cell_value(diff_cell_id) {
                persist_hold_value(diff_cell_id, &updated);
            }
            return;
        }
        Value::ListItemUpdate { cell_id: diff_cell_id, key, field_path, new_value } => {
            apply_list_item_update(diff_cell_id, key, field_path, new_value);
            if let Some(updated) = get_cell_value(diff_cell_id) {
                persist_hold_value(diff_cell_id, &updated);
            }
            return;
        }
        // Phase 12: Update MutableVec for list values (incremental rendering)
        Value::List(items) => {
            update_list_signal_vec(&cell_id, items);
        }
        Value::Collection(handle) => {
            let items: Vec<Value> = handle.iter().cloned().collect();
            update_list_signal_vec(&cell_id, &items);
        }
        _ => {}
    }

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

    // Phase 2.1: Handle ListDiff variants directly
    match &value {
        Value::ListPush { cell_id: diff_cell_id, item } => {
            apply_list_push(diff_cell_id, item);
            return;
        }
        Value::ListRemoveByKey { cell_id: diff_cell_id, key } => {
            apply_list_remove_by_key(diff_cell_id, key);
            return;
        }
        Value::ListClear { cell_id: diff_cell_id } => {
            apply_list_clear(diff_cell_id);
            return;
        }
        _ => {}
    }

    CELL_STATES.with(|states| {
        states.lock_mut().insert(cell_id, value);
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 2.1: ListDiff Application Functions
// These apply O(delta) operations directly to MutableVec and CELL_STATES
// ═══════════════════════════════════════════════════════════════════════════

/// Apply ListPush diff - O(1) append
fn apply_list_push(cell_id: &str, item: &Value) {
    // Update MutableVec for incremental rendering
    LIST_SIGNAL_VECS.with(|vecs| {
        let mut vecs = vecs.borrow_mut(); // ALLOWED: IO layer
        if let Some(mvec) = vecs.get_mut(cell_id) {
            mvec.lock_mut().push_cloned(item.clone());
        } else {
            // First item - create new MutableVec
            let mvec = MutableVec::new_with_values(vec![item.clone()]);
            vecs.insert(cell_id.to_string(), mvec);
        }
    });

    // Update CELL_STATES by appending to the stored list
    CELL_STATES.with(|states| {
        let mut lock = states.lock_mut();
        if let Some(current) = lock.get(cell_id) {
            if let Some(items) = current.as_list_items() {
                let mut new_items: Vec<Value> = items.to_vec();
                new_items.push(item.clone());
                lock.insert(cell_id.to_string(), Value::collection(new_items));
            }
        } else {
            // No existing value - create new collection
            lock.insert(cell_id.to_string(), Value::collection(vec![item.clone()]));
        }
    });
}

/// Apply ListRemoveAt diff - O(n) shift but no clone needed
fn apply_list_remove_at(cell_id: &str, index: usize) {
    // Update MutableVec
    LIST_SIGNAL_VECS.with(|vecs| {
        let vecs = vecs.borrow(); // ALLOWED: IO layer
        if let Some(mvec) = vecs.get(cell_id) {
            let mut lock = mvec.lock_mut();
            if index < lock.len() {
                lock.remove(index);
            }
        }
    });

    // Update CELL_STATES
    CELL_STATES.with(|states| {
        let mut lock = states.lock_mut();
        if let Some(current) = lock.get(cell_id).cloned() {
            if let Some(items) = current.as_list_items() {
                if index < items.len() {
                    let mut new_items: Vec<Value> = items.to_vec();
                    new_items.remove(index);
                    lock.insert(cell_id.to_string(), Value::collection(new_items));
                }
            }
        }
    });
}

/// Apply ListRemoveByKey diff - O(1) key lookup
fn apply_list_remove_by_key(cell_id: &str, key: &str) {
    // Find index by key first
    let index = CELL_STATES.with(|states| {
        let lock = states.lock_ref();
        if let Some(current) = lock.get(cell_id) {
            if let Some(items) = current.as_list_items() {
                return items.iter().position(|item| extract_item_key(item) == key);
            }
        }
        None
    });

    if let Some(idx) = index {
        // Update MutableVec
        LIST_SIGNAL_VECS.with(|vecs| {
            let vecs = vecs.borrow(); // ALLOWED: IO layer
            if let Some(mvec) = vecs.get(cell_id) {
                let mut lock = mvec.lock_mut();
                if idx < lock.len() {
                    lock.remove(idx);
                }
            }
        });

        // Update CELL_STATES
        CELL_STATES.with(|states| {
            let mut lock = states.lock_mut();
            if let Some(current) = lock.get(cell_id).cloned() {
                if let Some(items) = current.as_list_items() {
                    let mut new_items: Vec<Value> = items.to_vec();
                    if idx < new_items.len() {
                        new_items.remove(idx);
                    }
                    lock.insert(cell_id.to_string(), Value::collection(new_items));
                }
            }
        });

        if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} RemoveByKey({}) at index {}", cell_id, key, idx); }
    } else {
        if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} RemoveByKey({}) - key not found", cell_id, key); }
    }
}

/// Apply ListRemoveBatch diff - O(k) batch removal where k = keys.len()
/// Removes all items whose keys are in the provided set.
/// This is more efficient than multiple individual RemoveByKey operations
/// because it processes all removals in a single pass.
fn apply_list_remove_batch(cell_id: &str, keys: &[Arc<str>]) {
    // Build set of keys to remove for O(1) lookup
    let keys_to_remove: HashSet<&str> = keys.iter().map(|k| k.as_ref()).collect();

    if keys_to_remove.is_empty() {
        return;
    }

    // Find indices to remove (in reverse order for safe removal)
    let indices_to_remove: Vec<usize> = CELL_STATES.with(|states| {
        let lock = states.lock_ref();
        if let Some(current) = lock.get(cell_id) {
            if let Some(items) = current.as_list_items() {
                return items.iter().enumerate()
                    .filter_map(|(i, item)| {
                        let key = extract_item_key(item);
                        if keys_to_remove.contains(key.as_str()) {
                            Some(i)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()  // Reverse for safe removal from end
                    .collect();
            }
        }
        Vec::new()
    });

    if indices_to_remove.is_empty() {
        if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} RemoveBatch - no matching keys found", cell_id); }
        return;
    }

    // Update MutableVec (removing from end to front to preserve indices)
    LIST_SIGNAL_VECS.with(|vecs| {
        let vecs = vecs.borrow(); // ALLOWED: IO layer
        if let Some(mvec) = vecs.get(cell_id) {
            let mut lock = mvec.lock_mut();
            for &idx in &indices_to_remove {
                if idx < lock.len() {
                    lock.remove(idx);
                }
            }
        }
    });

    // Update CELL_STATES
    CELL_STATES.with(|states| {
        let mut lock = states.lock_mut();
        if let Some(current) = lock.get(cell_id).cloned() {
            if let Some(items) = current.as_list_items() {
                // Filter out items with matching keys (more efficient than individual removes)
                let new_items: Vec<Value> = items.iter()
                    .filter(|item| {
                        let key = extract_item_key(item);
                        !keys_to_remove.contains(key.as_str())
                    })
                    .cloned()
                    .collect();
                lock.insert(cell_id.to_string(), Value::collection(new_items));
            }
        }
    });

    if LOG_DD_DEBUG { zoon::println!("[DD ListDiff] {} RemoveBatch removed {} items", cell_id, indices_to_remove.len()); }
}

/// Apply ListClear diff - O(1) clear
fn apply_list_clear(cell_id: &str) {
    // Update MutableVec
    LIST_SIGNAL_VECS.with(|vecs| {
        let vecs = vecs.borrow(); // ALLOWED: IO layer
        if let Some(mvec) = vecs.get(cell_id) {
            mvec.lock_mut().clear();
        }
    });

    // Update CELL_STATES
    CELL_STATES.with(|states| {
        states.lock_mut().insert(cell_id.to_string(), Value::collection(Vec::<Value>::new()));
    });
}

/// Apply ListItemUpdate diff - O(1) lookup + O(1) field update
fn apply_list_item_update(cell_id: &str, key: &str, field_path: &[Arc<str>], new_value: &Value) {
    // Find index by key
    let index = CELL_STATES.with(|states| {
        let lock = states.lock_ref();
        if let Some(current) = lock.get(cell_id) {
            if let Some(items) = current.as_list_items() {
                return items.iter().position(|item| extract_item_key(item) == key);
            }
        }
        None
    });

    if let Some(idx) = index {
        // Update the item in CELL_STATES
        CELL_STATES.with(|states| {
            let mut lock = states.lock_mut();
            if let Some(current) = lock.get(cell_id).cloned() {
                if let Some(items) = current.as_list_items() {
                    let mut new_items: Vec<Value> = items.to_vec();
                    if idx < new_items.len() {
                        // Apply field update
                        new_items[idx] = apply_field_update(&new_items[idx], field_path, new_value);
                        lock.insert(cell_id.to_string(), Value::collection(new_items));

                        // Update MutableVec at same index
                        LIST_SIGNAL_VECS.with(|vecs| {
                            let vecs = vecs.borrow(); // ALLOWED: IO layer
                            if let Some(mvec) = vecs.get(cell_id) {
                                let mut mvec_lock = mvec.lock_mut();
                                if idx < mvec_lock.len() {
                                    mvec_lock.set_cloned(idx, apply_field_update(&mvec_lock[idx], field_path, new_value));
                                }
                            }
                        });
                    }
                }
            }
        });
    }
}

/// Apply a field update to a value at the given path
fn apply_field_update(value: &Value, field_path: &[Arc<str>], new_value: &Value) -> Value {
    if field_path.is_empty() {
        return new_value.clone();
    }

    let field = &field_path[0];
    let remaining = &field_path[1..];

    match value {
        Value::Object(fields) => {
            let mut new_fields = (**fields).clone();
            if remaining.is_empty() {
                new_fields.insert(field.clone(), new_value.clone());
            } else if let Some(inner) = fields.get(field.as_ref()) {
                new_fields.insert(field.clone(), apply_field_update(inner, remaining, new_value));
            }
            Value::Object(Arc::new(new_fields))
        }
        Value::Tagged { tag, fields } => {
            let mut new_fields = (**fields).clone();
            if remaining.is_empty() {
                new_fields.insert(field.clone(), new_value.clone());
            } else if let Some(inner) = fields.get(field.as_ref()) {
                new_fields.insert(field.clone(), apply_field_update(inner, remaining, new_value));
            }
            Value::Tagged {
                tag: tag.clone(),
                fields: Arc::new(new_fields),
            }
        }
        _ => value.clone(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════

// Global reactive state for HOLD values
// DD collections remain the source of truth; this just mirrors for rendering
thread_local! {
    static CELL_STATES: Mutable<HashMap<String, Value>> = Mutable::new(HashMap::new()); // ALLOWED: view state
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 12: List Signal Infrastructure
// Provides MutableVec per list cell for incremental rendering via VecDiff.
// Bridge uses list_signal_vec() with children_signal_vec() for O(delta) DOM updates.
// ═══════════════════════════════════════════════════════════════════════════

thread_local! {
    /// Per-list MutableVec for incremental rendering.
    /// Key: cell_id of list cell
    /// Value: MutableVec containing list items as cloneable handles
    ///
    /// When sync_cell_from_dd() is called with a List value, we update the MutableVec.
    /// Bridge uses list_signal_vec() to get SignalVec for children_signal_vec().
    static LIST_SIGNAL_VECS: std::cell::RefCell<HashMap<String, MutableVec<Value>>> =
        std::cell::RefCell::new(HashMap::new()); // ALLOWED: incremental rendering state
}

// ============================================================================
// Phase 7.3: DELETED OLD THREAD_LOCAL REGISTRIES (config-only, no reactive signals)
// - TEXT_CLEAR_HOLDS -> DataflowConfig.text_clear_cells
// - REMOVE_EVENT_PATH -> DataflowConfig.remove_event_path
// - BULK_REMOVE_EVENT_PATH -> DataflowConfig.bulk_remove_event_path
// - EDITING_EVENT_BINDINGS -> DataflowConfig.editing_bindings
// - TOGGLE_EVENT_BINDINGS -> DataflowConfig.toggle_bindings
// - GLOBAL_TOGGLE_BINDINGS -> DataflowConfig.global_toggle_bindings
//
// DELETED: CHECKBOX_TOGGLE_HOLDS - was set but never read (dead code)
// ============================================================================

thread_local! {
    static CURRENT_ROUTE: Mutable<String> = Mutable::new("/".to_string()); // ALLOWED: route state
}

// DEAD CODE DELETED: set_list_var_name(), get_list_var_name(), clear_list_var_name() - set but never read
// DEAD CODE DELETED: set_elements_field_name(), get_elements_field_name(), clear_elements_field_name() - set but never read

// Phase 7.3: set_remove_event_path DELETED - now set via DataflowConfig

/// Get the remove event path.
/// Used when cloning templates to wire the correct LinkRef to removal.
/// Phase 7.3: Delegates to DataflowConfig via core accessor.
pub fn get_remove_event_path() -> Vec<String> {
    core_get_remove_event_path()
}

// Phase 7.3: clear_remove_event_path DELETED - config cleared via clear_active_config

// Phase 7.3: set_bulk_remove_event_path DELETED - now set via DataflowConfig

/// Get the bulk remove event path.
/// Used by interpreter to wire the correct LinkRef to bulk removal.
/// Phase 7.3: Delegates to DataflowConfig via core accessor.
pub fn get_bulk_remove_event_path() -> Vec<String> {
    core_get_bulk_remove_event_path()
}

// Phase 7.3: clear_bulk_remove_event_path DELETED - config cleared via clear_active_config

// ============================================================================
// Phase 7.3: DELETED OLD REGISTRY TYPES AND FUNCTIONS
//
// The following were removed and replaced with DataflowConfig:
// - EditingEventBindings struct -> now EditingBinding in worker.rs
// - ToggleEventBinding struct -> now ToggleBinding in worker.rs
// - GlobalToggleEventBinding struct -> now GlobalToggleBinding in worker.rs
// - EDITING_EVENT_BINDINGS thread_local
// - TOGGLE_EVENT_BINDINGS thread_local
// - GLOBAL_TOGGLE_BINDINGS thread_local
// - set_editing_event_bindings() -> now DataflowConfig::set_editing_bindings()
// - add_toggle_event_binding() -> now DataflowConfig::add_toggle_binding()
// - add_global_toggle_binding() -> now DataflowConfig::add_global_toggle_binding()
// - clear_editing_event_bindings() -> cleared via clear_active_config()
// - clear_toggle_event_bindings() -> cleared via clear_active_config()
// - clear_global_toggle_bindings() -> cleared via clear_active_config()
// ============================================================================

// Re-export types from core for backward compatibility
pub type EditingEventBindings = EditingBinding;
pub type ToggleEventBinding = ToggleBinding;
pub type GlobalToggleEventBinding = GlobalToggleBinding;

/// Get the editing event bindings.
/// Phase 7.3: Delegates to DataflowConfig via core accessor.
pub fn get_editing_event_bindings() -> Vec<EditingBinding> {
    core_get_editing_bindings()
}

/// Get all toggle event bindings.
/// Phase 7.3: Delegates to DataflowConfig via core accessor.
pub fn get_toggle_event_bindings() -> Vec<ToggleBinding> {
    core_get_toggle_bindings()
}

/// Get all global toggle event bindings.
/// Phase 7.3: Delegates to DataflowConfig via core accessor.
pub fn get_global_toggle_bindings() -> Vec<GlobalToggleBinding> {
    core_get_global_toggle_bindings()
}

// Text input key_down LinkRef extracted from Element/text_input during evaluation
thread_local! {
    static TEXT_INPUT_KEY_DOWN_LINK: std::cell::Cell<Option<String>> = std::cell::Cell::new(None); // ALLOWED: config state
}

// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED: WHILE_PREEVAL_DEPTH, enter_while_preeval(), exit_while_preeval(), in_while_preeval()
//
// This hack was needed because cell_states_signal() (broadcast anti-pattern) caused
// spurious re-renders during WHILE pre-evaluation. With cell_states_signal() removed
// (Phase 11b), fine-grained signals prevent the issue. The actors engine never needed
// this hack because it has fine-grained reactivity by design.
// ═══════════════════════════════════════════════════════════════════════════

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

// Flag for template-based lists (now __mapped_list__ / __filtered_mapped_list__ Tagged values)
thread_local! {
    static HAS_TEMPLATE_LIST: std::cell::Cell<bool> = std::cell::Cell::new(false); // ALLOWED: config state
}

/// Set the has_template_list flag.
/// Called by evaluator when creating template-based list mappings.
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


// DEAD CODE DELETED: set_checkbox_toggle_holds, clear_checkbox_toggle_holds,
// checkbox_toggle_holds_signal, get_checkbox_toggle_holds - all were set but never read

// DEAD CODE DELETED: get_unchecked_checkbox_count() - never called

// Phase 7.3: add_text_clear_cell DELETED - now registered via DataflowConfig methods

/// Check if a HOLD ID is a text-clear HOLD.
/// Used by output listener to know when to clear text input DOM.
/// Phase 7.3: Delegates to DataflowConfig via core accessor.
pub fn is_text_clear_cell(cell_id: &str) -> bool {
    core_is_text_clear_cell(cell_id)
}

/// Clear text-clear hold registry.
/// Phase 7.3: Now a no-op - clearing happens via clear_active_config().
pub fn clear_text_clear_cells() {
    // No-op: text_clear_cells is now in DataflowConfig, cleared via clear_active_config()
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
        // Pure DD: Symbolic refs (WhileRef, PlaceholderField, etc.) were removed in Phase 7
        Value::Tagged { .. } | Value::LinkRef(_) | Value::TimerRef { .. } | Value::Placeholder | Value::Flushed(_) => None,
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

// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED: cell_states_signal()
//
// This was the global broadcast anti-pattern:
// - Fired on ANY cell change (O(n) re-evaluation)
// - Caused spurious re-renders throughout the UI
// - Root cause of blur issues in WHILE editing (required grace period hack)
//
// The actors engine doesn't have this problem because each actor subscribes
// only to its specific inputs (fine-grained reactivity).
//
// Use instead:
// - cell_signal(cell_id) - watch single cell
// - cells_signal(cell_ids) - watch multiple specific cells
// ═══════════════════════════════════════════════════════════════════════════

/// Get a granular signal for a specific cell.
/// Only fires when THIS cell's value changes - O(1) updates.
///
/// This is the correct pattern for fine-grained reactivity (actors-style):
/// ```ignore
/// // ✅ GRANULAR - only fires when "count" changes
/// cell_signal("count").map(|v| v.map(|v| v.to_display_string()).unwrap_or_default())
/// ```
pub fn cell_signal(cell_id: impl Into<String>) -> impl zoon::Signal<Item = Option<Value>> + Unpin {
    let cell_id = cell_id.into();
    CELL_STATES.with(|states| {
        states.signal_ref(move |map| map.get(&cell_id).cloned())
    })
}

/// Get a signal that fires when ANY of the specified cells change.
/// Use when you need to watch multiple specific cells (e.g., TEXT with CellRef parts).
///
/// The signal fires when any watched cell changes.
/// Use `get_cell_value()` in the map closure to read current values.
///
/// ```ignore
/// // ✅ TARGETED - fires only when cell_a or cell_b changes
/// cells_signal(vec!["cell_a", "cell_b"]).map(|_| compute_from_cells())
/// ```
pub fn cells_signal(cell_ids: Vec<String>) -> impl zoon::Signal<Item = ()> + Unpin {
    CELL_STATES.with(|states| {
        states.signal_ref(move |map| {
            // Extract only the values we care about
            // This signal fires when any of the watched cells change
            // We use a hash of the fingerprint for efficient change detection
            let fingerprint_hash: u64 = cell_ids.iter()
                .map(|id| {
                    map.get(id)
                        .map(|v| v.to_display_string())
                        .unwrap_or_default()
                })
                .fold(0u64, |acc, s| {
                    // Simple hash combination
                    acc.wrapping_mul(31).wrapping_add(s.len() as u64)
                        .wrapping_add(s.chars().map(|c| c as u64).sum::<u64>())
                });
            fingerprint_hash
        })
        .dedupe()
        .map(|_| ())
    })
}

/// Get the current value of a specific HOLD.
/// Returns None if the HOLD hasn't been set yet.
pub fn get_cell_value(cell_id: &str) -> Option<Value> {
    CELL_STATES.with(|states| {
        states.lock_ref().get(cell_id).cloned()
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 12: List SignalVec API
// Provides incremental list rendering via VecDiff for children_signal_vec().
// ═══════════════════════════════════════════════════════════════════════════

/// Get a SignalVec for a list cell that enables incremental DOM updates.
///
/// # Phase 12 Architecture
///
/// Instead of re-rendering the entire list when any item changes, use this
/// with Zoon's `children_signal_vec()` for O(delta) DOM operations:
///
/// ```ignore
/// // ✅ INCREMENTAL - only changed elements updated
/// El::new().children_signal_vec(
///     list_signal_vec("todos")
///         .map_signal_cloned(|item| render_todo_item(item))
/// )
/// ```
///
/// # Returns
///
/// A SignalVec that emits VecDiff when the list changes:
/// - `VecDiff::Replace` when list is initially loaded or fully replaced
/// - `VecDiff::Push` when item is appended (future optimization)
/// - `VecDiff::RemoveAt` when item is removed (future optimization)
///
/// # Current Implementation
///
/// Currently emits `VecDiff::Replace` on any list change. Future optimization
/// will compute actual diffs from DD worker output for true O(delta).
pub fn list_signal_vec(cell_id: impl Into<String>) -> impl SignalVec<Item = Value> {
    let cell_id = cell_id.into();

    LIST_SIGNAL_VECS.with(|vecs| {
        let mut vecs = vecs.borrow_mut(); // ALLOWED: IO layer
        // Get or create MutableVec for this list cell
        let mvec = vecs.entry(cell_id.clone()).or_insert_with(|| {
            // Initialize with current list value if available
            let initial_items = get_cell_value(&cell_id)
                .and_then(|v| match v {
                    Value::List(items) => Some(items.iter().cloned().collect::<Vec<_>>()),
                    Value::Collection(handle) => Some(handle.iter().cloned().collect::<Vec<_>>()),
                    _ => None,
                })
                .unwrap_or_default();
            MutableVec::new_with_values(initial_items)
        });
        mvec.signal_vec_cloned()
    })
}

/// Update the MutableVec for a list cell with incremental diff detection.
/// Called internally when sync_cell_from_dd receives a list value.
///
/// # Phase 12 Optimization: Diff Detection
///
/// Instead of always emitting VecDiff::Replace, this function detects:
/// - **Single append**: new_items = old_items + [item] → VecDiff::Push (O(1) DOM)
/// - **Single removal**: new_items = old_items - [item] → VecDiff::RemoveAt (O(1) DOM)
/// - **Complex change**: Fall back to VecDiff::Replace (O(n) DOM)
///
/// This gives O(delta) DOM updates for the common shopping_list/todo_mvc patterns
/// while still handling edge cases correctly.
/// Update MutableVec for a list cell - FALLBACK path for full list values.
///
/// # Phase 4.2 Note
/// Most list operations now use ListDiff variants (ListPush, ListRemoveByKey, etc.)
/// which apply O(delta) updates directly via `apply_list_*()` functions.
///
/// This function is the FALLBACK for:
/// - Initial list population (first sync of a cell)
/// - Bulk operations (ListRemoveCompleted) that emit full filtered lists
/// - Field updates (ListItemSetFieldByIdentity) that emit full updated lists
/// - Non-persistent worker path (dataflow.rs DdTransforms)
/// - Persisted state restoration
///
/// The diff detection here provides correct behavior for these edge cases.
fn update_list_signal_vec(cell_id: &str, new_items: &[Value]) {
    LIST_SIGNAL_VECS.with(|vecs| {
        let mut vecs = vecs.borrow_mut(); // ALLOWED: IO layer
        if let Some(mvec) = vecs.get_mut(cell_id) {
            let mut lock = mvec.lock_mut();
            let old_len = lock.len();
            let new_len = new_items.len();

            // Case 1: Single append - new list is old list + one item at end
            if new_len == old_len + 1 {
                // Check if first old_len items match
                let prefix_matches = lock.iter()
                    .zip(new_items.iter().take(old_len))
                    .all(|(old, new)| values_equal_for_diff(old, new));

                if prefix_matches {
                    // It's a push! O(1) DOM update
                    let new_item = new_items.last().unwrap().clone();
                    if LOG_DD_DEBUG {
                        zoon::println!("[DD ListDiff] {} Push detected (O(1))", cell_id);
                    }
                    lock.push_cloned(new_item);
                    return;
                }
            }

            // Case 2: Single removal - new list is old list minus one item
            // Phase 2.2: O(1) key-based removal detection using HashMap
            if new_len + 1 == old_len {
                // Build set of new item keys - O(new_len)
                let new_keys: HashSet<String> = new_items
                    .iter()
                    .map(extract_item_key)
                    .collect();

                // Find old item not in new items - O(old_len) but with O(1) lookup per item
                let mut removed_index = None;
                for (i, old_item) in lock.iter().enumerate() {
                    let old_key = extract_item_key(old_item);
                    if !new_keys.contains(&old_key) {
                        if removed_index.is_some() {
                            // More than one removal - fall back to replace
                            removed_index = None;
                            break;
                        }
                        removed_index = Some(i);
                    }
                }

                if let Some(index) = removed_index {
                    if LOG_DD_DEBUG {
                        zoon::println!("[DD ListDiff] {} RemoveAt({}) detected (O(1) key lookup)", cell_id, index);
                    }
                    lock.remove(index);
                    return;
                }
            }

            // Case 3: Complex change - fall back to replace
            if LOG_DD_DEBUG {
                zoon::println!("[DD ListDiff] {} Replace (old_len={}, new_len={})", cell_id, old_len, new_len);
            }
            lock.replace_cloned(new_items.iter().cloned().collect());
        } else {
            // Create new MutableVec if list cell is first synced
            if LOG_DD_DEBUG {
                zoon::println!("[DD ListDiff] {} Initial (len={})", cell_id, new_items.len());
            }
            let mvec = MutableVec::new_with_values(new_items.iter().cloned().collect());
            vecs.insert(cell_id.to_string(), mvec);
        }
    });
}

/// Extract a unique key from a Value for O(1) lookup.
/// Looks for CellRef or LinkRef IDs which are guaranteed unique per item.
/// Falls back to display string for simple values.
fn extract_item_key(value: &Value) -> String {
    match value {
        Value::CellRef(cell_id) => format!("hold:{}", cell_id.name()),
        Value::LinkRef(link_id) => format!("link:{}", link_id.name()),
        Value::Object(fields) => {
            // For objects, find first CellRef or LinkRef field (they're unique per item)
            for (_, field_value) in fields.iter() {
                match field_value {
                    Value::CellRef(cell_id) => return format!("hold:{}", cell_id.name()),
                    Value::LinkRef(link_id) => return format!("link:{}", link_id.name()),
                    Value::Object(inner) => {
                        // Check nested objects (e.g., todo_elements)
                        for (_, inner_value) in inner.iter() {
                            match inner_value {
                                Value::CellRef(cell_id) => return format!("hold:{}", cell_id.name()),
                                Value::LinkRef(link_id) => return format!("link:{}", link_id.name()),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Fallback to display string
            value.to_display_string()
        }
        _ => value.to_display_string(),
    }
}

/// Compare two Values for diff detection purposes.
/// Uses key-based comparison for objects (O(1) via unique IDs).
/// Falls back to display string comparison for simple values.
fn values_equal_for_diff(a: &Value, b: &Value) -> bool {
    // Phase 2.2: Use key-based comparison for O(1) lookup
    extract_item_key(a) == extract_item_key(b)
}

/// Clear all list signal vecs.
/// Called when clearing state between examples.
pub fn clear_list_signal_vecs() {
    LIST_SIGNAL_VECS.with(|vecs| {
        vecs.borrow_mut().clear(); // ALLOWED: IO layer
    });
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
