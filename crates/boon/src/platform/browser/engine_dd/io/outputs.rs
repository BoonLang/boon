//! Output handling for DD values.
//! Build marker: 2026-01-18-v2 (VecDiff diff detection)
//!
//! Provides global reactive state for HOLD values that the bridge
//! can observe for DOM updates.
//!
//! Persistence: HOLD values are saved to localStorage and restored on re-run.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use super::super::core::dataflow::ListState;
use super::super::core::value::{CellUpdate, CollectionHandle, CollectionId, Value};
use super::super::core::ITEM_KEY_FIELD;
#[allow(unused_imports)]
use super::super::dd_log;
use zoon::Mutable;
use zoon::futures_signals::signal_vec::{MutableVec, SignalVec};
use zoon::{local_storage, WebStorage};

// Text input clearing is driven by Boon code; no IO-side text-clear side effects.

const DD_HOLD_STORAGE_KEY: &str = "dd_hold_states";
const DD_PERSIST_VERSION_KEY: &str = "__dd_version__";
const DD_PERSIST_VERSION: u32 = 2;

/// Clear all DD persisted hold states.
/// Called when user clicks "Clear saved states" in playground.
pub fn clear_dd_persisted_states() {
    local_storage().remove(DD_HOLD_STORAGE_KEY);
    // Also clear in-memory CELL_STATES
    CELL_STATES.with(|states| {
        states.lock_mut().clear();
    });
    clear_list_states();
    clear_list_signal_vecs();
    dd_log!("[DD Persist] Cleared all DD states");
}

/// Clear in-memory CELL_STATES only (not localStorage).
/// Called at the start of each interpretation to prevent state contamination between examples.
pub fn clear_cells_memory() {
    CELL_STATES.with(|states| {
        states.lock_mut().clear();
    });
    clear_list_states();
    // Clear list signal vecs
    clear_list_signal_vecs();
    // Also reset route state to prevent cross-example contamination
    clear_current_route();
}

// ═══════════════════════════════════════════════════════════════════════════
/// Sync a cell value from DD output (called by DD worker after processing).
///
/// Sync a cell value from DD output, optionally persisting the state.
/// Called ONLY by the DD worker to update IO boundary state.
///
/// Side effects:
/// - If the value is a list, updates the MutableVec for incremental rendering
/// - If persist=true, also writes to localStorage for state recovery
fn sync_cell_impl(update: CellUpdate, persist: bool) {
    dd_log!("[DD Sync{}] {:?}", if persist { "+Persist" } else { "" }, update);

    match update {
        CellUpdate::Multi(updates) => {
            dd_log!("[DD MultiUpdate{}] {} updates", if persist { "+Persist" } else { "" }, updates.len());
            for update in updates {
                sync_cell_impl(update, persist);
            }
        }
        CellUpdate::NoOp => {}
        CellUpdate::ListPush { cell_id, item } => {
            dd_log!("[DD ListDiff] {} Push", cell_id);
            apply_list_push(cell_id.as_ref(), &item);
            if persist { persist_list_state(cell_id.as_ref()); }
        }
        CellUpdate::ListInsertAt { cell_id, index, item } => {
            dd_log!("[DD ListDiff] {} InsertAt({})", cell_id, index);
            apply_list_insert_at(cell_id.as_ref(), index, &item);
            if persist { persist_list_state(cell_id.as_ref()); }
        }
        CellUpdate::ListRemoveAt { cell_id, index } => {
            dd_log!("[DD ListDiff] {} RemoveAt({})", cell_id, index);
            apply_list_remove_at(cell_id.as_ref(), index);
            if persist { persist_list_state(cell_id.as_ref()); }
        }
        CellUpdate::ListRemoveByKey { cell_id, key } => {
            dd_log!("[DD ListDiff] {} RemoveByKey({})", cell_id, key);
            apply_list_remove_by_key(cell_id.as_ref(), key.as_ref());
            if persist { persist_list_state(cell_id.as_ref()); }
        }
        CellUpdate::ListRemoveBatch { cell_id, keys } => {
            dd_log!("[DD ListDiff] {} RemoveBatch({} keys)", cell_id, keys.len());
            apply_list_remove_batch(cell_id.as_ref(), &keys);
            if persist { persist_list_state(cell_id.as_ref()); }
        }
        CellUpdate::ListClear { cell_id } => {
            dd_log!("[DD ListDiff] {} Clear", cell_id);
            apply_list_clear(cell_id.as_ref());
            if persist { persist_list_state(cell_id.as_ref()); }
        }
        CellUpdate::ListItemUpdate { cell_id, key, field_path, new_value } => {
            dd_log!("[DD ListDiff] {} ItemUpdate({})", cell_id, key);
            apply_list_item_update(cell_id.as_ref(), key.as_ref(), &field_path, &new_value);
            if persist { persist_list_state(cell_id.as_ref()); }
        }
        CellUpdate::SetValue { cell_id, value } => {
            if matches!(value, Value::Placeholder | Value::PlaceholderField(_) | Value::PlaceholderWhile(_)) {
                panic!(
                    "[DD Sync] Placeholder value reached IO boundary for '{}': {:?}",
                    cell_id, value
                );
            }
            if matches!(value, Value::List(_)) {
                panic!(
                    "[DD Sync] Collection snapshot for '{}' is forbidden; initialize list state via sync_list_state_from_dd",
                    cell_id
                );
            }
            if persist {
                CELL_STATES.with(|states| {
                    states.lock_mut().insert(cell_id.to_string(), value.clone());
                });
                persist_hold_value(cell_id.as_ref(), &value);
            } else {
                CELL_STATES.with(|states| {
                    states.lock_mut().insert(cell_id.to_string(), value);
                });
            }
        }
    }
}

/// Sync a cell value from DD output (no persistence).
pub fn sync_cell_from_dd(update: CellUpdate) {
    sync_cell_impl(update, false);
}

/// Sync a cell value from DD output and persist it.
pub fn sync_cell_from_dd_with_persist(update: CellUpdate) {
    sync_cell_impl(update, true);
}

/// Initialize list state from DD, optionally persisting.
fn sync_list_state_impl(cell_id: impl Into<String>, items: Vec<Value>, persist: bool) {
    let cell_id = cell_id.into();
    dd_log!("[DD SyncList{}] {} ({} items)", if persist { "+Persist" } else { "" }, cell_id, items.len());
    init_list_cell_from_items(&cell_id, &items);
    if persist { persist_list_state(&cell_id); }
}

/// Initialize list state from DD (snapshot-free CollectionHandle).
pub fn sync_list_state_from_dd(cell_id: impl Into<String>, items: Vec<Value>) {
    sync_list_state_impl(cell_id, items, false);
}

/// Initialize list state from DD and persist it.
pub fn sync_list_state_from_dd_with_persist(cell_id: impl Into<String>, items: Vec<Value>) {
    sync_list_state_impl(cell_id, items, true);
}

// ═══════════════════════════════════════════════════════════════════════════
// ListDiff Application Functions
// These apply O(delta) operations directly to MutableVec and LIST_STATES
// ═══════════════════════════════════════════════════════════════════════════

/// Apply ListPush diff - O(1) append
fn apply_list_push(cell_id: &str, item: &Value) {
    let _ = extract_item_key(item);
    ensure_list_state_initialized(cell_id);
    // Update authoritative list state
    let index = LIST_STATES.with(|states| {
        let mut states = states.borrow_mut(); // ALLOWED: IO layer
        let state = states.get_mut(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] Missing list state for '{}'", cell_id);
        });
        state.push(item.clone(), "list push")
    });

    // Update MutableVec for incremental rendering
    LIST_SIGNAL_VECS.with(|vecs| {
        let mut vecs = vecs.borrow_mut(); // ALLOWED: IO layer
        let mvec = vecs.get_mut(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] Missing list signal vec for '{}'", cell_id);
        });
        let mut lock = mvec.lock_mut();
        if index != lock.len() {
            panic!(
                "[DD ListDiff] ListPush index mismatch for '{}': state={}, vec={}",
                cell_id,
                index,
                lock.len()
            );
        }
        lock.push_cloned(item.clone());
    });
}

/// Apply ListInsertAt diff - O(n) insert
fn apply_list_insert_at(cell_id: &str, index: usize, item: &Value) {
    let _ = extract_item_key(item);
    ensure_list_state_initialized(cell_id);
    // Update authoritative list state
    LIST_STATES.with(|states| {
        let mut states = states.borrow_mut(); // ALLOWED: IO layer
        let state = states.get_mut(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] Missing list state for '{}'", cell_id);
        });
        state.insert(index, item.clone(), "list insert");
    });
    // Update MutableVec for incremental rendering
    LIST_SIGNAL_VECS.with(|vecs| {
        let mut vecs = vecs.borrow_mut(); // ALLOWED: IO layer
        let mvec = vecs.get_mut(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] Missing list signal vec for '{}'", cell_id);
        });
        let mut lock = mvec.lock_mut();
        if index > lock.len() {
            panic!("[DD ListDiff] ListInsertAt index {} out of bounds for {}", index, cell_id);
        }
        lock.insert_cloned(index, item.clone());
    });
}

/// Apply ListRemoveAt diff - O(n) shift but no clone needed
fn apply_list_remove_at(cell_id: &str, index: usize) {
    ensure_list_state_initialized(cell_id);
    // Update authoritative list state
    LIST_STATES.with(|states| {
        let mut states = states.borrow_mut(); // ALLOWED: IO layer
        let state = states.get_mut(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] Missing list state for '{}'", cell_id);
        });
        state.remove_at(index, "list remove at");
    });
    // Update MutableVec
    LIST_SIGNAL_VECS.with(|vecs| {
        let vecs = vecs.borrow(); // ALLOWED: IO layer
        let mvec = vecs.get(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] ListRemoveAt missing MutableVec for {}", cell_id);
        });
        let mut lock = mvec.lock_mut();
        if index >= lock.len() {
            panic!("[DD ListDiff] ListRemoveAt index {} out of bounds for {}", index, cell_id);
        }
        lock.remove(index);
    });

}

/// Apply ListRemoveByKey diff - O(1) key lookup
fn apply_list_remove_by_key(cell_id: &str, key: &str) {
    ensure_list_state_initialized(cell_id);
    // Find index by key first
    let idx = LIST_STATES.with(|states| {
        let states = states.borrow(); // ALLOWED: IO layer
        let state = states.get(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] RemoveByKey missing list state '{}'", cell_id);
        });
        state.index_of(key, "list remove by key")
    });

    // Update MutableVec
    LIST_SIGNAL_VECS.with(|vecs| {
        let vecs = vecs.borrow(); // ALLOWED: IO layer
        let mvec = vecs.get(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] RemoveByKey missing MutableVec for {}", cell_id);
        });
        let mut lock = mvec.lock_mut();
        if idx < lock.len() {
            lock.remove(idx);
        } else {
            panic!("[DD ListDiff] RemoveByKey index {} out of bounds for {}", idx, cell_id);
        }
    });

    // Update authoritative list state
    LIST_STATES.with(|states| {
        let mut states = states.borrow_mut(); // ALLOWED: IO layer
        let state = states.get_mut(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] RemoveByKey missing list state '{}'", cell_id);
        });
        state.remove_by_key(key, "list remove by key");
    });

    dd_log!("[DD ListDiff] {} RemoveByKey({}) at index {}", cell_id, key, idx);
}

/// Apply ListRemoveBatch diff - O(k) batch removal where k = keys.len()
/// Removes all items whose keys are in the provided set.
/// This is more efficient than multiple individual RemoveByKey operations
/// because it processes all removals in a single pass.
fn apply_list_remove_batch(cell_id: &str, keys: &[Arc<str>]) {
    ensure_list_state_initialized(cell_id);
    if keys.is_empty() {
        return;
    }
    // Build set of keys to remove for O(1) lookup
    let mut keys_to_remove: HashSet<&str> = HashSet::new();
    for key in keys {
        if !keys_to_remove.insert(key.as_ref()) {
            panic!("[DD ListDiff] RemoveBatch duplicate keys for {}", cell_id);
        }
    }
    // Find indices to remove (in reverse order for safe removal)
    let mut indices_to_remove: Vec<usize> = LIST_STATES.with(|states| {
        let states = states.borrow(); // ALLOWED: IO layer
        let state = states.get(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] RemoveBatch missing list state '{}'", cell_id);
        });
        keys.iter()
            .map(|key| state.index_of(key.as_ref(), "list remove batch"))
            .collect()
    });
    indices_to_remove.sort_by(|a, b| b.cmp(a));

    // Update MutableVec (removing from end to front to preserve indices)
    LIST_SIGNAL_VECS.with(|vecs| {
        let vecs = vecs.borrow(); // ALLOWED: IO layer
        let mvec = vecs.get(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] RemoveBatch missing MutableVec for {}", cell_id);
        });
        let mut lock = mvec.lock_mut();
        for &idx in &indices_to_remove {
            if idx >= lock.len() {
                panic!("[DD ListDiff] RemoveBatch index {} out of bounds for {}", idx, cell_id);
            }
            lock.remove(idx);
        }
    });

    // Update authoritative list state
    LIST_STATES.with(|states| {
        let mut states = states.borrow_mut(); // ALLOWED: IO layer
        let state = states.get_mut(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] RemoveBatch missing list state '{}'", cell_id);
        });
        state.remove_batch(keys, "list remove batch");
    });

    dd_log!("[DD ListDiff] {} RemoveBatch removed {} items", cell_id, indices_to_remove.len());
}

/// Apply ListClear diff - O(1) clear
fn apply_list_clear(cell_id: &str) {
    ensure_list_state_initialized(cell_id);
    // Update authoritative list state
    LIST_STATES.with(|states| {
        let mut states = states.borrow_mut(); // ALLOWED: IO layer
        let state = states.get_mut(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] Missing list state for '{}'", cell_id);
        });
        state.clear();
    });
    // Update MutableVec
    LIST_SIGNAL_VECS.with(|vecs| {
        let vecs = vecs.borrow(); // ALLOWED: IO layer
        let mvec = vecs.get(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] ListClear missing MutableVec for {}", cell_id);
        });
        mvec.lock_mut().clear();
    });
}

/// Apply ListItemUpdate diff - O(1) lookup + O(1) field update
fn apply_list_item_update(cell_id: &str, key: &str, field_path: &[Arc<str>], new_value: &Value) {
    ensure_list_state_initialized(cell_id);
    let idx = LIST_STATES.with(|states| {
        let states = states.borrow(); // ALLOWED: IO layer
        let state = states.get(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] ListItemUpdate missing list state '{}'", cell_id);
        });
        state.index_of(key, "list item update")
    });

    // Update authoritative list state and get new item
    let new_item = LIST_STATES.with(|states| {
        let mut states = states.borrow_mut(); // ALLOWED: IO layer
        let state = states.get_mut(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] ListItemUpdate missing list state '{}'", cell_id);
        });
        let (_old_item, new_item) = state.update_field(key, field_path, new_value, "list item update");
        new_item
    });

    // Update MutableVec at same index
    LIST_SIGNAL_VECS.with(|vecs| {
        let vecs = vecs.borrow(); // ALLOWED: IO layer
        let mvec = vecs.get(cell_id).unwrap_or_else(|| {
            panic!("[DD ListDiff] ListItemUpdate missing MutableVec for {}", cell_id);
        });
        let mut mvec_lock = mvec.lock_mut();
        if idx < mvec_lock.len() {
            mvec_lock.set_cloned(idx, new_item);
        } else {
            panic!("[DD ListDiff] ListItemUpdate index {} out of bounds for {}", idx, cell_id);
        }
    });
}

// Global reactive state for HOLD values
// DD collections remain the source of truth; this just mirrors for rendering
thread_local! {
    static CELL_STATES: Mutable<HashMap<String, Value>> = Mutable::new(HashMap::new()); // ALLOWED: view state
}

// ═══════════════════════════════════════════════════════════════════════════
// List Signal Infrastructure
// Provides MutableVec per list cell for incremental rendering via VecDiff.
// Bridge uses list_signal_vec() with children_signal_vec() for O(delta) DOM updates.
// ═══════════════════════════════════════════════════════════════════════════

thread_local! {
    /// Per-list MutableVec for incremental rendering.
    /// Key: cell_id of list cell
    /// Value: MutableVec containing list items as cloneable handles
    ///
    /// Updated by list diffs (ListPush/Insert/Remove/etc.) or sync_list_state_from_dd() at init.
    /// Bridge uses list_signal_vec() to get SignalVec for children_signal_vec().
    static LIST_SIGNAL_VECS: RefCell<HashMap<String, MutableVec<Value>>> =
        RefCell::new(HashMap::new()); // ALLOWED: incremental rendering state
}

thread_local! {
    /// Authoritative list state for IO boundary (render/persist).
    /// This mirrors DD list state and is updated ONLY by list diffs.
    static LIST_STATES: RefCell<HashMap<String, ListState>> =
        RefCell::new(HashMap::new()); // ALLOWED: IO list state
}

thread_local! {
    static CURRENT_ROUTE: Mutable<String> = Mutable::new("/".to_string()); // ALLOWED: route state
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

/// Navigate to a new route path (Router/go_to).
/// Pushes browser history state and updates the internal route signal.
pub fn navigate_to_route(path: &str) {
    dd_log!("[DD Navigate] go_to: {}", path);
    CURRENT_ROUTE.with(|r| r.set(path.to_string()));
    #[cfg(target_arch = "wasm32")]
    {
        use zoon::*;
        let _ = window().history().unwrap().push_state_with_url(
            &wasm_bindgen::JsValue::NULL,
            "",
            Some(path),
        );
    }
}



// NOTE: Runtime updates already flow through DD; remaining gap is
// initial state hydration (persisted values) which still happens in the worker.

fn load_persisted_storage() -> HashMap<String, zoon::serde_json::Value> {
    let storage: HashMap<String, zoon::serde_json::Value> = match local_storage().get::<HashMap<String, zoon::serde_json::Value>>(DD_HOLD_STORAGE_KEY) {
        None => return HashMap::new(),
        Some(Ok(s)) => s,
        Some(Err(err)) => {
            panic!("[DD Persist] Failed to deserialize persisted state: {:?}", err);
        }
    };

    if let Some(version_value) = storage.get(DD_PERSIST_VERSION_KEY) {
        let version = version_value.as_u64().unwrap_or_else(|| {
            panic!(
                "[DD Persist] '{}' must be a number, found {:?}",
                DD_PERSIST_VERSION_KEY, version_value
            );
        });
        let version = u32::try_from(version).expect("[DD Persist] version number overflow");
        if version > DD_PERSIST_VERSION {
            panic!(
                "[DD Persist] Unsupported persisted version {} (max supported {})",
                version, DD_PERSIST_VERSION
            );
        }
    }

    storage
}

#[derive(Clone, Debug)]
pub struct PersistedValue {
    pub value: Value,
    pub collections: HashMap<CollectionId, Vec<Value>>,
}

#[derive(Clone, Debug)]
pub struct PersistedListItems {
    pub items: Vec<Value>,
    pub collections: HashMap<CollectionId, Vec<Value>>,
}

/// Load persisted HOLD value from localStorage.
/// Returns None if no persisted value exists.
pub fn load_persisted_cell_value(cell_id: &str) -> Option<Value> {
    let persisted = load_persisted_cell_value_with_collections(cell_id)?;
    if !persisted.collections.is_empty() {
        panic!(
            "[DD Persist] Nested collections require load_persisted_cell_value_with_collections for '{}'",
            cell_id
        );
    }
    Some(persisted.value)
}

/// Load persisted HOLD value and nested collections from localStorage.
/// Returns None if no persisted value exists.
pub fn load_persisted_cell_value_with_collections(cell_id: &str) -> Option<PersistedValue> {
    if cell_id == DD_PERSIST_VERSION_KEY {
        panic!("[DD Persist] '{}' is a reserved persistence key", DD_PERSIST_VERSION_KEY);
    }

    let storage = load_persisted_storage();
    let json_value = storage.get(cell_id)?;
    let mut collections = HashMap::new();
    let value = json_to_dd_value_root_with_collections(json_value, &mut collections);
    Some(PersistedValue { value, collections })
}

/// Load persisted list items from localStorage.
/// Returns None if no persisted list exists.
pub fn load_persisted_list_items(cell_id: &str) -> Option<Vec<Value>> {
    let persisted = load_persisted_list_items_with_collections(cell_id)?;
    if !persisted.collections.is_empty() {
        panic!(
            "[DD Persist] Nested collections require load_persisted_list_items_with_collections for '{}'",
            cell_id
        );
    }
    Some(persisted.items)
}

/// Load persisted list items and nested collections from localStorage.
/// Returns None if no persisted list exists.
pub fn load_persisted_list_items_with_collections(cell_id: &str) -> Option<PersistedListItems> {
    if cell_id == DD_PERSIST_VERSION_KEY {
        panic!("[DD Persist] '{}' is a reserved persistence key", DD_PERSIST_VERSION_KEY);
    }

    let storage = load_persisted_storage();
    let json_value = storage.get(cell_id)?;
    let mut collections = HashMap::new();
    let items = json_to_dd_list_items_with_collections(json_value, &mut collections);
    Some(PersistedListItems { items, collections })
}

// NOTE: Cell initialization currently happens in the worker to keep
// DD outputs as the single source of truth for runtime updates.

/// Persist a HOLD value to localStorage.
fn persist_hold_value(cell_id: &str, value: &Value) {
    let json = dd_value_to_json(value);
    persist_json_value(cell_id, json);
}

fn persist_json_value(cell_id: &str, json: zoon::serde_json::Value) {
    // Load existing storage
    let mut storage: HashMap<String, zoon::serde_json::Value> = match local_storage().get::<HashMap<String, zoon::serde_json::Value>>(DD_HOLD_STORAGE_KEY) {
        None => HashMap::new(),
        Some(Ok(s)) => s,
        Some(Err(err)) => {
            panic!("[DD Persist] Failed to deserialize persisted state: {:?}", err);
        }
    };

    storage.insert(cell_id.to_string(), json);
    storage.insert(
        DD_PERSIST_VERSION_KEY.to_string(),
        zoon::serde_json::Value::Number(DD_PERSIST_VERSION.into()),
    );
    if let Err(err) = local_storage().insert(DD_HOLD_STORAGE_KEY, &storage) {
        panic!("[DD Persist] Failed to save: {:?}", err);
    }
}

fn list_state_items(cell_id: &str, context: &str) -> Vec<Value> {
    CELL_STATES.with(|states| {
        if states.lock_ref().contains_key(cell_id) {
            panic!("[DD {}] List cell '{}' stored in CELL_STATES", context, cell_id);
        }
    });

    LIST_STATES.with(|states| {
        let states = states.borrow();
        let state = states.get(cell_id).unwrap_or_else(|| {
            panic!("[DD {}] Missing list state for '{}'", context, cell_id);
        });
        state.items().to_vec()
    })
}

fn persist_list_state(cell_id: &str) {
    let items = list_state_items(cell_id, "Persist");

    let arr: Vec<_> = items.iter().map(dd_value_to_json).collect();
    let json = zoon::serde_json::json!({ "__collection__": arr });
    persist_json_value(cell_id, json);
}

/// Convert Value to JSON for storage.
fn dd_value_to_json(value: &Value) -> zoon::serde_json::Value {
    use zoon::serde_json::json;
    use super::super::core::types::BoolTag;
    match value {
        Value::Unit => json!(null),
        Value::Bool(b) => json!(b),
        // Handle Tagged booleans (True/False) - serialize as JSON booleans
        Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => {
            json!(BoolTag::is_true(tag.as_ref()))
        }
        Value::Number(n) => json!(n.0),
        Value::Text(s) => json!(s.as_ref()),
        Value::List(handle) => {
            let list_id = handle
                .cell_id
                .as_deref()
                .map(|id| id.to_string())
                .unwrap_or_else(|| handle.id.to_string());
            let items = list_state_items(&list_id, "Persist");
            let arr: Vec<_> = items.iter().map(dd_value_to_json).collect();
            json!({ "__collection__": arr })
        }
        Value::Object(fields) => {
            // Persist Objects (like list items) by recursively converting fields
            let mut obj = zoon::serde_json::Map::new();
            for (key, val) in fields.iter() {
                let json_val = dd_value_to_json(val);
                obj.insert(key.to_string(), json_val);
            }
            zoon::serde_json::Value::Object(obj)
        }
        // Dereference CellRefs to persist their actual values
        Value::CellRef(cell_id) => {
            let cell_name = cell_id.name();
            if is_list_cell(&cell_name) {
                let items = list_state_items(&cell_name, "Persist");
                let arr: Vec<_> = items.iter().map(dd_value_to_json).collect();
                return zoon::serde_json::json!({ "__collection__": arr });
            }
            if let Some(value) = get_cell_value(&cell_name) {
                dd_log!("[DD Persist] CellRef {} -> {:?}", cell_id, value);
                dd_value_to_json(&value)
            } else {
                panic!("[DD Persist] CellRef {} missing from state", cell_id);
            }
        }
        // Tagged values: serialize bool tags as JSON booleans, others as objects
        Value::Tagged { tag, fields } => {
            if tag.as_ref() == "True" {
                zoon::serde_json::Value::Bool(true)
            } else if tag.as_ref() == "False" {
                zoon::serde_json::Value::Bool(false)
            } else {
                let mut obj = zoon::serde_json::Map::new();
                obj.insert("__tag__".to_string(), zoon::serde_json::Value::String(tag.to_string()));
                for (k, v) in fields.iter() {
                    obj.insert(k.to_string(), dd_value_to_json(v));
                }
                zoon::serde_json::Value::Object(obj)
            }
        }
        // LinkRef/TimerRef: event sources are not persisted, use null placeholder
        Value::LinkRef(_) | Value::TimerRef { .. } => zoon::serde_json::Value::Null,
        // Internal types that should never reach persistence
        Value::Placeholder
        | Value::PlaceholderField(_)
        | Value::PlaceholderBoolNot(_)
        | Value::WhileConfig(_)
        | Value::PlaceholderWhile(_)
        | Value::Flushed(_) => {
            panic!("[DD Persist] Unsupported Value for persistence: {:?}", value);
        }
    }
}

fn json_to_dd_value_with_collections(
    json: &zoon::serde_json::Value,
    collections: &mut HashMap<CollectionId, Vec<Value>>,
) -> Value {
    use zoon::serde_json::Value as JsonValue;
    use std::collections::BTreeMap;
    match json {
        JsonValue::Null => Value::Unit,
        // IMPORTANT: Boon uses Tagged booleans (Tagged { tag: "True/False" }), not Rust bools
        // Deserialize JSON booleans as Tagged to maintain type consistency
        JsonValue::Bool(b) => Value::Tagged {
            tag: std::sync::Arc::from(if *b { "True" } else { "False" }),
            fields: std::sync::Arc::new(BTreeMap::new()),
        },
        JsonValue::Number(n) => Value::float(n.as_f64().unwrap_or_else(|| {
            panic!("[DD Persist] Invalid JSON number: {:?}", n);
        })),
        JsonValue::String(s) => Value::text(s.clone()),
        JsonValue::Array(_) => {
            panic!("[DD Persist] Arrays are not supported; use '__collection__' marker for lists");
        }
        JsonValue::Object(obj) => {
            if obj.contains_key("__collection__") {
                if obj.len() != 1 {
                    panic!("[DD Persist] '__collection__' marker cannot be mixed with other fields");
                }
                let items = obj.get("__collection__").unwrap_or_else(|| {
                    panic!("[DD Persist] Missing '__collection__' field");
                });
                let JsonValue::Array(values) = items else {
                    panic!("[DD Persist] '__collection__' must be an array");
                };
                let mut parsed_items = Vec::new();
                for value in values {
                    parsed_items.push(json_to_dd_value_with_collections(value, collections));
                }
                let id = CollectionId::new();
                collections.insert(id, parsed_items);
                return Value::List(CollectionHandle::new_with_id(id));
            }
            // Restore Objects (like list items)
            let mut fields = BTreeMap::new();
            for (key, val) in obj.iter() {
                let dd_val = json_to_dd_value_with_collections(val, collections);
                fields.insert(std::sync::Arc::from(key.as_str()), dd_val);
            }
            Value::Object(std::sync::Arc::new(fields))
        }
    }
}

fn json_to_dd_value_root_with_collections(
    json: &zoon::serde_json::Value,
    collections: &mut HashMap<CollectionId, Vec<Value>>,
) -> Value {
    if matches!(json, zoon::serde_json::Value::Array(_)) {
        panic!("[DD Persist] Top-level arrays are not supported; use '__collection__' marker");
    }
    if let zoon::serde_json::Value::Object(obj) = json {
        if obj.contains_key("__collection__") {
            panic!("[DD Persist] '__collection__' is list data; use load_persisted_list_items()");
        }
    }
    json_to_dd_value_with_collections(json, collections)
}

fn json_to_dd_list_items_with_collections(
    json: &zoon::serde_json::Value,
    collections: &mut HashMap<CollectionId, Vec<Value>>,
) -> Vec<Value> {
    use zoon::serde_json::Value as JsonValue;
    let JsonValue::Object(obj) = json else {
        panic!("[DD Persist] List persistence requires '__collection__' object");
    };
    if obj.len() != 1 {
        panic!("[DD Persist] '__collection__' marker cannot be mixed with other fields");
    }
    let items = obj.get("__collection__").unwrap_or_else(|| {
        panic!("[DD Persist] Missing '__collection__' field");
    });
    let JsonValue::Array(values) = items else {
        panic!("[DD Persist] '__collection__' must be an array");
    };
    values
        .iter()
        .map(|value| json_to_dd_value_with_collections(value, collections))
        .collect()
}


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

/// Get the current value of a specific HOLD.
/// List cells are not readable as snapshots; use list_signal_vec instead.
/// Returns None if the HOLD hasn't been set yet.
pub fn get_cell_value(cell_id: &str) -> Option<Value> {
    if LIST_STATES.with(|states| states.borrow().contains_key(cell_id)) {
        panic!(
            "[DD IO] List cell '{}' cannot be read as a snapshot; use list_signal_vec",
            cell_id
        );
    }

    CELL_STATES.with(|states| {
        let value = states.lock_ref().get(cell_id).cloned();
        if let Some(ref current) = value {
            if matches!(current, Value::List(_)) {
                panic!("[DD IO] List cell '{}' stored in CELL_STATES", cell_id);
            }
        }
        value
    })
}

/// Check whether a cell is a list cell without materializing a snapshot.
/// Panics if the cell has no state (fail-fast invariant).
pub fn is_list_cell(cell_id: &str) -> bool {
    let has_list = LIST_STATES.with(|states| states.borrow().contains_key(cell_id));
    let has_scalar = CELL_STATES.with(|states| states.lock_ref().contains_key(cell_id));
    if has_list && has_scalar {
        panic!("[DD IO] Cell '{}' exists in both LIST_STATES and CELL_STATES", cell_id);
    }
    if !has_list && !has_scalar {
        if LIST_SIGNAL_VECS.with(|vecs| vecs.borrow().contains_key(cell_id)) {
            panic!(
                "[DD IO] Missing list state for existing signal vec '{}'",
                cell_id
            );
        }
        panic!("[DD IO] Missing cell value for '{}'", cell_id);
    }
    has_list
}

// ═══════════════════════════════════════════════════════════════════════════
// List SignalVec API
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
/// A SignalVec driven by DD list diffs.
///
/// Full list snapshots are forbidden after initialization; list updates must
/// flow through ListDiff variants (Push/Insert/Remove/Batch/Clear/ItemUpdate).
/// Initial items are seeded by ListDiffs during sync; SignalVec must already exist.
pub fn list_signal_vec(cell_id: impl Into<String>) -> impl SignalVec<Item = Value> {
    let cell_id = cell_id.into();

    LIST_SIGNAL_VECS.with(|vecs| {
        // Lazily create empty MutableVec if missing (e.g., list cell was Unit from forward ref)
        let mut vecs = vecs.borrow_mut(); // ALLOWED: IO layer
        let mvec = vecs.entry(cell_id).or_insert_with(MutableVec::new);
        mvec.signal_vec_cloned()
    })
}

/// This function is the FALLBACK for:
/// - Initial list population (first sync of a cell)
/// - Bulk operations that emit full filtered lists
/// - Non-persistent worker path (dataflow.rs DdTransforms)
/// - Persisted state restoration
///
/// The diff detection here provides correct behavior for these edge cases.
fn init_list_signal_vec_from_items(cell_id: &str, items: &[Value]) {
    if LIST_SIGNAL_VECS.with(|vecs| vecs.borrow().contains_key(cell_id)) {
        return;
    }

    let mvec = MutableVec::new();
    {
        let mut lock = mvec.lock_mut();
        for item in items {
            lock.push_cloned(item.clone());
        }
    }

    LIST_SIGNAL_VECS.with(|vecs| {
        vecs.borrow_mut().insert(cell_id.to_string(), mvec);
    });
}

fn init_list_cell_from_items(cell_id: &str, items: &[Value]) {
    CELL_STATES.with(|states| {
        if states.lock_ref().contains_key(cell_id) {
            panic!("[DD ListInit] List cell '{}' stored in CELL_STATES", cell_id);
        }
    });

    let mut seen = HashSet::new();
    for (idx, item) in items.iter().enumerate() {
        let key = match item {
            Value::Object(_) | Value::Tagged { .. } => extract_item_key(item),
            // Plain values use positional identity (duplicates allowed).
            _ => format!("__pos:{}", idx),
        };
        if !seen.insert(key) {
            panic!("[DD ListInit] Duplicate __key in list '{}'", cell_id);
        }
    }

    LIST_STATES.with(|states| {
        let mut states = states.borrow_mut(); // ALLOWED: IO layer
        if states.contains_key(cell_id) {
            panic!("[DD ListInit] List state for '{}' already initialized", cell_id);
        }
        states.insert(cell_id.to_string(), ListState::new(items.to_vec(), "list init"));
    });
    init_list_signal_vec_from_items(cell_id, items);
}

fn ensure_list_state_initialized(cell_id: &str) {
    CELL_STATES.with(|states| {
        if states.lock_ref().contains_key(cell_id) {
            panic!("[DD ListDiff] List cell '{}' stored in CELL_STATES", cell_id);
        }
    });

    let mut items_for_vec: Option<Vec<Value>> = None;
    let mut state_was_new = false;
    LIST_STATES.with(|states| {
        let mut states = states.borrow_mut(); // ALLOWED: IO layer
        if let Some(state) = states.get(cell_id) {
            items_for_vec = Some(state.items().to_vec());
        } else {
            states.insert(cell_id.to_string(), ListState::new(Vec::new(), cell_id));
            items_for_vec = Some(Vec::new());
            state_was_new = true;
        }
    });

    let has_vec = LIST_SIGNAL_VECS.with(|vecs| vecs.borrow().contains_key(cell_id));
    if has_vec && state_was_new {
        panic!("[DD ListDiff] Missing list state for existing signal vec '{}'", cell_id);
    }
    if !has_vec {
        let items = items_for_vec.unwrap_or_default();
        init_list_signal_vec_from_items(cell_id, &items);
    }
}

/// Extract the explicit __key from a list item for O(1) lookup.
/// Pure DD: list items must be Object/Tagged with __key (no scalar fallbacks).
fn extract_item_key(value: &Value) -> String {
    match value {
        Value::Object(fields) => match fields.get(ITEM_KEY_FIELD) {
            Some(Value::Text(key)) => key.to_string(),
            Some(other) => panic!("Bug: __key must be Text in list item object, found {:?}", other),
            None => panic!("Bug: missing __key in list item object"),
        },
        Value::Tagged { fields, .. } => match fields.get(ITEM_KEY_FIELD) {
            Some(Value::Text(key)) => key.to_string(),
            Some(other) => panic!("Bug: __key must be Text in list item element, found {:?}", other),
            None => panic!("Bug: missing __key in list item element"),
        },
        // Plain values (e.g., Text in font-family lists) use value as identity,
        // matching core::value::extract_item_key behavior.
        Value::Text(t) => t.to_string(),
        Value::Number(n) => format!("{}", n.0),
        Value::Bool(b) => if *b { "true".to_string() } else { "false".to_string() },
        other => format!("{:?}", other),
    }
}

/// Clear all list signal vecs.
/// Called when clearing state between examples.
pub fn clear_list_signal_vecs() {
    LIST_SIGNAL_VECS.with(|vecs| {
        vecs.borrow_mut().clear(); // ALLOWED: IO layer
    });
}

/// Clear all list states.
/// Called when clearing state between examples or persisted state reset.
pub fn clear_list_states() {
    LIST_STATES.with(|states| {
        states.borrow_mut().clear(); // ALLOWED: IO layer
    });
}

