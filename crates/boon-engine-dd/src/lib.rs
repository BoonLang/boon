//! Differential Dataflow v2 engine for Boon.
//!
//! Clean rewrite using DD natively: everything is a DD collection.
//! Scalars = single-element collections. Lists = multi-element collections.
//! All computation flows through DD operators.
#![allow(dead_code)]
//!
//! # Architecture
//!
//! - `core/` — Pure DD computation. NO Zoon, web_sys, Mutable, RefCell.
//! - `io/` — Bridges DD ↔ browser. Mutable<T> allowed here.
//! - `render/` — Value descriptors → Zoon UI elements.

pub mod core;
pub mod io;
pub mod render;

// Re-exports for public API
pub use core::types::{InputId, LinkId, ListKey, VarId};
pub use core::value::Value;

use boon_scene::{RenderRootHandle, RenderSurface, SceneHandles};
use std::cell::Cell;
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use zoon::*;

use core::compile::{self, CompiledProgram};

thread_local! {
    /// When true, the DD engine skips saving state to localStorage.
    /// Set by `clear_dd_persisted_states()` to prevent the running program from
    /// re-persisting its in-memory state after localStorage has been cleared.
    /// Reset when a new program starts.
    static SAVE_DISABLED: Cell<bool> = const { Cell::new(false) };
    /// Active JS interval IDs. Cleared when a new program starts.
    static ACTIVE_INTERVALS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    /// Last filename used for DD compilation. Used to detect example switches.
    static LAST_FILENAME: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Clear all active DD engine JS intervals.
/// Uses a JS global (`window.__boon_dd_intervals`) to track interval IDs
/// across WASM hot-reloads (thread_locals are reset on hot-reload).
fn clear_active_intervals() {
    if let Some(window) = web_sys::window() {
        let key = wasm_bindgen::JsValue::from_str("__boon_dd_intervals");
        if let Ok(arr_val) = js_sys::Reflect::get(&window, &key) {
            if let Some(arr) = arr_val.dyn_ref::<js_sys::Array>() {
                let count = arr.length();
                for i in 0..count {
                    if let Some(id) = arr.get(i).as_f64() {
                        window.clear_interval_with_handle(id as i32);
                    }
                }
            }
        }
        // Reset to empty array
        let _ = js_sys::Reflect::set(&window, &key, &js_sys::Array::new());
    }
    // Also clear the WASM-side tracking
    ACTIVE_INTERVALS.with(|ids| ids.borrow_mut().clear());
}

/// Register a JS interval ID for cleanup (both JS global and WASM-side).
fn register_interval(id: i32) {
    ACTIVE_INTERVALS.with(|ids| ids.borrow_mut().push(id));
    // Also store in JS global (survives WASM hot-reload)
    if let Some(window) = web_sys::window() {
        let key = wasm_bindgen::JsValue::from_str("__boon_dd_intervals");
        let arr = match js_sys::Reflect::get(&window, &key) {
            Ok(v) if v.is_instance_of::<js_sys::Array>() => v.unchecked_into::<js_sys::Array>(),
            _ => {
                let a = js_sys::Array::new();
                let _ = js_sys::Reflect::set(&window, &key, &a);
                a
            }
        };
        arr.push(&wasm_bindgen::JsValue::from(id));
    }
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
}

/// A rendered DD document with reactive output.
#[derive(Clone)]
pub struct DdDocument {
    /// The Mutable holding the current document value.
    pub value: Mutable<Value>,
    /// Monotonic revision used to wake render paths even when keyed diffs
    /// arrive without changing the scalar document value.
    pub revision: Mutable<u64>,
    /// Whether the root came from `document` or `scene`.
    pub render_surface: RenderSurface,
    /// Scene-only sub-handles exposed through the shared render-root contract.
    pub scene_lights: Option<Mutable<Value>>,
    pub scene_geometry: Option<Mutable<Value>>,
}

impl DdDocument {
    #[must_use]
    pub const fn is_scene(&self) -> bool {
        self.render_surface.is_scene()
    }

    #[must_use]
    pub fn render_root(&self) -> RenderRootHandle<Mutable<Value>> {
        if self.render_surface.is_scene() {
            RenderRootHandle {
                surface: self.render_surface,
                root: self.value.clone(),
                scene: Some(SceneHandles {
                    lights: self.scene_lights.clone(),
                    geometry: self.scene_geometry.clone(),
                }),
            }
        } else {
            RenderRootHandle::new(self.render_surface, self.value.clone())
        }
    }
}

fn scene_field_value(value: &Value, field: &str) -> Value {
    value.get_field(field).cloned().unwrap_or(Value::Unit)
}

fn build_dd_document(
    value: Mutable<Value>,
    revision: Mutable<u64>,
    render_surface: RenderSurface,
) -> DdDocument {
    let initial = value.get_cloned();
    let (scene_lights, scene_geometry) = if render_surface.is_scene() {
        (
            Some(Mutable::new(scene_field_value(&initial, "lights"))),
            Some(Mutable::new(scene_field_value(&initial, "geometry"))),
        )
    } else {
        (None, None)
    };

    DdDocument {
        value,
        revision,
        render_surface,
        scene_lights,
        scene_geometry,
    }
}

/// DD execution context (timers, accumulators, etc.)
pub struct DdContext {
    has_timers: bool,
}

impl DdContext {}

/// Run Boon source code with the DD engine.
///
/// Parses the source, compiles it, builds a DD dataflow if needed,
/// and returns the result with reactive document output.
///
/// `external_functions` provides pre-parsed functions from other module files
/// for multi-file support. Each entry is (qualified_name, params, body, module_name).
pub fn run_dd_reactive_with_persistence(
    filename: &str,
    source_code: &str,
    states_storage_key: Option<&str>,
    external_functions: Option<&[compile::ExternalFunction]>,
) -> Option<DdResult> {
    // Clean up from previous program
    reset_save_disabled();
    clear_active_intervals();

    // Clear persisted state when switching to a different example file.
    // Different examples share the same storage key, so we must clear
    // stale state to prevent one example's hold data from corrupting another.
    // Only clear if we previously ran a different file (not on first run after page load,
    // since that would break persistence across page reloads).
    let switched_example = LAST_FILENAME.with(|f| {
        let prev = f.borrow().clone();
        let switched = !prev.is_empty() && prev != filename;
        *f.borrow_mut() = filename.to_string();
        switched
    });
    if switched_example {
        clear_dd_persisted_states();
        // Re-enable saving after the clear (clear_dd_persisted_states sets SAVE_DISABLED=true)
        reset_save_disabled();
    }

    // Load persisted hold values from localStorage
    let persisted_holds = if let Some(key) = states_storage_key {
        io::persistence::load_holds_map(key)
    } else {
        std::collections::HashMap::new()
    };

    let compiled = match compile::compile(
        source_code,
        states_storage_key,
        &persisted_holds,
        external_functions,
    ) {
        Ok(program) => program,
        Err(e) => {
            zoon::eprintln!("DD compilation error: {}", e);
            return None;
        }
    };

    match compiled {
        CompiledProgram::Static {
            document_value,
            render_surface,
        } => {
            let output = Mutable::new(document_value);
            let revision = Mutable::new(0);
            Some(DdResult {
                document: Some(build_dd_document(output, revision, render_surface)),
                context: DdContext { has_timers: false },
                worker_handle: None,
            })
        }

        CompiledProgram::Dataflow { graph } => {
            let render_surface = graph.render_surface;
            let has_timers = graph
                .inputs
                .iter()
                .any(|i| i.kind == core::types::InputKind::Timer);

            let has_router = graph
                .inputs
                .iter()
                .any(|i| i.kind == core::types::InputKind::Router);

            // Collect timer specifications before moving graph into worker
            let timer_specs: Vec<(String, f64)> = graph
                .inputs
                .iter()
                .filter(|i| i.kind == core::types::InputKind::Timer)
                .filter_map(|i| {
                    let path = i.link_path.clone()?;
                    let secs = i.timer_interval_secs?;
                    Some((path, secs))
                })
                .collect();

            let output = Mutable::new(Value::Unit);
            let revision = Mutable::new(0);
            let output_for_callback = output.clone();
            let revision_for_callback = revision.clone();

            let worker_handle = io::worker::DdWorkerHandle::new_from_graph(graph, move |value| {
                output_for_callback.set(value.clone());
                revision_for_callback.set(revision_for_callback.get() + 1);
            });

            // Set up JavaScript intervals for timer inputs
            for (var_name, secs) in &timer_specs {
                let handle = worker_handle.clone();
                let name = var_name.clone();
                let millis = (*secs * 1000.0) as i32;
                let closure = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                    handle.inject_dd_event(io::worker::Event::TimerTick {
                        var_name: name.clone(),
                    });
                });
                if let Ok(id) = web_sys::window()
                    .unwrap()
                    .set_interval_with_callback_and_timeout_and_arguments_0(
                        closure.as_ref().unchecked_ref(),
                        millis,
                    )
                {
                    register_interval(id);
                }
                closure.forget(); // Closure must outlive the interval
            }

            // Set up popstate listener for browser back/forward navigation
            if has_router {
                let handle_for_popstate = worker_handle.clone();
                let popstate_closure =
                    wasm_bindgen::closure::Closure::<dyn Fn(web_sys::Event)>::new(
                        move |_event: web_sys::Event| {
                            let path = web_sys::window()
                                .and_then(|w| w.location().pathname().ok())
                                .unwrap_or_else(|| "/".to_string());
                            handle_for_popstate
                                .inject_dd_event(io::worker::Event::RouterChange { path });
                        },
                    );
                let _ = web_sys::window().unwrap().add_event_listener_with_callback(
                    "popstate",
                    popstate_closure.as_ref().unchecked_ref(),
                );
                popstate_closure.forget(); // Listener lives until page unload
            }

            Some(DdResult {
                document: Some(build_dd_document(output, revision, render_surface)),
                context: DdContext { has_timers },
                worker_handle: Some(worker_handle),
            })
        }
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
/// wiring LINK event handlers to the DD worker.
///
/// For all reactive programs, uses the worker rendering path:
/// rebuilds the element tree on each output change.
pub fn render_dd_result_reactive_signal(result: DdResult) -> impl Element {
    let worker = result.worker_handle;
    let document = result.document;

    match document {
        Some(doc) => {
            let render_root = doc.render_root();
            if let Some(ref w) = worker {
                // Dataflow programs: retained tree for efficient updates.
                let ready = Mutable::new(false);
                let root_cell: std::rc::Rc<std::cell::RefCell<Option<RawElOrText>>> =
                    Default::default();
                let retained: std::rc::Rc<
                    std::cell::RefCell<Option<render::bridge::RetainedTree>>,
                > = Default::default();
                let handle = w.clone();
                let revision = doc.revision.clone();

                let _task_handle = Task::start_droppable({
                    let ready = ready.clone();
                    let root_cell = root_cell.clone();
                    let retained = retained.clone();
                    let _scene_lights = render_root
                        .scene
                        .as_ref()
                        .and_then(|scene| scene.lights.clone());
                    let scene_geometry = render_root
                        .scene
                        .as_ref()
                        .and_then(|scene| scene.geometry.clone());
                    let render_surface = render_root.surface;
                    async move {
                        let stream = map_ref! {
                            let value = render_root.root.signal_cloned(),
                            let _revision = revision.signal() => value.clone()
                        }
                        .to_stream();
                        futures_util::pin_mut!(stream);
                        while let Some(value) = stream.next().await {
                            if matches!(&value, Value::Unit) {
                                continue;
                            }
                            #[cfg(target_arch = "wasm32")]
                            if let Some(scene_lights) = &scene_lights {
                                scene_lights.set_neq(scene_field_value(&value, "lights"));
                            }
                            if let Some(scene_geometry) = &scene_geometry {
                                scene_geometry.set_neq(scene_field_value(&value, "geometry"));
                            }
                            let mut ret = retained.borrow_mut();
                            if let Some(tree) = ret.as_mut() {
                                // Update tree first — conditional sections may appear/disappear,
                                // creating or destroying the keyed Stripe.
                                tree.update(&value, &handle);
                                // Then drain and apply keyed diffs to the (now existing) keyed Stripe.
                                let diffs = handle.drain_keyed_diffs();
                                if !diffs.is_empty() {
                                    #[cfg(target_arch = "wasm32")]
                                    zoon::eprintln!("[dd-keyed-diffs] apply {} diffs", diffs.len());
                                    tree.apply_keyed_diffs(&diffs, &handle);
                                }
                            } else {
                                let (element, mut tree) = render::bridge::build_retained_tree(
                                    &value,
                                    &handle,
                                    render_surface,
                                );
                                let diffs = handle.drain_keyed_diffs();
                                if !diffs.is_empty() {
                                    #[cfg(target_arch = "wasm32")]
                                    zoon::eprintln!(
                                        "[dd-keyed-diffs] apply {} diffs on initial render",
                                        diffs.len()
                                    );
                                    tree.apply_keyed_diffs(&diffs, &handle);
                                }
                                *root_cell.borrow_mut() = Some(element);
                                *ret = Some(tree);
                                drop(ret);
                                ready.set_neq(true);
                            }
                        }
                    }
                });

                El::new()
                    .s(Width::fill())
                    .s(Height::fill())
                    .update_raw_el(move |raw_el| raw_el.after_remove(move |_| drop(_task_handle)))
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
                // Static: single render
                El::new().child_signal(
                    render_root
                        .root
                        .signal_cloned()
                        .map(|value| render::bridge::render_value_static(&value)),
                )
            }
        }
        None => El::new().child("DD Engine: No document"),
    }
}

/// Clear persisted DD states (localStorage).
/// Also disables saving so the running program doesn't re-persist its in-memory state.
pub fn clear_dd_persisted_states() {
    SAVE_DISABLED.with(|f| f.set(true));

    if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
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

#[cfg(test)]
mod tests {
    use super::{Value, build_dd_document};
    use boon_scene::RenderSurface;
    use zoon::Mutable;

    #[test]
    fn scene_document_exposes_scene_handles_in_render_root() {
        let document = build_dd_document(
            Mutable::new(Value::tagged(
                "SceneNew",
                [
                    ("root", Value::text("root")),
                    ("lights", Value::text("lights")),
                    ("geometry", Value::text("geometry")),
                ],
            )),
            Mutable::new(0),
            RenderSurface::Scene,
        );

        let render_root = document.render_root();
        assert!(render_root.is_scene());
        let scene = render_root
            .scene
            .as_ref()
            .expect("scene handles should exist");
        assert_eq!(
            scene.lights.as_ref().expect("lights").get_cloned(),
            Value::text("lights")
        );
        assert_eq!(
            scene.geometry.as_ref().expect("geometry").get_cloned(),
            Value::text("geometry")
        );
    }
}
