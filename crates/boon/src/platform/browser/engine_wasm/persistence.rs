//! WASM engine persistence — save/restore full state snapshots via localStorage.
//!
//! After every event, the entire engine state (global cells, list contents,
//! per-item cells, WASM linear memory) is serialized to localStorage.
//! On reload, the snapshot is restored after init() to recreate the UI state.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Serialize, Deserialize};
use zoon::web_sys;

use super::runtime::WasmInstance;

// ---------------------------------------------------------------------------
// localStorage helpers
// ---------------------------------------------------------------------------

fn get_local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn local_storage_set(key: &str, value: &str) {
    if let Some(storage) = get_local_storage() {
        let _ = storage.set_item(key, value);
    }
}

fn local_storage_get(key: &str) -> Option<String> {
    get_local_storage()?.get_item(key).ok()?
}

// ---------------------------------------------------------------------------
// Snapshot types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub(super) struct WasmSnapshot {
    /// Global cell f64 values: (cell_id, value) for non-NaN cells.
    cells: Vec<(u32, f64)>,
    /// Global cell text values: (cell_id, text) for non-empty texts.
    texts: Vec<(u32, String)>,
    /// List snapshots, one per list (index = list_id - 1).
    lists: Vec<ListSnapshot>,
    /// Per-item cell snapshots.
    item_cells: Vec<ItemCellSnapshot>,
    /// WASM linear memory (per-item region), stored as raw bytes.
    wasm_memory: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct ListSnapshot {
    items: Vec<f64>,
    texts: Vec<String>,
    is_index_based: bool,
    next_memory_index: usize,
}

#[derive(Serialize, Deserialize)]
struct ItemCellSnapshot {
    item_idx: u32,
    cells: Vec<(u32, f64)>,
    texts: Vec<(u32, String)>,
}

// ---------------------------------------------------------------------------
// Storage key derivation
// ---------------------------------------------------------------------------

/// Derive a stable localStorage key from the source code.
pub fn storage_key(source: &str) -> String {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    format!("wasm_{:016x}", hasher.finish())
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

/// Capture current engine state and save to localStorage.
pub fn save_and_store(instance: &WasmInstance, key: &str) {
    let snapshot = capture_snapshot(instance);
    if let Ok(json) = serde_json::to_string(&snapshot) {
        local_storage_set(key, &json);
    }
}

fn capture_snapshot(instance: &WasmInstance) -> WasmSnapshot {
    let cs = &instance.cell_store;
    let ls = &instance.list_store;

    // 1. Global cells.
    let num_cells = cs.cell_count();
    let mut cells = Vec::new();
    let mut texts = Vec::new();
    for id in 0..num_cells {
        let cid = id as u32;
        let val = cs.get_cell_value(cid);
        if !val.is_nan() {
            cells.push((cid, val));
        }
        let text = cs.get_cell_text(cid);
        if !text.is_empty() {
            texts.push((cid, text));
        }
    }

    // 2. Lists.
    let num_lists = ls.list_count();
    let mut lists = Vec::with_capacity(num_lists);
    for i in 0..num_lists {
        let list_id = (i + 1) as f64; // 1-based
        lists.push(ListSnapshot {
            items: ls.items(list_id),
            texts: ls.items_text(list_id),
            is_index_based: ls.is_index_based(list_id),
            next_memory_index: ls.next_memory_index(list_id),
        });
    }

    // 3. Per-item cells.
    let mut item_cells = Vec::new();
    if let Some(ref ics) = instance.item_cell_store {
        let item_count = ics.item_count();
        for idx in 0..item_count {
            let item_idx = idx as u32;
            let cell_vals = ics.all_cell_values(item_idx);
            let text_vals = ics.all_text_values(item_idx);
            if !cell_vals.is_empty() || !text_vals.is_empty() {
                item_cells.push(ItemCellSnapshot {
                    item_idx,
                    cells: cell_vals,
                    texts: text_vals,
                });
            }
        }
    }

    // 4. WASM linear memory (per-item region).
    let wasm_memory = if let Some(ref ics) = instance.item_cell_store {
        let cell_count = ics.template_cell_count();
        if cell_count > 0 {
            // Find the max memory index used across all source lists.
            let mut max_mem_idx: usize = 0;
            for i in 0..num_lists {
                let list_id = (i + 1) as f64;
                let nmi = ls.next_memory_index(list_id);
                if nmi > max_mem_idx {
                    max_mem_idx = nmi;
                }
            }
            if max_mem_idx > 0 {
                let stride = (cell_count as usize) * 8;
                let byte_len = max_mem_idx * stride;
                instance.read_memory(0, byte_len)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    WasmSnapshot { cells, texts, lists, item_cells, wasm_memory }
}

// ---------------------------------------------------------------------------
// Load
// ---------------------------------------------------------------------------

/// Load a snapshot from localStorage.
/// Returns the opaque snapshot for two-phase restore, or None if no snapshot.
pub fn load_snapshot(key: &str) -> Option<Box<WasmSnapshot>> {
    let snap = load_from_storage(key)?;
    Some(Box::new(snap))
}

/// Phase 1: Restore global cells, texts, list structure, and WASM memory.
/// Call BEFORE build_ui so the list has the right items when the bridge renders.
/// Also restores WASM linear memory and re-runs retain filters so that filtered
/// lists (e.g., All/Active/Completed) have the correct items for the first render.
pub fn restore_phase1(instance: &WasmInstance, snapshot: &WasmSnapshot) {
    let cs = &instance.cell_store;
    let ls = &instance.list_store;

    // Restore global cell f64 values (writes to both WASM global + CellStore).
    for &(cell_id, value) in &snapshot.cells {
        instance.set_cell_value(cell_id, value);
    }

    // Restore global cell texts.
    for (cell_id, text) in &snapshot.texts {
        cs.set_cell_text(*cell_id, text.clone());
    }

    // Restore lists: clear init() defaults and rebuild from snapshot.
    // Create any additional lists that the snapshot requires (retain filters
    // create temporary lists during events that don't exist after a fresh init).
    while ls.list_count() < snapshot.lists.len() {
        ls.create();
    }
    for (i, list_snap) in snapshot.lists.iter().enumerate() {
        let list_id = (i + 1) as f64;

        ls.clear(list_id);

        for &item_val in &list_snap.items {
            ls.append_with_index(list_id, item_val);
        }
        for text in &list_snap.texts {
            ls.append_text(list_id, text.clone());
        }
        if list_snap.is_index_based {
            ls.restore_index_based(list_id);
        }
        ls.set_next_memory_index(list_id, list_snap.next_memory_index);
        ls.bump_version(list_id);
    }

    // Restore WASM linear memory (per-item cell values in WASM memory).
    // This must happen before rerun_retain_filters so that filter predicates
    // (e.g., completed status) can read correct per-item values.
    if !snapshot.wasm_memory.is_empty() {
        instance.write_memory(0, &snapshot.wasm_memory);
    }

    // Re-run retain filters so filtered lists reflect the restored state.
    // Without this, the bridge's first render would see stale filtered lists
    // (e.g., "All" filter showing only 2 of 3 items because the retain filter
    // hadn't been re-evaluated with restored per-item data).
    let _ = instance.call_rerun_retain_filters();
}

/// Restore a single item's per-item cells and WASM memory region from the snapshot.
/// Called immediately after `init_item` so the WASM defaults are overwritten
/// with persisted values before the bridge builds the element tree.
pub(super) fn restore_single_item(instance: &WasmInstance, item_idx: u32, snapshot: &WasmSnapshot) {
    // Restore per-item cells (overwrites init_item defaults).
    if let Some(ref ics) = instance.item_cell_store {
        for item_snap in &snapshot.item_cells {
            if item_snap.item_idx == item_idx {
                for &(cell_id, value) in &item_snap.cells {
                    ics.set_cell(item_idx, cell_id, value);
                }
                for (cell_id, text) in &item_snap.texts {
                    ics.set_text(item_idx, *cell_id, text.clone());
                }
                break;
            }
        }

        // Restore WASM memory for this specific item.
        if !snapshot.wasm_memory.is_empty() {
            let stride = ics.template_cell_count() as usize * 8;
            let offset = item_idx as usize * stride;
            if stride > 0 && offset + stride <= snapshot.wasm_memory.len() {
                instance.write_memory(offset, &snapshot.wasm_memory[offset..offset + stride]);
            }
        }
    }
}

/// Restore global cells and re-derive global values after all items are initialized.
/// Called once from the bridge after the first list render completes.
pub(super) fn restore_globals(instance: &WasmInstance, snapshot: &WasmSnapshot) {
    // Re-run retain filters so WASM code re-derives global values
    // (e.g., active count) from the now-correct per-item WASM memory.
    let _ = instance.call_rerun_retain_filters();

    // Re-apply global cells from snapshot (overwrite any values that
    // rerun_retain_filters may have computed differently).
    for &(cell_id, value) in &snapshot.cells {
        instance.set_cell_value(cell_id, value);
    }
    for (cell_id, text) in &snapshot.texts {
        instance.cell_store.set_cell_text(*cell_id, text.clone());
    }
}

fn load_from_storage(key: &str) -> Option<WasmSnapshot> {
    let json = local_storage_get(key)?;
    serde_json::from_str::<WasmSnapshot>(&json).ok()
}

// ---------------------------------------------------------------------------
// Clear
// ---------------------------------------------------------------------------

/// Clear all WASM persistence keys from localStorage.
pub fn clear_wasm_persisted_states() {
    if let Some(storage) = get_local_storage() {
        let len = storage.length().unwrap_or(0);
        let mut keys_to_remove = Vec::new();
        for i in 0..len {
            if let Ok(Some(key)) = storage.key(i) {
                if key.starts_with("wasm_") {
                    keys_to_remove.push(key);
                }
            }
        }
        for key in keys_to_remove {
            let _ = storage.remove_item(&key);
        }
    }
}
