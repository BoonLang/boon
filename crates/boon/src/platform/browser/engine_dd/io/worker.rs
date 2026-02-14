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

use super::super::core::compile::LinkBinding;
use super::super::core::operators;
use super::super::core::types::{InputId, LinkId};
use super::super::core::value::Value;

/// Handle to the running DD engine. Stored in the IO layer.
///
/// Uses Rc<RefCell<>> for interior mutability — this is allowed in io/
/// but NOT in core/ (anti-cheat boundary).
pub struct DdWorkerHandle {
    inner: Rc<RefCell<DdWorkerInner>>,
}

struct DdWorkerInner {
    worker: Worker<Thread>,
    inputs: HashMap<InputId, InputSession<u64, Value, isize>>,
    link_to_input: HashMap<LinkId, InputId>,
    epoch: u64,
    /// Output cell written by DD inspect callback during worker.step().
    output_cell: Rc<RefCell<Value>>,
    /// Last value we notified about (to detect changes).
    last_notified: Value,
    /// Callback to notify when output changes.
    on_output_change: Option<Box<dyn Fn(&Value)>>,
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
    /// Create a DD worker for a SingleHold reactive program.
    ///
    /// Builds a DD dataflow with:
    /// - One input (LINK events)
    /// - A HOLD operator that applies `transform` on each event
    /// - Output observed via inspect
    pub fn new_single_hold(
        initial_value: Value,
        transform: Arc<dyn Fn(&Value, &Value) -> Value>,
        link_bindings: &[LinkBinding],
        on_output_change: impl Fn(&Value) + 'static,
    ) -> Self {
        let alloc = Thread::default();
        let mut worker = Worker::new(Default::default(), alloc, None);

        // Build link → input mapping
        let mut link_to_input = HashMap::new();
        for binding in link_bindings {
            link_to_input.insert(binding.link_id.clone(), binding.input_id);
        }

        // Output value will be captured via inspect
        let output_cell: Rc<RefCell<Value>> = Rc::new(RefCell::new(initial_value.clone()));
        let output_for_inspect = output_cell.clone();

        let initial_val = initial_value.clone();

        let mut input_session = worker.dataflow::<u64, _, _>(|scope| {
            use differential_dataflow::input::Input;

            // Create input for button press events
            let (input_session, events) = scope.new_collection::<Value, isize>();

            // Initial value collection
            let (mut init_session, initial) = scope.new_collection::<Value, isize>();
            init_session.update(initial_val.clone(), 1);
            init_session.flush();

            // Build HOLD state operator
            let counter = operators::hold_state(
                &initial,
                &events,
                initial_val,
                move |state, event| transform(state, event),
            );

            // Observe output changes via inspect
            let output_ref = output_for_inspect;
            counter.inspect(move |(value, _time, diff)| {
                if *diff > 0 {
                    *output_ref.borrow_mut() = value.clone();
                }
            });

            input_session
        });

        // Advance past the initial epoch
        input_session.advance_to(1);
        input_session.flush();
        worker.step();

        // Read initial output
        let initial_output = output_cell.borrow().clone();

        let mut inputs = HashMap::new();
        let first_input_id = link_bindings
            .first()
            .map(|b| b.input_id)
            .unwrap_or(InputId(0));
        inputs.insert(first_input_id, input_session);

        let inner = DdWorkerInner {
            worker,
            inputs,
            link_to_input,
            epoch: 1,
            output_cell,
            last_notified: initial_output.clone(),
            on_output_change: Some(Box::new(on_output_change)),
        };

        let handle = DdWorkerHandle {
            inner: Rc::new(RefCell::new(inner)),
        };

        // Notify with initial value
        if let Some(ref cb) = handle.inner.borrow().on_output_change {
            cb(&initial_output);
        }

        handle
    }

    /// Create a DD worker for a LatestSum reactive program.
    ///
    /// Builds a DD dataflow with:
    /// - One input (LINK events)
    /// - A running sum: each event adds 1 to the total
    /// - Output observed via inspect
    pub fn new_latest_sum(
        initial_sum: f64,
        link_bindings: &[LinkBinding],
        on_output_change: impl Fn(&Value) + 'static,
    ) -> Self {
        let alloc = Thread::default();
        let mut worker = Worker::new(Default::default(), alloc, None);

        let mut link_to_input = HashMap::new();
        for binding in link_bindings {
            link_to_input.insert(binding.link_id.clone(), binding.input_id);
        }

        let output_cell: Rc<RefCell<Value>> = Rc::new(RefCell::new(Value::number(initial_sum)));
        let output_for_inspect = output_cell.clone();

        let initial_value = Value::number(initial_sum);

        let mut input_session = worker.dataflow::<u64, _, _>(|scope| {
            use differential_dataflow::input::Input;

            let (input_session, events) = scope.new_collection::<Value, isize>();

            // Initial value from persisted state or 0
            let (mut init_session, initial) = scope.new_collection::<Value, isize>();
            init_session.update(Value::number(initial_sum), 1);
            init_session.flush();

            // Running sum: on each event, add 1
            let sum = operators::hold_state(&initial, &events, Value::number(initial_sum), |state, _event| {
                let n = state.as_number().unwrap_or(0.0);
                Value::number(n + 1.0)
            });

            let output_ref = output_for_inspect;
            sum.inspect(move |(value, _time, diff)| {
                if *diff > 0 {
                    *output_ref.borrow_mut() = value.clone();
                }
            });

            input_session
        });

        input_session.advance_to(1);
        input_session.flush();
        worker.step();

        let initial_output = output_cell.borrow().clone();

        let mut inputs = HashMap::new();
        let first_input_id = link_bindings
            .first()
            .map(|b| b.input_id)
            .unwrap_or(InputId(0));
        inputs.insert(first_input_id, input_session);

        let inner = DdWorkerInner {
            worker,
            inputs,
            link_to_input,
            epoch: 1,
            output_cell,
            last_notified: initial_output.clone(),
            on_output_change: Some(Box::new(on_output_change)),
        };

        let handle = DdWorkerHandle {
            inner: Rc::new(RefCell::new(inner)),
        };

        if let Some(ref cb) = handle.inner.borrow().on_output_change {
            cb(&initial_output);
        }

        handle
    }

    /// Inject an event for a LINK and step the DD worker.
    pub fn inject_event(&self, link_id: &LinkId, event_value: Value) {
        let mut inner = self.inner.borrow_mut();

        let input_id = match inner.link_to_input.get(link_id) {
            Some(id) => *id,
            None => {
                zoon::eprintln!("[DD] Unknown link: {}", link_id.as_str());
                return;
            }
        };

        // Advance epoch before borrowing session to avoid borrow conflicts
        inner.epoch += 1;
        let epoch = inner.epoch;

        if let Some(session) = inner.inputs.get_mut(&input_id) {
            // Insert event
            session.update(event_value, 1);
            // Advance epoch
            session.advance_to(epoch);
            session.flush();
        }

        // Step the worker — this triggers inspect callbacks
        // which update output_cell
        inner.worker.step();

        // Propagate output change to the Mutable<Value>
        inner.notify_if_changed();
    }

    /// Get the current output value.
    pub fn current_output(&self) -> Value {
        self.inner.borrow().output_cell.borrow().clone()
    }

    /// Get a clone of the handle for sharing with event handlers.
    pub fn clone_ref(&self) -> DdWorkerHandle {
        DdWorkerHandle {
            inner: self.inner.clone(),
        }
    }
}
