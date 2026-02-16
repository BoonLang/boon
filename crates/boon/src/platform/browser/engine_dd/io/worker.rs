//! Timely worker lifecycle and DD dataflow execution.
//!
//! This module handles creating the timely worker, building the DD dataflow,
//! and driving the worker loop in response to events.
//!
//! This is in io/ (not core/) because it holds mutable state (Rc<RefCell>)
//! needed to bridge the pure DD dataflow with browser event injection.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use differential_dataflow::input::InputSession;
use timely::communication::allocator::thread::Thread;
use timely::worker::Worker;
use zoon::*;

use super::super::core::runtime;
use super::super::core::types::{
    DataflowGraph, InputId, InputKind, LinkId, ListKey, SideEffectKind,
};
use super::super::core::value::Value;

// ---------------------------------------------------------------------------
// Event types that can be injected into the DD engine
// ---------------------------------------------------------------------------

/// Event types from browser interaction.
///
/// Events are injected into the DD engine via DdWorkerHandle::inject_dd_event().
/// Each event maps to one or more DD InputSessions based on the compiled
/// DataflowGraph's InputSpec entries.
#[derive(Clone, Debug)]
pub enum Event {
    LinkPress { link_path: String },
    LinkClick { link_path: String },
    KeyDown { link_path: String, key: String },
    TextChange { link_path: String, text: String },
    Blur { link_path: String },
    Focus { link_path: String },
    DoubleClick { link_path: String },
    HoverChange { link_path: String, hovered: bool },
    TimerTick { var_name: String },
    RouterChange { path: String },
}

// ---------------------------------------------------------------------------
// DdWorkerHandle — the IO bridge for DD engine
// ---------------------------------------------------------------------------

/// Handle to the running DD engine. Stored in the IO layer.
///
/// Uses Rc<RefCell<>> for interior mutability — this is allowed in io/
/// but NOT in core/ (anti-cheat boundary).
#[derive(Clone)]
pub struct DdWorkerHandle {
    inner: Rc<RefCell<DdWorkerInner>>,
    /// Side-effect buffer — separate from inner to avoid borrow conflicts
    /// during worker.step() (inspect callbacks fire while inner is borrowed).
    side_effect_buffer: Rc<RefCell<Vec<(SideEffectKind, Value)>>>,
}

struct DdWorkerInner {
    worker: Worker<Thread>,
    inputs: HashMap<InputId, InputSession<u64, Value, isize>>,
    /// Maps link paths to InputIds for event routing.
    link_path_to_input: HashMap<String, InputId>,
    epoch: u64,
    /// Output cell written by DD inspect callback during worker.step().
    output_cell: Rc<RefCell<Value>>,
    /// Last value we notified about (to detect changes).
    last_notified: Value,
    /// Callback to notify when output changes.
    on_output_change: Option<Box<dyn Fn(&Value)>>,
    /// Literal sessions must be kept alive for the dataflow to work.
    _literal_sessions: Vec<InputSession<u64, Value, isize>>,
    _literal_sessions_keyed: Vec<InputSession<u64, (ListKey, Value), isize>>,
}

impl DdWorkerInner {
    /// Check if output changed after a worker.step() and notify.
    fn notify_if_changed(&mut self) {
        let current = self.output_cell.borrow().clone();
        if current != self.last_notified {
            self.last_notified = current.clone();
            if let Some(ref cb) = self.on_output_change {
                cb(&current);
            }
        }
    }
}

impl DdWorkerHandle {
    /// Create a DD worker from a compiled DataflowGraph.
    ///
    /// This is the primary constructor for DD3. The graph specifies all
    /// collections and operators; this method materializes them into a
    /// live DD dataflow and returns a handle for event injection.
    pub fn new_from_graph(
        graph: DataflowGraph,
        on_output_change: impl Fn(&Value) + 'static,
    ) -> Self {
        let alloc = Thread::default();
        let mut worker = Worker::new(Default::default(), alloc, None);
        let output_cell: Rc<RefCell<Value>> = Rc::new(RefCell::new(Value::Unit));
        let output_for_inspect = output_cell.clone();

        // Side-effect buffer (separate Rc for borrow safety)
        let side_effect_buffer: Rc<RefCell<Vec<(SideEffectKind, Value)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let se_buffer_for_inspect = side_effect_buffer.clone();
        let on_side_effect: Arc<dyn Fn(&SideEffectKind, &Value) + 'static> =
            Arc::new(move |kind: &SideEffectKind, value: &Value| {
                se_buffer_for_inspect
                    .borrow_mut()
                    .push((kind.clone(), value.clone()));
            });

        // Build link_path → InputId mapping from graph inputs
        let mut link_path_to_input: HashMap<String, InputId> = HashMap::new();
        let mut has_router_input = false;
        for input_spec in &graph.inputs {
            if let Some(ref path) = input_spec.link_path {
                link_path_to_input.insert(path.clone(), input_spec.id);
            }
            if input_spec.kind == InputKind::Router {
                has_router_input = true;
            }
        }

        // Materialize the dataflow inside the worker
        let materialized = worker.dataflow::<u64, _, _>(|scope| {
            runtime::materialize(
                &graph,
                scope,
                move |value, _time, diff| {
                    if *diff > 0 {
                        *output_for_inspect.borrow_mut() = value.clone();
                    }
                },
                on_side_effect,
            )
        });

        // Extract sessions from materialized graph
        let input_sessions = materialized.input_sessions;
        let mut literal_sessions = materialized.literal_sessions;
        let mut literal_sessions_keyed = materialized.literal_sessions_keyed;

        // Advance ALL sessions (literals + event inputs) past initial epoch.
        // Timely requires all input frontiers to advance before data can propagate.
        for session in literal_sessions.iter_mut() {
            session.advance_to(1);
            session.flush();
        }
        for session in literal_sessions_keyed.iter_mut() {
            session.advance_to(1);
            session.flush();
        }

        // Build inner state
        let mut inner = DdWorkerInner {
            worker,
            inputs: input_sessions,
            link_path_to_input,
            epoch: 0,
            output_cell: output_cell.clone(),
            last_notified: Value::Unit,
            on_output_change: Some(Box::new(on_output_change)),
            _literal_sessions: literal_sessions,
            _literal_sessions_keyed: literal_sessions_keyed,
        };

        // Advance event input sessions past initial epoch
        for session in inner.inputs.values_mut() {
            session.advance_to(1);
            session.flush();
        }
        inner.epoch = 1;

        // Inject initial route for Router inputs
        if has_router_input {
            if let Some(input_id) = inner.link_path_to_input.get("__router") {
                let path = web_sys::window()
                    .and_then(|w| w.location().pathname().ok())
                    .unwrap_or_else(|| "/".to_string());
                if let Some(session) = inner.inputs.get_mut(input_id) {
                    session.update(Value::text(path), 1);
                    session.flush();
                }
            }
        }

        // Step to propagate initial data through the dataflow chain.
        // Multiple steps may be needed for deeply chained operators.
        for _ in 0..10 {
            inner.worker.step();
        }

        // Read initial output and notify
        let initial_output = inner.output_cell.borrow().clone();
        inner.last_notified = initial_output.clone();
        if let Some(ref cb) = inner.on_output_change {
            cb(&initial_output);
        }

        let handle = DdWorkerHandle {
            inner: Rc::new(RefCell::new(inner)),
            side_effect_buffer,
        };

        // Process any side effects from the initial step (e.g., persist initial hold values)
        handle.process_side_effects();

        handle
    }

    /// Inject an event and step the DD worker.
    ///
    /// Maps the event to the appropriate InputSession based on the
    /// compiled DataflowGraph's InputSpec entries, then steps the worker.
    pub fn inject_dd_event(&self, event: Event) {
        let (link_path, event_value) = match event {
            Event::LinkPress { link_path } => {
                (link_path, Value::tag("Press"))
            }
            Event::LinkClick { link_path } => {
                (link_path, Value::tag("Click"))
            }
            Event::KeyDown { link_path, key } => {
                (link_path, Value::text(key))
            }
            Event::TextChange { link_path, text } => {
                (link_path, Value::text(text))
            }
            Event::Blur { link_path } => {
                (link_path, Value::tag("Blur"))
            }
            Event::Focus { link_path } => {
                (link_path, Value::tag("Focus"))
            }
            Event::DoubleClick { link_path } => {
                (link_path, Value::tag("DoubleClick"))
            }
            Event::HoverChange { link_path, hovered } => {
                (link_path, Value::bool(hovered))
            }
            Event::TimerTick { var_name } => {
                (var_name, Value::tag("Tick"))
            }
            Event::RouterChange { path } => {
                // Router events map to the __router input with the route text as value
                ("__router".to_string(), Value::text(path))
            }
        };

        {
            let mut inner = self.inner.borrow_mut();

            let input_id = match inner.link_path_to_input.get(&link_path) {
                Some(id) => *id,
                None => {
                    // Fallback: route to __wildcard input if registered.
                    // This handles per-item events with dynamic paths (e.g., todo items).
                    // The wildcard event includes the full path so the transform can parse it.
                    match inner.link_path_to_input.get("__wildcard") {
                        Some(id) => {
                            let wildcard_id = *id;
                            inner.epoch += 1;
                            let epoch = inner.epoch;
                            // Send tagged event with full path
                            let tagged = Value::object([
                                ("path", Value::text(link_path.as_str())),
                                ("value", event_value),
                            ]);
                            if let Some(session) = inner.inputs.get_mut(&wildcard_id) {
                                session.update(tagged, 1);
                                session.advance_to(epoch);
                                session.flush();
                            }
                            inner.worker.step();
                            inner.notify_if_changed();
                            drop(inner);
                            self.process_side_effects();
                            return;
                        }
                        None => {
                            return;
                        }
                    }
                }
            };

            inner.epoch += 1;
            let epoch = inner.epoch;

            if let Some(session) = inner.inputs.get_mut(&input_id) {
                session.update(event_value, 1);
                session.advance_to(epoch);
                session.flush();
            }

            inner.worker.step();
            inner.notify_if_changed();
        }
        // inner borrow released — now process side effects
        self.process_side_effects();
    }

    /// Process buffered side effects after a worker.step().
    fn process_side_effects(&self) {
        let effects: Vec<_> = self.side_effect_buffer.borrow_mut().drain(..).collect();
        for (kind, value) in effects {
            match kind {
                SideEffectKind::PersistHold { ref key, ref hold_name } => {
                    super::persistence::save_hold_state(key, hold_name, &value);
                }
                SideEffectKind::RouterGoTo => {
                    if let Some(route) = value.as_text() {
                        // Push URL state
                        if let Some(window) = web_sys::window() {
                            if let Ok(history) = window.history() {
                                let _ = history.push_state_with_url(
                                    &wasm_bindgen::JsValue::NULL,
                                    "",
                                    Some(route),
                                );
                            }
                        }
                        // Inject route change into the Router input
                        self.inject_dd_event(Event::RouterChange {
                            path: route.to_string(),
                        });
                    }
                }
            }
        }
    }

    /// Inject an event for a LINK by LinkId and step the DD worker.
    /// (Legacy API used by bridge.rs render_value path)
    pub fn inject_event(&self, link_id: &LinkId, event_value: Value) {
        let link_path = link_id.as_str().to_string();
        {
            let mut inner = self.inner.borrow_mut();

            let input_id = match inner.link_path_to_input.get(&link_path) {
                Some(id) => *id,
                None => {
                    zoon::eprintln!("[DD] Unknown link: {}", link_path);
                    return;
                }
            };

            inner.epoch += 1;
            let epoch = inner.epoch;

            if let Some(session) = inner.inputs.get_mut(&input_id) {
                session.update(event_value, 1);
                session.advance_to(epoch);
                session.flush();
            }

            inner.worker.step();
            inner.notify_if_changed();
        }
        self.process_side_effects();
    }

    /// Get the current output value.
    pub fn current_output(&self) -> Value {
        self.inner.borrow().output_cell.borrow().clone()
    }

    /// Get a clone of the handle for sharing with event handlers.
    pub fn clone_ref(&self) -> DdWorkerHandle {
        self.clone()
    }
}
