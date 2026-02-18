//! Shared persistence helpers for DD engine state.
//!
//! Provides save/load functions for localStorage persistence used by
//! both the worker (SingleHold/LatestSum) and general interpreter.

use std::collections::{BTreeMap, HashMap};

use zoon::{web_sys, serde_json};

use super::super::core::types::ListKey;
use super::super::core::value::Value;

/// Save a single hold state value to localStorage.
pub fn save_hold_state(storage_key: &str, hold_name: &str, value: &Value) {
    if super::super::is_save_disabled() {
        return;
    }
    if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
        if let Ok(json) = serde_json::to_string(value) {
            let key = format!("dd_{}_{}", storage_key, hold_name);
            let _ = storage.set_item(&key, &json);
        }
    }
}

/// Load a single hold state value from localStorage.
pub fn load_hold_state(storage_key: &str, hold_name: &str) -> Option<Value> {
    let storage = web_sys::window()?.local_storage().ok()??;
    let key = format!("dd_{}_{}", storage_key, hold_name);
    let json = storage.get_item(&key).ok()??;
    serde_json::from_str(&json).ok()
}

/// Load all persisted hold values as a HashMap (for compiler initial value override).
///
/// Scans localStorage for individual hold keys matching `dd_{storage_key}_{hold_name}`
/// (the same format used by `save_hold_state`).
pub fn load_holds_map(storage_key: &str) -> std::collections::HashMap<String, Value> {
    let mut result = std::collections::HashMap::new();
    if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
        let prefix = format!("dd_{}_", storage_key);
        let len = storage.length().unwrap_or(0);
        for i in 0..len {
            if let Ok(Some(key)) = storage.key(i) {
                if let Some(hold_name) = key.strip_prefix(&prefix) {
                    // Skip aggregate keys like "holds", "lists", etc.
                    if matches!(hold_name, "holds" | "lists" | "list_counters" | "sums") {
                        continue;
                    }
                    if let Ok(Some(json)) = storage.get_item(&key) {
                        if let Ok(value) = serde_json::from_str::<Value>(&json) {
                            result.insert(hold_name.to_string(), value);
                        }
                    }
                }
            }
        }
    }
    result
}

/// Save full program state (holds, lists, list_counters, sum_accumulators) to localStorage.
pub fn save_program_state(
    storage_key: &str,
    holds: &BTreeMap<String, Value>,
    lists: &BTreeMap<String, Vec<Value>>,
    list_counters: &BTreeMap<String, usize>,
    sum_accumulators: &BTreeMap<String, f64>,
) {
    if super::super::is_save_disabled() {
        return;
    }
    if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
        if let Ok(json) = serde_json::to_string(holds) {
            let _ = storage.set_item(&format!("dd_{}_holds", storage_key), &json);
        }
        if let Ok(json) = serde_json::to_string(lists) {
            let _ = storage.set_item(&format!("dd_{}_lists", storage_key), &json);
        }
        if let Ok(json) = serde_json::to_string(list_counters) {
            let _ = storage.set_item(&format!("dd_{}_list_counters", storage_key), &json);
        }
        if let Ok(json) = serde_json::to_string(sum_accumulators) {
            let _ = storage.set_item(&format!("dd_{}_sums", storage_key), &json);
        }
    }
}

/// Save a keyed list to localStorage from a HashMap of items.
///
/// The HashMap is assembled in the IO layer from individual keyed diffs,
/// then serialized as a Value::Tagged("List", BTreeMap) for compatibility
/// with the existing persistence format (load_holds_map reads it back).
pub fn save_keyed_list(storage_key: &str, hold_name: &str, items: &HashMap<ListKey, Value>) {
    if super::super::is_save_disabled() {
        return;
    }
    // Convert HashMap<ListKey, Value> → Value::Tagged("List", BTreeMap)
    // to match the format that load_holds_map / the compiler expects.
    let fields: BTreeMap<std::sync::Arc<str>, Value> = items
        .iter()
        .map(|(k, v)| (std::sync::Arc::from(k.0.as_ref()), v.clone()))
        .collect();
    let list_value = Value::Tagged {
        tag: std::sync::Arc::from("List"),
        fields: std::sync::Arc::new(fields),
    };
    save_hold_state(storage_key, hold_name, &list_value);
}

/// Load full program state from localStorage into the provided maps.
pub fn load_program_state(
    storage_key: &str,
    holds: &mut BTreeMap<String, Value>,
    lists: &mut BTreeMap<String, Vec<Value>>,
    list_counters: &mut BTreeMap<String, usize>,
    sum_accumulators: &mut BTreeMap<String, f64>,
) {
    if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
        if let Ok(Some(json)) = storage.get_item(&format!("dd_{}_holds", storage_key)) {
            if let Ok(h) = serde_json::from_str::<BTreeMap<String, Value>>(&json) {
                *holds = h;
            }
        }
        if let Ok(Some(json)) = storage.get_item(&format!("dd_{}_lists", storage_key)) {
            if let Ok(l) = serde_json::from_str::<BTreeMap<String, Vec<Value>>>(&json) {
                *lists = l;
            }
        }
        if let Ok(Some(json)) = storage.get_item(&format!("dd_{}_list_counters", storage_key)) {
            if let Ok(c) = serde_json::from_str::<BTreeMap<String, usize>>(&json) {
                *list_counters = c;
            }
        }
        if let Ok(Some(json)) = storage.get_item(&format!("dd_{}_sums", storage_key)) {
            if let Ok(s) = serde_json::from_str::<BTreeMap<String, f64>>(&json) {
                *sum_accumulators = s;
            }
        }
    }
}
