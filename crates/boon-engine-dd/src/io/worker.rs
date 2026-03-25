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
    DataflowGraph, InputId, InputKind, KeyedDiff, LIST_TAG, LinkId, ListKey, ROUTER_INPUT,
    SideEffectKind,
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
    LinkPress {
        link_path: String,
    },
    LinkClick {
        link_path: String,
    },
    KeyDown {
        link_path: String,
        key: String,
        text: String,
    },
    TextChange {
        link_path: String,
        text: String,
    },
    NumberChange {
        link_path: String,
        value: f64,
    },
    Blur {
        link_path: String,
    },
    Focus {
        link_path: String,
    },
    DoubleClick {
        link_path: String,
    },
    SvgClick {
        link_path: String,
        x: f64,
        y: f64,
    },
    HoverChange {
        link_path: String,
        hovered: bool,
    },
    TimerTick {
        var_name: String,
    },
    RouterChange {
        path: String,
    },
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
    /// Keyed diff buffer for bridge display (element Values from display_var).
    keyed_diff_buffer: Rc<RefCell<Vec<KeyedDiff>>>,
    /// Keyed diff buffer for persistence (raw data from persistence_var).
    keyed_persist_buffer: Rc<RefCell<Vec<KeyedDiff>>>,
    /// Keyed persistence state — HashMap maintained from persistence diffs.
    /// Updated after each step, serialized to localStorage.
    keyed_persistence: Rc<RefCell<Option<KeyedPersistenceState>>>,
    /// Whether the graph has a keyed list output configured.
    has_keyed_output: bool,
    /// Element tag of the Stripe that displays keyed list items (e.g., "Ul").
    keyed_stripe_element_tag: Option<String>,
}

/// State for keyed list persistence in the IO layer.
struct KeyedPersistenceState {
    items: HashMap<ListKey, Value>,
    storage_key: String,
    hold_name: String,
    dirty: bool,
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
    fn drive_until_settled(&mut self) {
        // DD pipelines in this repo often require several worker steps for an
        // event to reach keyed list diffs and the assembled document output.
        for _ in 0..1000 {
            self.worker.step();
        }
    }

    /// Check if output changed after a worker.step() and notify.
    /// When `force` is true, notify even if the document value hasn't changed
    /// (used when keyed diffs arrive without scalar changes).
    fn notify_if_changed(&mut self, force: bool) {
        let current = self.output_cell.borrow().clone();
        if current != self.last_notified || force {
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

        // Keyed diff buffer for bridge display (element Values from display_var)
        let keyed_diff_buffer: Rc<RefCell<Vec<KeyedDiff>>> = Rc::new(RefCell::new(Vec::new()));

        // Build keyed diff callback for bridge display
        let has_keyed_output = graph.keyed_list_output.is_some();
        let keyed_stripe_element_tag = graph
            .keyed_list_output
            .as_ref()
            .and_then(|klo| klo.element_tag.clone());
        let kd_buffer_for_inspect = keyed_diff_buffer.clone();
        let on_keyed_diff: Option<Arc<dyn Fn(KeyedDiff) + 'static>> = if has_keyed_output {
            Some(Arc::new(move |diff: KeyedDiff| {
                kd_buffer_for_inspect.borrow_mut().push(diff);
            }))
        } else {
            None
        };

        // Keyed diff buffer for persistence (raw data from persistence_var)
        let keyed_persist_buffer: Rc<RefCell<Vec<KeyedDiff>>> = Rc::new(RefCell::new(Vec::new()));
        let kp_buffer_for_inspect = keyed_persist_buffer.clone();
        let on_keyed_persist: Option<Arc<dyn Fn(KeyedDiff) + 'static>> = if has_keyed_output {
            Some(Arc::new(move |diff: KeyedDiff| {
                kp_buffer_for_inspect.borrow_mut().push(diff);
            }))
        } else {
            None
        };

        // Set up keyed persistence state if applicable
        let keyed_persistence: Rc<RefCell<Option<KeyedPersistenceState>>> =
            Rc::new(RefCell::new(None));
        if let Some(ref keyed_output) = graph.keyed_list_output {
            if let (Some(sk), Some(hn)) = (&keyed_output.storage_key, &keyed_output.hold_name) {
                // Initialize from persisted state (load existing items)
                let persisted_items = super::persistence::load_hold_state(sk, hn)
                    .map(|v| {
                        if let Value::Tagged {
                            ref tag,
                            ref fields,
                        } = v
                        {
                            if tag.as_ref() == LIST_TAG {
                                return fields
                                    .iter()
                                    .map(|(k, v)| (ListKey::new(k.as_ref()), v.clone()))
                                    .collect();
                            }
                        }
                        HashMap::new()
                    })
                    .unwrap_or_default();
                *keyed_persistence.borrow_mut() = Some(KeyedPersistenceState {
                    items: persisted_items,
                    storage_key: sk.clone(),
                    hold_name: hn.clone(),
                    dirty: false,
                });
            }
        }

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
                on_keyed_diff,
                on_keyed_persist,
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

        // Inject initial route for Router inputs at epoch 0 (BEFORE advancing).
        // This ensures Router-derived collections have data at the same timestamp
        // as LiteralList/KeyedHoldState initial data, so DD joins can match them.
        if has_router_input {
            if let Some(input_id) = inner.link_path_to_input.get(ROUTER_INPUT) {
                let path = web_sys::window()
                    .and_then(|w| w.location().pathname().ok())
                    .unwrap_or_else(|| "/".to_string());
                if let Some(session) = inner.inputs.get_mut(input_id) {
                    session.update(Value::text(path), 1);
                    session.flush();
                }
            }
        }

        // Advance event input sessions past initial epoch
        for session in inner.inputs.values_mut() {
            session.advance_to(1);
            session.flush();
        }
        inner.epoch = 1;

        // Step to propagate initial data through the dataflow chain.
        // Multiple steps may be needed for deeply chained operators.
        inner.drive_until_settled();

        // Read initial output and notify
        let initial_output = inner.output_cell.borrow().clone();
        inner.last_notified = initial_output.clone();
        if let Some(ref cb) = inner.on_output_change {
            cb(&initial_output);
        }

        let handle = DdWorkerHandle {
            inner: Rc::new(RefCell::new(inner)),
            side_effect_buffer,
            keyed_diff_buffer,
            keyed_persist_buffer,
            keyed_persistence,
            has_keyed_output,
            keyed_stripe_element_tag,
        };

        // Process any side effects from the initial step (e.g., persist initial hold values)
        handle.process_side_effects();
        // Process initial keyed diffs (persist initial list state)
        handle.process_keyed_persistence();

        handle
    }

    /// Drain keyed diffs accumulated during the last worker.step().
    /// Returns the diffs for bridge consumption.
    pub fn drain_keyed_diffs(&self) -> Vec<KeyedDiff> {
        self.keyed_diff_buffer.borrow_mut().drain(..).collect()
    }

    /// Check if this worker has a keyed list output configured.
    pub fn has_keyed_list(&self) -> bool {
        self.has_keyed_output
    }

    /// Get the element tag of the Stripe that displays keyed list items.
    pub fn keyed_stripe_element_tag(&self) -> Option<&str> {
        self.keyed_stripe_element_tag.as_deref()
    }

    /// Inject an event and step the DD worker.
    ///
    /// Maps the event to the appropriate InputSession based on the
    /// compiled DataflowGraph's InputSpec entries, then steps the worker.
    pub fn inject_dd_event(&self, event: Event) {
        let (link_path, event_value) = match event {
            Event::LinkPress { link_path } => normalize_dd_link_press(link_path),
            Event::LinkClick { link_path } => (link_path, Value::tag("Click")),
            Event::KeyDown {
                link_path,
                key,
                text,
            } => (
                link_path,
                Value::object([
                    ("key", Value::tag(key.as_str())),
                    ("text", Value::text(text)),
                ]),
            ),
            Event::TextChange { link_path, text } => (
                link_path,
                Value::object([
                    ("text", Value::text(text.clone())),
                    ("value", Value::text(text)),
                ]),
            ),
            Event::NumberChange { link_path, value } => {
                (link_path, Value::object([("value", Value::number(value))]))
            }
            Event::SvgClick { link_path, x, y } => (
                link_path,
                Value::object([("x", Value::number(x)), ("y", Value::number(y))]),
            ),
            Event::Blur { link_path } => (link_path, Value::tag("Blur")),
            Event::Focus { link_path } => (link_path, Value::tag("Focus")),
            Event::DoubleClick { link_path } => (link_path, Value::tag("DoubleClick")),
            Event::HoverChange { link_path, hovered } => (link_path, Value::bool(hovered)),
            Event::TimerTick { var_name } => (var_name, Value::tag("Tick")),
            Event::RouterChange { path } => {
                // Router events map to the __router input with the route text as value
                (ROUTER_INPUT.to_string(), Value::text(path))
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
                            }
                            for session in inner.inputs.values_mut() {
                                session.advance_to(epoch);
                                session.flush();
                            }
                            inner.drive_until_settled();
                            let has_keyed = !self.keyed_diff_buffer.borrow().is_empty();
                            inner.notify_if_changed(has_keyed);
                            drop(inner);
                            self.process_side_effects();
                            self.process_keyed_persistence();
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

            // Insert event into the target input
            if let Some(session) = inner.inputs.get_mut(&input_id) {
                session.update(event_value, 1);
            }

            // Advance ALL input sessions to the new epoch.
            // Timely requires all input frontiers to advance past a timestamp
            // before data at that timestamp can propagate through operators.
            for session in inner.inputs.values_mut() {
                session.advance_to(epoch);
                session.flush();
            }

            inner.drive_until_settled();
            let has_keyed = !self.keyed_diff_buffer.borrow().is_empty();
            inner.notify_if_changed(has_keyed);
        }
        // inner borrow released — now process side effects
        self.process_side_effects();
        self.process_keyed_persistence();
    }

    /// Process buffered side effects after a worker.step().
    fn process_side_effects(&self) {
        let effects: Vec<_> = self.side_effect_buffer.borrow_mut().drain(..).collect();
        for (kind, value) in effects {
            match kind {
                SideEffectKind::PersistHold {
                    ref key,
                    ref hold_name,
                } => {
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

    /// Process keyed persistence diffs (raw data from persistence_var).
    /// Updates the IO-layer HashMap from buffered diffs, then saves to localStorage.
    fn process_keyed_persistence(&self) {
        let diffs: Vec<KeyedDiff> = self.keyed_persist_buffer.borrow_mut().drain(..).collect();
        if diffs.is_empty() {
            return;
        }

        let mut persistence = self.keyed_persistence.borrow_mut();
        if let Some(ref mut state) = *persistence {
            for diff in &diffs {
                match diff {
                    KeyedDiff::Upsert { key, value } => {
                        state.items.insert(key.clone(), value.clone());
                        state.dirty = true;
                    }
                    KeyedDiff::Remove { key } => {
                        state.items.remove(key);
                        state.dirty = true;
                    }
                }
            }
            if state.dirty {
                super::persistence::save_keyed_list(
                    &state.storage_key,
                    &state.hold_name,
                    &state.items,
                );
                state.dirty = false;
            }
        }
    }

    /// Inject an event for a LINK by LinkId and step the DD worker.
    pub fn inject_event(&self, link_id: &LinkId, event_value: Value) {
        let link_path = link_id.as_str().to_string();
        {
            let mut inner = self.inner.borrow_mut();

            let input_id = match inner.link_path_to_input.get(&link_path) {
                Some(id) => *id,
                None => {
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

            inner.drive_until_settled();
            let has_keyed = !self.keyed_diff_buffer.borrow().is_empty();
            inner.notify_if_changed(has_keyed);
        }
        self.process_side_effects();
        self.process_keyed_persistence();
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

fn normalize_dd_link_press(link_path: String) -> (String, Value) {
    if link_path.ends_with(".event.press") && link_path.contains("checkbox") {
        let base = link_path
            .strip_suffix(".event.press")
            .unwrap_or(link_path.as_str())
            .to_string();
        (base, Value::tag("Click"))
    } else {
        (link_path, Value::tag("Press"))
    }
}

#[cfg(test)]
mod tests {
    use super::{DdWorkerHandle, Event};
    use crate::core::compile::{CompiledProgram, compile};
    use crate::core::types::KeyedDiff;
    use crate::core::value::Value;
    use crate::render::bridge::build_retained_tree;
    use boon::platform::browser::kernel::{
        ExprId, KernelValue, LatestCandidate, Runtime as KernelRuntime, RuntimeUpdate, ScopeId,
        SlotKey, TickId, TickSeq, Trigger, select_latest,
    };
    use boon_scene::RenderSurface;
    use std::path::PathBuf;

    fn read_example(path: &str) -> String {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(base.join(path)).expect("read example source")
    }

    fn latest_conformance_source() -> &'static str {
        r#"
left_button: LINK
right_button: LINK

selected: LATEST {
    left_button.event.press |> THEN { TEXT { left } }
    right_button.event.press |> THEN { TEXT { right } }
}

document: Document/new(root:
    Element/stripe(
        element: []
        direction: Column
        gap: 0
        style: []
        items: LIST {
            Element/button(
                element: [event: [press: LINK]]
                label: TEXT { Left }
                style: []
            ) |> LINK { left_button }
            Element/button(
                element: [event: [press: LINK]]
                label: TEXT { Right }
                style: []
            ) |> LINK { right_button }
            Element/label(
                element: []
                style: []
                label: selected
            )
        }
    )
)
"#
    }

    fn hold_conformance_source() -> &'static str {
        r#"
increment_button: LINK

counter: 0 |> HOLD state {
    increment_button.event.press |> THEN { state + 1 }
}

document: Document/new(root:
    Element/stripe(
        element: []
        direction: Column
        gap: 0
        style: []
        items: LIST {
            Element/button(
                element: [event: [press: LINK]]
                label: TEXT { Increment }
                style: []
            ) |> LINK { increment_button }
            Element/label(
                element: []
                style: []
                label: counter
            )
        }
    )
)
"#
    }

    fn kernel_counter_after_presses(presses: usize) -> KernelValue {
        let mut runtime = KernelRuntime::new();
        let slot = SlotKey::new(ScopeId::ROOT, ExprId(1));
        runtime.create_hold(slot, 0.0.into());

        for seq in 1..=presses {
            let current = match runtime.hold(slot).expect("counter hold").value() {
                KernelValue::Number(value) => *value,
                other => panic!("expected numeric HOLD state, got {other:?}"),
            };
            runtime.commit_updates(vec![RuntimeUpdate::HoldValue {
                slot,
                value: KernelValue::from(current + 1.0),
                trigger: Trigger::System {
                    seq: TickSeq::new(runtime.tick(), seq as u32),
                },
            }]);
        }

        runtime.hold(slot).expect("counter hold").value().clone()
    }

    #[test]
    fn crud_graph_materializes_into_worker() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        {
            let mut inner = handle.inner.borrow_mut();
            for _ in 0..200 {
                inner.worker.step();
            }
            inner.notify_if_changed(false);
        }
        let output = handle.current_output();

        assert_ne!(output, Value::Unit);
    }

    #[test]
    fn latest_press_sequence_matches_reference_kernel_selection() {
        let program = compile(
            latest_conformance_source(),
            None,
            &std::collections::HashMap::new(),
            None,
        )
        .expect("LATEST conformance program should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected LATEST conformance program to compile as dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});

        handle.inject_dd_event(Event::LinkPress {
            link_path: "left_button.event.press".to_string(),
        });
        handle.inject_dd_event(Event::LinkPress {
            link_path: "right_button.event.press".to_string(),
        });

        let expected = select_latest(&[
            LatestCandidate::new(KernelValue::from("left"), TickSeq::new(TickId(1), 1)),
            LatestCandidate::new(KernelValue::from("right"), TickSeq::new(TickId(1), 2)),
        ]);
        let output_text = handle.current_output().to_display_string();

        let KernelValue::Text(expected_text) = expected else {
            panic!("expected reference kernel to produce text");
        };

        assert!(
            output_text.contains(&format!("label: {expected_text}")),
            "expected DD output to match reference-kernel LATEST result {expected_text:?}, got {output_text}"
        );

        handle.inject_dd_event(Event::LinkPress {
            link_path: "left_button.event.press".to_string(),
        });

        let expected = select_latest(&[
            LatestCandidate::new(KernelValue::from("left"), TickSeq::new(TickId(1), 3)),
            LatestCandidate::new(KernelValue::from("right"), TickSeq::new(TickId(1), 2)),
        ]);
        let output_text = handle.current_output().to_display_string();

        let KernelValue::Text(expected_text) = expected else {
            panic!("expected reference kernel to produce text");
        };

        assert!(
            output_text.contains(&format!("label: {expected_text}")),
            "expected DD output to match reference-kernel LATEST result {expected_text:?} after later left press, got {output_text}"
        );
    }

    #[test]
    fn hold_press_sequence_matches_reference_kernel_counter_progression() {
        let program = compile(
            hold_conformance_source(),
            None,
            &std::collections::HashMap::new(),
            None,
        )
        .expect("HOLD conformance program should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected HOLD conformance program to compile as dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});

        for presses in 0..=2 {
            if presses > 0 {
                handle.inject_dd_event(Event::LinkPress {
                    link_path: "increment_button.event.press".to_string(),
                });
            }

            let expected = kernel_counter_after_presses(presses);
            let KernelValue::Number(expected_number) = expected else {
                panic!("expected numeric counter");
            };
            let output_text = handle.current_output().to_display_string();

            assert!(
                output_text.contains(&format!("label: {}", expected_number)),
                "expected DD HOLD output to match reference-kernel counter value {expected_number}, got {output_text}"
            );
        }
    }

    fn value_tree_contains_keyed_stripe(value: &Value) -> bool {
        match value {
            Value::Tagged { tag, fields } => {
                if tag.as_ref() == "ElementStripe"
                    && fields
                        .get("__keyed__")
                        .and_then(Value::as_tag)
                        .is_some_and(|tag| tag == "True")
                {
                    return true;
                }
                fields.values().any(value_tree_contains_keyed_stripe)
            }
            Value::Object(fields) => fields.values().any(value_tree_contains_keyed_stripe),
            _ => false,
        }
    }

    #[test]
    fn crud_output_marks_people_stripe_as_keyed() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        let output = handle.current_output();

        assert!(
            value_tree_contains_keyed_stripe(&output),
            "expected CRUD document output to contain a keyed stripe marker, got: {}",
            output
        );
    }

    #[test]
    fn crud_worker_emits_initial_keyed_diffs() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        let diffs = handle.drain_keyed_diffs();

        assert!(
            !diffs.is_empty(),
            "expected CRUD worker to emit initial keyed diffs for people list"
        );
    }

    #[test]
    fn crud_initial_keyed_diffs_include_people_rows() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        let diffs = handle.drain_keyed_diffs();
        let diff_text = diffs
            .iter()
            .map(|diff| match diff {
                KeyedDiff::Upsert { value, .. } => value.to_display_string(),
                KeyedDiff::Remove { key } => format!("remove:{}", key.0),
            })
            .collect::<Vec<_>>()
            .join(" | ");

        assert!(
            diff_text.contains("Emil")
                && diff_text.contains("Mustermann")
                && diff_text.contains("Tansen"),
            "expected initial keyed diffs to contain CRUD people rows, got: {diff_text}"
        );
    }

    #[test]
    fn crud_create_uses_current_name_and_surname_input_text() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        let _ = handle.drain_keyed_diffs();

        handle.inject_dd_event(Event::TextChange {
            link_path: "store.elements.name_input.event.change".to_string(),
            text: "John".to_string(),
        });
        handle.inject_dd_event(Event::TextChange {
            link_path: "store.elements.surname_input.event.change".to_string(),
            text: "Doe".to_string(),
        });
        handle.inject_dd_event(Event::LinkPress {
            link_path: "store.elements.create_button.event.press".to_string(),
        });

        let output_after_create = handle.current_output();
        let diff_text = handle
            .drain_keyed_diffs()
            .into_iter()
            .map(|diff| match diff {
                KeyedDiff::Upsert { value, .. } => value.to_display_string(),
                KeyedDiff::Remove { key } => format!("remove:{}", key.0),
            })
            .collect::<Vec<_>>()
            .join(" | ");

        assert!(
            diff_text.contains("Doe") && diff_text.contains("John"),
            "expected create flow to emit a Doe/John row, got diffs: {diff_text}; output: {output_after_create}"
        );
    }

    #[test]
    fn crud_filter_text_change_updates_visible_rows() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        let _ = handle.drain_keyed_diffs();

        handle.inject_dd_event(Event::TextChange {
            link_path: "store.elements.filter_input.event.change".to_string(),
            text: "M".to_string(),
        });

        let diff_text = handle
            .drain_keyed_diffs()
            .into_iter()
            .map(|diff| match diff {
                KeyedDiff::Upsert { value, .. } => value.to_display_string(),
                KeyedDiff::Remove { key } => format!("remove:{}", key.0),
            })
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            diff_text.contains("remove:0000") && diff_text.contains("remove:0002"),
            "expected filter input change to remove the non-matching keyed rows, got diffs: {diff_text}"
        );
    }

    #[test]
    fn crud_row_press_selects_the_keyed_person() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        let _ = handle.drain_keyed_diffs();

        handle.inject_dd_event(Event::LinkPress {
            link_path: "store.people.0002.person_elements.row.event.press".to_string(),
        });

        let output = handle.current_output();
        let output_text = output.to_display_string();
        let diff_text = handle
            .drain_keyed_diffs()
            .into_iter()
            .map(|diff| match diff {
                KeyedDiff::Upsert { value, .. } => value.to_display_string(),
                KeyedDiff::Remove { key } => format!("remove:{}", key.0),
            })
            .collect::<Vec<_>>()
            .join(" | ");

        assert!(
            output_text.contains("► Tansen") || diff_text.contains("► Tansen"),
            "expected keyed row press to select Tansen, got diffs: {diff_text}; output: {output_text}"
        );
    }

    #[test]
    fn crud_delete_removes_the_selected_person() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        let _ = handle.drain_keyed_diffs();

        handle.inject_dd_event(Event::LinkPress {
            link_path: "store.people.0002.person_elements.row.event.press".to_string(),
        });
        let _ = handle.drain_keyed_diffs();

        handle.inject_dd_event(Event::LinkPress {
            link_path: "store.elements.delete_button.event.press".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        let diff_text = handle
            .drain_keyed_diffs()
            .into_iter()
            .map(|diff| match diff {
                KeyedDiff::Upsert { value, .. } => value.to_display_string(),
                KeyedDiff::Remove { key } => format!("remove:{}", key.0),
            })
            .collect::<Vec<_>>()
            .join(" | ");

        assert!(
            !output_text.contains("Tansen") && !diff_text.contains("Tansen"),
            "expected delete flow to remove Tansen, got diffs: {diff_text}; output: {output_text}"
        );
    }

    #[test]
    fn flight_booker_book_button_updates_confirmation_text() {
        let source =
            read_example("../../playground/frontend/src/examples/flight_booker/flight_booker.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("flight_booker should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected Flight Booker dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        handle.inject_dd_event(Event::LinkPress {
            link_path: "store.elements.book_button.event.press".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("Booked one-way flight on 2026-03-03"),
            "expected booking confirmation after clicking Book, got output: {output_text}"
        );
    }

    #[test]
    fn shopping_list_enter_adds_item() {
        let source =
            read_example("../../playground/frontend/src/examples/shopping_list/shopping_list.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("shopping_list should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("shopping_list should compile to dataflow"),
        };

        handle.inject_dd_event(Event::TextChange {
            link_path: "store.elements.item_input.event.change".to_string(),
            text: "Milk".to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "store.elements.item_input.event.key_down".to_string(),
            key: "Enter".to_string(),
            text: "Milk".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("1 items") && output_text.contains("Milk"),
            "expected Enter in shopping list input to append Milk; got output: {output_text}"
        );
    }

    #[test]
    fn todo_mvc_initial_output_shows_seed_items_and_input() {
        let source = read_example("../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("todo_mvc should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("todo_mvc should compile to dataflow"),
        };

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("todos")
                && output_text.contains("Buy groceries")
                && output_text.contains("Clean room")
                && output_text.contains("ElementTextInput"),
            "expected todo_mvc initial output to render title, seed todos, and input; got output: {output_text}"
        );
    }

    #[test]
    fn todo_mvc_enter_adds_a_new_item() {
        let source = read_example("../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("todo_mvc should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("todo_mvc should compile to dataflow"),
        };

        handle.inject_dd_event(Event::TextChange {
            link_path: "store.elements.new_todo_title_text_input.event.change".to_string(),
            text: "Test todo".to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "store.elements.new_todo_title_text_input.event.key_down".to_string(),
            key: "Enter".to_string(),
            text: "Test todo".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("3 items left") && output_text.contains("Test todo"),
            "expected Enter in the new-todo input to append a new item; got output: {output_text}"
        );
    }

    #[test]
    fn todo_mvc_physical_initial_output_renders_text_input() {
        let source =
            read_example("../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("todo_mvc_physical should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => {
                panic!("todo_mvc_physical should compile to dataflow")
            }
        };

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("ElementTextInput") || output_text.contains("text_input"),
            "expected todo_mvc_physical initial output to render a text input; got output: {output_text}"
        );
    }

    #[test]
    fn todo_mvc_physical_enter_adds_a_new_item() {
        let source =
            read_example("../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("todo_mvc_physical should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => {
                panic!("todo_mvc_physical should compile to dataflow")
            }
        };

        handle.inject_dd_event(Event::TextChange {
            link_path: "store.elements.new_todo_title_text_input.event.change".to_string(),
            text: "Buy groceries".to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "store.elements.new_todo_title_text_input.event.key_down".to_string(),
            key: "Enter".to_string(),
            text: "Buy groceries".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("Buy groceries") && output_text.contains("1 item"),
            "expected Enter in the physical new-todo input to append a new item; got output: {output_text}"
        );
    }

    #[test]
    fn todo_mvc_edit_title_enter_saves_the_edit() {
        let source = read_example("../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("todo_mvc should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("todo_mvc should compile to dataflow"),
        };

        handle.inject_dd_event(Event::DoubleClick {
            link_path: "store.todos.0000.todo_elements.todo_title_element.event.double_click"
                .to_string(),
        });
        handle.inject_dd_event(Event::TextChange {
            link_path: "store.todos.0000.todo_elements.editing_todo_title_element.event.change"
                .to_string(),
            text: "Edited groceries".to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "store.todos.0000.todo_elements.editing_todo_title_element.event.key_down"
                .to_string(),
            key: "Enter".to_string(),
            text: "Edited groceries".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("Edited groceries"),
            "expected Enter in the edit-title input to save the new title; got output: {output_text}"
        );
    }

    #[test]
    fn todo_mvc_toggle_all_click_marks_all_completed() {
        let source = read_example("../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("todo_mvc should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("todo_mvc should compile to dataflow"),
        };

        handle.inject_dd_event(Event::LinkClick {
            link_path: "store.elements.toggle_all_checkbox".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("0 items left"),
            "expected toggle-all click to mark all todos completed; got output: {output_text}"
        );
    }

    #[test]
    fn todo_mvc_toggle_all_press_path_marks_all_completed() {
        let source = read_example("../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("todo_mvc should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("todo_mvc should compile to dataflow"),
        };

        handle.inject_dd_event(Event::LinkPress {
            link_path: "store.elements.toggle_all_checkbox.event.press".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("0 items left"),
            "expected checkbox press path to normalize to click semantics; got output: {output_text}"
        );
    }

    #[test]
    fn todo_mvc_clear_completed_leaves_only_new_item_after_toggle_all_cycle() {
        let source = read_example("../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("todo_mvc should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("todo_mvc should compile to dataflow"),
        };

        let checkbox_press = |handle: &DdWorkerHandle, path: &str| {
            handle.inject_dd_event(Event::LinkPress {
                link_path: format!("{path}.event.press"),
            });
        };

        let add_todo = |handle: &DdWorkerHandle, title: &str| {
            handle.inject_dd_event(Event::TextChange {
                link_path: "store.elements.new_todo_title_text_input.event.change".to_string(),
                text: title.to_string(),
            });
            handle.inject_dd_event(Event::KeyDown {
                link_path: "store.elements.new_todo_title_text_input.event.key_down".to_string(),
                key: "Enter".to_string(),
                text: title.to_string(),
            });
        };

        add_todo(&handle, "Test todo");
        add_todo(&handle, "Walk the dog");
        add_todo(&handle, "Feed the cat");

        checkbox_press(&handle, "store.todos.0002.todo_elements.todo_checkbox");
        checkbox_press(&handle, "store.todos.0002.todo_elements.todo_checkbox");
        checkbox_press(&handle, "store.todos.0000.todo_elements.todo_checkbox");
        checkbox_press(&handle, "store.todos.0000.todo_elements.todo_checkbox");
        checkbox_press(&handle, "store.elements.toggle_all_checkbox");
        checkbox_press(&handle, "store.elements.toggle_all_checkbox");
        checkbox_press(&handle, "store.elements.toggle_all_checkbox");
        handle.inject_dd_event(Event::LinkPress {
            link_path: "store.elements.remove_completed_button.event.press".to_string(),
        });

        add_todo(&handle, "Buy milk");

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("1 items left")
                && output_text.contains("Buy milk")
                && !output_text.contains("Buy groceries")
                && !output_text.contains("Clean room")
                && !output_text.contains("Test todo")
                && !output_text.contains("Walk the dog")
                && !output_text.contains("Feed the cat"),
            "expected clear-completed cycle to leave only Buy milk; got output: {output_text}"
        );
    }

    #[test]
    fn timer_reaches_100_after_slider_change_and_ticks() {
        let source = read_example("../../playground/frontend/src/examples/timer/timer.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("timer should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("timer should compile to dataflow"),
        };

        handle.inject_dd_event(Event::LinkPress {
            link_path: "store.elements.reset_button.event.press".to_string(),
        });
        handle.inject_dd_event(Event::NumberChange {
            link_path: "store.elements.duration_slider.event.change".to_string(),
            value: 2.0,
        });

        for _ in 0..30 {
            handle.inject_dd_event(Event::TimerTick {
                var_name: "tick".to_string(),
            });
        }

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("100%") && output_text.contains("2s"),
            "expected timer to reach 100% after slider change and ticks; got output: {output_text}"
        );
    }

    #[test]
    fn timer_extending_duration_after_100_percent_resumes_below_full_progress() {
        let source = read_example("../../playground/frontend/src/examples/timer/timer.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("timer should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("timer should compile to dataflow"),
        };

        handle.inject_dd_event(Event::LinkPress {
            link_path: "store.elements.reset_button.event.press".to_string(),
        });
        handle.inject_dd_event(Event::NumberChange {
            link_path: "store.elements.duration_slider.event.change".to_string(),
            value: 2.0,
        });

        for _ in 0..30 {
            handle.inject_dd_event(Event::TimerTick {
                var_name: "tick".to_string(),
            });
        }

        handle.inject_dd_event(Event::NumberChange {
            link_path: "store.elements.duration_slider.event.change".to_string(),
            value: 15.0,
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("Duration:15s") && !output_text.contains("Elapsed Time:100%"),
            "expected timer to resume below 100% after extending duration; got output: {output_text}"
        );
    }

    #[test]
    fn cells_initial_output_shows_seed_formula_values() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let output = match program {
            CompiledProgram::Dataflow { graph } => {
                let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
                handle.current_output().to_display_string()
            }
            CompiledProgram::Static { document_value, .. } => document_value.to_display_string(),
        };

        assert!(
            output.contains(
                "all_row_cells.0000.cells.0000.display_element, element: [event: [double_click: LINK]], label: 5"
            ) && output.contains(
                "all_row_cells.0000.cells.0001.display_element, element: [event: [double_click: LINK]], label: 15"
            ) && output.contains(
                "all_row_cells.0000.cells.0002.display_element, element: [event: [double_click: LINK]], label: 30"
            ),
            "expected first visible row to include A1=5, B1=15, C1=30; got output: {}",
            output
        );
    }

    #[test]
    fn cells_a1_double_click_enters_edit_mode() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("cells should compile to dataflow"),
        };

        handle.inject_dd_event(Event::DoubleClick {
            link_path: "all_row_cells.0000.cells.0000.display_element.event.double_click"
                .to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        let input_count = output_text.matches("ElementTextInput").count()
            + output_text.matches("text_input").count();
        assert!(
            output_text.contains("ElementTextInput") || output_text.contains("text_input"),
            "expected cells A1 double click to show a text input; got output: {output_text}"
        );
        assert!(
            input_count <= 2,
            "expected only the active cell to enter edit mode; got {input_count} inputs in output: {output_text}"
        );
    }

    #[test]
    fn cells_enter_commits_a1_and_recomputes_dependents() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("cells should compile to dataflow"),
        };

        handle.inject_dd_event(Event::DoubleClick {
            link_path: "all_row_cells.0000.cells.0000.display_element.event.double_click"
                .to_string(),
        });
        handle.inject_dd_event(Event::TextChange {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.change".to_string(),
            text: "7".to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.key_down".to_string(),
            key: "Enter".to_string(),
            text: "7".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains(
                "all_row_cells.0000.cells.0000.display_element, element: [event: [double_click: LINK]], label: 7"
            ) && output_text.contains(
                "all_row_cells.0000.cells.0001.display_element, element: [event: [double_click: LINK]], label: 17"
            ) && output_text.contains(
                "all_row_cells.0000.cells.0002.display_element, element: [event: [double_click: LINK]], label: 32"
            ),
            "expected A1=7, B1=17, C1=32 after commit; got output: {output_text}"
        );
    }

    #[test]
    fn cells_browser_like_edit_sequence_commits_a1_and_recomputes_dependents() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("cells should compile to dataflow"),
        };

        handle.inject_dd_event(Event::DoubleClick {
            link_path: "all_row_cells.0000.cells.0000.display_element.event.double_click"
                .to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.key_down".to_string(),
            key: "Backspace".to_string(),
            text: "5".to_string(),
        });
        handle.inject_dd_event(Event::TextChange {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.change".to_string(),
            text: "".to_string(),
        });
        handle.inject_dd_event(Event::TextChange {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.change".to_string(),
            text: "7".to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.key_down".to_string(),
            key: "Enter".to_string(),
            text: "7".to_string(),
        });
        handle.inject_dd_event(Event::Blur {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.blur".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains(
                "all_row_cells.0000.cells.0000.display_element, element: [event: [double_click: LINK]], label: 7"
            ) && output_text.contains(
                "all_row_cells.0000.cells.0001.display_element, element: [event: [double_click: LINK]], label: 17"
            ) && output_text.contains(
                "all_row_cells.0000.cells.0002.display_element, element: [event: [double_click: LINK]], label: 32"
            ),
            "expected browser-like edit sequence to leave A1=7, B1=17, C1=32; got output: {output_text}"
        );
    }

    #[test]
    fn cells_backspace_during_edit_keeps_a1_in_edit_mode() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("cells should compile to dataflow"),
        };

        handle.inject_dd_event(Event::DoubleClick {
            link_path: "all_row_cells.0000.cells.0000.display_element.event.double_click"
                .to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.key_down".to_string(),
            key: "Backspace".to_string(),
            text: "".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("ElementTextInput")
                && output_text.contains("all_row_cells.0000.cells.0000.editing_element"),
            "expected Backspace to keep A1 in edit mode; got output: {output_text}"
        );
    }

    #[test]
    fn cells_reopen_after_commit_shows_the_committed_formula_text() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("cells should compile to dataflow"),
        };

        handle.inject_dd_event(Event::DoubleClick {
            link_path: "all_row_cells.0000.cells.0000.display_element.event.double_click"
                .to_string(),
        });
        handle.inject_dd_event(Event::TextChange {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.change".to_string(),
            text: "7".to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.key_down".to_string(),
            key: "Enter".to_string(),
            text: "7".to_string(),
        });
        handle.inject_dd_event(Event::DoubleClick {
            link_path: "all_row_cells.0000.cells.0000.display_element.event.double_click"
                .to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("ElementTextInput")
                && output_text.contains("all_row_cells.0000.cells.0000.editing_element")
                && output_text.contains("text: 7"),
            "expected reopening A1 to show input text 7; got output: {output_text}"
        );
    }

    #[test]
    fn cells_escape_after_reopen_preserves_committed_values() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("cells should compile to dataflow"),
        };

        handle.inject_dd_event(Event::DoubleClick {
            link_path: "all_row_cells.0000.cells.0000.display_element.event.double_click"
                .to_string(),
        });
        handle.inject_dd_event(Event::TextChange {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.change".to_string(),
            text: "7".to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.key_down".to_string(),
            key: "Enter".to_string(),
            text: "7".to_string(),
        });
        handle.inject_dd_event(Event::DoubleClick {
            link_path: "all_row_cells.0000.cells.0000.display_element.event.double_click"
                .to_string(),
        });
        handle.inject_dd_event(Event::TextChange {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.change".to_string(),
            text: "9".to_string(),
        });
        handle.inject_dd_event(Event::KeyDown {
            link_path: "all_row_cells.0000.cells.0000.editing_element.event.key_down".to_string(),
            key: "Escape".to_string(),
            text: "9".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains(
                "all_row_cells.0000.cells.0000.display_element, element: [event: [double_click: LINK]], label: 7"
            ) && output_text.contains(
                "all_row_cells.0000.cells.0001.display_element, element: [event: [double_click: LINK]], label: 17"
            ) && output_text.contains(
                "all_row_cells.0000.cells.0002.display_element, element: [event: [double_click: LINK]], label: 32"
            ),
            "expected Escape after reopen to preserve committed A1=7, B1=17, C1=32; got output: {output_text}"
        );
    }

    #[test]
    fn cells_compile_registers_a_concrete_double_click_input_path() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("cells should compile to dataflow");
        };

        let paths: Vec<String> = graph
            .inputs
            .iter()
            .filter_map(|input| input.link_path.clone())
            .collect();

        assert!(
            paths.iter().any(|path| {
                path == "all_row_cells.0000.cells.0000.display_element.event.double_click"
            }),
            "expected concrete A1 double-click input path; got inputs: {paths:?}"
        );
    }

    #[test]
    fn static_item_text_change_then_body_reads_nested_event_text() {
        let source = r#"
items: LIST {
    [cell_elements: [editing: LINK]]
}
edit_changed: items
    |> List/map(cell, new:
        cell.cell_elements.editing.event.change |> THEN {
            [text: cell.cell_elements.editing.event.change.text]
        }
    )
    |> List/latest()
editing_text: TEXT {  } |> HOLD state {
    edit_changed |> THEN { edit_changed.text }
}
document: Document/new(root:
    Element/label(element: [], style: [], label: editing_text)
)
"#;

        let program = compile(source, None, &std::collections::HashMap::new(), None)
            .expect("program should compile");
        let handle = match program {
            CompiledProgram::Dataflow { graph } => {
                let paths: Vec<String> = graph
                    .inputs
                    .iter()
                    .filter_map(|input| input.link_path.clone())
                    .collect();
                assert!(
                    paths
                        .iter()
                        .any(|path| path == "items.0000.cell_elements.editing.event.change"),
                    "expected concrete text-change input path; got inputs: {paths:?}"
                );
                DdWorkerHandle::new_from_graph(graph, |_value| {})
            }
            CompiledProgram::Static { .. } => panic!("program should compile to dataflow"),
        };

        handle.inject_dd_event(Event::TextChange {
            link_path: "items.0000.cell_elements.editing.event.change".to_string(),
            text: "7".to_string(),
        });

        let output_text = handle.current_output().to_display_string();
        assert!(
            output_text.contains("label: 7"),
            "expected static item text change event to expose nested event text; got output: {output_text}"
        );
    }

    #[test]
    fn cells_worker_boot_reaches_output_clone() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected cells dataflow");
        };

        eprintln!("[cells-dd] before new_from_graph");
        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        eprintln!("[cells-dd] after new_from_graph");
        let output = handle.current_output();
        eprintln!("[cells-dd] after current_output clone");
        assert_ne!(output, Value::Unit);
    }

    #[test]
    #[ignore = "browser-only retained-tree path requires wasm/js runtime"]
    fn crud_output_builds_retained_tree() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD dataflow");
        };

        let handle = DdWorkerHandle::new_from_graph(graph, |_value| {});
        {
            let mut inner = handle.inner.borrow_mut();
            for _ in 0..200 {
                inner.worker.step();
            }
            inner.notify_if_changed(false);
        }
        let output = handle.current_output();
        assert_ne!(output, Value::Unit);

        let _ = build_retained_tree(&output, &handle, RenderSurface::Document);
    }
}
