//! Differential Dataflow v2 engine for Boon.
//!
//! Clean rewrite using DD natively: everything is a DD collection.
//! Scalars = single-element collections. Lists = multi-element collections.
//! All computation flows through DD operators.
//!
//! # Architecture
//!
//! - `core/` — Pure DD computation. NO Zoon, web_sys, Mutable, RefCell.
//! - `io/` — Bridges DD ↔ browser. Mutable<T> allowed here.
//! - `render/` — Value descriptors → Zoon elements.

pub mod core;
pub mod io;
pub mod render;

// Re-exports for public API
pub use core::types::{InputId, LinkId, ListKey, VarId};
pub use core::value::Value;

use std::cell::Cell;
use zoon::*;

use core::compile::{self, CompiledProgram};

thread_local! {
    /// When true, the DD general interpreter skips saving state to localStorage.
    /// Set by `clear_dd_persisted_states()` to prevent the running program from
    /// re-persisting its in-memory state after localStorage has been cleared.
    /// Reset when a new program starts.
    static SAVE_DISABLED: Cell<bool> = const { Cell::new(false) };
}

/// Check if state saving is currently disabled.
pub fn is_save_disabled() -> bool {
    SAVE_DISABLED.with(|f| f.get())
}

/// Reset the save-disabled flag (called when a new program starts).
pub fn reset_save_disabled() {
    SAVE_DISABLED.with(|f| f.set(false));
}

/// Result of running DD engine on Boon source code.
pub struct DdResult {
    pub document: Option<DdDocument>,
    pub context: DdContext,
    worker_handle: Option<io::worker::DdWorkerHandle>,
    general_handle: Option<io::general::GeneralHandle>,
}

/// A rendered DD document with reactive output.
#[derive(Clone)]
pub struct DdDocument {
    /// The Mutable holding the current document value.
    pub value: Mutable<Value>,
}

/// DD execution context (timers, accumulators, etc.)
pub struct DdContext {
    has_timers: bool,
}

impl DdContext {
    pub fn get_timers(&self) -> &[()] {
        // Return non-empty slice if has_timers to trigger timer rendering path
        if self.has_timers {
            &[()]
        } else {
            &[]
        }
    }
    pub fn has_sum_accumulators(&self) -> bool {
        false
    }
}

/// Run Boon source code with the DD engine.
///
/// Parses the source, compiles it, builds a DD dataflow if needed,
/// and returns the result with reactive document output.
pub fn run_dd_reactive_with_persistence(
    _filename: &str,
    source_code: &str,
    states_storage_key: Option<&str>,
) -> Option<DdResult> {
    // Re-enable saving for the new program
    reset_save_disabled();

    zoon::println!("[DD v2] Compiling...");

    let compiled = match compile::compile(source_code) {
        Ok(program) => program,
        Err(e) => {
            zoon::eprintln!("[DD v2] Compilation error: {}", e);
            return None;
        }
    };

    match compiled {
        CompiledProgram::Static { document_value } => {
            zoon::println!("[DD v2] Static program");
            let output = Mutable::new(document_value);
            Some(DdResult {
                document: Some(DdDocument { value: output }),
                context: DdContext { has_timers: false },
                worker_handle: None,
                general_handle: None,
            })
        }

        CompiledProgram::SingleHold {
            initial_value,
            hold_transform,
            build_document,
            link_bindings,
        } => {
            zoon::println!("[DD v2] Reactive program (SingleHold)");

            // Load persisted state if available
            let actual_initial = if let Some(key) = states_storage_key {
                load_hold_state(key, "counter").unwrap_or(initial_value.clone())
            } else {
                initial_value.clone()
            };

            let initial_doc = build_document(&actual_initial);
            let output = Mutable::new(initial_doc);
            let output_for_callback = output.clone();
            let build_doc = build_document.clone();
            let storage_key = states_storage_key.map(|s| s.to_string());

            let worker_handle =
                io::worker::DdWorkerHandle::new_single_hold(
                    actual_initial,
                    hold_transform,
                    &link_bindings,
                    move |value| {
                        let doc = build_doc(value);
                        output_for_callback.set(doc);
                        // Persist state
                        if let Some(ref key) = storage_key {
                            save_hold_state(key, "counter", value);
                        }
                    },
                );

            Some(DdResult {
                document: Some(DdDocument { value: output }),
                context: DdContext { has_timers: false },
                worker_handle: Some(worker_handle),
                general_handle: None,
            })
        }

        CompiledProgram::LatestSum {
            build_document,
            link_bindings,
        } => {
            zoon::println!("[DD v2] Reactive program (LatestSum)");

            // Load persisted state if available
            let actual_initial = if let Some(key) = states_storage_key {
                load_hold_state(key, "counter")
                    .and_then(|v| v.as_number().map(Value::number))
                    .unwrap_or_else(|| Value::number(0.0))
            } else {
                Value::number(0.0)
            };

            let initial_doc = build_document(&actual_initial);
            let output = Mutable::new(initial_doc);
            let output_for_callback = output.clone();
            let build_doc = build_document.clone();
            let storage_key = states_storage_key.map(|s| s.to_string());

            let initial_sum = actual_initial.as_number().unwrap_or(0.0);
            let worker_handle =
                io::worker::DdWorkerHandle::new_latest_sum(
                    initial_sum,
                    &link_bindings,
                    move |value| {
                        let doc = build_doc(value);
                        output_for_callback.set(doc);
                        // Persist state
                        if let Some(ref key) = storage_key {
                            save_hold_state(key, "counter", value);
                        }
                    },
                );

            Some(DdResult {
                document: Some(DdDocument { value: output }),
                context: DdContext { has_timers: false },
                worker_handle: Some(worker_handle),
                general_handle: None,
            })
        }

        CompiledProgram::General {
            variables,
            functions,
        } => {
            zoon::println!("[DD v2] General reactive program");

            let has_timers = variables.iter().any(|(_, expr)| {
                contains_timer(expr)
            });

            let output = Mutable::new(Value::Unit);
            let general_handle = io::general::GeneralHandle::new(
                variables,
                functions,
                output.clone(),
                states_storage_key,
            );

            Some(DdResult {
                document: Some(DdDocument { value: output }),
                context: DdContext { has_timers },
                worker_handle: None,
                general_handle: Some(general_handle),
            })
        }
    }
}

fn contains_timer(expr: &crate::parser::static_expression::Spanned<crate::parser::static_expression::Expression>) -> bool {
    use crate::parser::static_expression::Expression;
    match &expr.node {
        Expression::FunctionCall { path, .. } => {
            let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            path_strs.as_slice() == &["Timer", "interval"]
        }
        Expression::Pipe { from, to } => contains_timer(from) || contains_timer(to),
        Expression::Variable(var) => contains_timer(&var.value),
        Expression::Object(obj) => obj.variables.iter().any(|v| contains_timer(&v.node.value)),
        Expression::Block { variables, output, .. } => {
            variables.iter().any(|v| contains_timer(&v.node.value)) || contains_timer(output)
        }
        _ => false,
    }
}

/// Render DD document as a reactive Zoon element (simple text).
pub fn render_dd_document_reactive_signal(
    document: DdDocument,
    _context: DdContext,
) -> impl Element {
    El::new().child_signal(
        document
            .value
            .signal_cloned()
            .map(|v| v.to_display_string()),
    )
}

/// Render full DD result as a reactive Zoon element.
///
/// Builds the complete UI from the document Value descriptor,
/// wiring LINK event handlers to the DD worker or general handler.
///
/// For General programs, builds a retained element tree once and updates
/// only changed Mutables on state changes (efficient incremental DOM updates).
/// For Worker/Static programs, rebuilds the element tree on each change.
pub fn render_dd_result_reactive_signal(result: DdResult) -> impl Element {
    zoon::println!("[DD v2] render_dd_result_reactive_signal called");
    let worker = result.worker_handle;
    let general = result.general_handle;
    let document = result.document;

    match document {
        Some(doc) => {
            if let Some(handle) = general {
                // General programs: retained tree for efficient updates.
                // Build the element tree once, then diff-update only changed Mutables.
                let ready = Mutable::new(false);
                let root_cell: std::rc::Rc<
                    std::cell::RefCell<Option<RawElOrText>>,
                > = Default::default();
                let retained: std::rc::Rc<
                    std::cell::RefCell<Option<render::bridge::RetainedTree>>,
                > = Default::default();

                // Use Task::start_droppable so the async loop is cancelled
                // when the element is removed from the DOM (example switch, re-run).
                let _task_handle = Task::start_droppable({
                    let ready = ready.clone();
                    let root_cell = root_cell.clone();
                    let retained = retained.clone();
                    async move {
                        let stream = doc.value.signal_cloned().to_stream();
                        futures_util::pin_mut!(stream);
                        while let Some(value) = stream.next().await {
                            if matches!(&value, Value::Unit) {
                                continue;
                            }
                            let mut ret = retained.borrow_mut();
                            if let Some(tree) = ret.as_mut() {
                                tree.update(&value, &handle);
                            } else {
                                zoon::println!("[DD v2] Building retained tree");
                                let (element, tree) =
                                    render::bridge::build_retained_tree(&value, &handle);
                                *root_cell.borrow_mut() = Some(element);
                                *ret = Some(tree);
                                drop(ret);
                                ready.set_neq(true);
                            }
                        }
                    }
                });

                // Store the task handle on the element so dropping the element
                // cancels the async loop and cleans up the retained tree + timers.
                El::new()
                    .s(Width::fill())
                    .s(Height::fill())
                    .update_raw_el(move |raw_el| {
                        raw_el.after_remove(move |_| drop(_task_handle))
                    })
                    .child_signal(ready.signal().map({
                        let root_cell = root_cell.clone();
                        move |is_ready| {
                            if is_ready {
                                root_cell.borrow_mut().take()
                            } else {
                                None
                            }
                        }
                    }))
            } else {
                // Worker/Static: rebuild on every change
                El::new().child_signal(doc.value.signal_cloned().map(move |value| {
                    if let Some(ref w) = worker {
                        render::bridge::render_value(&value, w)
                    } else {
                        render::bridge::render_value_static(&value)
                    }
                }))
            }
        }
        None => El::new().child("DD Engine: No document"),
    }
}

/// Clear persisted DD states (localStorage).
/// Also disables saving so the running program doesn't re-persist its in-memory state.
pub fn clear_dd_persisted_states() {
    // Disable saving first so in-flight events don't re-persist
    SAVE_DISABLED.with(|f| f.set(true));

    if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
        // Clear all dd_ prefixed keys
        let len = storage.length().unwrap_or(0);
        let mut keys_to_remove = Vec::new();
        for i in 0..len {
            if let Ok(Some(key)) = storage.key(i) {
                if key.starts_with("dd_") {
                    keys_to_remove.push(key);
                }
            }
        }
        for key in keys_to_remove {
            let _ = storage.remove_item(&key);
        }
    }
}

/// Clear in-memory cell states.
pub fn clear_cells_memory() {
    // General interpreter state is dropped when the program is re-run
}

// ---------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------

fn save_hold_state(storage_key: &str, hold_name: &str, value: &Value) {
    if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
        if let Ok(json) = serde_json::to_string(value) {
            let key = format!("dd_{}_{}", storage_key, hold_name);
            let _ = storage.set_item(&key, &json);
        }
    }
}

fn load_hold_state(storage_key: &str, hold_name: &str) -> Option<Value> {
    let storage = web_sys::window()?.local_storage().ok()??;
    let key = format!("dd_{}_{}", storage_key, hold_name);
    let json = storage.get_item(&key).ok()??;
    serde_json::from_str(&json).ok()
}
