//! Host-side runtime for the WASM engine.
//!
//! Instantiates the generated WASM module in the browser, provides host import
//! functions, and exposes a `fire_event` API for the bridge to call.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Object, Reflect, Uint8Array, WebAssembly};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use zoon::*;

use super::ir::{BinOp, IrExpr, IrNode, IrProgram, IrValue};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setInterval)]
    fn js_set_interval(handler: &js_sys::Function, timeout: i32) -> f64;
}

// ---------------------------------------------------------------------------
// Cell store — shared between host imports and bridge
// ---------------------------------------------------------------------------

/// Holds the reactive cell values. Each cell has a `Mutable<f64>` that the bridge
/// reads via signals, and the host import `host_set_cell_f64` writes to.
#[derive(Clone)]
pub struct CellStore {
    inner: Rc<CellStoreInner>,
}

struct CellStoreInner {
    cells: Vec<Mutable<f64>>,
    /// Text values stored separately (not in WASM globals).
    text_cells: RefCell<Vec<String>>,
}

impl CellStore {
    pub fn new(num_cells: usize) -> Self {
        // Initialize with NaN so cells that aren't explicitly set during init()
        // display as empty. The init() function calls host_set_cell_f64 for each
        // cell that should have a visible initial value.
        let cells: Vec<Mutable<f64>> = (0..num_cells).map(|_| Mutable::new(f64::NAN)).collect();
        let text_cells = RefCell::new(vec![String::new(); num_cells]);
        Self {
            inner: Rc::new(CellStoreInner { cells, text_cells }),
        }
    }

    pub fn set_cell_f64(&self, cell_id: u32, value: f64) {
        if let Some(cell) = self.inner.cells.get(cell_id as usize) {
            cell.set(value);
        }
    }

    pub fn get_cell_signal(&self, cell_id: u32) -> impl Signal<Item = f64> + use<> {
        self.inner.cells[cell_id as usize].signal()
    }

    pub fn get_cell_value(&self, cell_id: u32) -> f64 {
        self.inner.cells[cell_id as usize].get()
    }

    pub fn set_cell_text(&self, cell_id: u32, text: String) {
        if let Some(entry) = self.inner.text_cells.borrow_mut().get_mut(cell_id as usize) {
            *entry = text;
        }
    }

    pub fn get_cell_text(&self, cell_id: u32) -> String {
        self.inner.text_cells.borrow().get(cell_id as usize).cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// List store — host-side list management
// ---------------------------------------------------------------------------

/// Manages list data on the host side. Each list has a unique ID.
/// Lists are stored as `Vec<f64>` and the list cell's value is the list ID.
#[derive(Clone)]
pub struct ListStore {
    inner: Rc<ListStoreInner>,
}

struct ListStoreInner {
    lists: RefCell<Vec<Vec<f64>>>,
    /// Text items stored separately (parallel to `lists`).
    text_lists: RefCell<Vec<Vec<String>>>,
    /// Version counter per list, incremented on each mutation.
    /// Used to trigger reactive updates.
    versions: RefCell<Vec<Mutable<f64>>>,
}

impl ListStore {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(ListStoreInner {
                lists: RefCell::new(Vec::new()),
                text_lists: RefCell::new(Vec::new()),
                versions: RefCell::new(Vec::new()),
            }),
        }
    }

    /// Create a new empty list, return its ID (1-based to distinguish from 0.0 void).
    pub fn create(&self) -> f64 {
        let mut lists = self.inner.lists.borrow_mut();
        let mut text_lists = self.inner.text_lists.borrow_mut();
        let mut versions = self.inner.versions.borrow_mut();
        let id = lists.len();
        lists.push(Vec::new());
        text_lists.push(Vec::new());
        versions.push(Mutable::new(0.0));
        (id + 1) as f64 // 1-based
    }

    /// Append an item to a list.
    pub fn append(&self, list_id: f64, value: f64) {
        let idx = (list_id as usize).wrapping_sub(1);
        let mut lists = self.inner.lists.borrow_mut();
        if let Some(list) = lists.get_mut(idx) {
            list.push(value);
        }
        // Bump version.
        let versions = self.inner.versions.borrow();
        if let Some(ver) = versions.get(idx) {
            ver.set(ver.get() + 1.0);
        }
    }

    /// Append a text item to a list.
    pub fn append_text(&self, list_id: f64, text: String) {
        let idx = (list_id as usize).wrapping_sub(1);
        let mut text_lists = self.inner.text_lists.borrow_mut();
        if let Some(list) = text_lists.get_mut(idx) {
            list.push(text);
        }
        // Bump version.
        let versions = self.inner.versions.borrow();
        if let Some(ver) = versions.get(idx) {
            ver.set(ver.get() + 1.0);
        }
    }

    /// Clear all items from a list.
    pub fn clear(&self, list_id: f64) {
        let idx = (list_id as usize).wrapping_sub(1);
        let mut lists = self.inner.lists.borrow_mut();
        if let Some(list) = lists.get_mut(idx) {
            list.clear();
        }
        let mut text_lists = self.inner.text_lists.borrow_mut();
        if let Some(list) = text_lists.get_mut(idx) {
            list.clear();
        }
        let versions = self.inner.versions.borrow();
        if let Some(ver) = versions.get(idx) {
            ver.set(ver.get() + 1.0);
        }
    }

    /// Get the number of items in a list (f64 items + text items).
    pub fn count(&self, list_id: f64) -> f64 {
        let idx = (list_id as usize).wrapping_sub(1);
        let lists = self.inner.lists.borrow();
        let text_lists = self.inner.text_lists.borrow();
        let f64_count = lists.get(idx).map(|l| l.len()).unwrap_or(0);
        let text_count = text_lists.get(idx).map(|l| l.len()).unwrap_or(0);
        (f64_count + text_count) as f64
    }

    /// Get a signal for the list version (triggers on mutations).
    pub fn version_signal(&self, list_id: f64) -> impl Signal<Item = f64> + use<> {
        let idx = (list_id as usize).wrapping_sub(1);
        let versions = self.inner.versions.borrow();
        if let Some(ver) = versions.get(idx) {
            ver.signal()
        } else {
            Mutable::new(0.0).signal()
        }
    }

    /// Get a snapshot of list items.
    pub fn items(&self, list_id: f64) -> Vec<f64> {
        let idx = (list_id as usize).wrapping_sub(1);
        let lists = self.inner.lists.borrow();
        lists.get(idx).cloned().unwrap_or_default()
    }

    /// Get a snapshot of text list items.
    pub fn items_text(&self, list_id: f64) -> Vec<String> {
        let idx = (list_id as usize).wrapping_sub(1);
        let text_lists = self.inner.text_lists.borrow();
        text_lists.get(idx).cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Per-item cell store — manages per-item Mutable<f64> and text for template cells
// ---------------------------------------------------------------------------

/// Stores per-item reactive signals and text for template-scoped cells.
/// Each item gets its own set of Mutable<f64> cells (for signal delivery to Zoon)
/// and text cells (for variable-length strings).
#[derive(Clone)]
pub struct ItemCellStore {
    inner: Rc<ItemCellStoreInner>,
}

struct ItemCellStoreInner {
    /// [item_idx][local_offset] = Mutable<f64> for signal delivery.
    cells: RefCell<Vec<Vec<Mutable<f64>>>>,
    /// [item_idx][local_offset] = String for text.
    text_cells: RefCell<Vec<Vec<String>>>,
    /// Template cell range start (CellId).
    template_cell_start: u32,
    /// Number of template cells.
    template_cell_count: u32,
}

impl ItemCellStore {
    pub fn new(template_cell_start: u32, template_cell_count: u32) -> Self {
        Self {
            inner: Rc::new(ItemCellStoreInner {
                cells: RefCell::new(Vec::new()),
                text_cells: RefCell::new(Vec::new()),
                template_cell_start,
                template_cell_count,
            }),
        }
    }

    /// Ensure storage exists for the given item index, growing if needed.
    pub fn ensure_item(&self, item_idx: u32) {
        let idx = item_idx as usize;
        let count = self.inner.template_cell_count as usize;
        let mut cells = self.inner.cells.borrow_mut();
        while cells.len() <= idx {
            cells.push((0..count).map(|_| Mutable::new(f64::NAN)).collect());
        }
        let mut text_cells = self.inner.text_cells.borrow_mut();
        while text_cells.len() <= idx {
            text_cells.push(vec![String::new(); count]);
        }
    }

    /// Check if a cell_id is in the template range.
    pub fn is_template_cell(&self, cell_id: u32) -> bool {
        cell_id >= self.inner.template_cell_start
            && cell_id < self.inner.template_cell_start + self.inner.template_cell_count
    }

    /// Set a per-item cell's f64 value (signal delivery).
    pub fn set_cell(&self, item_idx: u32, cell_id: u32, value: f64) {
        let local = (cell_id - self.inner.template_cell_start) as usize;
        let cells = self.inner.cells.borrow();
        if let Some(item) = cells.get(item_idx as usize) {
            if let Some(cell) = item.get(local) {
                cell.set(value);
            }
        }
    }

    /// Get a signal for a per-item cell.
    pub fn get_signal(&self, item_idx: u32, cell_id: u32) -> impl Signal<Item = f64> + use<> {
        let local = (cell_id - self.inner.template_cell_start) as usize;
        let cells = self.inner.cells.borrow();
        if let Some(item) = cells.get(item_idx as usize) {
            if let Some(cell) = item.get(local) {
                return cell.signal();
            }
        }
        // Fallback: return a static signal (shouldn't happen if ensure_item was called).
        Mutable::new(f64::NAN).signal()
    }

    /// Get a per-item cell's current f64 value.
    pub fn get_value(&self, item_idx: u32, cell_id: u32) -> f64 {
        let local = (cell_id - self.inner.template_cell_start) as usize;
        let cells = self.inner.cells.borrow();
        if let Some(item) = cells.get(item_idx as usize) {
            if let Some(cell) = item.get(local) {
                return cell.get();
            }
        }
        f64::NAN
    }

    /// Set a per-item cell's text content.
    pub fn set_text(&self, item_idx: u32, cell_id: u32, text: String) {
        let local = (cell_id - self.inner.template_cell_start) as usize;
        let mut text_cells = self.inner.text_cells.borrow_mut();
        if let Some(item) = text_cells.get_mut(item_idx as usize) {
            if let Some(entry) = item.get_mut(local) {
                *entry = text;
            }
        }
    }

    /// Get a per-item cell's text content.
    pub fn get_text(&self, item_idx: u32, cell_id: u32) -> String {
        let local = (cell_id - self.inner.template_cell_start) as usize;
        let text_cells = self.inner.text_cells.borrow();
        if let Some(item) = text_cells.get(item_idx as usize) {
            if let Some(entry) = item.get(local) {
                return entry.clone();
            }
        }
        String::new()
    }

    /// Remove an item (clear its cells). Doesn't shrink the vec.
    pub fn remove_item(&self, item_idx: u32) {
        let idx = item_idx as usize;
        let count = self.inner.template_cell_count as usize;
        let mut cells = self.inner.cells.borrow_mut();
        if let Some(item) = cells.get_mut(idx) {
            *item = (0..count).map(|_| Mutable::new(f64::NAN)).collect();
        }
        let mut text_cells = self.inner.text_cells.borrow_mut();
        if let Some(item) = text_cells.get_mut(idx) {
            *item = vec![String::new(); count];
        }
    }

    pub fn template_cell_start(&self) -> u32 {
        self.inner.template_cell_start
    }

    pub fn template_cell_count(&self) -> u32 {
        self.inner.template_cell_count
    }
}

// ---------------------------------------------------------------------------
// Item context thread-local for routing host function calls
// ---------------------------------------------------------------------------

thread_local! {
    static CURRENT_ITEM_CTX: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

// ---------------------------------------------------------------------------
// WASM instance wrapper
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct WasmInstance {
    instance: WebAssembly::Instance,
    pub cell_store: CellStore,
    pub list_store: ListStore,
    pub item_cell_store: Option<ItemCellStore>,
    /// The IR program, stored for bridge access during reactive rendering.
    pub program: Rc<IrProgram>,
    /// Tag table from the IR program (for tag name → index lookup).
    tag_table: Vec<String>,
    /// Text patterns from codegen (for host-side text matching in WHEN/WHILE).
    text_patterns: Vec<String>,
    /// Cached exported functions to avoid repeated JS reflection lookups.
    on_event_fn: js_sys::Function,
    set_global_fn: js_sys::Function,
    init_item_fn: js_sys::Function,
    on_item_event_fn: js_sys::Function,
    get_item_cell_fn: js_sys::Function,
    /// Post-event hooks for cross-scope event propagation (global events → per-item updates).
    post_event_hooks: Rc<RefCell<Vec<Box<dyn Fn(u32)>>>>,
}

impl WasmInstance {
    /// Compile and instantiate the WASM binary with host imports.
    pub fn new(wasm_bytes: &[u8], program: Rc<IrProgram>, text_patterns: Vec<String>) -> Result<Self, String> {
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();

        // Find ListMap template range for ItemCellStore.
        let mut template_range: Option<(u32, u32)> = None;
        for node in &program.nodes {
            if let IrNode::ListMap { template_cell_range, .. } = node {
                template_range = Some(*template_cell_range);
                break;
            }
        }
        let item_cell_store = template_range.map(|(start, end)| {
            ItemCellStore::new(start, end - start)
        });

        // Create import object.
        let imports = Object::new();
        let env = Object::new();

        // host_set_cell_f64(cell_id: i32, value: f64)
        // Dual-mode: routes to ItemCellStore when item context is active.
        let store_clone = cell_store.clone();
        let ics_clone = item_cell_store.clone();
        let set_cell_f64 = Closure::wrap(Box::new(move |cell_id: i32, value: f64| {
            let cid = cell_id as u32;
            let item_ctx = CURRENT_ITEM_CTX.with(|c| c.get());
            if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(cid) {
                    ics.set_cell(item_idx, cid, value);
                    return;
                }
            }
            store_clone.set_cell_f64(cid, value);
        }) as Box<dyn FnMut(i32, f64)>);
        Reflect::set(&env, &"host_set_cell_f64".into(), set_cell_f64.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_set_cell_f64: {:?}", e))?;
        set_cell_f64.forget();

        // host_notify_init_done()
        let notify_init = Closure::wrap(Box::new(move || {
            // No-op for now; could trigger UI refresh.
        }) as Box<dyn FnMut()>);
        Reflect::set(&env, &"host_notify_init_done".into(), notify_init.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_notify_init_done: {:?}", e))?;
        notify_init.forget();

        // host_list_create() -> f64 (returns list_id)
        let ls_clone = list_store.clone();
        let list_create = Closure::wrap(Box::new(move || -> f64 {
            ls_clone.create()
        }) as Box<dyn FnMut() -> f64>);
        Reflect::set(&env, &"host_list_create".into(), list_create.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_list_create: {:?}", e))?;
        list_create.forget();

        // host_list_append(list_cell_id: i32, item_value: f64)
        let ls_clone = list_store.clone();
        let cs_clone = cell_store.clone();
        let list_append = Closure::wrap(Box::new(move |list_cell_id: i32, item_value: f64| {
            let list_id = cs_clone.get_cell_value(list_cell_id as u32);
            ls_clone.append(list_id, item_value);
        }) as Box<dyn FnMut(i32, f64)>);
        Reflect::set(&env, &"host_list_append".into(), list_append.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_list_append: {:?}", e))?;
        list_append.forget();

        // host_list_clear(list_cell_id: i32)
        let ls_clone = list_store.clone();
        let cs_clone = cell_store.clone();
        let list_clear = Closure::wrap(Box::new(move |list_cell_id: i32| {
            let list_id = cs_clone.get_cell_value(list_cell_id as u32);
            ls_clone.clear(list_id);
        }) as Box<dyn FnMut(i32)>);
        Reflect::set(&env, &"host_list_clear".into(), list_clear.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_list_clear: {:?}", e))?;
        list_clear.forget();

        // host_list_count(list_cell_id: i32) -> f64
        let ls_clone = list_store.clone();
        let cs_clone = cell_store.clone();
        let list_count = Closure::wrap(Box::new(move |list_cell_id: i32| -> f64 {
            let list_id = cs_clone.get_cell_value(list_cell_id as u32);
            ls_clone.count(list_id)
        }) as Box<dyn FnMut(i32) -> f64>);
        Reflect::set(&env, &"host_list_count".into(), list_count.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_list_count: {:?}", e))?;
        list_count.forget();

        // host_list_copy_item(new_list_id: f64, source_cell_id: i32, item_idx: i32)
        // Copies one item from source list to dest list, storing original index.
        let ls_clone = list_store.clone();
        let cs_clone = cell_store.clone();
        let list_copy_item = Closure::wrap(Box::new(move |new_list_id: f64, source_cell_id: i32, item_idx: i32| {
            let source_list_id = cs_clone.get_cell_value(source_cell_id as u32);
            let text_items = ls_clone.items_text(source_list_id);
            if let Some(text) = text_items.get(item_idx as usize) {
                ls_clone.append_text(new_list_id, text.clone());
            }
            ls_clone.append(new_list_id, item_idx as f64);
        }) as Box<dyn FnMut(f64, i32, i32)>);
        Reflect::set(&env, &"host_list_copy_item".into(), list_copy_item.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_list_copy_item: {:?}", e))?;
        list_copy_item.forget();

        // host_text_trim(dest_cell: i32, src_cell: i32)
        // Dual-mode: routes to ItemCellStore when item context is active.
        let cs_clone = cell_store.clone();
        let ics_clone = item_cell_store.clone();
        let text_trim = Closure::wrap(Box::new(move |dest_cell: i32, src_cell: i32| {
            let item_ctx = CURRENT_ITEM_CTX.with(|c| c.get());
            let text = if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(src_cell as u32) {
                    ics.get_text(item_idx, src_cell as u32)
                } else {
                    cs_clone.get_cell_text(src_cell as u32)
                }
            } else {
                cs_clone.get_cell_text(src_cell as u32)
            };
            let trimmed = text.trim().to_string();
            if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(dest_cell as u32) {
                    ics.set_text(item_idx, dest_cell as u32, trimmed);
                    return;
                }
            }
            cs_clone.set_cell_text(dest_cell as u32, trimmed);
        }) as Box<dyn FnMut(i32, i32)>);
        Reflect::set(&env, &"host_text_trim".into(), text_trim.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_text_trim: {:?}", e))?;
        text_trim.forget();

        // host_text_is_not_empty(cell_id: i32) -> f64
        // Dual-mode: reads from ItemCellStore when item context is active.
        let cs_clone = cell_store.clone();
        let ics_clone = item_cell_store.clone();
        let text_is_not_empty = Closure::wrap(Box::new(move |cell_id: i32| -> f64 {
            let item_ctx = CURRENT_ITEM_CTX.with(|c| c.get());
            let text = if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(cell_id as u32) {
                    ics.get_text(item_idx, cell_id as u32)
                } else {
                    cs_clone.get_cell_text(cell_id as u32)
                }
            } else {
                cs_clone.get_cell_text(cell_id as u32)
            };
            if text.is_empty() { 0.0 } else { 1.0 }
        }) as Box<dyn FnMut(i32) -> f64>);
        Reflect::set(&env, &"host_text_is_not_empty".into(), text_is_not_empty.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_text_is_not_empty: {:?}", e))?;
        text_is_not_empty.forget();

        // host_copy_text(dest_cell: i32, src_cell: i32)
        // Dual-mode: routes to ItemCellStore when item context is active.
        let cs_clone = cell_store.clone();
        let ics_clone = item_cell_store.clone();
        let copy_text = Closure::wrap(Box::new(move |dest_cell: i32, src_cell: i32| {
            let item_ctx = CURRENT_ITEM_CTX.with(|c| c.get());
            let src_text = if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(src_cell as u32) {
                    ics.get_text(item_idx, src_cell as u32)
                } else {
                    cs_clone.get_cell_text(src_cell as u32)
                }
            } else {
                cs_clone.get_cell_text(src_cell as u32)
            };
            if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(dest_cell as u32) {
                    ics.set_text(item_idx, dest_cell as u32, src_text);
                    return;
                }
            }
            cs_clone.set_cell_text(dest_cell as u32, src_text);
        }) as Box<dyn FnMut(i32, i32)>);
        Reflect::set(&env, &"host_copy_text".into(), copy_text.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_copy_text: {:?}", e))?;
        copy_text.forget();

        // host_list_append_text(list_cell_id: i32, item_cell_id: i32)
        let ls_clone = list_store.clone();
        let cs_clone = cell_store.clone();
        let list_append_text = Closure::wrap(Box::new(move |list_cell_id: i32, item_cell_id: i32| {
            let list_id = cs_clone.get_cell_value(list_cell_id as u32);
            let text = cs_clone.get_cell_text(item_cell_id as u32);
            ls_clone.append_text(list_id, text);
        }) as Box<dyn FnMut(i32, i32)>);
        Reflect::set(&env, &"host_list_append_text".into(), list_append_text.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_list_append_text: {:?}", e))?;
        list_append_text.forget();

        // host_text_matches(cell_id: i32, pattern_idx: i32) -> i32
        // Dual-mode: reads from ItemCellStore when item context is active.
        let cs_clone = cell_store.clone();
        let ics_clone = item_cell_store.clone();
        let patterns_clone = text_patterns.clone();
        let text_matches = Closure::wrap(Box::new(move |cell_id: i32, pattern_idx: i32| -> i32 {
            let item_ctx = CURRENT_ITEM_CTX.with(|c| c.get());
            let cell_text = if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(cell_id as u32) {
                    ics.get_text(item_idx, cell_id as u32)
                } else {
                    cs_clone.get_cell_text(cell_id as u32)
                }
            } else {
                cs_clone.get_cell_text(cell_id as u32)
            };
            if let Some(pattern) = patterns_clone.get(pattern_idx as usize) {
                if cell_text == *pattern { 1 } else { 0 }
            } else {
                0
            }
        }) as Box<dyn FnMut(i32, i32) -> i32>);
        Reflect::set(&env, &"host_text_matches".into(), text_matches.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_text_matches: {:?}", e))?;
        text_matches.forget();

        // host_set_cell_text_pattern(cell_id: i32, pattern_idx: i32) -> ()
        // Dual-mode: routes to ItemCellStore when item context is active.
        let cs_clone = cell_store.clone();
        let ics_clone = item_cell_store.clone();
        let patterns_clone = text_patterns.clone();
        let set_text_pattern = Closure::wrap(Box::new(move |cell_id: i32, pattern_idx: i32| {
            if let Some(pattern) = patterns_clone.get(pattern_idx as usize) {
                let item_ctx = CURRENT_ITEM_CTX.with(|c| c.get());
                if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                    if ics.is_template_cell(cell_id as u32) {
                        ics.set_text(item_idx, cell_id as u32, pattern.clone());
                        return;
                    }
                }
                cs_clone.set_cell_text(cell_id as u32, pattern.clone());
            }
        }) as Box<dyn FnMut(i32, i32)>);
        Reflect::set(&env, &"host_set_cell_text_pattern".into(), set_text_pattern.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_set_cell_text_pattern: {:?}", e))?;
        set_text_pattern.forget();

        // host_text_build_start(target_cell: i32) -> ()
        // Dual-mode: routes to ItemCellStore when item context is active.
        let text_build_target: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
        let target_clone = text_build_target.clone();
        let cs_clone = cell_store.clone();
        let ics_clone = item_cell_store.clone();
        let build_start = Closure::wrap(Box::new(move |target_cell: i32| {
            *target_clone.borrow_mut() = target_cell as u32;
            let item_ctx = CURRENT_ITEM_CTX.with(|c| c.get());
            if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(target_cell as u32) {
                    ics.set_text(item_idx, target_cell as u32, String::new());
                    return;
                }
            }
            cs_clone.set_cell_text(target_cell as u32, String::new());
        }) as Box<dyn FnMut(i32)>);
        Reflect::set(&env, &"host_text_build_start".into(), build_start.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_text_build_start: {:?}", e))?;
        build_start.forget();

        // host_text_build_literal(pattern_idx: i32) -> ()
        // Dual-mode: routes to ItemCellStore when item context is active.
        let target_clone = text_build_target.clone();
        let cs_clone = cell_store.clone();
        let ics_clone = item_cell_store.clone();
        let patterns_clone = text_patterns.clone();
        let build_literal = Closure::wrap(Box::new(move |pattern_idx: i32| {
            let target_cell = *target_clone.borrow();
            if let Some(text) = patterns_clone.get(pattern_idx as usize) {
                let item_ctx = CURRENT_ITEM_CTX.with(|c| c.get());
                if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                    if ics.is_template_cell(target_cell) {
                        let mut current = ics.get_text(item_idx, target_cell);
                        current.push_str(text);
                        ics.set_text(item_idx, target_cell, current);
                        return;
                    }
                }
                let mut current = cs_clone.get_cell_text(target_cell);
                current.push_str(text);
                cs_clone.set_cell_text(target_cell, current);
            }
        }) as Box<dyn FnMut(i32)>);
        Reflect::set(&env, &"host_text_build_literal".into(), build_literal.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_text_build_literal: {:?}", e))?;
        build_literal.forget();

        // host_text_build_cell(cell_id: i32) -> ()
        // Dual-mode: routes to ItemCellStore when item context is active.
        let target_clone = text_build_target.clone();
        let cs_clone = cell_store.clone();
        let ics_clone = item_cell_store.clone();
        let build_cell = Closure::wrap(Box::new(move |cell_id: i32| {
            let target_cell = *target_clone.borrow();
            let item_ctx = CURRENT_ITEM_CTX.with(|c| c.get());
            // Read cell text: check item store first for template cells.
            let cell_text = if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(cell_id as u32) {
                    ics.get_text(item_idx, cell_id as u32)
                } else {
                    cs_clone.get_cell_text(cell_id as u32)
                }
            } else {
                cs_clone.get_cell_text(cell_id as u32)
            };
            let formatted = if !cell_text.is_empty() {
                cell_text
            } else {
                let val = cs_clone.get_cell_value(cell_id as u32);
                format_f64_for_text(val)
            };
            // Write to target: check item store first.
            if let (Some(item_idx), Some(ics)) = (item_ctx, &ics_clone) {
                if ics.is_template_cell(target_cell) {
                    let mut current = ics.get_text(item_idx, target_cell);
                    current.push_str(&formatted);
                    ics.set_text(item_idx, target_cell, current);
                    return;
                }
            }
            let mut current = cs_clone.get_cell_text(target_cell);
            current.push_str(&formatted);
            cs_clone.set_cell_text(target_cell, current);
        }) as Box<dyn FnMut(i32)>);
        Reflect::set(&env, &"host_text_build_cell".into(), build_cell.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_text_build_cell: {:?}", e))?;
        build_cell.forget();

        // host_set_item_context(item_idx: i32)
        let ics_clone = item_cell_store.clone();
        let set_item_ctx = Closure::wrap(Box::new(move |item_idx: i32| {
            let idx = item_idx as u32;
            CURRENT_ITEM_CTX.with(|c| c.set(Some(idx)));
            if let Some(store) = &ics_clone {
                store.ensure_item(idx);
            }
        }) as Box<dyn FnMut(i32)>);
        Reflect::set(&env, &"host_set_item_context".into(), set_item_ctx.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_set_item_context: {:?}", e))?;
        set_item_ctx.forget();

        // host_clear_item_context()
        let clear_item_ctx = Closure::wrap(Box::new(move || {
            CURRENT_ITEM_CTX.with(|c| c.set(None));
        }) as Box<dyn FnMut()>);
        Reflect::set(&env, &"host_clear_item_context".into(), clear_item_ctx.as_ref().unchecked_ref())
            .map_err(|e| format!("Failed to set host_clear_item_context: {:?}", e))?;
        clear_item_ctx.forget();

        Reflect::set(&imports, &"env".into(), &env)
            .map_err(|e| format!("Failed to set env: {:?}", e))?;

        // Compile WASM module.
        let wasm_buffer = Uint8Array::from(wasm_bytes);
        let module = WebAssembly::Module::new(&wasm_buffer.into())
            .map_err(|e| format!("WASM compile error: {:?}", e))?;

        // Instantiate.
        let instance = WebAssembly::Instance::new(&module, &imports)
            .map_err(|e| format!("WASM instantiate error: {:?}", e))?;

        // Cache exported functions for fast access.
        let exports = instance.exports();
        let on_event_fn: js_sys::Function = Reflect::get(&exports, &"on_event".into())
            .map_err(|e| format!("No on_event export: {:?}", e))?
            .dyn_into()
            .map_err(|_| "on_event is not a function".to_string())?;
        let set_global_fn: js_sys::Function = Reflect::get(&exports, &"set_global".into())
            .map_err(|e| format!("No set_global export: {:?}", e))?
            .dyn_into()
            .map_err(|_| "set_global is not a function".to_string())?;
        let init_item_fn: js_sys::Function = Reflect::get(&exports, &"init_item".into())
            .map_err(|e| format!("No init_item export: {:?}", e))?
            .dyn_into()
            .map_err(|_| "init_item is not a function".to_string())?;
        let on_item_event_fn: js_sys::Function = Reflect::get(&exports, &"on_item_event".into())
            .map_err(|e| format!("No on_item_event export: {:?}", e))?
            .dyn_into()
            .map_err(|_| "on_item_event is not a function".to_string())?;
        let get_item_cell_fn: js_sys::Function = Reflect::get(&exports, &"get_item_cell".into())
            .map_err(|e| format!("No get_item_cell export: {:?}", e))?
            .dyn_into()
            .map_err(|_| "get_item_cell is not a function".to_string())?;

        let program_tag_table = program.tag_table.clone();

        Ok(Self {
            instance,
            cell_store,
            list_store,
            item_cell_store,
            program,
            tag_table: program_tag_table,
            text_patterns,
            on_event_fn,
            set_global_fn,
            init_item_fn,
            on_item_event_fn,
            get_item_cell_fn,
            post_event_hooks: Rc::new(RefCell::new(Vec::new())),
        })
    }

    /// Call the `init()` exported function.
    pub fn call_init(&self) -> Result<(), String> {
        let exports = self.instance.exports();
        let init_fn = Reflect::get(&exports, &"init".into())
            .map_err(|e| format!("No init export: {:?}", e))?;
        let init_fn: js_sys::Function = init_fn
            .dyn_into()
            .map_err(|_| "init is not a function".to_string())?;
        init_fn
            .call0(&JsValue::NULL)
            .map_err(|e| format!("init() failed: {:?}", e))?;
        Ok(())
    }

    /// Start JS setInterval timers for each Timer node in the program.
    /// Must be called after the instance is wrapped in Rc.
    pub fn start_timers(self: &Rc<Self>, program: &IrProgram) {
        for node in &program.nodes {
            if let IrNode::Timer { event, interval_ms } = node {
                let ms = eval_const_f64(interval_ms).unwrap_or(1000.0);
                let event_id = event.0;
                let inst = self.clone();
                let cb = Closure::wrap(Box::new(move || {
                    let _ = inst.fire_event(event_id);
                }) as Box<dyn FnMut()>);
                js_set_interval(cb.as_ref().unchecked_ref(), ms as i32);
                cb.forget(); // Leak so the interval keeps working.
            }
        }
    }

    /// Look up a tag name in the program's tag table and return its encoded f64 value.
    /// Tags are 1-based (first tag = 1.0). Returns 0.0 if not found.
    pub fn program_tag_index(&self, tag: &str) -> f64 {
        for (i, t) in self.tag_table.iter().enumerate() {
            if t == tag {
                return (i + 1) as f64;
            }
        }
        0.0
    }

    /// Set a cell value on both the host side and the WASM global.
    pub fn set_cell_value(&self, cell_id: u32, value: f64) {
        self.cell_store.set_cell_f64(cell_id, value);
        // Also update the WASM global via the cached set_global export.
        let _ = self.set_global_fn.call2(
            &JsValue::NULL,
            &JsValue::from(cell_id as f64),
            &JsValue::from(value),
        );
    }

    /// Call `on_event(event_id)` to fire an event.
    /// After the WASM handler runs, calls any registered post-event hooks
    /// (used for cross-scope event propagation to per-item templates).
    pub fn fire_event(&self, event_id: u32) -> Result<(), String> {
        self.on_event_fn
            .call1(&JsValue::NULL, &JsValue::from(event_id as f64))
            .map_err(|e| format!("on_event({}) failed: {:?}", event_id, e))?;
        // Run post-event hooks (cross-scope event propagation).
        for hook in self.post_event_hooks.borrow().iter() {
            hook(event_id);
        }
        Ok(())
    }

    /// Register a hook that runs after every fire_event call.
    /// Used by the bridge to propagate global events to per-item templates.
    pub fn add_post_event_hook(&self, hook: Box<dyn Fn(u32)>) {
        self.post_event_hooks.borrow_mut().push(hook);
    }

    /// Call `init_item(item_idx)` to initialize per-item template cells.
    pub fn call_init_item(&self, item_idx: u32) -> Result<(), String> {
        self.init_item_fn
            .call1(&JsValue::NULL, &JsValue::from(item_idx as f64))
            .map_err(|e| format!("init_item({}) failed: {:?}", item_idx, e))?;
        Ok(())
    }

    /// Call `on_item_event(item_idx, event_id)` to handle a per-item event.
    pub fn call_on_item_event(&self, item_idx: u32, event_id: u32) -> Result<(), String> {
        self.on_item_event_fn
            .call2(
                &JsValue::NULL,
                &JsValue::from(item_idx as f64),
                &JsValue::from(event_id as f64),
            )
            .map_err(|e| format!("on_item_event({}, {}) failed: {:?}", item_idx, event_id, e))?;
        Ok(())
    }

    /// Call `get_item_cell(item_idx, cell_offset)` to read a per-item cell value.
    pub fn call_get_item_cell(&self, item_idx: u32, cell_offset: u32) -> f64 {
        match self.get_item_cell_fn.call2(
            &JsValue::NULL,
            &JsValue::from(item_idx as f64),
            &JsValue::from(cell_offset as f64),
        ) {
            Ok(val) => val.as_f64().unwrap_or(0.0),
            Err(_) => 0.0,
        }
    }
}

/// Format an f64 value as text for display (integers without decimals).
fn format_f64_for_text(val: f64) -> String {
    if val.is_nan() {
        String::new()
    } else if val.fract() == 0.0 && val.is_finite() && val.abs() < (i64::MAX as f64) {
        format!("{}", val as i64)
    } else {
        format!("{}", val)
    }
}

/// Evaluate a constant IrExpr to f64 (simple constant folding for timer intervals).
fn eval_const_f64(expr: &IrExpr) -> Option<f64> {
    match expr {
        IrExpr::Constant(IrValue::Number(n)) => Some(*n),
        IrExpr::BinOp { op, lhs, rhs } => {
            let l = eval_const_f64(lhs)?;
            let r = eval_const_f64(rhs)?;
            Some(match op {
                BinOp::Add => l + r,
                BinOp::Sub => l - r,
                BinOp::Mul => l * r,
                BinOp::Div => l / r,
            })
        }
        _ => None,
    }
}
