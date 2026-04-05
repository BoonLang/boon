//! ActorsLite browser engine crate.
//!
//! Current source of truth:
//! `docs/plans/ACTORSLITE_STRICT_UNIFIED_ENGINE_PLAN.md`
//!
//! The current preview-dispatch architecture in this crate is transitional.
//! Keep the repo aligned with the strict unified-engine plan and avoid adding
//! new example-specific lowering or acceptance shortcuts.

use boon::zoon::*;

pub mod acceptance;
#[cfg(test)]
mod append_list_runtime;
pub mod bridge;
pub mod browser_debug;
pub mod cells_acceptance;
pub mod cells_lower;
pub mod cells_preview;
pub mod cells_runtime;
pub mod chained_list_remove_bug_preview;
pub mod checkbox_test_preview;
pub mod circle_drawer_preview;
pub mod clock;
pub mod complex_counter_preview;
pub mod counter_acceptance;
pub mod counter_persistence;
pub mod crud_preview;
pub mod dispatch;
pub mod edit_session;
pub mod editable_list_actions;
mod editable_mapped_list_preview_runtime;
pub mod editable_mapped_list_runtime;
pub mod fibonacci_preview;
pub mod filter_checkbox_bug_preview;
pub mod filtered_list_view;
pub mod flight_booker_preview;
pub mod generic_preview;
mod host_view_preview;
pub mod host_view_template;
pub mod ids;
pub mod input_form_runtime;
mod interactive_preview;
pub mod interval_preview;
pub mod ir;
mod ir_executor;
pub mod latest_preview;
pub mod layers_preview;
pub mod list_form_actions;
pub mod list_map_block_preview;
pub mod list_map_external_dep_preview;
pub mod list_object_state_preview;
pub mod list_retain_count_preview;
pub mod list_retain_reactive_preview;
pub mod list_retain_remove_preview;
pub mod list_semantics;
pub mod lower;
mod lower_legacy;
pub mod lowered_preview;
pub mod mapped_click_runtime;
pub mod mapped_item_state_runtime;
pub mod mapped_list_runtime;
pub mod mapped_list_view_runtime;
pub mod metrics;
pub mod multi_input_state;
pub mod pages_preview;
pub mod parse;
pub mod persist;
#[cfg(target_arch = "wasm32")]
pub mod persist_browser;
pub mod persistence;
pub mod persistent_executor;
pub mod preview;
mod preview_runtime;
pub mod preview_shell;
mod retained_ui_state;
mod runtime;
mod runtime_backed_domain;
mod runtime_backed_preview;
pub mod selected_filter_click_runtime;
pub mod selected_list_filter;
pub mod semantics;
pub mod shopping_list_preview;
pub mod slot_projection;
pub mod static_preview;
pub mod targeted_list_runtime;
pub mod temperature_converter_preview;
pub mod text_filtered_editable_list_preview_runtime;
pub mod text_input;
pub mod text_interpolation_update_preview;
pub mod timed_math_preview;
pub mod timer_preview;
pub mod todo_acceptance;
pub mod todo_physical_preview;
pub mod todo_preview;
pub mod toggle_examples_preview;
pub mod validated_form_runtime;

pub use acceptance::{
    actors_lite_public_exposure_enabled,
};
pub use dispatch::{
    MILESTONE_PLAYGROUND_EXAMPLES, PUBLIC_PLAYGROUND_EXAMPLES, SUPPORTED_PLAYGROUND_EXAMPLES,
    is_public_playground_example, is_supported_playground_example,
};
pub use metrics::{
    ActorsLiteMetricsComparison, ActorsLiteMetricsReport, CellsMetricsReport, CounterMetricsReport,
    InteractionMetricsReport, LatencySummary, RuntimeCoreMetricsReport, TodoMetricsReport,
    actors_lite_metrics_snapshot,
};

pub fn run_actors_lite(source: &str) -> impl Element {
    crate::browser_debug::clear_debug_marker();
    crate::browser_debug::set_debug_marker("run_actors_lite:start");
    match lower::lower_program(source) {
        Ok(program) => {
            crate::browser_debug::set_debug_marker("run_actors_lite:lowered");
            match lowered_preview::LoweredPreview::from_program(program) {
                Ok(preview) => lowered_preview::render_lowered_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("lowered preview: {error}")).unify(),
            }
        }
        Err(error) => {
            crate::browser_debug::set_debug_marker("run_actors_lite:unsupported");
            render_dispatch_error(format!("unsupported source: {error}")).unify()
        }
    }
}

/// Run ActorsLite with persistence enabled.
///
/// On wasm32, this uses the browser's localStorage for persistence.
/// On other targets, this falls back to the non-persistent path.
#[cfg(target_arch = "wasm32")]
pub fn run_actors_lite_with_persistence(source: &str) -> impl Element {
    use crate::lower::LoweredProgram;
    use crate::persist::{PersistedRecord, PersistenceAdapter};
    use crate::persist_browser::BrowserLocalStorage;
    use boon::platform::browser::kernel::KernelValue;

    crate::browser_debug::clear_debug_marker();
    crate::browser_debug::set_debug_marker("run_persist:start");
    crate::browser_debug::set_debug_marker(&format!("run_persist:source_len:{}", source.len()));
    match lower::lower_program(source) {
        Ok(program) => {
            crate::browser_debug::set_debug_marker("run_persist:lowered");
            // Try to restore persisted state based on program type
            crate::browser_debug::set_debug_marker("run_persist:program_variant_START"); let variant_name = match &program {
                LoweredProgram::ShoppingList(_) => "ShoppingList",
                LoweredProgram::Counter(_) => "Counter",
                LoweredProgram::TodoMvc(_) => "TodoMvc",
                _ => "Other"}; crate::browser_debug::set_debug_marker(&format!("run_persist:program_variant:{}", variant_name));
            
            let program = match program {
                LoweredProgram::Counter(mut counter_program) => {
                    if let Some(restored) = try_restore_counter_value() {
                        counter_program.initial_value = restored;
                        crate::browser_debug::set_debug_marker(&format!("run_persist:counter_initial_value_set:{}", restored));
                        // Also update the IR's literal node with the restored value
                        let mut modified = false;
                        for node in &mut counter_program.ir.nodes {
                            if let crate::ir::IrNodeKind::Literal(KernelValue::Number(n)) = &mut node.kind {
                                crate::browser_debug::set_debug_marker(&format!("run_persist:found_literal_node:{n}"));
                                if *n == 0.0 {
                                    *n = restored as f64;
                                    modified = true;
                                    crate::browser_debug::set_debug_marker(&format!("run_persist:modified_literal_to:{}", restored));
                                    break;
                                }
                            }
                        }
                        if !modified {
                            crate::browser_debug::set_debug_marker("run_persist:NO_literal_modified");
                        }
                        crate::browser_debug::set_debug_marker(&format!("run_persist:counter_restored:{}", restored));
                        LoweredProgram::Counter(counter_program)
                    } else {
                        crate::browser_debug::set_debug_marker("run_persist:NO_value_to_restore");
                        LoweredProgram::Counter(counter_program)
                    }
                }
                LoweredProgram::TodoMvc(mut todo_program) => {
                    if let Some(restored_todos) = try_restore_todo_list() {
                        crate::browser_debug::set_debug_marker(&format!("run_persist:todo_list_restored:{} items", restored_todos.len()));
                        // Convert restored todos to KernelValue::List
                        let todos_kv: Vec<KernelValue> = restored_todos.iter().map(|(id, title, completed)| {
                            KernelValue::Object(std::collections::BTreeMap::from([
                                ("id".to_string(), KernelValue::Number(*id as f64)),
                                ("title".to_string(), KernelValue::Text(title.clone())),
                                ("completed".to_string(), KernelValue::Bool(*completed)),
                            ]))
                        }).collect();
                        // The TODOS_LIST_HOLD_NODE is NodeId(1430)
                        let todos_hold_node_id = crate::ir::NodeId(1430);
                        // Find the hold node and update its seed with the restored list
                        let mut modified = false;
                        for node in &mut todo_program.ir.nodes {
                            if node.id == todos_hold_node_id {
                                if let crate::ir::IrNodeKind::Hold { seed, .. } = &mut node.kind {
                                    // Find the seed node and update its value
                                    let seed_id = *seed;
                                    if let Some(seed_node) = todo_program.ir.nodes.iter_mut().find(|n| n.id == seed_id) {
                                        if let crate::ir::IrNodeKind::Literal(ref mut val) = seed_node.kind {
                                            *val = KernelValue::List(todos_kv.clone());
                                            modified = true;
                                            crate::browser_debug::set_debug_marker(&format!("run_persist:modified_todos_seed_node:{} items", restored_todos.len()));
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                        if !modified {
                            crate::browser_debug::set_debug_marker("run_persist:NO_todos_seed_modified");
                        }
                        LoweredProgram::TodoMvc(todo_program)
                    } else {
                        crate::browser_debug::set_debug_marker("run_persist:NO_todo_list_to_restore");
                        LoweredProgram::TodoMvc(todo_program)
                    }
                }
                LoweredProgram::ShoppingList(mut shopping_program) => {
                    // Use persistence metadata from the IR to find the hold node to restore
                    let persistence_entries: Vec<_> = shopping_program.ir.persistence.iter()
                        .filter(|p| matches!(p.policy, crate::ir::PersistPolicy::Durable { persist_kind: crate::ir::PersistKind::ListStore, .. }))
                        .cloned()
                        .collect();
                    
                    if !persistence_entries.is_empty() {
                        if let Some(restored_items) = try_restore_shopping_list() {
                            crate::browser_debug::set_debug_marker(&format!("run_persist:shopping_list_restored:{} items", restored_items.len()));
                            let items_kv: Vec<KernelValue> = restored_items.iter()
                                .map(|s| KernelValue::Text(s.clone()))
                                .collect();
                            
                            let mut modified = false;
                            for persist_entry in &persistence_entries {
                                // Find the hold node index
                                let hold_node_idx = shopping_program.ir.nodes.iter()
                                    .position(|n| n.id == persist_entry.node);
                                
                                if let Some(idx) = hold_node_idx {
                                    // Get the seed node ID from the hold node
                                    if let crate::ir::IrNodeKind::Hold { seed, .. } = &shopping_program.ir.nodes[idx].kind {
                                        let seed_id = *seed;
                                        // Find the seed node index
                                        let seed_node_idx = shopping_program.ir.nodes.iter()
                                            .position(|n| n.id == seed_id);
                                        
                                        if let Some(seed_idx) = seed_node_idx {
                                            if let crate::ir::IrNodeKind::Literal(ref mut val) = shopping_program.ir.nodes[seed_idx].kind {
                                                *val = KernelValue::List(items_kv.clone());
                                                modified = true;
                                                crate::browser_debug::set_debug_marker("run_persist:shopping_list_seed_modified");
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            if !modified {
                                crate::browser_debug::set_debug_marker("run_persist:NO_shopping_list_seed_modified");
                            }
                        } else {
                            crate::browser_debug::set_debug_marker("run_persist:NO_shopping_list_to_restore");
                        }
                    } else {
                        crate::browser_debug::set_debug_marker("run_persist:NO_shopping_list_persistence_metadata");
                    }
                    LoweredProgram::ShoppingList(shopping_program)
                }
                other => other,
            };
            // Enable persistence on the preview
            match lowered_preview::LoweredPreview::from_program(program) {
                Ok(preview) => {
                    crate::browser_debug::set_debug_marker("run_actors_lite_with_persistence:preview_with_persistence");
                    lowered_preview::render_lowered_preview(preview.with_persistence()).unify()
                }
                Err(error) => render_dispatch_error(format!("lowered preview: {error}")).unify(),
            }
        }
        Err(error) => {
            crate::browser_debug::set_debug_marker("run_actors_lite_with_persistence:unsupported");
            render_dispatch_error(format!("unsupported source: {error}")).unify()
        }
    }
}

/// Try to restore a persisted counter value from localStorage.
#[cfg(target_arch = "wasm32")]
fn try_restore_counter_value() -> Option<i64> {
    use crate::persist::{PersistedRecord, PersistenceAdapter};
    use crate::persist_browser::BrowserLocalStorage;
    use crate::persistence::json_to_kernel_value;
    use boon::platform::browser::kernel::KernelValue;
    use boon::zoon::web_sys;

    crate::browser_debug::set_debug_marker("restore_counter:start");
    let adapter = BrowserLocalStorage::instance();
    let records = match adapter.load_records() {
        Ok(r) => {
            crate::browser_debug::set_debug_marker(&format!("restore_counter:records:{}", r.len()));
            for rec in &r {
                crate::browser_debug::set_debug_marker(&format!("restore_counter:record:{rec:?}"));
            }
            r
        }
        Err(e) => {
            crate::browser_debug::set_debug_marker(&format!("restore_counter:load_err:{e}"));
            return None;
        }
    };
    for record in records {
        if let PersistedRecord::Hold { value, .. } = record {
            let kernel_value = json_to_kernel_value(&value);
            crate::browser_debug::set_debug_marker(&format!("restore_counter:kernel:{kernel_value:?}"));
            if let KernelValue::Number(n) = kernel_value {
                crate::browser_debug::set_debug_marker(&format!("restore_counter:restored:{n}"));
                return Some(n as i64);
            }
        }
    }
    crate::browser_debug::set_debug_marker("restore_counter:none_found");
    None
}

/// Try to restore a persisted todo list from localStorage.
#[cfg(target_arch = "wasm32")]
fn try_restore_todo_list() -> Option<Vec<(u64, String, bool)>> {
    use crate::persist::{PersistedRecord, PersistenceAdapter};
    use crate::persist_browser::BrowserLocalStorage;
    use crate::persistence::json_to_kernel_value;
    use boon::platform::browser::kernel::KernelValue;

    crate::browser_debug::set_debug_marker("restore_todo_list:start");
    let adapter = BrowserLocalStorage::instance();
    let records = match adapter.load_records() {
        Ok(r) => {
            crate::browser_debug::set_debug_marker(&format!("restore_todo_list:records:{}", r.len()));
            r
        }
        Err(e) => {
            crate::browser_debug::set_debug_marker(&format!("restore_todo_list:load_err:{e}"));
            return None;
        }
    };
    for record in records {
        if let PersistedRecord::Hold { value, .. } = record {
            let kernel_value = json_to_kernel_value(&value);
            crate::browser_debug::set_debug_marker(&format!("restore_todo_list:kernel:{kernel_value:?}"));
            if let KernelValue::List(items) = kernel_value {
                let mut todos = Vec::new();
                for (idx, item) in items.iter().enumerate() {
                    if let KernelValue::Object(fields) = item {
                        let id = fields.get("id")
                            .and_then(|v| match v { KernelValue::Number(n) => Some(*n as u64), _ => None })
                            .unwrap_or((idx + 1) as u64);
                        let title = fields.get("title")
                            .and_then(|v| match v { KernelValue::Text(s) | KernelValue::Tag(s) => Some(s.clone()), _ => None })
                            .unwrap_or_default();
                        let completed = fields.get("completed")
                            .and_then(|v| match v { KernelValue::Bool(b) => Some(*b), _ => None })
                            .unwrap_or(false);
                        todos.push((id, title, completed));
                    }
                }
                if !todos.is_empty() {
                    crate::browser_debug::set_debug_marker(&format!("restore_todo_list:restored:{} items", todos.len()));
                    return Some(todos);
                }
            }
        }
    }
    crate::browser_debug::set_debug_marker("restore_todo_list:none_found");
    None
}

/// Try to restore a persisted shopping list from localStorage.
#[cfg(target_arch = "wasm32")]
fn try_restore_shopping_list() -> Option<Vec<String>> {
    use crate::persist::{PersistedRecord, PersistenceAdapter};
    use crate::persist_browser::BrowserLocalStorage;
    use crate::persistence::json_to_kernel_value;
    use boon::platform::browser::kernel::KernelValue;

    crate::browser_debug::set_debug_marker("restore_shopping_list:start");
    let adapter = BrowserLocalStorage::instance();
    let records = match adapter.load_records() {
        Ok(r) => {
            crate::browser_debug::set_debug_marker(&format!("restore_shopping_list:records:{}", r.len()));
            r
        }
        Err(e) => {
            crate::browser_debug::set_debug_marker(&format!("restore_shopping_list:load_err:{e}"));
            return None;
        }
    };
    for record in records {
        if let PersistedRecord::Hold { value, .. } = record {
            let kernel_value = json_to_kernel_value(&value);
            crate::browser_debug::set_debug_marker(&format!("restore_shopping_list:kernel:{kernel_value:?}"));
            if let KernelValue::List(items) = kernel_value {
                let mut list_items = Vec::new();
                for item in items.iter() {
                    if let KernelValue::Text(s) | KernelValue::Tag(s) = item {
                        list_items.push(s.clone());
                    }
                }
                if !list_items.is_empty() {
                    crate::browser_debug::set_debug_marker(&format!("restore_shopping_list:restored:{} items", list_items.len()));
                    return Some(list_items);
                }
            }
        }
    }
    crate::browser_debug::set_debug_marker("restore_shopping_list:none_found");
    None
}

/// Run ActorsLite without persistence (for non-wasm targets).
#[cfg(not(target_arch = "wasm32"))]
pub fn run_actors_lite_with_persistence(source: &str) -> impl Element {
    run_actors_lite(source)
}

fn render_dispatch_error(message: String) -> impl Element {
    El::new()
        .s(Font::new().color(color!("LightCoral")))
        .child(format!("ActorsLite: {message}"))
}
