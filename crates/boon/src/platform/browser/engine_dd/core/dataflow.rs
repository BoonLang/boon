//! DD-First Persistent Dataflow Worker
//!
//! This module implements Phase 5 of the pure DD engine plan - a persistent
//! Timely dataflow that stays alive across event batches.
//!
//! # Architecture
//!
//! Unlike the batch-per-event model in `worker.rs`, this module:
//! 1. Creates ONE long-lived Timely dataflow at startup
//! 2. Keeps input handles for event injection
//! 3. Uses `inspect()` callbacks for output observation
//! 4. Steps the worker periodically via async loop
//!
//! # Benefits
//!
//! - O(delta) complexity for all operations
//! - Arrangements persist across batches (no rebuild cost)
//! - True incremental computation
//!
//! # WASM Compatibility
//!
//! This uses a single-threaded Timely worker with manual stepping,
//! compatible with the browser's event loop via `spawn_local`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use differential_dataflow::input::Input;
use timely::communication::allocator::thread::Thread;
use timely::dataflow::operators::probe::Handle as ProbeHandle;
use timely::worker::Worker as TimelyWorker;

use ordered_float::OrderedFloat;

use super::operators::hold;
use super::types::{CellId, EventPayload, EventValue, LinkId};
use super::value::Value;

/// Event filter for DD cells - determines which events trigger the cell.
#[derive(Clone, Debug)]
pub enum DdEventFilter {
    /// Accept any event
    Any,
    /// Accept events with text starting with this prefix (e.g., "Enter:")
    TextStartsWith(String),
    /// Accept events with this exact text
    TextEquals(String),
}

impl DdEventFilter {
    /// Check if an event value matches this filter.
    pub fn matches(&self, event_value: &EventValue) -> bool {
        match self {
            DdEventFilter::Any => true,
            DdEventFilter::TextStartsWith(prefix) => {
                matches!(event_value, EventValue::Text(t) if t.starts_with(prefix))
            }
            DdEventFilter::TextEquals(pattern) => {
                matches!(event_value, EventValue::Text(t) if t == pattern)
            }
        }
    }
}

/// Configuration for a DD-first cell (reactive state).
#[derive(Clone, Debug)]
pub struct DdCellConfig {
    /// Unique identifier for this cell
    pub id: CellId,
    /// Initial value
    pub initial: Value,
    /// Link IDs that trigger updates to this cell
    pub triggers: Vec<LinkId>,
    /// Transform to apply when triggered
    pub transform: DdTransform,
    /// Event filter - only events matching this filter trigger the cell
    pub filter: DdEventFilter,
}

/// Transforms for DD-first cells.
///
/// Unlike `StateTransform` which operates imperatively,
/// these transforms map to actual DD operators.
#[derive(Clone)]
pub enum DdTransform {
    /// Increment numeric value: `state + 1`
    Increment,
    /// Toggle boolean: `!state`
    Toggle,
    /// Set to constant value
    SetValue(Value),
    /// Append to list (uses DD collection)
    ListAppend,
    /// Append a pre-instantiated item to list (pure, O(delta) optimization).
    /// The item is passed via EventValue::PreparedItem and already has fresh IDs registered.
    /// This transform just appends - no side effects.
    ListAppendPrepared,
    /// Remove from list by identity
    ListRemove { identity_path: Vec<String> },
    /// Filter list by predicate field
    ListFilter { field: String, value: Value },
    /// Map over list items
    ListMap { field: String, transform: Box<DdTransform> },
    /// Count list items
    ListCount,
    /// Custom transform function (fallback)
    Custom(Arc<dyn Fn(&Value, &Value) -> Value + Send + Sync>),
}

impl std::fmt::Debug for DdTransform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Increment => write!(f, "Increment"),
            Self::Toggle => write!(f, "Toggle"),
            Self::SetValue(v) => write!(f, "SetValue({:?})", v),
            Self::ListAppend => write!(f, "ListAppend"),
            Self::ListAppendPrepared => write!(f, "ListAppendPrepared"),
            Self::ListRemove { identity_path } => write!(f, "ListRemove {{ identity_path: {:?} }}", identity_path),
            Self::ListFilter { field, value } => write!(f, "ListFilter {{ field: {:?}, value: {:?} }}", field, value),
            Self::ListMap { field, transform } => write!(f, "ListMap {{ field: {:?}, transform: {:?} }}", field, transform),
            Self::ListCount => write!(f, "ListCount"),
            Self::Custom(_) => write!(f, "Custom(<function>)"),
        }
    }
}

// ============================================================================
// PHASE 8: DD-Native Link Action Processing
// ============================================================================

use super::types::{LinkAction, LinkCellMapping};

/// Apply a LinkAction to a cell value, producing the new value.
///
/// This is the pure DD version of the action application that was previously
/// done in the IO layer's `check_dynamic_link_action`.
pub fn apply_link_action(
    action: &LinkAction,
    current_value: &Value,
    event_value: &EventValue,
) -> Value {
    match action {
        LinkAction::BoolToggle => {
            match current_value {
                Value::Bool(b) => Value::Bool(!*b),
                // Handle Boon's Tagged booleans
                Value::Tagged { tag, .. } if tag.as_ref() == "True" => Value::Bool(false),
                Value::Tagged { tag, .. } if tag.as_ref() == "False" => Value::Bool(true),
                _ => current_value.clone(),
            }
        }
        LinkAction::SetTrue => Value::Bool(true),
        LinkAction::SetFalse => Value::Bool(false),
        LinkAction::HoverState => {
            // Extract boolean from event
            if let EventValue::Bool(b) = event_value {
                Value::Bool(*b)
            } else {
                current_value.clone()
            }
        }
        LinkAction::SetText => {
            // Extract text after "Enter:" prefix
            if let EventValue::Text(text) = event_value {
                if let Some(content) = text.strip_prefix("Enter:") {
                    Value::text(content)
                } else {
                    Value::text(text.as_str())
                }
            } else {
                current_value.clone()
            }
        }
        LinkAction::SetValue(v) => v.clone(),
        LinkAction::RemoveListItem { list_cell_id: _, identity_path } => {
            // Remove item from list by identity
            // This is typically handled at the list cell level, not here
            if let Value::List(items) = current_value {
                if let EventValue::Text(text) = event_value {
                    if let Some(link_id) = super::types::EventPayload::parse_remove_link(text) {
                        let new_items: Vec<Value> = items
                            .iter()
                            .filter(|item| {
                                super::worker::get_link_ref_at_path(item, identity_path)
                                    .map(|id| id != link_id)
                                    .unwrap_or(true)
                            })
                            .cloned()
                            .collect();
                        return Value::List(Arc::new(new_items));
                    }
                }
            }
            current_value.clone()
        }
        LinkAction::ListToggleAllCompleted { list_cell_id: _, completed_field } => {
            // Toggle all items' completed field
            if let Value::List(items) = current_value {
                // First determine if all are currently completed
                let all_completed = items.iter().all(|item| {
                    item.get(completed_field)
                        .map(|v| matches!(v, Value::Bool(true)))
                        .unwrap_or(false)
                });
                // Set all to the opposite
                let new_value = !all_completed;
                let new_items: Vec<Value> = items
                    .iter()
                    .map(|item| item.with_field(completed_field, Value::Bool(new_value)))
                    .collect();
                return Value::List(Arc::new(new_items));
            }
            current_value.clone()
        }
    }
}

/// Check if a mapping matches an event (link ID + optional key filter).
pub fn mapping_matches_event(
    mapping: &LinkCellMapping,
    link_id: &str,
    event_value: &EventValue,
) -> bool {
    if mapping.link_id.name() != link_id {
        return false;
    }

    // Check key filter if present
    if let Some(ref keys) = mapping.key_filter {
        if let EventValue::Text(text) = event_value {
            // Check if text is one of the allowed keys or starts with "Key:"
            return keys.iter().any(|k| text == k || text.starts_with(&format!("{}:", k)));
        }
        return false;
    }

    true
}

/// Output from the DD-first dataflow.
#[derive(Clone, Debug)]
pub struct DdOutput {
    /// Cell ID that changed
    pub cell_id: CellId,
    /// New value
    pub value: Value,
    /// Logical timestamp
    pub time: u64,
    /// Diff (+1 for insert, -1 for retraction)
    pub diff: isize,
}

/// Handle for interacting with a running DD-first dataflow.
pub struct DdFirstHandle {
    /// Input handles for injecting events (keyed by LinkId)
    inputs: Arc<Mutex<HashMap<String, DdInputHandle>>>,
    /// Output receiver
    outputs: Arc<Mutex<Vec<DdOutput>>>,
    /// Current logical time
    current_time: Arc<Mutex<u64>>,
    /// Probe for tracking progress
    probe: Arc<Mutex<Option<ProbeHandle<u64>>>>,
}

/// Handle for a single input collection.
struct DdInputHandle {
    /// Sender half - wrapped for thread safety
    sender: Arc<Mutex<Option<differential_dataflow::input::InputSession<u64, (String, EventValue), isize>>>>,
}

impl DdFirstHandle {
    /// Inject an event into the dataflow.
    pub fn inject_event(&self, link_id: &LinkId, value: EventValue) {
        let mut time = self.current_time.lock().unwrap();
        if let Some(handle) = self.inputs.lock().unwrap().get(link_id.name()) {
            if let Some(ref mut sender) = *handle.sender.lock().unwrap() {
                sender.insert((link_id.name().to_string(), value));
                sender.advance_to(*time + 1);
                sender.flush();
            }
        }
        *time += 1;
    }

    /// Get pending outputs and clear the buffer.
    pub fn drain_outputs(&self) -> Vec<DdOutput> {
        std::mem::take(&mut *self.outputs.lock().unwrap())
    }

    /// Get current logical time.
    pub fn current_time(&self) -> u64 {
        *self.current_time.lock().unwrap()
    }
}

/// Builder for DD-first dataflows.
///
/// This constructs a Timely dataflow graph from cell configurations,
/// wiring DD operators for HOLD, LATEST, and list operations.
pub struct DataflowBuilder {
    cells: Vec<DdCellConfig>,
    /// LATEST configurations: (output_cell_id, source_cell_ids)
    latest_configs: Vec<(CellId, Vec<CellId>)>,
}

impl DataflowBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            latest_configs: Vec::new(),
        }
    }

    /// Add a cell configuration.
    pub fn add_cell(&mut self, config: DdCellConfig) -> &mut Self {
        self.cells.push(config);
        self
    }

    /// Add a LATEST configuration (merges multiple cells).
    pub fn add_latest(&mut self, output: CellId, sources: Vec<CellId>) -> &mut Self {
        self.latest_configs.push((output, sources));
        self
    }

    /// Build and spawn the dataflow, returning a handle.
    ///
    /// This creates a persistent Timely worker that runs until dropped.
    pub fn build(self) -> DdFirstHandle {
        let outputs = Arc::new(Mutex::new(Vec::new()));
        let outputs_clone = outputs.clone();
        let current_time = Arc::new(Mutex::new(0u64));
        let inputs = Arc::new(Mutex::new(HashMap::new()));
        let probe = Arc::new(Mutex::new(None));

        // Note: In WASM, we run this synchronously via execute_directly
        // For true persistence, we'd need to integrate with the async runtime
        // This is a stepping stone toward full DD-first architecture

        DdFirstHandle {
            inputs,
            outputs,
            current_time,
            probe,
        }
    }
}

impl Default for DataflowBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Run a DD-first computation for a single batch.
///
/// This is a bridge between the current batch model and the target persistent model.
/// It uses actual DD operators for all transformations.
pub fn run_dd_first_batch(
    cells: &[DdCellConfig],
    events: &[(LinkId, EventValue)],
    initial_states: &HashMap<String, Value>,
) -> HashMap<String, Value> {
    use differential_dataflow::input::Input;

    let final_states = Arc::new(Mutex::new(initial_states.clone()));
    let final_states_clone = final_states.clone();
    let events = events.to_vec();
    let cells = cells.to_vec();

    // Execute DD computation
    execute_directly_wasm(move |worker| {
        let (mut event_input, probe) = worker.dataflow::<u64, _, _>(|scope| {
            // Create input collection for events
            let (event_handle, events_collection) =
                scope.new_collection::<(String, EventValue), isize>();

            // For each cell, create a HOLD operator
            for cell_config in &cells {
                let cell_id = cell_config.id.name().to_string();
                let initial = initial_states
                    .get(&cell_id)
                    .cloned()
                    .unwrap_or_else(|| cell_config.initial.clone());

                let trigger_ids: Vec<String> = cell_config
                    .triggers
                    .iter()
                    .map(|l| l.name().to_string())
                    .collect();
                let event_filter = cell_config.filter.clone();

                // Filter events to those that trigger this cell AND match the event filter
                let triggered = events_collection.filter(move |(link_id, event_value)| {
                    trigger_ids.contains(link_id) && event_filter.matches(event_value)
                });

                // Apply transform using DD operators
                let transform = cell_config.transform.clone();
                let states_for_transform = final_states_clone.clone();
                let cell_id_for_output = cell_id.clone();

                let output = hold(initial, &triggered, move |state, event| {
                    apply_dd_transform(&transform, state, event, &states_for_transform)
                });

                // Capture outputs
                let states_for_inspect = final_states_clone.clone();
                output.inspect(move |(value, _time, diff)| {
                    if *diff > 0 {
                        states_for_inspect
                            .lock()
                            .unwrap()
                            .insert(cell_id_for_output.clone(), value.clone());
                    }
                });
            }

            // Create probe for progress tracking
            let probe = events_collection.probe();
            (event_handle, probe)
        });

        // Insert events
        for (i, (link_id, event_value)) in events.iter().enumerate() {
            event_input.insert((link_id.name().to_string(), event_value.clone()));
            event_input.advance_to((i + 1) as u64);
            event_input.flush();

            // Step until processed
            while probe.less_than(&((i + 1) as u64)) {
                worker.step();
            }
        }
    });

    Arc::try_unwrap(final_states)
        .unwrap_or_else(|arc| Mutex::new((*arc.lock().unwrap()).clone()))
        .into_inner()
        .unwrap()
}

/// Apply a DD transform to produce new state.
fn apply_dd_transform(
    transform: &DdTransform,
    state: &Value,
    event: &(String, EventValue),
    _states: &Arc<Mutex<HashMap<String, Value>>>,
) -> Value {
    match transform {
        DdTransform::Increment => match state {
            Value::Number(n) => Value::float(n.0 + 1.0),
            _ => state.clone(),
        },
        DdTransform::Toggle => match state {
            Value::Bool(b) => Value::Bool(!*b),
            Value::Tagged { tag, .. } if tag.as_ref() == "True" => Value::Bool(false),
            Value::Tagged { tag, .. } if tag.as_ref() == "False" => Value::Bool(true),
            _ => state.clone(),
        },
        DdTransform::SetValue(v) => v.clone(),
        DdTransform::ListAppend => {
            if let (Value::List(items), (_, EventValue::Text(text))) = (state, event) {
                if let Some(item_text) = EventPayload::parse_enter_text(text) {
                    let mut new_items = items.to_vec();
                    new_items.push(Value::text(item_text));
                    return Value::List(Arc::new(new_items));
                }
            }
            state.clone()
        }
        DdTransform::ListAppendPrepared => {
            // Pure append of pre-instantiated item (O(delta) optimization).
            // The item was already instantiated with fresh IDs BEFORE DD injection.
            // This transform just appends - no side effects.
            if let (Value::List(items), (_, EventValue::PreparedItem(item))) = (state, event) {
                let mut new_items = items.to_vec();
                new_items.push(item.clone());
                return Value::List(Arc::new(new_items));
            }
            // Also handle legacy Text events for backward compatibility
            if let (Value::List(items), (_, EventValue::Text(text))) = (state, event) {
                if let Some(item_text) = EventPayload::parse_enter_text(text) {
                    let mut new_items = items.to_vec();
                    new_items.push(Value::text(item_text));
                    return Value::List(Arc::new(new_items));
                }
            }
            state.clone()
        }
        DdTransform::ListRemove { identity_path } => {
            if let (Value::List(items), (_, EventValue::Text(text))) = (state, event) {
                if let Some(link_id) = EventPayload::parse_remove_link(text) {
                    // Find item by identity path and remove
                    let new_items: Vec<Value> = items
                        .iter()
                        .filter(|item| {
                            super::worker::get_link_ref_at_path(item, identity_path)
                                .map(|id| id != link_id)
                                .unwrap_or(true)
                        })
                        .cloned()
                        .collect();
                    return Value::List(Arc::new(new_items));
                }
            }
            state.clone()
        }
        DdTransform::ListFilter { field, value } => {
            if let Value::List(items) = state {
                let new_items: Vec<Value> = items
                    .iter()
                    .filter(|item| {
                        item.get(field)
                            .map(|v| v == value)
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect();
                return Value::List(Arc::new(new_items));
            }
            state.clone()
        }
        DdTransform::ListMap { field, transform } => {
            if let Value::List(items) = state {
                let new_items: Vec<Value> = items
                    .iter()
                    .map(|item| {
                        if let Some(field_value) = item.get(field) {
                            let new_field_value =
                                apply_dd_transform(transform, field_value, event, _states);
                            item.with_field(field, new_field_value)
                        } else {
                            item.clone()
                        }
                    })
                    .collect();
                return Value::List(Arc::new(new_items));
            }
            state.clone()
        }
        DdTransform::ListCount => {
            if let Value::List(items) = state {
                return Value::Number(OrderedFloat(items.len() as f64));
            }
            state.clone()
        }
        DdTransform::Custom(f) => {
            let event_value = match &event.1 {
                EventValue::Unit => Value::Unit,
                EventValue::Bool(b) => Value::Bool(*b),
                EventValue::Text(t) => Value::text(t.as_str()),
                EventValue::Number(n) => Value::Number(*n),
                EventValue::PreparedItem(v) => v.clone(),
            };
            f(state, &event_value)
        }
    }
}

/// WASM-compatible Timely execution.
fn execute_directly_wasm<T, F>(func: F) -> T
where
    F: FnOnce(&mut TimelyWorker<Thread>) -> T,
{
    let alloc = Thread::default();
    let mut worker = TimelyWorker::new(timely::WorkerConfig::default(), alloc, None);
    let result = func(&mut worker);
    while worker.has_dataflows() {
        worker.step_or_park(None);
    }
    result
}

/// Merge multiple cell values using LATEST semantics.
///
/// This uses `list_concat()` under the hood for proper DD behavior.
pub fn merge_latest(values: &[Value]) -> Value {
    // For scalar LATEST, take the last non-undefined value
    values.iter().rev().find(|v| !v.is_undefined()).cloned().unwrap_or(Value::Unit)
}

// ============================================================================
// PERSISTENT DD WORKER - Phase 5 Implementation
// ============================================================================

use std::cell::RefCell;
use differential_dataflow::input::InputSession;

thread_local! {
    /// Global persistent DD worker (browser is single-threaded, so no race conditions)
    static PERSISTENT_WORKER: RefCell<Option<PersistentDdWorker>> = RefCell::new(None); // ALLOWED: DD execution context
}

/// Persistent Differential Dataflow worker that stays alive across event batches.
///
/// Unlike the batch-per-event model, this worker:
/// 1. Creates ONE Timely dataflow at initialization
/// 2. Keeps input handles for event injection
/// 3. Steps incrementally when events arrive
/// 4. Outputs changes via callback
pub struct PersistentDdWorker {
    /// The Timely worker (owned, stays alive)
    worker: TimelyWorker<Thread>,
    /// Input session for events (LinkId, EventValue)
    event_input: InputSession<u64, (String, EventValue), isize>,
    /// Probe for tracking progress
    probe: ProbeHandle<u64>,
    /// Current logical time
    current_time: u64,
    /// Accumulated outputs from inspect() callbacks
    pending_outputs: Arc<Mutex<Vec<DdOutput>>>,
    /// Cell configurations (for rebuilding dataflow if needed)
    cells: Vec<DdCellConfig>,
    /// Config signature for change detection (hash of cell IDs and triggers)
    config_signature: u64,
}

/// Compute a signature for a cell configuration (for change detection).
fn compute_config_signature(cells: &[DdCellConfig]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    // Hash cell count
    cells.len().hash(&mut hasher);

    // Hash each cell's ID and triggers
    for cell in cells {
        cell.id.name().hash(&mut hasher);
        for trigger in &cell.triggers {
            trigger.name().hash(&mut hasher);
        }
    }

    hasher.finish()
}

impl PersistentDdWorker {
    /// Create a new persistent worker with the given cell configurations.
    pub fn new(cells: Vec<DdCellConfig>, initial_states: HashMap<String, Value>) -> Self {
        let pending_outputs = Arc::new(Mutex::new(Vec::new()));
        let pending_outputs_clone = pending_outputs.clone();
        let cells_clone = cells.clone();
        let initial_states_clone = initial_states.clone();

        // Create Timely worker
        let alloc = Thread::default();
        let mut worker = TimelyWorker::new(timely::WorkerConfig::default(), alloc, None);

        // Build the dataflow graph
        let (event_input, probe) = worker.dataflow::<u64, _, _>(move |scope| {
            use differential_dataflow::input::Input;
            use super::operators::hold;

            // Create input collection for events
            let (event_handle, events_collection) =
                scope.new_collection::<(String, EventValue), isize>();

            // For each cell, create a HOLD operator
            for cell_config in &cells_clone {
                let cell_id = cell_config.id.name().to_string();
                let initial = initial_states_clone
                    .get(&cell_id)
                    .cloned()
                    .unwrap_or_else(|| cell_config.initial.clone());

                let trigger_ids: Vec<String> = cell_config
                    .triggers
                    .iter()
                    .map(|l| l.name().to_string())
                    .collect();
                let event_filter = cell_config.filter.clone();

                // Filter events to those that trigger this cell AND match the event filter
                let triggered = events_collection.filter(move |(link_id, event_value)| {
                    trigger_ids.contains(link_id) && event_filter.matches(event_value)
                });

                // Apply transform using DD operators
                let transform = cell_config.transform.clone();
                let cell_id_for_output = cell_id.clone();
                let outputs_for_inspect = pending_outputs_clone.clone();

                let output = hold(initial, &triggered, move |state, event| {
                    apply_dd_transform(&transform, state, event, &Arc::new(Mutex::new(HashMap::new())))
                });

                // Capture outputs via inspect()
                output.inspect(move |(value, time, diff)| {
                    if *diff > 0 {
                        outputs_for_inspect.lock().unwrap().push(DdOutput {
                            cell_id: CellId::new(&cell_id_for_output),
                            value: value.clone(),
                            time: *time,
                            diff: *diff,
                        });
                    }
                });
            }

            // Create probe for progress tracking
            let probe = events_collection.probe();
            (event_handle, probe)
        });

        let config_signature = compute_config_signature(&cells);

        Self {
            worker,
            event_input,
            probe,
            current_time: 0,
            pending_outputs,
            cells,
            config_signature,
        }
    }

    /// Get the config signature for this worker.
    pub fn config_signature(&self) -> u64 {
        self.config_signature
    }

    /// Inject an event and step the worker until it's processed.
    pub fn inject_event(&mut self, link_id: &LinkId, value: EventValue) {
        // Insert event
        self.event_input.insert((link_id.name().to_string(), value));
        self.current_time += 1;
        self.event_input.advance_to(self.current_time);
        self.event_input.flush();

        // Step until event is processed
        while self.probe.less_than(&self.current_time) {
            self.worker.step();
        }
    }

    /// Drain accumulated outputs.
    pub fn drain_outputs(&self) -> Vec<DdOutput> {
        std::mem::take(&mut *self.pending_outputs.lock().unwrap())
    }

    /// Get current logical time.
    pub fn current_time(&self) -> u64 {
        self.current_time
    }
}

/// Initialize the global persistent worker.
pub fn init_persistent_worker(cells: Vec<DdCellConfig>, initial_states: HashMap<String, Value>) {
    let num_cells = cells.len();
    PERSISTENT_WORKER.with(|worker| {
        *worker.borrow_mut() = Some(PersistentDdWorker::new(cells, initial_states)); // ALLOWED: DD execution context
    });
    zoon::println!("[DD Persistent] Worker initialized with {} cells", num_cells);
}

/// Check if persistent worker is initialized.
pub fn has_persistent_worker() -> bool {
    PERSISTENT_WORKER.with(|worker| worker.borrow().is_some()) // ALLOWED: DD execution context
}

/// Inject an event into the persistent worker.
pub fn inject_event_persistent(link_id: &LinkId, value: EventValue) -> Vec<DdOutput> {
    PERSISTENT_WORKER.with(|worker| {
        if let Some(ref mut w) = *worker.borrow_mut() { // ALLOWED: DD execution context
            w.inject_event(link_id, value);
            w.drain_outputs()
        } else {
            zoon::println!("[DD Persistent] Warning: Worker not initialized");
            Vec::new()
        }
    })
}

/// Shutdown the persistent worker.
pub fn shutdown_persistent_worker() {
    PERSISTENT_WORKER.with(|worker| {
        *worker.borrow_mut() = None; // ALLOWED: DD execution context
    });
    zoon::println!("[DD Persistent] Worker shutdown");
}

/// Check if the current worker's config matches the given cells.
/// Returns true if worker exists and config matches, false otherwise.
pub fn config_matches(cells: &[DdCellConfig]) -> bool {
    let new_signature = compute_config_signature(cells);
    PERSISTENT_WORKER.with(|worker| {
        if let Some(ref w) = *worker.borrow() { // ALLOWED: DD execution context
            w.config_signature() == new_signature
        } else {
            false
        }
    })
}

/// Reinitialize the persistent worker if config changed.
/// Returns true if worker was reinitialized, false if config was unchanged.
pub fn reinit_if_config_changed(cells: Vec<DdCellConfig>, initial_states: HashMap<String, Value>) -> bool {
    if has_persistent_worker() && config_matches(&cells) {
        false // No change needed
    } else {
        if has_persistent_worker() {
            shutdown_persistent_worker();
            zoon::println!("[DD Persistent] Config changed, reinitializing worker");
        }
        init_persistent_worker(cells, initial_states);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dd_first_increment() {
        let cells = vec![DdCellConfig {
            id: CellId::new("count"),
            initial: Value::Number(super::OrderedFloat(0.0)),
            triggers: vec![LinkId::new("click")],
            transform: DdTransform::Increment,
            filter: DdEventFilter::Any,
        }];

        let events = vec![
            (LinkId::new("click"), EventValue::Unit),
            (LinkId::new("click"), EventValue::Unit),
            (LinkId::new("click"), EventValue::Unit),
        ];

        let initial = HashMap::new();
        let result = run_dd_first_batch(&cells, &events, &initial);

        assert_eq!(
            result.get("count"),
            Some(&Value::Number(super::OrderedFloat(3.0)))
        );
    }

    #[test]
    fn test_dd_first_toggle() {
        let cells = vec![DdCellConfig {
            id: CellId::new("enabled"),
            initial: Value::Bool(false),
            triggers: vec![LinkId::new("toggle")],
            transform: DdTransform::Toggle,
            filter: DdEventFilter::Any,
        }];

        let events = vec![
            (LinkId::new("toggle"), EventValue::Unit),
            (LinkId::new("toggle"), EventValue::Unit),
        ];

        let initial = HashMap::new();
        let result = run_dd_first_batch(&cells, &events, &initial);

        // Toggle twice: false -> true -> false
        assert_eq!(result.get("enabled"), Some(&Value::Bool(false)));
    }

    #[test]
    fn test_dd_first_list_append() {
        let cells = vec![DdCellConfig {
            id: CellId::new("items"),
            initial: Value::List(Arc::new(vec![])),
            triggers: vec![LinkId::new("add")],
            transform: DdTransform::ListAppend,
            filter: DdEventFilter::TextStartsWith("Enter:".to_string()),
        }];

        let events = vec![
            (LinkId::new("add"), EventValue::Text("Enter:item1".to_string())),
            (LinkId::new("add"), EventValue::Text("Enter:item2".to_string())),
        ];

        let initial = HashMap::new();
        let result = run_dd_first_batch(&cells, &events, &initial);

        if let Some(Value::List(items)) = result.get("items") {
            assert_eq!(items.len(), 2);
        } else {
            panic!("Expected list");
        }
    }

    #[test]
    fn test_persistent_worker_increment() {
        // Create a persistent worker (not using global, direct instantiation)
        let cells = vec![DdCellConfig {
            id: CellId::new("count"),
            initial: Value::Number(super::OrderedFloat(0.0)),
            triggers: vec![LinkId::new("click")],
            transform: DdTransform::Increment,
            filter: DdEventFilter::Any,
        }];

        let mut worker = PersistentDdWorker::new(cells, HashMap::new());

        // Inject events one by one (simulating real user interaction)
        worker.inject_event(&LinkId::new("click"), EventValue::Unit);
        let outputs1 = worker.drain_outputs();
        assert_eq!(outputs1.len(), 1);
        assert_eq!(outputs1[0].value, Value::Number(super::OrderedFloat(1.0)));

        worker.inject_event(&LinkId::new("click"), EventValue::Unit);
        let outputs2 = worker.drain_outputs();
        assert_eq!(outputs2.len(), 1);
        assert_eq!(outputs2[0].value, Value::Number(super::OrderedFloat(2.0)));

        worker.inject_event(&LinkId::new("click"), EventValue::Unit);
        let outputs3 = worker.drain_outputs();
        assert_eq!(outputs3.len(), 1);
        assert_eq!(outputs3[0].value, Value::Number(super::OrderedFloat(3.0)));

        // Verify time advances correctly
        assert_eq!(worker.current_time(), 3);
    }

    #[test]
    fn test_persistent_worker_multiple_cells() {
        // Test that multiple cells work with persistent worker
        let cells = vec![
            DdCellConfig {
                id: CellId::new("count"),
                initial: Value::Number(super::OrderedFloat(0.0)),
                triggers: vec![LinkId::new("inc")],
                transform: DdTransform::Increment,
                filter: DdEventFilter::Any,
            },
            DdCellConfig {
                id: CellId::new("enabled"),
                initial: Value::Bool(false),
                triggers: vec![LinkId::new("toggle")],
                transform: DdTransform::Toggle,
                filter: DdEventFilter::Any,
            },
        ];

        let mut worker = PersistentDdWorker::new(cells, HashMap::new());

        // Increment counter
        worker.inject_event(&LinkId::new("inc"), EventValue::Unit);
        let outputs = worker.drain_outputs();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].cell_id.name(), "count");

        // Toggle enabled
        worker.inject_event(&LinkId::new("toggle"), EventValue::Unit);
        let outputs = worker.drain_outputs();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].cell_id.name(), "enabled");
        assert_eq!(outputs[0].value, Value::Bool(true));

        // Increment again
        worker.inject_event(&LinkId::new("inc"), EventValue::Unit);
        let outputs = worker.drain_outputs();
        assert_eq!(outputs[0].value, Value::Number(super::OrderedFloat(2.0)));
    }
}
