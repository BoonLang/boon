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
//! 3. Uses `capture()` for pure output observation (no side effects)
//! 4. Steps the worker periodically via async loop
//!
//! # Benefits
//!
//! - O(delta) complexity for all operations
//! - Arrangements persist across batches (no rebuild cost)
//! - True incremental computation
//! - Pure dataflow (no Mutex/inspect side effects)
//!
//! # WASM Compatibility
//!
//! This uses a single-threaded Timely worker with manual stepping,
//! compatible with the browser's event loop via `spawn_local`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[allow(unused_imports)]
use super::super::dd_log;
use differential_dataflow::input::Input;
use differential_dataflow::collection::AsCollection;
use timely::communication::allocator::thread::Thread;
use timely::dataflow::channels::pact::Pipeline;
use timely::dataflow::operators::Operator;
use timely::dataflow::operators::probe::Handle as ProbeHandle;
use timely::dataflow::operators::Capture;
use timely::dataflow::operators::capture::Extract;
use timely::worker::Worker as TimelyWorker;

use super::operators::hold_with_output;
use super::types::{CellId, EventValue, Key, LinkId, BoolTag, EventFilter};
use super::value::{
    attach_or_validate_item_key, contains_placeholder, ensure_unique_item_keys, extract_item_key,
    CellUpdate, CollectionId, TemplateValue, Value,
};
use super::collection_ops::{CollectionOp, CollectionOpConfig, ComputedTextPart};
use super::value::{CollectionHandle, WhileArm, WhileConfig};

// Note: EventFilter removed - now using consolidated EventFilter from types.rs

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
    pub filter: EventFilter,
}

/// Configuration for DD-native collection ops.
#[derive(Clone, Debug)]
pub struct DdCollectionConfig {
    /// Collection operations to apply (filter/map/count/etc.).
    pub ops: Vec<CollectionOpConfig>,
    /// Initial collections (list literals evaluated at startup).
    pub initial_collections: HashMap<CollectionId, Vec<Value>>,
    /// Mapping from collection id to source list cell id (reactive list sources).
    pub collection_sources: HashMap<CollectionId, String>,
}

/// Transforms for DD-first cells.
///
/// Unlike `StateTransform` which operates imperatively,
/// these transforms map to actual DD operators.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum DdTransform {
    /// Increment numeric value: `state + 1`
    Increment,
    /// Toggle boolean: `!state`
    Toggle,
    /// Set to constant value
    SetValue(Value),
    /// Append a pre-instantiated item to list (pure, O(delta) optimization).
    /// The item is passed via EventValue::PreparedItem with fresh IDs and initializations.
    /// This transform just appends - no side effects.
    ListAppendPrepared,
    /// Append a pre-instantiated item or clear on unit event.
    ListAppendPreparedWithClear { clear_link_id: String },
    /// Append raw event text as a simple list item (no template, no pre-instantiation).
    ListAppendSimple,
    /// Append raw event text or clear on unit event (no template).
    ListAppendSimpleWithClear { clear_link_id: String },
    /// Remove from list by identity.
    /// This uses LinkRef identity (link:ID) emitted by events.
    ListRemove,
    /// Identity transform - returns state unchanged.
    /// Replaces Custom(|state, _| state.clone()) for serializable/deterministic transforms.
    Identity,
    /// Apply link mappings before base transform (pure DD link handling).
    /// base_triggers + base_filter gate the base transform; mappings match independently.
    WithLinkMappings {
        base: Box<DdTransform>,
        base_triggers: Vec<LinkId>,
        base_filter: EventFilter,
        mappings: Vec<LinkCellMapping>,
    },
}

impl std::fmt::Debug for DdTransform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Increment => write!(f, "Increment"),
            Self::Toggle => write!(f, "Toggle"),
            Self::SetValue(v) => write!(f, "SetValue({:?})", v),
            Self::ListAppendPrepared => write!(f, "ListAppendPrepared"),
            Self::ListAppendPreparedWithClear { clear_link_id } => {
                write!(f, "ListAppendPreparedWithClear {{ clear_link_id: {:?} }}", clear_link_id)
            }
            Self::ListAppendSimple => write!(f, "ListAppendSimple"),
            Self::ListAppendSimpleWithClear { clear_link_id } => {
                write!(f, "ListAppendSimpleWithClear {{ clear_link_id: {:?} }}", clear_link_id)
            }
            Self::ListRemove => write!(f, "ListRemove"),
            Self::Identity => write!(f, "Identity"),
            Self::WithLinkMappings { base, base_triggers, base_filter, mappings } => {
                write!(
                    f,
                    "WithLinkMappings {{ base: {:?}, base_triggers: {:?}, base_filter: {:?}, mappings: {:?} }}",
                    base, base_triggers, base_filter, mappings
                )
            }
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
    current_state: CellStateRef<'_>,
    event_value: &EventValue,
    event_link_id: &str,
    cell_id: &str,
) -> CellUpdate {
    match action {
        LinkAction::BoolToggle => {
            let current_value = current_state.as_scalar("LinkAction::BoolToggle");
            let value = match current_value {
                Value::Bool(b) => Value::Bool(!*b),
                // Handle Boon's Tagged booleans using type-safe BoolTag
                Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag) => {
                    match BoolTag::from_tag(tag) {
                        Some(BoolTag::True) => Value::Bool(false),
                        Some(BoolTag::False) => Value::Bool(true),
                        None => panic!("[DD LinkAction] BoolToggle invalid BoolTag for {}", event_link_id),
                    }
                }
                other => panic!("[DD LinkAction] BoolToggle expected Bool, found {:?} for {}", other, event_link_id),
            };
            CellUpdate::SetValue {
                cell_id: Arc::from(cell_id),
                value,
            }
        }
        LinkAction::SetTrue => CellUpdate::SetValue {
            cell_id: Arc::from(cell_id),
            value: Value::Bool(true),
        },
        LinkAction::SetFalse => CellUpdate::SetValue {
            cell_id: Arc::from(cell_id),
            value: Value::Bool(false),
        },
        LinkAction::AddValue(v) => {
            if current_state.is_list_like() {
                panic!(
                    "[DD LinkAction] AddValue on list cell '{}' is forbidden; use list diffs",
                    event_link_id
                );
            }
            let addend = match v {
                Value::Number(n) => n.0,
                other => {
                    panic!(
                        "[DD LinkAction] AddValue expects numeric constant, found {:?} for {}",
                        other,
                        event_link_id
                    );
                }
            };
            let current_value = current_state.as_scalar("LinkAction::AddValue");
            let value = match current_value {
                Value::Number(n) => Value::float(n.0 + addend),
                other => panic!(
                    "[DD LinkAction] AddValue expected Number state, found {:?} for {}",
                    other,
                    event_link_id
                ),
            };
            CellUpdate::SetValue {
                cell_id: Arc::from(cell_id),
                value,
            }
        }
        LinkAction::HoverState => {
            // Extract boolean from event
            if let EventValue::Bool(b) = event_value {
                CellUpdate::SetValue {
                    cell_id: Arc::from(cell_id),
                    value: Value::Bool(*b),
                }
            } else {
                panic!("[DD LinkAction] HoverState expected Bool event for {}", event_link_id);
            }
        }
        LinkAction::SetText => {
            let value = match event_value {
                EventValue::KeyDown { key: super::types::Key::Enter, text: Some(text) } => {
                    Value::text(text.as_str())
                }
                EventValue::Text(text) => Value::text(text.as_str()),
                EventValue::PreparedItem { source_text: Some(text), .. } => {
                    Value::text(text.as_str())
                }
                EventValue::KeyDown { key: super::types::Key::Enter, text: None } => {
                    panic!("[DD LinkAction] SetText expected Enter key with text for {}", event_link_id)
                }
                _ => panic!("[DD LinkAction] SetText expected text payload for {}", event_link_id),
            };
            CellUpdate::SetValue {
                cell_id: Arc::from(cell_id),
                value,
            }
        }
        LinkAction::SetValue(v) => {
            if current_state.is_list_like() {
                panic!(
                    "[DD LinkAction] SetValue on list cell '{}' is forbidden; use list diffs",
                    event_link_id
                );
            }
            if v.is_list_like() {
                panic!(
                    "[DD LinkAction] SetValue to list for '{}' is forbidden; use list diffs",
                    event_link_id
                );
            }
            CellUpdate::SetValue {
                cell_id: Arc::from(cell_id),
                value: v.clone(),
            }
        }
        LinkAction::RemoveListItem { list_cell_id } => {
            // Emit a list diff keyed by the identity link id.
            // Pure DD: no list cloning or path scanning here.
            if !current_state.is_list_like() {
                panic!("[DD LinkAction] RemoveListItem expected list for {}", event_link_id);
            }
            let key = format!("link:{}", event_link_id);
            CellUpdate::ListRemoveByKey {
                cell_id: Arc::from(list_cell_id.name()),
                key: Arc::from(key),
            }
        }
    }
}

fn build_prepared_list_append_output(
    cell_id: &str,
    item: &Value,
    initializations: &Vec<(String, Value)>,
) -> CellUpdate {
    if initializations.is_empty() {
        return CellUpdate::ListPush {
            cell_id: Arc::from(cell_id),
            item: item.clone(),
        };
    }

    let mut updates: Vec<CellUpdate> = Vec::with_capacity(1 + initializations.len());
    updates.push(CellUpdate::ListPush {
        cell_id: Arc::from(cell_id),
        item: item.clone(),
    });
    for (init_id, init_value) in initializations {
        updates.push(CellUpdate::SetValue {
            cell_id: Arc::from(init_id.as_str()),
            value: init_value.clone(),
        });
    }
    CellUpdate::Multi(updates)
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

    // Restrict SetText to text-carrying events to avoid ambiguous matches.
    if matches!(mapping.action, LinkAction::SetText) {
        return matches!(event_value, EventValue::Text(_))
            || matches!(event_value, EventValue::PreparedItem { source_text: Some(_), .. });
    }

    // Check key filter if present
    if let Some(ref keys) = mapping.key_filter {
        return matches!(event_value, EventValue::KeyDown { key, .. } if keys.contains(key))
            || matches!(event_value, EventValue::PreparedItem { source_key: Some(key), .. } if keys.contains(key));
    }

    true
}

enum CellStateRef<'a> {
    Scalar(&'a Value),
    List(&'a ListState),
}

impl<'a> CellStateRef<'a> {
    fn as_scalar(&self, context: &str) -> &'a Value {
        match self {
            CellStateRef::Scalar(value) => value,
            CellStateRef::List(_) => {
                panic!("[DD State] {} expected scalar state, found list state", context);
            }
        }
    }

    fn is_list_like(&self) -> bool {
        matches!(self, CellStateRef::List(_))
    }
}

/// Output from the DD-first dataflow.
#[derive(Clone, Debug)]
pub struct DdOutput {
    /// Cell ID that changed
    pub cell_id: CellId,
    /// New value
    pub value: CellUpdate,
    /// Logical timestamp
    pub time: u64,
    /// Diff (+1 for insert, -1 for retraction)
    pub diff: isize,
}

/// Result from DD-first batch execution.
#[derive(Clone, Debug)]
pub struct DdBatchResult {
    pub(crate) scalar_states: HashMap<String, Value>,
    pub(crate) list_states: HashMap<String, ListState>,
}

#[derive(Clone)]
pub(crate) enum CellState {
    Scalar(Value),
    List(ListState),
}

impl CellState {
    fn as_scalar(&self, context: &str) -> &Value {
        match self {
            CellState::Scalar(value) => value,
            CellState::List(_) => {
                panic!("[DD State] {} expected scalar state, found list state", context);
            }
        }
    }

    fn is_list(&self) -> bool {
        matches!(self, CellState::List(_))
    }
}

/// Run a DD-first computation for a single batch.
///
/// This is a bridge between the current batch model and the target persistent model.
/// It uses actual DD operators for all transformations.
///
/// # Pure DD Architecture
///
/// This uses `capture()` instead of `inspect()` for output observation:
/// - NO Mutex locks during dataflow execution
/// - Outputs flow through mpsc channel (pure message passing)
pub fn run_dd_first_batch(
    cells: Vec<DdCellConfig>,
    collections: DdCollectionConfig,
    events: Vec<(LinkId, EventValue)>,
    initial_states: &HashMap<String, Value>,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
) -> DdBatchResult {
    let initial_collection_items = initial_collection_items.clone();
    let initial_states_clone: HashMap<String, Value> = initial_states
        .iter()
        .map(|(cell_id, value)| {
            (cell_id.clone(), validate_collection_initial(cell_id, value.clone()))
        })
        .collect();
    let mut list_cell_hints = HashSet::new();
    for (cell_id, value) in &initial_states_clone {
        if matches!(value, Value::List(_)) {
            list_cell_hints.insert(cell_id.clone());
        }
    }
    let (list_cells, derived_list_outputs) = list_cells_from_configs(&cells, &collections, &list_cell_hints);
    let list_cells_for_closure = list_cells.clone();

    // Execute DD computation with capture() for pure output observation
    execute_directly_wasm(move |worker| {
        let (mut event_input, probe, outputs_rx) = worker.dataflow::<u64, _, _>(|scope| {
            // Create input collection for events
            let (event_handle, events_collection) =
                scope.new_collection::<(String, EventValue), isize>();

            // Collect all cell outputs to merge into single capture stream
            let mut all_outputs: Vec<differential_dataflow::collection::VecCollection<_, TaggedCellOutput>> = Vec::new();

            // For each cell, create a HOLD operator
            for cell_config in &cells {
                let cell_id = cell_config.id.name().to_string();
                let initial_value = initial_states_clone
                    .get(&cell_id)
                    .cloned()
                    .unwrap_or_else(|| cell_config.initial.clone());
                let initial_value = validate_collection_initial(&cell_id, initial_value);
                let initial = cell_state_from_value(
                    &cell_id,
                    initial_value,
                    &list_cells_for_closure,
                    &initial_collection_items,
                );

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

                // Apply transform using DD operators (pure, no side effects)
                let transform = cell_config.transform.clone();
                let cell_id_for_tag = cell_id.clone();
                let cell_id_for_transform = cell_id.clone();  // For O(delta) list ops

                let output = hold_with_output(initial, &triggered, move |state, event| {
                    apply_dd_transform_with_state(&transform, state, event, &cell_id_for_transform)
                }).filter(|update| !matches!(update, CellUpdate::NoOp));

                // Tag output with cell ID for identification
                let tagged_output = output.map(move |value| TaggedCellOutput {
                    cell_id: cell_id_for_tag.clone(),
                    value,
                });

                all_outputs.push(tagged_output);
            }

            // Merge all outputs from HOLD cells
            let merged = if all_outputs.is_empty() {
                scope.new_collection::<TaggedCellOutput, isize>().1
            } else {
                let first = all_outputs.remove(0);
                all_outputs.into_iter().fold(first, |acc, c| acc.concat(&c))
            };

            // Attach DD-native collection ops
            let collection_outputs = build_collection_op_outputs(
                scope,
                &merged,
                &collections,
                &initial_collection_items,
                &initial_states_clone,
            );

            let merged = merged.concat(&collection_outputs);

            // Use capture() for pure output observation - NO Mutex, NO side effects!
            let outputs_rx = merged.inner.capture();

            // Create probe for progress tracking
            let probe = events_collection.probe();
            (event_handle, probe, outputs_rx)
        });

        // Process init-only sources at time 0 (collection op initialization).
        while worker.step() {}

        // Insert events
        for (i, (link_id, event_value)) in events.into_iter().enumerate() {
            event_input.insert((link_id.name().to_string(), event_value));
            event_input.advance_to((i + 1) as u64);
            event_input.flush();

            // Step until processed
            while probe.less_than(&((i + 1) as u64)) {
                worker.step();
            }
        }

        // Extract outputs from capture channel AFTER all events processed
        let captured: Vec<(u64, Vec<(TaggedCellOutput, u64, isize)>)> = outputs_rx.extract();

        // Build initial map for fail-fast state initialization
        let mut initial_by_id: HashMap<String, Value> = cells
            .iter()
            .map(|cell| {
                let cell_id = cell.id.name();
                (cell_id.to_string(), validate_collection_initial(&cell_id, cell.initial.clone()))
            })
            .collect();

        // Build final states from captured outputs by applying diffs
        let mut scalar_states = initial_states_clone.clone();
        strip_list_cells_from_states(&mut scalar_states, &list_cells);
        let mut list_states = build_initial_list_states(
            &list_cells,
            &derived_list_outputs,
            &initial_states_clone,
            &initial_by_id,
            &initial_collection_items,
        );
        for (_time, items) in captured {
            for (tagged, _t, diff) in items {
                if diff > 0 {
                    let cell_id = tagged.cell_id;
                    apply_output_to_state_maps_for_batch(
                        &list_cells,
                        &derived_list_outputs,
                        &mut list_states,
                        &mut scalar_states,
                        &cell_id,
                        &tagged.value,
                        &initial_by_id,
                        &initial_collection_items,
                    );
                }
            }
        }

        DdBatchResult {
            scalar_states,
            list_states,
        }
    })
}

fn list_cells_from_configs(
    cells: &[DdCellConfig],
    collections: &DdCollectionConfig,
    list_cell_hints: &HashSet<String>,
) -> (HashSet<String>, HashSet<String>) {
    let mut list_cells: HashSet<String> = list_cell_hints.iter().cloned().collect();
    for cell in cells {
        let is_list_transform = match &cell.transform {
            DdTransform::ListAppendPrepared
            | DdTransform::ListAppendPreparedWithClear { .. }
            | DdTransform::ListAppendSimple
            | DdTransform::ListAppendSimpleWithClear { .. }
            | DdTransform::ListRemove => true,
            DdTransform::WithLinkMappings { base, .. } => matches!(
                base.as_ref(),
                DdTransform::ListAppendPrepared
                    | DdTransform::ListAppendPreparedWithClear { .. }
                    | DdTransform::ListAppendSimple
                    | DdTransform::ListAppendSimpleWithClear { .. }
                    | DdTransform::ListRemove
            ),
            _ => false,
        };
        if is_list_transform {
            list_cells.insert(cell.id.name());
        }
    }
    for cell_id in collections.collection_sources.values() {
        list_cells.insert(cell_id.clone());
    }

    let mut derived_outputs = HashSet::new();
    for op in &collections.ops {
        if matches!(
            op.op,
            CollectionOp::Filter { .. } | CollectionOp::Map { .. } | CollectionOp::Concat { .. }
        ) {
            let output_id = op.output_id.to_string();
            derived_outputs.insert(output_id.clone());
            list_cells.insert(output_id);
        }
    }

    (list_cells, derived_outputs)
}

fn list_state_from_value(
    cell_id: &str,
    value: &Value,
    context: &str,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
) -> ListState {
    match value {
        Value::List(handle) => {
            let existing = handle.cell_id.as_deref().unwrap_or_else(|| {
                panic!("[DD State] Missing collection cell_id for '{}'", cell_id);
            });
            if existing != cell_id {
                panic!(
                    "[DD State] Collection cell_id mismatch: expected '{}', found '{}'",
                    cell_id, existing
                );
            }
            let items = initial_collection_items.get(&handle.id).unwrap_or_else(|| {
                panic!("[DD State] Missing initial items for collection '{}' ({})", cell_id, context);
            });
            ListState::new(items.clone(), context)
        }
        other => panic!("[DD State] List cell '{}' must be List, found {:?}", cell_id, other),
    }
}

pub(crate) fn cell_state_from_value(
    cell_id: &str,
    value: Value,
    list_cells: &HashSet<String>,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
) -> CellState {
    if list_cells.contains(cell_id) {
        let list_state = list_state_from_value(cell_id, &value, cell_id, initial_collection_items);
        return CellState::List(list_state);
    }
    match value {
        Value::List(_) => {
            panic!(
                "[DD State] Non-list cell '{}' stored as list-like state",
                cell_id
            );
        }
        other => CellState::Scalar(other),
    }
}

fn build_initial_list_states(
    list_cells: &HashSet<String>,
    derived_list_outputs: &HashSet<String>,
    initial_states: &HashMap<String, Value>,
    initial_by_id: &HashMap<String, Value>,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
) -> HashMap<String, ListState> {
    let mut list_states = HashMap::new();
    for cell_id in list_cells {
        if let Some(value) = initial_states.get(cell_id) {
            list_states.insert(
                cell_id.clone(),
                list_state_from_value(cell_id, value, cell_id, initial_collection_items),
            );
            continue;
        }
        if let Some(value) = initial_by_id.get(cell_id) {
            list_states.insert(
                cell_id.clone(),
                list_state_from_value(cell_id, value, cell_id, initial_collection_items),
            );
            continue;
        }
        if derived_list_outputs.contains(cell_id) {
            list_states.insert(cell_id.clone(), ListState::new(Vec::new(), cell_id));
            continue;
        }
        panic!("[DD State] Missing initial list state for '{}'", cell_id);
    }
    list_states
}

fn strip_list_cells_from_states(cell_states: &mut HashMap<String, Value>, list_cells: &HashSet<String>) {
    for cell_id in list_cells {
        cell_states.remove(cell_id);
    }
}

fn apply_output_to_state_maps_for_batch(
    list_cells: &HashSet<String>,
    derived_list_outputs: &HashSet<String>,
    list_states: &mut HashMap<String, ListState>,
    cell_states: &mut HashMap<String, Value>,
    cell_id: &str,
    output: &CellUpdate,
    initial_by_id: &HashMap<String, Value>,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
) {
    match output {
        CellUpdate::Multi(updates) => {
            for update in updates.iter() {
                let update_cell_id = update.cell_id().unwrap_or_else(|| {
                    panic!("[DD State] Missing cell id for update {:?}", update);
                });
                apply_output_to_state_maps_for_batch(
                    list_cells,
                    derived_list_outputs,
                    list_states,
                    cell_states,
                    update_cell_id,
                    update,
                    initial_by_id,
                    initial_collection_items,
                );
            }
        }
        CellUpdate::NoOp => {}
        _ => {
            let update_cell_id = output.cell_id().unwrap_or_else(|| {
                panic!("[DD State] Missing cell id for update {:?}", output);
            });
            if update_cell_id != cell_id {
                panic!(
                    "[DD State] Output cell '{}' does not match '{}': {:?}",
                    update_cell_id, cell_id, output
                );
            }
            if list_cells.contains(cell_id) {
                apply_list_output_to_state_for_batch(
                    list_states,
                    derived_list_outputs,
                    cell_id,
                    output,
                    initial_by_id,
                    initial_collection_items,
                );
            } else {
                apply_output_to_states(cell_states, output, initial_by_id);
            }
        }
    }
}

fn apply_list_output_to_state_for_batch(
    list_states: &mut HashMap<String, ListState>,
    derived_list_outputs: &HashSet<String>,
    cell_id: &str,
    output: &CellUpdate,
    initial_by_id: &HashMap<String, Value>,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
) {
    let list_state = list_states.entry(cell_id.to_string()).or_insert_with(|| {
        if let Some(initial) = initial_by_id.get(cell_id) {
            list_state_from_value(cell_id, initial, cell_id, initial_collection_items)
        } else if derived_list_outputs.contains(cell_id) {
            ListState::new(Vec::new(), cell_id)
        } else {
            panic!("[DD State] Missing initial list state for '{}'", cell_id);
        }
    });

    match output {
        CellUpdate::ListPush { cell_id: diff_cell_id, item } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListPush");
            list_state.push(item.clone(), "list push");
        }
        CellUpdate::ListInsertAt { cell_id: diff_cell_id, index, item } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListInsertAt");
            list_state.insert(*index, item.clone(), "list insert");
        }
        CellUpdate::ListRemoveAt { cell_id: diff_cell_id, index } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListRemoveAt");
            list_state.remove_at(*index, "list remove");
        }
        CellUpdate::ListRemoveByKey { cell_id: diff_cell_id, key } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListRemoveByKey");
            list_state.remove_by_key(key.as_ref(), "list remove by key");
        }
        CellUpdate::ListRemoveBatch { cell_id: diff_cell_id, keys } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListRemoveBatch");
            list_state.remove_batch(keys, "list remove batch");
        }
        CellUpdate::ListClear { cell_id: diff_cell_id } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListClear");
            list_state.clear();
        }
        CellUpdate::ListItemUpdate { cell_id: diff_cell_id, key, field_path, new_value } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListItemUpdate");
            list_state.update_field(key.as_ref(), field_path, new_value, "list item update");
        }
        CellUpdate::SetValue { .. } => {
            panic!(
                "[DD State] List cell '{}' received non-diff output {:?}",
                cell_id, output
            );
        }
        CellUpdate::Multi(_) => {
            panic!("[DD State] Multi update must be expanded before list state update");
        }
        CellUpdate::NoOp => {}
    }
}

/// Apply a DD transform to produce new state.
///
/// # Pure Function
///
/// This is a pure function with no side effects - it takes state and event,
/// returns a CellUpdate. No Mutex, no global state access.
pub(crate) fn apply_dd_transform(
    transform: &DdTransform,
    state: &CellState,
    event: &(String, EventValue),
    cell_id: &str,
) -> CellUpdate {
    let (event_link_id, event_value) = event;
    let event_link_id = event_link_id.as_str();
    match transform {
        DdTransform::WithLinkMappings { base, base_triggers, base_filter, mappings } => {
            let mut matched = None;
            for mapping in mappings {
                if mapping_matches_event(mapping, event_link_id, event_value) {
                    if matched.is_some() {
                        panic!("[DD LinkMapping] Multiple mappings matched for link {}", event_link_id);
                    }
                    matched = Some(mapping);
                }
            }

            if let Some(mapping) = matched {
                if mapping.cell_id.name() != cell_id {
                    panic!(
                        "[DD LinkMapping] Mapping cell_id {} does not match transform cell {}",
                        mapping.cell_id.name(),
                        cell_id
                    );
                }
                let current = match state {
                    CellState::Scalar(value) => CellStateRef::Scalar(value),
                    CellState::List(list_state) => CellStateRef::List(list_state),
                };
                return apply_link_action(&mapping.action, current, event_value, event_link_id, cell_id);
            }

            // Fall back to base transform if this event targets base triggers + filter.
            if base_triggers.iter().any(|id| id.name() == event_link_id) && base_filter.matches(event_value) {
                return apply_dd_transform(base, state, event, cell_id);
            }
            CellUpdate::NoOp
        }
        DdTransform::Increment => {
            let state = state.as_scalar("Increment");
            let value = match state {
                Value::Number(n) => Value::float(n.0 + 1.0),
                // Unit = "not yet rendered" initial state for timer cells.
                // First timer tick increments from 0 → 1.
                Value::Unit => Value::float(1.0),
                other => panic!("[DD Dataflow] Increment expected Number, found {:?} in {}", other, cell_id),
            };
            CellUpdate::SetValue { cell_id: Arc::from(cell_id), value }
        }
        DdTransform::Toggle => {
            let state = state.as_scalar("Toggle");
            let value = match state {
                Value::Bool(b) => Value::Bool(!*b),
                // Use type-safe BoolTag instead of string comparison
                Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag) => {
                    match BoolTag::from_tag(tag) {
                        Some(BoolTag::True) => Value::Bool(false),
                        Some(BoolTag::False) => Value::Bool(true),
                        None => panic!("[DD Dataflow] Toggle invalid BoolTag for {}", cell_id),
                    }
                }
                other => panic!("[DD Dataflow] Toggle expected Bool, found {:?} in {}", other, cell_id),
            };
            CellUpdate::SetValue { cell_id: Arc::from(cell_id), value }
        }
        DdTransform::SetValue(v) => {
            if state.is_list() {
                panic!(
                    "[DD Dataflow] SetValue on list-like cell '{}' is forbidden; use list diffs",
                    cell_id
                );
            }
            if matches!(v, Value::List(_)) {
                panic!(
                    "[DD Dataflow] SetValue to list for '{}' is forbidden; use list diffs",
                    cell_id
                );
            }
            CellUpdate::SetValue { cell_id: Arc::from(cell_id), value: v.clone() }
        }
        DdTransform::ListAppendPrepared => {
            if !state.is_list() {
                panic!("[DD Dataflow] ListAppendPrepared expected list state for {}", cell_id);
            }
            if let EventValue::PreparedItem { item, initializations, .. } = event_value {
                return build_prepared_list_append_output(cell_id, item, initializations);
            }
            panic!(
                "[DD Dataflow] ListAppendPrepared expected PreparedItem; pre-instantiation missing for {} (event={:?})",
                cell_id, event_value
            );
        }
        DdTransform::ListAppendPreparedWithClear { clear_link_id } => {
            if !state.is_list() {
                panic!("[DD Dataflow] ListAppendPreparedWithClear expected list state for {}", cell_id);
            }
            if let EventValue::PreparedItem { item, initializations, .. } = event_value {
                return build_prepared_list_append_output(cell_id, item, initializations);
            }
            if matches!(event_value, EventValue::Unit) && clear_link_id == event_link_id {
                return CellUpdate::ListClear { cell_id: Arc::from(cell_id) };
            }
            panic!(
                "[DD Dataflow] ListAppendPreparedWithClear expected PreparedItem or clear Unit; missing pre-instantiation for {} (event={:?})",
                cell_id, event_value
            );
        }
        // TODO(shopping_list test): ListAppendSimple/WithClear currently BROKEN for the
        // shopping_list example. The Enter key match below correctly rejects on_change Text
        // events (which were causing per-keystroke appends), but now Enter doesn't work either.
        //
        // Root cause: Store-level LINKs use a SINGLE LinkRef for all event types (key_down,
        // change, blur). Both on_change and on_key_down fire on the same link ID (e.g.,
        // "store.elements.item_input"). The EventFilter::Any on ListAppendSimpleWithClear
        // lets everything through.
        //
        // Debug steps:
        // 1. Check if DdKey::Enter maps correctly to Key::Enter in EventValue
        //    (fire_global_key_down in io/inputs.rs sends DdKey::Enter → EventValue::key_down())
        // 2. Check if get_dd_text_input_value() in bridge.rs returns the typed text
        // 3. Enable LOG_DD_DEBUG=true in mod.rs and check console after pressing Enter
        // 4. The proper fix may be to use separate link IDs per event type at store level,
        //    or to use the Boon-level WHEN { Enter => ... } as the gatekeeper instead of
        //    hardcoding Enter filtering in the DD transform (this is business logic in engine).
        DdTransform::ListAppendSimple => {
            if !state.is_list() {
                panic!("[DD Dataflow] ListAppendSimple expected list state for {}", cell_id);
            }
            let text = match event_value {
                EventValue::KeyDown { key: Key::Enter, text: Some(t) } => t.clone(),
                _ => return CellUpdate::NoOp,
            };
            if text.trim().is_empty() {
                return CellUpdate::NoOp;
            }
            CellUpdate::ListPush {
                cell_id: Arc::from(cell_id),
                item: Value::Text(Arc::from(text)),
            }
        }
        DdTransform::ListAppendSimpleWithClear { clear_link_id } => {
            if !state.is_list() {
                panic!("[DD Dataflow] ListAppendSimpleWithClear expected list state for {}", cell_id);
            }
            if matches!(event_value, EventValue::Unit) && clear_link_id == event_link_id {
                return CellUpdate::ListClear { cell_id: Arc::from(cell_id) };
            }
            let text = match event_value {
                EventValue::KeyDown { key: Key::Enter, text: Some(t) } => t.clone(),
                _ => return CellUpdate::NoOp,
            };
            if text.trim().is_empty() {
                return CellUpdate::NoOp;
            }
            CellUpdate::ListPush {
                cell_id: Arc::from(cell_id),
                item: Value::Text(Arc::from(text)),
            }
        }
        DdTransform::ListRemove => {
            if !state.is_list() {
                panic!("[DD Dataflow] ListRemove expected list state for {}", cell_id);
            }
            let key = format!("link:{}", event_link_id);
            CellUpdate::ListRemoveByKey { cell_id: Arc::from(cell_id), key: Arc::from(key) }
        }
        DdTransform::Identity => {
            match state {
                CellState::List(_) => {
                    panic!("[DD Dataflow] Identity on list-like cell '{}' is forbidden; use list diffs", cell_id);
                }
                CellState::Scalar(value) => {
                    CellUpdate::SetValue { cell_id: Arc::from(cell_id), value: value.clone() }
                }
            }
        }
    }
}

/// Apply a DD transform, returning both the next state and the output update.
pub(crate) fn apply_dd_transform_with_state(
    transform: &DdTransform,
    state: &CellState,
    event: &(String, EventValue),
    cell_id: &str,
) -> (CellState, CellUpdate) {
    let output = apply_dd_transform(transform, state, event, cell_id);
    let next_state = apply_output_to_cell_state(state, &output, cell_id);
    (next_state, output)
}

fn apply_output_to_cell_state(state: &CellState, output: &CellUpdate, cell_id: &str) -> CellState {
    match state {
        CellState::Scalar(current) => {
            match output {
                CellUpdate::Multi(updates) => {
                    let mut matched: Option<&CellUpdate> = None;
                    for update in updates.iter() {
                        let update_cell_id = update.cell_id().unwrap_or_else(|| {
                            panic!("[DD State] Missing cell id for update {:?}", update);
                        });
                        if update_cell_id == cell_id {
                            if matched.is_some() {
                                panic!("[DD State] Multiple updates for cell {} in Multi", cell_id);
                            }
                            matched = Some(update);
                        }
                    }
                    if let Some(update) = matched {
                        return apply_output_to_cell_state(state, update, cell_id);
                    }
                    return CellState::Scalar(current.clone());
                }
                CellUpdate::SetValue { value, .. } => CellState::Scalar(value.clone()),
                CellUpdate::NoOp => CellState::Scalar(current.clone()),
                _ => {
                    panic!(
                        "[DD State] List diff applied to non-list cell '{}': {:?}",
                        cell_id, output
                    );
                }
            }
        }
        CellState::List(list_state) => {
            let mut list_state = list_state.clone();
            apply_output_to_list_state(&mut list_state, output, cell_id);
            CellState::List(list_state)
        }
    }
}

fn apply_output_to_list_state(state: &mut ListState, output: &CellUpdate, cell_id: &str) {
    match output {
        CellUpdate::Multi(updates) => {
            let mut matched: Option<&CellUpdate> = None;
            for update in updates.iter() {
                let update_cell_id = update.cell_id().unwrap_or_else(|| {
                    panic!("[DD State] Missing cell id for update {:?}", update);
                });
                if update_cell_id == cell_id {
                    if matched.is_some() {
                        panic!("[DD State] Multiple updates for cell {} in Multi", cell_id);
                    }
                    matched = Some(update);
                }
            }
            if let Some(update) = matched {
                apply_output_to_list_state(state, update, cell_id);
            }
        }
        CellUpdate::ListPush { cell_id: diff_cell_id, item } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListPush");
            state.push(item.clone(), "list push");
        }
        CellUpdate::ListInsertAt { cell_id: diff_cell_id, index, item } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListInsertAt");
            state.insert(*index, item.clone(), "list insert");
        }
        CellUpdate::ListRemoveAt { cell_id: diff_cell_id, index } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListRemoveAt");
            state.remove_at(*index, "list remove");
        }
        CellUpdate::ListRemoveByKey { cell_id: diff_cell_id, key } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListRemoveByKey");
            state.remove_by_key(key.as_ref(), "list remove by key");
        }
        CellUpdate::ListRemoveBatch { cell_id: diff_cell_id, keys } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListRemoveBatch");
            state.remove_batch(keys, "list remove batch");
        }
        CellUpdate::ListClear { cell_id: diff_cell_id } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListClear");
            state.clear();
        }
        CellUpdate::ListItemUpdate { cell_id: diff_cell_id, key, field_path, new_value } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListItemUpdate");
            state.update_field(key.as_ref(), field_path, new_value, "list item update");
        }
        CellUpdate::SetValue { .. } => {
            panic!(
                "[DD State] List cell '{}' received non-diff output {:?}",
                cell_id, output
            );
        }
        CellUpdate::NoOp => {}
    }
}

/// Apply an output update to a cell's state snapshot (pure).
pub(crate) fn apply_output_to_state(state: &Value, output: &CellUpdate, cell_id: &str) -> Value {
    if let Some(out_cell_id) = output.cell_id() {
        if out_cell_id != cell_id {
            panic!(
                "[DD State] Output cell '{}' does not match '{}': {:?}",
                out_cell_id, cell_id, output
            );
        }
    }
    match output {
        CellUpdate::Multi(updates) => {
            let mut matched: Option<&CellUpdate> = None;
            for update in updates.iter() {
                let update_cell_id = update.cell_id().unwrap_or_else(|| {
                    panic!("[DD State] Missing cell id for update {:?}", update);
                });
                if update_cell_id == cell_id {
                    if matched.is_some() {
                        panic!("[DD State] Multiple updates for cell {} in Multi", cell_id);
                    }
                    matched = Some(update);
                }
            }
            if let Some(update) = matched {
                return apply_output_to_state(state, update, cell_id);
            }
            state.clone()
        }
        CellUpdate::SetValue { value, .. } => {
            if matches!(value, Value::List(_)) {
                panic!(
                    "[DD State] List-like output applied to non-list cell '{}': {:?}",
                    cell_id, value
                );
            }
            value.clone()
        }
        CellUpdate::NoOp => state.clone(),
        _ => {
            panic!(
                "[DD State] List diff applied to non-list cell '{}': {:?}",
                cell_id, output
            );
        }
    }
}

/// Apply an output update to the state map, handling Multi fan-out.
pub(crate) fn apply_output_to_states(
    states: &mut HashMap<String, Value>,
    output: &CellUpdate,
    initial_by_id: &HashMap<String, Value>,
) {
    match output {
        CellUpdate::Multi(updates) => {
            for update in updates.iter() {
                apply_single_output(states, update, initial_by_id);
            }
        }
        CellUpdate::NoOp => {}
        _ => {
            apply_single_output(states, output, initial_by_id);
        }
    }
}

fn apply_single_output(
    states: &mut HashMap<String, Value>,
    output: &CellUpdate,
    initial_by_id: &HashMap<String, Value>,
) {
    let cell_id = output.cell_id().unwrap_or_else(|| {
        panic!("[DD State] Missing cell id for update {:?}", output);
    });
    let current = states
        .get(cell_id)
        .cloned()
        .or_else(|| initial_by_id.get(cell_id).cloned());

    if let Some(current) = current {
        let next = apply_output_to_state(&current, output, cell_id);
        let next = validate_collection_output(cell_id, next);
        states.insert(cell_id.to_string(), next);
        return;
    }

    match output {
        CellUpdate::ListPush { .. }
        | CellUpdate::ListInsertAt { .. }
        | CellUpdate::ListRemoveAt { .. }
        | CellUpdate::ListRemoveByKey { .. }
        | CellUpdate::ListRemoveBatch { .. }
        | CellUpdate::ListClear { .. }
        | CellUpdate::ListItemUpdate { .. } => {
            panic!(
                "[DD State] List diff applied to non-list cell '{}': {:?}",
                cell_id, output
            );
        }
        CellUpdate::Multi(_) => {
            panic!("[DD State] Multi update must be expanded before apply_single_output");
        }
        CellUpdate::NoOp => {}
        CellUpdate::SetValue { value, .. } => {
            let next = validate_collection_output(cell_id, value.clone());
            states.insert(cell_id.to_string(), next);
        }
    }
}

fn ensure_same_cell(diff_cell_id: &Arc<str>, cell_id: &str, kind: &str) {
    if diff_cell_id.as_ref() != cell_id {
        panic!(
            "[DD State] {} diff for '{}' applied to cell '{}'",
            kind, diff_cell_id, cell_id
        );
    }
}

fn validate_collection_output(cell_id: &str, value: Value) -> Value {
    match value {
        Value::List(handle) => {
            let existing = handle.cell_id.as_deref().unwrap_or_else(|| {
                panic!("[DD State] Missing collection cell_id for '{}'", cell_id);
            });
            if existing != cell_id {
                panic!("[DD State] Collection cell_id mismatch: expected '{}', found '{}'", cell_id, existing);
            }
            Value::List(handle)
        }
        other => other,
    }
}

fn validate_collection_initial(cell_id: &str, value: Value) -> Value {
    match value {
        Value::List(handle) => {
            let existing = handle.cell_id.as_deref().unwrap_or_else(|| {
                panic!("[DD State] Missing collection cell_id for '{}'", cell_id);
            });
            if existing != cell_id {
                panic!("[DD State] Collection cell_id mismatch: expected '{}', found '{}'", cell_id, existing);
            }
            Value::List(handle)
        }
        other => other,
    }
}

fn apply_field_update(value: &Value, field_path: &[Arc<str>], new_value: &Value) -> Value {
    if field_path.is_empty() {
        return new_value.clone();
    }

    let field = &field_path[0];
    let remaining = &field_path[1..];

    match value {
        Value::Object(fields) => {
            let mut new_fields = (**fields).clone();
            if remaining.is_empty() {
                new_fields.insert(field.clone(), new_value.clone());
            } else {
                let inner = fields.get(field.as_ref()).unwrap_or_else(|| {
                    panic!("[DD State] apply_field_update: missing field '{}' in Object", field);
                });
                new_fields.insert(field.clone(), apply_field_update(inner, remaining, new_value));
            }
            Value::Object(Arc::new(new_fields))
        }
        Value::Tagged { tag, fields } => {
            let mut new_fields = (**fields).clone();
            if remaining.is_empty() {
                new_fields.insert(field.clone(), new_value.clone());
            } else {
                let inner = fields.get(field.as_ref()).unwrap_or_else(|| {
                    panic!("[DD State] apply_field_update: missing field '{}' in Tagged", field);
                });
                new_fields.insert(field.clone(), apply_field_update(inner, remaining, new_value));
            }
            Value::Tagged {
                tag: tag.clone(),
                fields: Arc::new(new_fields),
            }
        }
        _ => panic!("[DD State] apply_field_update: non-object value at path {:?}", field_path),
    }
}

// ============================================================================
// COLLECTION OP RUNTIME HELPERS
// ============================================================================

#[derive(Clone, Debug)]
pub struct ListState {
    items: Vec<Value>,
    index_by_key: HashMap<Arc<str>, usize>,
    /// Monotonic counter for auto-generating unique keys for plain value items.
    /// Object/Tagged items use their `__key` field; plain values (Text, Number, etc.)
    /// need auto-generated keys because their values may repeat.
    next_auto_key: usize,
}

impl ListState {
    pub(crate) fn new(items: Vec<Value>, context: &str) -> Self {
        ensure_unique_item_keys(&items, context);
        let mut index_by_key = HashMap::new();
        let mut next_auto_key: usize = 0;
        for (idx, item) in items.iter().enumerate() {
            let key = match item {
                Value::Object(_) | Value::Tagged { .. } => extract_item_key(item, context),
                // Plain values use auto-generated keys (duplicates allowed).
                _ => {
                    let key = Arc::from(format!("__auto:{}", next_auto_key));
                    next_auto_key += 1;
                    key
                }
            };
            if index_by_key.insert(key.clone(), idx).is_some() {
                panic!("[DD CollectionOp] Duplicate __key '{}' in {}", key, context);
            }
        }
        Self { items, index_by_key, next_auto_key }
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn items(&self) -> &[Value] {
        &self.items
    }

    pub(crate) fn index_of(&self, key: &str, context: &str) -> usize {
        *self.index_by_key.get(key).unwrap_or_else(|| {
            panic!("[DD CollectionOp] {} missing key '{}'", context, key);
        })
    }

    pub(crate) fn item_by_key(&self, key: &str, context: &str) -> &Value {
        let idx = self.index_of(key, context);
        &self.items[idx]
    }

    pub(crate) fn rebuild_index(&mut self, context: &str) {
        self.index_by_key.clear();
        self.next_auto_key = 0;
        for (idx, item) in self.items.iter().enumerate() {
            let key = match item {
                Value::Object(_) | Value::Tagged { .. } => extract_item_key(item, context),
                _ => {
                    let key = Arc::from(format!("__auto:{}", self.next_auto_key));
                    self.next_auto_key += 1;
                    key
                }
            };
            if self.index_by_key.insert(key.clone(), idx).is_some() {
                panic!("[DD CollectionOp] Duplicate __key '{}' in {}", key, context);
            }
        }
    }

    pub(crate) fn push(&mut self, item: Value, context: &str) -> usize {
        let key = match &item {
            Value::Object(_) | Value::Tagged { .. } => {
                let key = extract_item_key(&item, context);
                if self.index_by_key.contains_key(key.as_ref()) {
                    panic!("[DD CollectionOp] {} duplicate __key '{}' on push", context, key);
                }
                key
            }
            // Plain values (Text, Number, etc.) get auto-generated unique keys.
            // This allows duplicate values (e.g., two "Milk" items in a shopping list).
            _ => {
                let key = Arc::from(format!("__auto:{}", self.next_auto_key));
                self.next_auto_key += 1;
                key
            }
        };
        let index = self.items.len();
        self.items.push(item);
        self.index_by_key.insert(key, index);
        index
    }

    pub(crate) fn insert(&mut self, index: usize, item: Value, context: &str) {
        if index > self.items.len() {
            panic!(
                "[DD CollectionOp] {} insert index {} out of bounds (len={})",
                context, index, self.items.len()
            );
        }
        let key = match &item {
            Value::Object(_) | Value::Tagged { .. } => {
                let key = extract_item_key(&item, context);
                if self.index_by_key.contains_key(key.as_ref()) {
                    panic!("[DD CollectionOp] {} duplicate __key '{}' on insert", context, key);
                }
                key
            }
            _ => {
                let key = Arc::from(format!("__auto:{}", self.next_auto_key));
                self.next_auto_key += 1;
                key
            }
        };
        self.items.insert(index, item);
        // Shift indices for items after the insertion point
        for idx in self.index_by_key.values_mut() {
            if *idx >= index {
                *idx += 1;
            }
        }
        self.index_by_key.insert(key, index);
    }

    pub(crate) fn remove_at(&mut self, index: usize, context: &str) -> Value {
        if index >= self.items.len() {
            panic!(
                "[DD CollectionOp] {} remove index {} out of bounds (len={})",
                context, index, self.items.len()
            );
        }
        let removed = self.items.remove(index);
        // Find the key by index lookup (works for both __key and __auto keys)
        let removed_key = self.index_by_key.iter()
            .find(|(_, idx)| **idx == index)
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| panic!("[DD CollectionOp] {} no key for index {}", context, index));
        self.index_by_key.remove(removed_key.as_ref());
        for idx in self.index_by_key.values_mut() {
            if *idx > index {
                *idx -= 1;
            }
        }
        removed
    }

    pub(crate) fn remove_by_key(&mut self, key: &str, context: &str) -> Value {
        let index = self.index_of(key, context);
        let removed = self.items.remove(index);
        // Remove the key and shift indices after the removal point
        self.index_by_key.remove(key);
        for idx in self.index_by_key.values_mut() {
            if *idx > index {
                *idx -= 1;
            }
        }
        removed
    }

    pub(crate) fn remove_batch(&mut self, keys: &[Arc<str>], context: &str) -> Vec<(Arc<str>, Value)> {
        if keys.is_empty() {
            return Vec::new();
        }
        let mut key_set: HashSet<Arc<str>> = keys.iter().cloned().collect();
        if key_set.len() != keys.len() {
            panic!("[DD CollectionOp] {} duplicate keys in batch remove", context);
        }
        let mut removed = Vec::new();
        let mut retained = Vec::with_capacity(self.items.len());
        for item in self.items.drain(..) {
            let key = extract_item_key(&item, context);
            if key_set.contains(key.as_ref()) {
                removed.push((key.clone(), item));
                key_set.remove(key.as_ref());
            } else {
                retained.push(item);
            }
        }
        if !key_set.is_empty() {
            panic!(
                "[DD CollectionOp] {} missing keys in batch remove: {:?}",
                context, key_set
            );
        }
        self.items = retained;
        self.rebuild_index(context);
        removed
    }

    pub(crate) fn clear(&mut self) -> Vec<Value> {
        let removed = std::mem::take(&mut self.items);
        self.index_by_key.clear();
        self.next_auto_key = 0;
        removed
    }

    pub(crate) fn update_field(
        &mut self,
        key: &str,
        field_path: &[Arc<str>],
        new_value: &Value,
        context: &str,
    ) -> (Value, Value) {
        let index = self.index_of(key, context);
        let old_item = self.items[index].clone();
        let new_item = apply_field_update(&old_item, field_path, new_value);
        let new_key = extract_item_key(&new_item, context);
        if new_key.as_ref() != key {
            panic!(
                "[DD CollectionOp] {} __key changed on update: '{}' != '{}'",
                context, new_key, key
            );
        }
        self.items[index] = new_item.clone();
        (old_item, new_item)
    }

    pub(crate) fn set_item(&mut self, key: &str, new_item: Value, context: &str) -> (Value, Value) {
        let index = self.index_of(key, context);
        let old_item = self.items[index].clone();
        let new_key = extract_item_key(&new_item, context);
        if new_key.as_ref() != key {
            panic!(
                "[DD CollectionOp] {} __key changed on set: '{}' != '{}'",
                context, new_key, key
            );
        }
        self.items[index] = new_item.clone();
        (old_item, new_item)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CollectionEvent {
    Init,
    ListDiff(CellUpdate),
    CellUpdate { cell_id: String, value: Value },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ConcatEvent {
    Init,
    Left(CellUpdate),
    Right(CellUpdate),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScalarEvent {
    Left(CellUpdate),
    Right(CellUpdate),
}

fn resolve_cell_value(
    value: &Value,
    cell_values: &HashMap<String, Value>,
    context: &str,
) -> Value {
    match value {
        Value::CellRef(cell_id) => {
            let cell_name = cell_id.name();
            cell_values.get(&cell_name).cloned().unwrap_or_else(|| {
                panic!("[DD CollectionOp] missing cell state '{}' for {}", cell_name, context);
            })
        }
        Value::WhileConfig(config) => {
            // Evaluate the WHILE/WHEN config by looking up the cell's current value
            // and matching against the arms.
            let cell_name = config.cell_id.name();
            let cell_value = cell_values.get(&cell_name).unwrap_or_else(|| {
                panic!("[DD CollectionOp] missing cell state '{}' for WhileConfig in {}", cell_name, context);
            });
            for arm in config.arms.iter() {
                if while_pattern_matches(cell_value, &arm.pattern) {
                    // Recursively resolve in case the arm body contains more CellRefs/WhileConfigs
                    return resolve_cell_value(&arm.body, cell_values, context);
                }
            }
            if !matches!(config.default.as_ref(), Value::Unit) {
                return resolve_cell_value(&config.default, cell_values, context);
            }
            Value::Unit
        }
        other => other.clone(),
    }
}

fn while_pattern_matches(value: &Value, pattern: &Value) -> bool {
    match (value, pattern) {
        (Value::Bool(b), Value::Tagged { tag, .. }) if BoolTag::is_bool_tag(tag.as_ref()) => {
            BoolTag::matches_bool(tag.as_ref(), *b)
        }
        _ => value == pattern,
    }
}

fn bool_from_value(value: &Value, cell_values: &HashMap<String, Value>, context: &str) -> bool {
    let resolved = resolve_cell_value(value, cell_values, context);
    match resolved {
        Value::Bool(b) => b,
        Value::Tagged { tag, .. } if BoolTag::is_bool_tag(&tag) => {
            match BoolTag::from_tag(&tag) {
                Some(BoolTag::True) => true,
                Some(BoolTag::False) => false,
                None => panic!("[DD CollectionOp] invalid BoolTag for {}", context),
            }
        }
        other => panic!("[DD CollectionOp] {} must evaluate to Bool/BoolTag, found {:?}", context, other),
    }
}

fn is_item_field_equal(
    item: &Value,
    field_name: &str,
    expected: &Value,
    cell_values: &HashMap<String, Value>,
) -> bool {
    let actual = match item {
        Value::Object(fields) => fields.get(field_name).unwrap_or_else(|| {
            panic!("[DD CollectionOp] missing '{}' field on item", field_name);
        }),
        Value::Tagged { fields, .. } => fields.get(field_name).unwrap_or_else(|| {
            panic!("[DD CollectionOp] missing '{}' field on item", field_name);
        }),
        other => panic!("[DD CollectionOp] expected Object/Tagged item, found {:?}", other),
    };
    let actual = resolve_cell_value(actual, cell_values, "item field");
    let expected = resolve_cell_value(expected, cell_values, "expected value");
    match (actual, expected) {
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Tagged { tag: a, .. }, Value::Tagged { tag: b, .. })
            if BoolTag::is_bool_tag(&a) && BoolTag::is_bool_tag(&b) => {
            BoolTag::from_tag(&a) == BoolTag::from_tag(&b)
        }
        (a, b) => a == b,
    }
}

/// Resolve CellRefs in an item's fields to their current values.
/// This is needed before template substitution so that operations like
/// `PlaceholderBoolNot` see actual booleans instead of CellRef wrappers.
pub fn resolve_item_cellrefs(item: &Value, cell_values: &HashMap<String, Value>) -> Value {
    match item {
        Value::CellRef(_) => resolve_cell_value(item, cell_values, "item field"),
        Value::Object(fields) => {
            let resolved: std::collections::BTreeMap<Arc<str>, Value> = fields.iter()
                .map(|(k, v)| (k.clone(), resolve_item_cellrefs(v, cell_values)))
                .collect();
            Value::Object(Arc::new(resolved))
        }
        Value::Tagged { tag, fields } => {
            let resolved: std::collections::BTreeMap<Arc<str>, Value> = fields.iter()
                .map(|(k, v)| (k.clone(), resolve_item_cellrefs(v, cell_values)))
                .collect();
            Value::Tagged { tag: tag.clone(), fields: Arc::new(resolved) }
        }
        other => other.clone(),
    }
}

/// Evaluate whether a list item matches a filter predicate.
/// Used by Filter, CountWhere, and other collection operations.
fn evaluate_filter_predicate(
    item: &Value,
    field_filter: &Option<(Arc<str>, Value)>,
    predicate_template: &Option<TemplateValue>,
    cell_values: &HashMap<String, Value>,
) -> bool {
    match (field_filter, predicate_template) {
        (Some((field, expected)), None) => {
            is_item_field_equal(item, field.as_ref(), expected, cell_values)
        }
        (None, Some(template)) => {
            // Resolve CellRefs in item before substitution so PlaceholderBoolNot
            // and other eager operations see actual values, not cell references.
            let resolved_item = resolve_item_cellrefs(item, cell_values);
            let substituted = template.substitute_placeholders(&resolved_item);
            if contains_placeholder(&substituted) {
                panic!("[DD CollectionOp] predicate_template substitution left Placeholder in {:?}", substituted);
            }
            bool_from_value(&substituted, cell_values, "predicate_template")
        }
        _ => unreachable!(),
    }
}

fn list_state_from_items(
    initial_items: &HashMap<CollectionId, Vec<Value>>,
    collection_id: &CollectionId,
    context: &str,
) -> ListState {
    let items = initial_items.get(collection_id).unwrap_or_else(|| {
        panic!("[DD CollectionOp] missing initial items for {}", context);
    });
    ListState::new(items.clone(), context)
}

fn rebuild_output_list(
    output_state: &mut ListState,
    new_items: Vec<Value>,
    output_cell_id: &Arc<str>,
    context: &str,
) -> Vec<CellUpdate> {
    if output_state.items() == new_items.as_slice() {
        return Vec::new();
    }

    let mut diffs = Vec::new();
    if !output_state.is_empty() {
        diffs.push(CellUpdate::list_clear(output_cell_id.as_ref()));
    }
    for item in &new_items {
        diffs.push(CellUpdate::list_push(output_cell_id.as_ref(), item.clone()));
    }
    *output_state = ListState::new(new_items, context);
    diffs
}

fn init_event_collection<G: timely::dataflow::Scope<Timestamp = u64>>(
    scope: &mut G,
) -> differential_dataflow::collection::VecCollection<G, ()> {
    scope.new_collection_from(std::iter::once(())).1
}

fn build_list_diff_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    outputs: &differential_dataflow::collection::VecCollection<G, TaggedCellOutput>,
    sources_by_cell: Arc<HashMap<String, CollectionId>>,
) -> differential_dataflow::collection::VecCollection<G, (CollectionId, CellUpdate)> {
    let stream = outputs.inner.unary(Pipeline, "CollectionListDiffs", move |_cap, _info| {
        let sources_by_cell = sources_by_cell.clone();
        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (tagged, _t, diff) in data.iter() {
                    if *diff <= 0 {
                        continue;
                    }
                    let mut updates: Vec<CellUpdate> = Vec::new();
                    match &tagged.value {
                        CellUpdate::Multi(items) => {
                            for update in items {
                                updates.push(update.clone());
                            }
                        }
                        other => updates.push(other.clone()),
                    }
                    for update in updates {
                        let update_cell_id = update.cell_id().unwrap_or_else(|| {
                            panic!("[DD CollectionOp] Missing cell id for update {:?}", update);
                        });
                        if let Some(collection_id) = sources_by_cell.get(update_cell_id) {
                            match &update {
                                CellUpdate::ListPush { .. }
                                | CellUpdate::ListInsertAt { .. }
                                | CellUpdate::ListRemoveAt { .. }
                                | CellUpdate::ListRemoveByKey { .. }
                                | CellUpdate::ListRemoveBatch { .. }
                                | CellUpdate::ListClear { .. }
                                | CellUpdate::ListItemUpdate { .. } => {
                                    assert_list_diff_cell_id(&update, update_cell_id);
                                    session.give(((collection_id.clone(), update), time.time().clone(), 1isize));
                                }
                                CellUpdate::NoOp => {}
                                CellUpdate::SetValue { .. } => {
                                    panic!(
                                        "[DD CollectionOp] Non-list diff for list cell '{}': {:?}",
                                        update_cell_id, update
                                    );
                                }
                                CellUpdate::Multi(_) => {
                                    panic!("[DD CollectionOp] Multi update must be expanded before list diff");
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    stream.as_collection()
}

fn assert_list_diff_cell_id(value: &CellUpdate, cell_id: &str) {
    match value {
        CellUpdate::ListPush { cell_id: diff_cell_id, .. } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListPush");
        }
        CellUpdate::ListInsertAt { cell_id: diff_cell_id, .. } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListInsertAt");
        }
        CellUpdate::ListRemoveAt { cell_id: diff_cell_id, .. } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListRemoveAt");
        }
        CellUpdate::ListRemoveByKey { cell_id: diff_cell_id, .. } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListRemoveByKey");
        }
        CellUpdate::ListRemoveBatch { cell_id: diff_cell_id, .. } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListRemoveBatch");
        }
        CellUpdate::ListClear { cell_id: diff_cell_id } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListClear");
        }
        CellUpdate::ListItemUpdate { cell_id: diff_cell_id, .. } => {
            ensure_same_cell(diff_cell_id, cell_id, "ListItemUpdate");
        }
        _ => {}
    }
}

fn build_cell_update_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    outputs: &differential_dataflow::collection::VecCollection<G, TaggedCellOutput>,
) -> differential_dataflow::collection::VecCollection<G, (String, Value)> {
    let stream = outputs.inner.unary(Pipeline, "CollectionCellUpdates", move |_cap, _info| {
        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (tagged, _t, diff) in data.iter() {
                    if *diff <= 0 {
                        continue;
                    }
                    let mut updates: Vec<CellUpdate> = Vec::new();
                    match &tagged.value {
                        CellUpdate::Multi(items) => {
                            for update in items {
                                updates.push(update.clone());
                            }
                        }
                        other => updates.push(other.clone()),
                    }
                    for update in updates {
                        match update {
                            CellUpdate::SetValue { cell_id, value } => {
                                session.give(((cell_id.as_ref().to_string(), value), time.time().clone(), 1isize));
                            }
                            CellUpdate::NoOp => {}
                            CellUpdate::Multi(_) => {
                                panic!("[DD CollectionOp] Multi update must be expanded before cell update");
                            }
                            _ => {}
                        }
                    }
                }
            });
        }
    });
    stream.as_collection()
}

fn build_filter_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    source: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    cell_updates: &differential_dataflow::collection::VecCollection<G, (String, Value)>,
    init_events: &differential_dataflow::collection::VecCollection<G, ()>,
    output_cell_id: Arc<str>,
    field_filter: Option<(Arc<str>, Value)>,
    predicate_template: Option<TemplateValue>,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
    initial_cell_values: &HashMap<String, Value>,
    source_id: &CollectionId,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    if field_filter.is_some() && predicate_template.is_some() {
        panic!("[DD CollectionOp] Filter cannot use both field_filter and predicate_template");
    }
    if field_filter.is_none() && predicate_template.is_none() {
        panic!("[DD CollectionOp] Filter requires field_filter or predicate_template");
    }
    // Note: predicate_template may be a constant (e.g., `True` from `List/retain(item, if: True)`).
    // This is valid — evaluate_filter_predicate handles it by evaluating the constant directly.

    let source_is_base = initial_collection_items.contains_key(source_id);
    let mut source_state = if source_is_base {
        list_state_from_items(initial_collection_items, source_id, "Filter source")
    } else {
        ListState::new(Vec::new(), "Filter source")
    };
    let mut output_state = ListState::new(Vec::new(), "Filter output");
    let initial_cells = initial_cell_values.clone();
    let field_filter = field_filter.clone();
    let predicate_template = predicate_template.clone();

    let list_events = source.clone().map(CollectionEvent::ListDiff);
    let cell_events = cell_updates.clone().map(|(cell_id, value)| CollectionEvent::CellUpdate { cell_id, value });
    let init_events = init_events.clone().map(|_| CollectionEvent::Init);
    let events = list_events.concat(&cell_events).concat(&init_events);

    let stream = events.inner.unary(Pipeline, "CollectionFilter", move |_cap, _info| {
        let mut cell_values = initial_cells.clone();
        let mut source_state = source_state.clone();
        let mut output_state = output_state.clone();
        let output_cell_id = output_cell_id.clone();
        let field_filter = field_filter.clone();
        let predicate_template = predicate_template.clone();

        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (event, _t, diff) in data.iter() {
                    if *diff <= 0 {
                        continue;
                    }

                    let mut emit: Vec<CellUpdate> = Vec::new();
                    match event {
                        CollectionEvent::Init => {
                            let new_items: Vec<Value> = source_state.items()
                                .iter()
                                .filter(|item| {
                                    evaluate_filter_predicate(item, &field_filter, &predicate_template, &cell_values)
                                })
                                .cloned()
                                .collect();
                            if new_items.is_empty() && output_state.is_empty() {
                                emit.push(CellUpdate::list_clear(output_cell_id.as_ref()));
                            } else {
                                emit.extend(rebuild_output_list(
                                    &mut output_state,
                                    new_items,
                                    &output_cell_id,
                                    "Filter init",
                                ));
                            }
                        }
                        CollectionEvent::CellUpdate { cell_id, value } => {
                            cell_values.insert(cell_id.clone(), value.clone());
                            let new_items: Vec<Value> = source_state.items()
                                .iter()
                                .filter(|item| {
                                    evaluate_filter_predicate(item, &field_filter, &predicate_template, &cell_values)
                                })
                                .cloned()
                                .collect();
                            emit.extend(rebuild_output_list(
                                &mut output_state,
                                new_items,
                                &output_cell_id,
                                "Filter rebuild",
                            ));
                        }
                        CollectionEvent::ListDiff(diff) => {
                            match diff {
                                CellUpdate::ListPush { item, .. } => {
                                    let index = source_state.push(item.clone(), "Filter source push");
                                    let matches = evaluate_filter_predicate(item, &field_filter, &predicate_template, &cell_values);
                                    if matches {
                                        let output_index = source_state.items()
                                            .iter()
                                            .take(index)
                                            .filter(|item| {
                                                evaluate_filter_predicate(item, &field_filter, &predicate_template, &cell_values)
                                            })
                                            .count();
                                        output_state.insert(output_index, item.clone(), "Filter output insert");
                                        emit.push(CellUpdate::list_insert_at(output_cell_id.as_ref(), output_index, item.clone()));
                                    }
                                }
                                CellUpdate::ListInsertAt { index, item, .. } => {
                                    source_state.insert(*index, item.clone(), "Filter source insert");
                                    let matches = evaluate_filter_predicate(item, &field_filter, &predicate_template, &cell_values);
                                    if matches {
                                        let output_index = source_state.items()
                                            .iter()
                                            .take(*index)
                                            .filter(|item| {
                                                evaluate_filter_predicate(item, &field_filter, &predicate_template, &cell_values)
                                            })
                                            .count();
                                        output_state.insert(output_index, item.clone(), "Filter output insert");
                                        emit.push(CellUpdate::list_insert_at(output_cell_id.as_ref(), output_index, item.clone()));
                                    }
                                }
                                CellUpdate::ListRemoveAt { index, .. } => {
                                    let removed = source_state.remove_at(*index, "Filter source remove");
                                    let matches = evaluate_filter_predicate(&removed, &field_filter, &predicate_template, &cell_values);
                                    if matches {
                                        let key = extract_item_key(&removed, "Filter remove");
                                        output_state.remove_by_key(key.as_ref(), "Filter output remove");
                                        emit.push(CellUpdate::list_remove_by_key(output_cell_id.as_ref(), key));
                                    }
                                }
                                CellUpdate::ListRemoveByKey { key, .. } => {
                                    let removed = source_state.remove_by_key(key.as_ref(), "Filter source remove");
                                    let matches = evaluate_filter_predicate(&removed, &field_filter, &predicate_template, &cell_values);
                                    if matches {
                                        output_state.remove_by_key(key.as_ref(), "Filter output remove");
                                        emit.push(CellUpdate::list_remove_by_key(output_cell_id.as_ref(), key.clone()));
                                    }
                                }
                                CellUpdate::ListRemoveBatch { keys, .. } => {
                                    let removed = source_state.remove_batch(keys, "Filter source batch remove");
                                    let mut removed_keys: Vec<Arc<str>> = Vec::new();
                                    for (key, item) in removed {
                                        let matches = evaluate_filter_predicate(&item, &field_filter, &predicate_template, &cell_values);
                                        if matches {
                                            output_state.remove_by_key(key.as_ref(), "Filter output remove");
                                            removed_keys.push(key);
                                        }
                                    }
                                    if !removed_keys.is_empty() {
                                        emit.push(CellUpdate::list_remove_batch(output_cell_id.as_ref(), removed_keys));
                                    }
                                }
                                CellUpdate::ListClear { .. } => {
                                    source_state.clear();
                                    if !output_state.is_empty() {
                                        output_state.clear();
                                        emit.push(CellUpdate::list_clear(output_cell_id.as_ref()));
                                    }
                                }
                                CellUpdate::ListItemUpdate { key, field_path, new_value, .. } => {
                                    let (old_item, new_item) = source_state.update_field(
                                        key.as_ref(),
                                        field_path.as_ref(),
                                        new_value,
                                        "Filter source update",
                                    );
                                    let old_match = evaluate_filter_predicate(&old_item, &field_filter, &predicate_template, &cell_values);
                                    let new_match = evaluate_filter_predicate(&new_item, &field_filter, &predicate_template, &cell_values);
                                    match (old_match, new_match) {
                                        (true, true) => {
                                            output_state.update_field(
                                                key.as_ref(),
                                                field_path.as_ref(),
                                                new_value,
                                                "Filter output update",
                                            );
                                            emit.push(CellUpdate::list_item_update(
                                                output_cell_id.as_ref(),
                                                key.clone(),
                                                field_path.as_ref().clone(),
                                                new_value.clone(),
                                            ));
                                        }
                                        (true, false) => {
                                            output_state.remove_by_key(key.as_ref(), "Filter output remove");
                                            emit.push(CellUpdate::list_remove_by_key(output_cell_id.as_ref(), key.clone()));
                                        }
                                        (false, true) => {
                                            let index = source_state.index_of(key.as_ref(), "Filter source index");
                                            let output_index = source_state.items()
                                                .iter()
                                                .take(index)
                                                .filter(|item| {
                                                    evaluate_filter_predicate(item, &field_filter, &predicate_template, &cell_values)
                                                })
                                                .count();
                                            output_state.insert(output_index, new_item.clone(), "Filter output insert");
                                            emit.push(CellUpdate::list_insert_at(output_cell_id.as_ref(), output_index, new_item));
                                        }
                                        (false, false) => {}
                                    }
                                }
                                CellUpdate::Multi(_) => {
                                    panic!("[DD CollectionOp] Filter received Multi update");
                                }
                                CellUpdate::SetValue { .. } => {
                                    panic!("[DD CollectionOp] Filter received SetValue");
                                }
                                CellUpdate::NoOp => {}
                                other => {
                                    panic!("[DD CollectionOp] Filter received non-list diff {:?}", other);
                                }
                            }
                        }
                    }

                    for out in emit {
                        session.give((out, time.time().clone(), 1isize));
                    }
                }
            });
        }
    });

    stream.as_collection()
}

/// Resolve nested `Value::List(handle)` references in a mapped item by substituting
/// placeholders in the nested list items using the source item data.
///
/// The DD Map operator uses `substitute_placeholders` which doesn't recurse into
/// List references (they're just handles to items stored in initial_collections).
/// This function walks the mapped item tree, finds nested Lists whose items contain
/// placeholders, substitutes them per-item, creates new CollectionIds, and syncs
/// the resolved items directly to LIST_SIGNAL_VECS.
fn resolve_nested_list_templates(
    value: Value,
    source_item: &Value,
    initial_collections: &HashMap<CollectionId, Vec<Value>>,
) -> Value {
    match value {
        Value::List(ref handle) if handle.cell_id.is_none() => {
            if let Some(original_items) = initial_collections.get(&handle.id) {
                if original_items.iter().any(|item| contains_placeholder(item)) {
                    // Create per-item substituted copies
                    let new_id = CollectionId::new();
                    let new_items: Vec<Value> = original_items.iter()
                        .map(|item| {
                            let substituted = item.substitute_placeholders(source_item);
                            // Recursively resolve any further nested lists
                            resolve_nested_list_templates(
                                substituted, source_item, initial_collections,
                            )
                        })
                        .collect();
                    // Sync directly to LIST_SIGNAL_VECS so the bridge can render them
                    super::super::io::sync_list_state_from_dd(
                        new_id.to_string(),
                        new_items,
                    );
                    Value::List(CollectionHandle::new_with_id(new_id))
                } else {
                    value
                }
            } else {
                value
            }
        }
        Value::Object(fields) => {
            let new_fields: std::collections::BTreeMap<Arc<str>, Value> = fields.iter()
                .map(|(k, v)| (k.clone(), resolve_nested_list_templates(
                    v.clone(), source_item, initial_collections,
                )))
                .collect();
            Value::Object(Arc::new(new_fields))
        }
        Value::Tagged { ref tag, ref fields } if tag.as_ref() == "__text_template__" => {
            // Text template from Map context — resolve nested lists in parts,
            // then flatten to Value::Text if all parts are concrete (no CellRef).
            let new_fields: std::collections::BTreeMap<Arc<str>, Value> = fields.iter()
                .map(|(k, v)| (k.clone(), resolve_nested_list_templates(
                    v.clone(), source_item, initial_collections,
                )))
                .collect();
            let has_cellref = new_fields.values().any(|v| matches!(v, Value::CellRef(_)));
            if has_cellref {
                // Keep as template — bridge will create reactive signal
                Value::Tagged { tag: tag.clone(), fields: Arc::new(new_fields) }
            } else {
                // All parts concrete — flatten to Text
                let mut parts: Vec<&Value> = Vec::new();
                let mut i = 0;
                while let Some(v) = new_fields.get(i.to_string().as_str()) {
                    parts.push(v);
                    i += 1;
                }
                let text: String = parts.iter().map(|v| v.to_display_string()).collect();
                Value::text(text)
            }
        }
        Value::Tagged { tag, fields } => {
            let new_fields: std::collections::BTreeMap<Arc<str>, Value> = fields.iter()
                .map(|(k, v)| (k.clone(), resolve_nested_list_templates(
                    v.clone(), source_item, initial_collections,
                )))
                .collect();
            Value::Tagged { tag, fields: Arc::new(new_fields) }
        }
        Value::WhileConfig(config) => {
            let arms: Vec<WhileArm> = config.arms.iter()
                .map(|arm| WhileArm {
                    pattern: resolve_nested_list_templates(
                        arm.pattern.clone(), source_item, initial_collections,
                    ),
                    body: resolve_nested_list_templates(
                        arm.body.clone(), source_item, initial_collections,
                    ),
                })
                .collect();
            let default = resolve_nested_list_templates(
                (*config.default).clone(), source_item, initial_collections,
            );
            Value::WhileConfig(Arc::new(WhileConfig {
                cell_id: config.cell_id.clone(),
                arms: Arc::new(arms),
                default: Box::new(default),
            }))
        }
        _ => value,
    }
}

fn build_map_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    source: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    init_events: &differential_dataflow::collection::VecCollection<G, ()>,
    output_cell_id: Arc<str>,
    element_template: TemplateValue,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
    source_id: &CollectionId,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let source_is_base = initial_collection_items.contains_key(source_id);
    let mut source_state = if source_is_base {
        list_state_from_items(initial_collection_items, source_id, "Map source")
    } else {
        ListState::new(Vec::new(), "Map source")
    };
    let mut output_state = ListState::new(Vec::new(), "Map output");
    let element_template = element_template.clone();

    let list_events = source.clone().map(CollectionEvent::ListDiff);
    let init_events = init_events.clone().map(|_| CollectionEvent::Init);
    let events = list_events.concat(&init_events);

    // Capture for resolving nested List references that contain Placeholders
    let nested_collections = initial_collection_items.clone();

    let stream = events.inner.unary(Pipeline, "CollectionMap", move |_cap, _info| {
        let mut source_state = source_state.clone();
        let mut output_state = output_state.clone();
        let output_cell_id = output_cell_id.clone();
        let element_template = element_template.clone();
        let nested_collections = nested_collections.clone();

        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (event, _t, diff_count) in data.iter() {
                    if *diff_count <= 0 {
                        continue;
                    }
                    let mut emit: Vec<CellUpdate> = Vec::new();
                    match event {
                        CollectionEvent::Init => {
                            let mut mapped: Vec<Value> = Vec::with_capacity(source_state.len());
                            for item in source_state.items() {
                                let source_key = extract_item_key(item, "Map source item");
                                let mut mapped_item = element_template.substitute_placeholders(item);
                                mapped_item = resolve_nested_list_templates(
                                    mapped_item, item, &nested_collections,
                                );
                                if contains_placeholder(&mapped_item) {
                                    panic!("[DD CollectionOp] Map substitution left Placeholder in {:?}", mapped_item);
                                }
                                mapped_item = attach_or_validate_item_key(mapped_item, &source_key, "Map output item");
                                mapped.push(mapped_item);
                            }
                            if mapped.is_empty() && output_state.is_empty() {
                                emit.push(CellUpdate::list_clear(output_cell_id.as_ref()));
                            } else {
                                emit.extend(rebuild_output_list(
                                    &mut output_state,
                                    mapped,
                                    &output_cell_id,
                                    "Map init",
                                ));
                            }
                        }
                        CollectionEvent::ListDiff(diff) => match diff {
                            CellUpdate::ListPush { item, .. } => {
                            let index = source_state.push(item.clone(), "Map source push");
                            let source_key = extract_item_key(item, "Map source item");
                            let mut mapped_item = element_template.substitute_placeholders(item);
                            mapped_item = resolve_nested_list_templates(
                                mapped_item, item, &nested_collections,
                            );
                            if contains_placeholder(&mapped_item) {
                                panic!("[DD CollectionOp] Map substitution left Placeholder in {:?}", mapped_item);
                            }
                            mapped_item = attach_or_validate_item_key(mapped_item, &source_key, "Map output item");
                            if index == output_state.len() {
                                output_state.push(mapped_item.clone(), "Map output push");
                                emit.push(CellUpdate::list_push(output_cell_id.as_ref(), mapped_item));
                            } else {
                                output_state.insert(index, mapped_item.clone(), "Map output insert");
                                emit.push(CellUpdate::list_insert_at(output_cell_id.as_ref(), index, mapped_item));
                            }
                        }
                        CellUpdate::ListInsertAt { index, item, .. } => {
                            source_state.insert(*index, item.clone(), "Map source insert");
                            let source_key = extract_item_key(item, "Map source item");
                            let mut mapped_item = element_template.substitute_placeholders(item);
                            mapped_item = resolve_nested_list_templates(
                                mapped_item, item, &nested_collections,
                            );
                            if contains_placeholder(&mapped_item) {
                                panic!("[DD CollectionOp] Map substitution left Placeholder in {:?}", mapped_item);
                            }
                            mapped_item = attach_or_validate_item_key(mapped_item, &source_key, "Map output item");
                            output_state.insert(*index, mapped_item.clone(), "Map output insert");
                            emit.push(CellUpdate::list_insert_at(output_cell_id.as_ref(), *index, mapped_item));
                        }
                        CellUpdate::ListRemoveAt { index, .. } => {
                            let removed = source_state.remove_at(*index, "Map source remove");
                            let key = extract_item_key(&removed, "Map remove");
                            output_state.remove_by_key(key.as_ref(), "Map output remove");
                            emit.push(CellUpdate::list_remove_by_key(output_cell_id.as_ref(), key));
                        }
                        CellUpdate::ListRemoveByKey { key, .. } => {
                            source_state.remove_by_key(key.as_ref(), "Map source remove");
                            output_state.remove_by_key(key.as_ref(), "Map output remove");
                            emit.push(CellUpdate::list_remove_by_key(output_cell_id.as_ref(), key.clone()));
                        }
                        CellUpdate::ListRemoveBatch { keys, .. } => {
                            source_state.remove_batch(keys, "Map source batch remove");
                            let removed = output_state.remove_batch(keys, "Map output batch remove");
                            let removed_keys: Vec<Arc<str>> = removed.into_iter().map(|(key, _)| key).collect();
                            if !removed_keys.is_empty() {
                                emit.push(CellUpdate::list_remove_batch(output_cell_id.as_ref(), removed_keys));
                            }
                        }
                        CellUpdate::ListClear { .. } => {
                            source_state.clear();
                            if !output_state.is_empty() {
                                output_state.clear();
                                emit.push(CellUpdate::list_clear(output_cell_id.as_ref()));
                            }
                        }
                        CellUpdate::ListItemUpdate { key, field_path, new_value, .. } => {
                            let (_old, new_item) = source_state.update_field(
                                key.as_ref(),
                                field_path.as_ref(),
                                new_value,
                                "Map source update",
                            );
                            let source_key = extract_item_key(&new_item, "Map source item");
                            let mut mapped_item = element_template.substitute_placeholders(&new_item);
                            mapped_item = resolve_nested_list_templates(
                                mapped_item, &new_item, &nested_collections,
                            );
                            if contains_placeholder(&mapped_item) {
                                panic!("[DD CollectionOp] Map substitution left Placeholder in {:?}", mapped_item);
                            }
                            mapped_item = attach_or_validate_item_key(mapped_item, &source_key, "Map output item");
                            output_state.set_item(key.as_ref(), mapped_item.clone(), "Map output update");
                            emit.push(CellUpdate::list_item_update(
                                output_cell_id.as_ref(),
                                key.clone(),
                                Vec::new(),
                                mapped_item,
                            ));
                        }
                            CellUpdate::Multi(_) => {
                                panic!("[DD CollectionOp] Map received Multi update");
                            }
                            CellUpdate::SetValue { .. } => {
                                panic!("[DD CollectionOp] Map received SetValue");
                            }
                            CellUpdate::NoOp => {}
                            other => {
                                panic!("[DD CollectionOp] Map received non-list diff {:?}", other);
                            }
                        },
                        CollectionEvent::CellUpdate { .. } => {
                            panic!("[DD CollectionOp] Map received CellUpdate");
                        }
                    }

                    for out in emit {
                        session.give((out, time.time().clone(), 1isize));
                    }
                }
            });
        }
    });

    stream.as_collection()
}

fn build_count_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    source: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    init_events: &differential_dataflow::collection::VecCollection<G, ()>,
    output_cell_id: Arc<str>,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
    source_id: &CollectionId,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let source_is_base = initial_collection_items.contains_key(source_id);
    let mut source_state = if source_is_base {
        list_state_from_items(initial_collection_items, source_id, "Count source")
    } else {
        ListState::new(Vec::new(), "Count source")
    };
    let mut current_count: Option<i64> = None;

    let list_events = source.clone().map(CollectionEvent::ListDiff);
    let init_events = init_events.clone().map(|_| CollectionEvent::Init);
    let events = list_events.concat(&init_events);

    let stream = events.inner.unary(Pipeline, "CollectionCount", move |_cap, _info| {
        let mut source_state = source_state.clone();
        let mut current_count = current_count;
        let output_cell_id = output_cell_id.clone();
        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (event, _t, diff_count) in data.iter() {
                    if *diff_count <= 0 {
                        continue;
                    }
                    match event {
                        CollectionEvent::Init => {}
                        CollectionEvent::ListDiff(diff) => match diff {
                            CellUpdate::ListPush { item, .. } => {
                                source_state.push(item.clone(), "Count source push");
                            }
                            CellUpdate::ListInsertAt { index, item, .. } => {
                                source_state.insert(*index, item.clone(), "Count source insert");
                            }
                            CellUpdate::ListRemoveAt { index, .. } => {
                                source_state.remove_at(*index, "Count source remove");
                            }
                            CellUpdate::ListRemoveByKey { key, .. } => {
                                source_state.remove_by_key(key.as_ref(), "Count source remove");
                            }
                            CellUpdate::ListRemoveBatch { keys, .. } => {
                                source_state.remove_batch(keys, "Count source batch remove");
                            }
                            CellUpdate::ListClear { .. } => {
                                source_state.clear();
                            }
                            CellUpdate::ListItemUpdate { key, field_path, new_value, .. } => {
                                source_state.update_field(
                                    key.as_ref(),
                                    field_path.as_ref(),
                                    new_value,
                                    "Count source update",
                                );
                            }
                            CellUpdate::Multi(_) => {
                                panic!("[DD CollectionOp] Count received Multi update");
                            }
                            CellUpdate::SetValue { .. } => {
                                panic!("[DD CollectionOp] Count received SetValue");
                            }
                            CellUpdate::NoOp => {}
                            other => {
                                panic!("[DD CollectionOp] Count received non-list diff {:?}", other);
                            }
                        },
                        CollectionEvent::CellUpdate { .. } => {
                            panic!("[DD CollectionOp] Count received CellUpdate");
                        }
                    }

                    let new_count = source_state.len() as i64;
                    if current_count.map_or(true, |current| current != new_count) {
                        current_count = Some(new_count);
                        session.give((
                            CellUpdate::set_value(output_cell_id.as_ref(), Value::int(new_count)),
                            time.time().clone(),
                            1isize,
                        ));
                    }
                }
            });
        }
    });

    stream.as_collection()
}

fn build_count_where_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    source: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    cell_updates: &differential_dataflow::collection::VecCollection<G, (String, Value)>,
    init_events: &differential_dataflow::collection::VecCollection<G, ()>,
    output_cell_id: Arc<str>,
    filter_field: Arc<str>,
    filter_value: Value,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
    initial_cell_values: &HashMap<String, Value>,
    source_id: &CollectionId,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let source_is_base = initial_collection_items.contains_key(source_id);
    let mut source_state = if source_is_base {
        list_state_from_items(initial_collection_items, source_id, "CountWhere source")
    } else {
        ListState::new(Vec::new(), "CountWhere source")
    };
    let mut current_count: Option<i64> = None;
    let initial_cells = initial_cell_values.clone();
    let filter_field = filter_field.clone();
    let filter_value = filter_value.clone();

    let list_events = source.clone().map(CollectionEvent::ListDiff);
    let cell_events = cell_updates.clone().map(|(cell_id, value)| CollectionEvent::CellUpdate { cell_id, value });
    let init_events = init_events.clone().map(|_| CollectionEvent::Init);
    let events = list_events.concat(&cell_events).concat(&init_events);

    let stream = events.inner.unary(Pipeline, "CollectionCountWhere", move |_cap, _info| {
        let mut cell_values = initial_cells.clone();
        let mut source_state = source_state.clone();
        let mut current_count = current_count;
        let filter_field = filter_field.clone();
        let filter_value = filter_value.clone();
        let output_cell_id = output_cell_id.clone();

        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (event, _t, diff) in data.iter() {
                    if *diff <= 0 {
                        continue;
                    }

                    let mut updated = false;
                    match event {
                        CollectionEvent::Init => {
                            let new_count = source_state.items()
                                .iter()
                                .filter(|item| is_item_field_equal(item, filter_field.as_ref(), &filter_value, &cell_values))
                                .count() as i64;
                            if current_count.map_or(true, |current| current != new_count) {
                                current_count = Some(new_count);
                                session.give((
                                    CellUpdate::set_value(output_cell_id.as_ref(), Value::int(new_count)),
                                    time.time().clone(),
                                    1isize,
                                ));
                            }
                            continue;
                        }
                        CollectionEvent::CellUpdate { cell_id, value } => {
                            cell_values.insert(cell_id.clone(), value.clone());
                            let new_count = source_state.items()
                                .iter()
                                .filter(|item| is_item_field_equal(item, filter_field.as_ref(), &filter_value, &cell_values))
                                .count() as i64;
                            if current_count.map_or(true, |current| current != new_count) {
                                current_count = Some(new_count);
                                session.give((
                                    CellUpdate::set_value(output_cell_id.as_ref(), Value::int(new_count)),
                                    time.time().clone(),
                                    1isize,
                                ));
                            }
                        }
                        CollectionEvent::ListDiff(diff) => {
                            match diff {
                                CellUpdate::ListPush { item, .. } => {
                                    source_state.push(item.clone(), "CountWhere source push");
                                    if is_item_field_equal(item, filter_field.as_ref(), &filter_value, &cell_values) {
                                        let next = current_count.unwrap_or(0) + 1;
                                        current_count = Some(next);
                                        updated = true;
                                    }
                                }
                                CellUpdate::ListInsertAt { index, item, .. } => {
                                    source_state.insert(*index, item.clone(), "CountWhere source insert");
                                    if is_item_field_equal(item, filter_field.as_ref(), &filter_value, &cell_values) {
                                        let next = current_count.unwrap_or(0) + 1;
                                        current_count = Some(next);
                                        updated = true;
                                    }
                                }
                                CellUpdate::ListRemoveAt { index, .. } => {
                                    let removed = source_state.remove_at(*index, "CountWhere source remove");
                                    if is_item_field_equal(&removed, filter_field.as_ref(), &filter_value, &cell_values) {
                                        let next = current_count.unwrap_or(0) - 1;
                                        current_count = Some(next);
                                        updated = true;
                                    }
                                }
                                CellUpdate::ListRemoveByKey { key, .. } => {
                                    let removed = source_state.remove_by_key(key.as_ref(), "CountWhere source remove");
                                    if is_item_field_equal(&removed, filter_field.as_ref(), &filter_value, &cell_values) {
                                        let next = current_count.unwrap_or(0) - 1;
                                        current_count = Some(next);
                                        updated = true;
                                    }
                                }
                                CellUpdate::ListRemoveBatch { keys, .. } => {
                                    let removed = source_state.remove_batch(keys, "CountWhere source batch remove");
                                    let mut delta = 0i64;
                                    for (_key, item) in removed {
                                        if is_item_field_equal(&item, filter_field.as_ref(), &filter_value, &cell_values) {
                                            delta -= 1;
                                        }
                                    }
                                    if delta != 0 {
                                        let next = current_count.unwrap_or(0) + delta;
                                        current_count = Some(next);
                                        updated = true;
                                    }
                                }
                                CellUpdate::ListClear { .. } => {
                                    source_state.clear();
                                    if current_count.map_or(false, |current| current != 0) {
                                        current_count = Some(0);
                                        updated = true;
                                    }
                                }
                                CellUpdate::ListItemUpdate { key, field_path, new_value, .. } => {
                                    let (old_item, new_item) = source_state.update_field(
                                        key.as_ref(),
                                        field_path.as_ref(),
                                        new_value,
                                        "CountWhere source update",
                                    );
                                    let old_match = is_item_field_equal(&old_item, filter_field.as_ref(), &filter_value, &cell_values);
                                    let new_match = is_item_field_equal(&new_item, filter_field.as_ref(), &filter_value, &cell_values);
                                    match (old_match, new_match) {
                                        (true, false) => {
                                            let next = current_count.unwrap_or(0) - 1;
                                            current_count = Some(next);
                                            updated = true;
                                        }
                                        (false, true) => {
                                            let next = current_count.unwrap_or(0) + 1;
                                            current_count = Some(next);
                                            updated = true;
                                        }
                                        _ => {}
                                    }
                                }
                                CellUpdate::Multi(_) => {
                                    panic!("[DD CollectionOp] CountWhere received Multi update");
                                }
                                CellUpdate::SetValue { .. } => {
                                    panic!("[DD CollectionOp] CountWhere received SetValue");
                                }
                                CellUpdate::NoOp => {}
                                other => {
                                    panic!("[DD CollectionOp] CountWhere received non-list diff {:?}", other);
                                }
                            }

                            if updated {
                                let count = current_count.unwrap_or(0);
                                session.give((
                                    CellUpdate::set_value(output_cell_id.as_ref(), Value::int(count)),
                                    time.time().clone(),
                                    1isize,
                                ));
                            }
                        }
                    }
                }
            });
        }
    });

    stream.as_collection()
}

fn build_is_empty_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    source: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    init_events: &differential_dataflow::collection::VecCollection<G, ()>,
    output_cell_id: Arc<str>,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
    source_id: &CollectionId,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let source_is_base = initial_collection_items.contains_key(source_id);
    let mut source_state = if source_is_base {
        list_state_from_items(initial_collection_items, source_id, "IsEmpty source")
    } else {
        ListState::new(Vec::new(), "IsEmpty source")
    };
    let mut current_empty: Option<bool> = None;

    let list_events = source.clone().map(CollectionEvent::ListDiff);
    let init_events = init_events.clone().map(|_| CollectionEvent::Init);
    let events = list_events.concat(&init_events);

    let stream = events.inner.unary(Pipeline, "CollectionIsEmpty", move |_cap, _info| {
        let mut source_state = source_state.clone();
        let mut current_empty = current_empty;
        let output_cell_id = output_cell_id.clone();
        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (event, _t, diff_count) in data.iter() {
                    if *diff_count <= 0 {
                        continue;
                    }
                    match event {
                        CollectionEvent::Init => {}
                        CollectionEvent::ListDiff(diff) => match diff {
                            CellUpdate::ListPush { item, .. } => {
                                source_state.push(item.clone(), "IsEmpty source push");
                            }
                            CellUpdate::ListInsertAt { index, item, .. } => {
                                source_state.insert(*index, item.clone(), "IsEmpty source insert");
                            }
                            CellUpdate::ListRemoveAt { index, .. } => {
                                source_state.remove_at(*index, "IsEmpty source remove");
                            }
                            CellUpdate::ListRemoveByKey { key, .. } => {
                                source_state.remove_by_key(key.as_ref(), "IsEmpty source remove");
                            }
                            CellUpdate::ListRemoveBatch { keys, .. } => {
                                source_state.remove_batch(keys, "IsEmpty source batch remove");
                            }
                            CellUpdate::ListClear { .. } => {
                                source_state.clear();
                            }
                            CellUpdate::ListItemUpdate { key, field_path, new_value, .. } => {
                                source_state.update_field(
                                    key.as_ref(),
                                    field_path.as_ref(),
                                    new_value,
                                    "IsEmpty source update",
                                );
                            }
                            CellUpdate::Multi(_) => {
                                panic!("[DD CollectionOp] IsEmpty received Multi update");
                            }
                            CellUpdate::SetValue { .. } => {
                                panic!("[DD CollectionOp] IsEmpty received SetValue");
                            }
                            CellUpdate::NoOp => {}
                            other => {
                                panic!("[DD CollectionOp] IsEmpty received non-list diff {:?}", other);
                            }
                        },
                        CollectionEvent::CellUpdate { .. } => {
                            panic!("[DD CollectionOp] IsEmpty received CellUpdate");
                        }
                    }

                    let new_empty = source_state.is_empty();
                    if current_empty.map_or(true, |current| current != new_empty) {
                        current_empty = Some(new_empty);
                        session.give((
                            CellUpdate::set_value(output_cell_id.as_ref(), Value::Bool(new_empty)),
                            time.time().clone(),
                            1isize,
                        ));
                    }
                }
            });
        }
    });

    stream.as_collection()
}

fn build_concat_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    left: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    right: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    init_events: &differential_dataflow::collection::VecCollection<G, ()>,
    output_cell_id: Arc<str>,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
    left_id: &CollectionId,
    right_id: &CollectionId,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let left_is_base = initial_collection_items.contains_key(left_id);
    let right_is_base = initial_collection_items.contains_key(right_id);
    let mut left_state = if left_is_base {
        list_state_from_items(initial_collection_items, left_id, "Concat left")
    } else {
        ListState::new(Vec::new(), "Concat left")
    };
    let mut right_state = if right_is_base {
        list_state_from_items(initial_collection_items, right_id, "Concat right")
    } else {
        ListState::new(Vec::new(), "Concat right")
    };
    let mut output_state = ListState::new(Vec::new(), "Concat output");

    let left_events = left.clone().map(ConcatEvent::Left);
    let right_events = right.clone().map(ConcatEvent::Right);
    let init_events = init_events.clone().map(|_| ConcatEvent::Init);
    let events = left_events.concat(&right_events).concat(&init_events);

    let stream = events.inner.unary(Pipeline, "CollectionConcat", move |_cap, _info| {
        let mut left_state = left_state.clone();
        let mut right_state = right_state.clone();
        let mut output_state = output_state.clone();
        let output_cell_id = output_cell_id.clone();

        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (event, _t, diff_count) in data.iter() {
                    if *diff_count <= 0 {
                        continue;
                    }
                    let mut emit: Vec<CellUpdate> = Vec::new();
                    match event {
                        ConcatEvent::Init => {
                            let mut combined: Vec<Value> = Vec::with_capacity(left_state.len() + right_state.len());
                            combined.extend(left_state.items().iter().cloned());
                            combined.extend(right_state.items().iter().cloned());
                            if combined.is_empty() && output_state.is_empty() {
                                emit.push(CellUpdate::list_clear(output_cell_id.as_ref()));
                            } else {
                                emit.extend(rebuild_output_list(
                                    &mut output_state,
                                    combined,
                                    &output_cell_id,
                                    "Concat init",
                                ));
                            }
                        }
                        ConcatEvent::Left(diff) => {
                            match diff {
                                CellUpdate::ListPush { item, .. } => {
                                    let index = left_state.push(item.clone(), "Concat left push");
                                    output_state.insert(index, item.clone(), "Concat output insert");
                                    emit.push(CellUpdate::list_insert_at(output_cell_id.as_ref(), index, item.clone()));
                                }
                                CellUpdate::ListInsertAt { index, item, .. } => {
                                    left_state.insert(*index, item.clone(), "Concat left insert");
                                    output_state.insert(*index, item.clone(), "Concat output insert");
                                    emit.push(CellUpdate::list_insert_at(output_cell_id.as_ref(), *index, item.clone()));
                                }
                                CellUpdate::ListRemoveAt { index, .. } => {
                                    let removed = left_state.remove_at(*index, "Concat left remove");
                                    let key = extract_item_key(&removed, "Concat left remove");
                                    output_state.remove_by_key(key.as_ref(), "Concat output remove");
                                    emit.push(CellUpdate::list_remove_by_key(output_cell_id.as_ref(), key));
                                }
                                CellUpdate::ListRemoveByKey { key, .. } => {
                                    left_state.remove_by_key(key.as_ref(), "Concat left remove");
                                    output_state.remove_by_key(key.as_ref(), "Concat output remove");
                                    emit.push(CellUpdate::list_remove_by_key(output_cell_id.as_ref(), key.clone()));
                                }
                                CellUpdate::ListRemoveBatch { keys, .. } => {
                                    left_state.remove_batch(keys, "Concat left batch remove");
                                    let removed = output_state.remove_batch(keys, "Concat output batch remove");
                                    let removed_keys: Vec<Arc<str>> = removed.into_iter().map(|(key, _)| key).collect();
                                    if !removed_keys.is_empty() {
                                        emit.push(CellUpdate::list_remove_batch(output_cell_id.as_ref(), removed_keys));
                                    }
                                }
                                CellUpdate::ListClear { .. } => {
                                    let removed = left_state.clear();
                                    if !removed.is_empty() {
                                        let removed_keys: Vec<Arc<str>> = removed
                                            .iter()
                                            .map(|item| extract_item_key(item, "Concat left clear"))
                                            .collect();
                                        output_state.remove_batch(&removed_keys, "Concat output remove");
                                        emit.push(CellUpdate::list_remove_batch(output_cell_id.as_ref(), removed_keys));
                                    }
                                }
                                CellUpdate::ListItemUpdate { key, field_path, new_value, .. } => {
                                    left_state.update_field(
                                        key.as_ref(),
                                        field_path.as_ref(),
                                        new_value,
                                        "Concat left update",
                                    );
                                    output_state.update_field(
                                        key.as_ref(),
                                        field_path.as_ref(),
                                        new_value,
                                        "Concat output update",
                                    );
                                    emit.push(CellUpdate::list_item_update(
                                        output_cell_id.as_ref(),
                                        key.clone(),
                                        field_path.as_ref().clone(),
                                        new_value.clone(),
                                    ));
                                }
                                CellUpdate::Multi(_) => {
                                    panic!("[DD CollectionOp] Concat left received Multi update");
                                }
                                CellUpdate::SetValue { .. } => {
                                    panic!("[DD CollectionOp] Concat left received SetValue");
                                }
                                CellUpdate::NoOp => {}
                                other => {
                                    panic!("[DD CollectionOp] Concat left received non-list diff {:?}", other);
                                }
                            }
                        }
                        ConcatEvent::Right(diff) => {
                            match diff {
                                CellUpdate::ListPush { item, .. } => {
                                    let index = right_state.push(item.clone(), "Concat right push");
                                    let output_index = left_state.len() + index;
                                    output_state.insert(output_index, item.clone(), "Concat output insert");
                                    emit.push(CellUpdate::list_insert_at(output_cell_id.as_ref(), output_index, item.clone()));
                                }
                                CellUpdate::ListInsertAt { index, item, .. } => {
                                    right_state.insert(*index, item.clone(), "Concat right insert");
                                    let output_index = left_state.len() + *index;
                                    output_state.insert(output_index, item.clone(), "Concat output insert");
                                    emit.push(CellUpdate::list_insert_at(output_cell_id.as_ref(), output_index, item.clone()));
                                }
                                CellUpdate::ListRemoveAt { index, .. } => {
                                    let removed = right_state.remove_at(*index, "Concat right remove");
                                    let key = extract_item_key(&removed, "Concat right remove");
                                    output_state.remove_by_key(key.as_ref(), "Concat output remove");
                                    emit.push(CellUpdate::list_remove_by_key(output_cell_id.as_ref(), key));
                                }
                                CellUpdate::ListRemoveByKey { key, .. } => {
                                    right_state.remove_by_key(key.as_ref(), "Concat right remove");
                                    output_state.remove_by_key(key.as_ref(), "Concat output remove");
                                    emit.push(CellUpdate::list_remove_by_key(output_cell_id.as_ref(), key.clone()));
                                }
                                CellUpdate::ListRemoveBatch { keys, .. } => {
                                    right_state.remove_batch(keys, "Concat right batch remove");
                                    let removed = output_state.remove_batch(keys, "Concat output batch remove");
                                    let removed_keys: Vec<Arc<str>> = removed.into_iter().map(|(key, _)| key).collect();
                                    if !removed_keys.is_empty() {
                                        emit.push(CellUpdate::list_remove_batch(output_cell_id.as_ref(), removed_keys));
                                    }
                                }
                                CellUpdate::ListClear { .. } => {
                                    let removed = right_state.clear();
                                    if !removed.is_empty() {
                                        let removed_keys: Vec<Arc<str>> = removed
                                            .iter()
                                            .map(|item| extract_item_key(item, "Concat right clear"))
                                            .collect();
                                        output_state.remove_batch(&removed_keys, "Concat output remove");
                                        emit.push(CellUpdate::list_remove_batch(output_cell_id.as_ref(), removed_keys));
                                    }
                                }
                                CellUpdate::ListItemUpdate { key, field_path, new_value, .. } => {
                                    right_state.update_field(
                                        key.as_ref(),
                                        field_path.as_ref(),
                                        new_value,
                                        "Concat right update",
                                    );
                                    output_state.update_field(
                                        key.as_ref(),
                                        field_path.as_ref(),
                                        new_value,
                                        "Concat output update",
                                    );
                                    emit.push(CellUpdate::list_item_update(
                                        output_cell_id.as_ref(),
                                        key.clone(),
                                        field_path.as_ref().clone(),
                                        new_value.clone(),
                                    ));
                                }
                                CellUpdate::Multi(_) => {
                                    panic!("[DD CollectionOp] Concat right received Multi update");
                                }
                                CellUpdate::SetValue { .. } => {
                                    panic!("[DD CollectionOp] Concat right received SetValue");
                                }
                                CellUpdate::NoOp => {}
                                other => {
                                    panic!("[DD CollectionOp] Concat right received non-list diff {:?}", other);
                                }
                            }
                        }
                    }

                    for out in emit {
                        session.give((out, time.time().clone(), 1isize));
                    }
                }
            });
        }
    });

    stream.as_collection()
}

fn build_subtract_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    left: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    right: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    output_cell_id: Arc<str>,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let mut left_value: Option<Value> = None;
    let mut right_value: Option<Value> = None;
    let mut current_output: Option<Value> = None;

    let left_events = left.clone().map(ScalarEvent::Left);
    let right_events = right.clone().map(ScalarEvent::Right);
    let events = left_events.concat(&right_events);

    let stream = events.inner.unary(Pipeline, "CollectionSubtract", move |_cap, _info| {
        let mut left_value = left_value.clone();
        let mut right_value = right_value.clone();
        let mut current_output = current_output.clone();
        let output_cell_id = output_cell_id.clone();
        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (event, _t, diff_count) in data.iter() {
                    if *diff_count <= 0 {
                        continue;
                    }
                    match event {
                        ScalarEvent::Left(update) => match update {
                            CellUpdate::SetValue { value, .. } => left_value = Some(value.clone()),
                            CellUpdate::NoOp => continue,
                            other => panic!("[DD CollectionOp] Subtract left expects SetValue, found {:?}", other),
                        },
                        ScalarEvent::Right(update) => match update {
                            CellUpdate::SetValue { value, .. } => right_value = Some(value.clone()),
                            CellUpdate::NoOp => continue,
                            other => panic!("[DD CollectionOp] Subtract right expects SetValue, found {:?}", other),
                        },
                    }
                    let (left_value, right_value) = match (&left_value, &right_value) {
                        (Some(left), Some(right)) => (left, right),
                        _ => continue,
                    };
                    let left_num = match left_value {
                        Value::Number(n) => n.0,
                        other => panic!("[DD CollectionOp] Subtract expects Number, found {:?}", other),
                    };
                    let right_num = match right_value {
                        Value::Number(n) => n.0,
                        other => panic!("[DD CollectionOp] Subtract expects Number, found {:?}", other),
                    };
                    let new_output = Value::float(left_num - right_num);
                    if current_output.as_ref().map_or(true, |current| current != &new_output) {
                        current_output = Some(new_output.clone());
                        session.give((
                            CellUpdate::set_value(output_cell_id.as_ref(), new_output),
                            time.time().clone(),
                            1isize,
                        ));
                    }
                }
            });
        }
    });

    stream.as_collection()
}

fn build_greater_than_zero_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    source: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    output_cell_id: Arc<str>,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let stream = source.inner.unary(Pipeline, "CollectionGreaterThanZero", move |_cap, _info| {
        let mut current_output: Option<Value> = None;
        let output_cell_id = output_cell_id.clone();
        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (value, _t, diff_count) in data.iter() {
                    if *diff_count <= 0 {
                        continue;
                    }
                    let num = match value {
                        CellUpdate::SetValue { value, .. } => match value {
                            Value::Number(n) => n.0,
                            other => panic!("[DD CollectionOp] GreaterThanZero expects Number, found {:?}", other),
                        },
                        CellUpdate::NoOp => continue,
                        other => panic!("[DD CollectionOp] GreaterThanZero expects SetValue, found {:?}", other),
                    };
                    let new_output = Value::Bool(num > 0.0);
                    if current_output.as_ref().map_or(true, |current| current != &new_output) {
                        current_output = Some(new_output.clone());
                        session.give((
                            CellUpdate::set_value(output_cell_id.as_ref(), new_output),
                            time.time().clone(),
                            1isize,
                        ));
                    }
                }
            });
        }
    });

    stream.as_collection()
}

fn build_equal_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    left: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    right: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    output_cell_id: Arc<str>,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let mut left_value: Option<Value> = None;
    let mut right_value: Option<Value> = None;
    let mut current_output: Option<Value> = None;

    let left_events = left.clone().map(ScalarEvent::Left);
    let right_events = right.clone().map(ScalarEvent::Right);
    let events = left_events.concat(&right_events);

    let stream = events.inner.unary(Pipeline, "CollectionEqual", move |_cap, _info| {
        let mut left_value = left_value.clone();
        let mut right_value = right_value.clone();
        let mut current_output = current_output.clone();
        let output_cell_id = output_cell_id.clone();
        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (event, _t, diff_count) in data.iter() {
                    if *diff_count <= 0 {
                        continue;
                    }
                    match event {
                        ScalarEvent::Left(update) => match update {
                            CellUpdate::SetValue { value, .. } => left_value = Some(value.clone()),
                            CellUpdate::NoOp => continue,
                            other => panic!("[DD CollectionOp] Equal left expects SetValue, found {:?}", other),
                        },
                        ScalarEvent::Right(update) => match update {
                            CellUpdate::SetValue { value, .. } => right_value = Some(value.clone()),
                            CellUpdate::NoOp => continue,
                            other => panic!("[DD CollectionOp] Equal right expects SetValue, found {:?}", other),
                        },
                    }
                    let (left_value, right_value) = match (&left_value, &right_value) {
                        (Some(left), Some(right)) => (left, right),
                        _ => continue,
                    };
                    let new_output = Value::Bool(left_value == right_value);
                    if current_output.as_ref().map_or(true, |current| current != &new_output) {
                        current_output = Some(new_output.clone());
                        session.give((
                            CellUpdate::set_value(output_cell_id.as_ref(), new_output),
                            time.time().clone(),
                            1isize,
                        ));
                    }
                }
            });
        }
    });

    stream.as_collection()
}

fn build_scalar_when_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    source: &differential_dataflow::collection::VecCollection<G, CellUpdate>,
    output_cell_id: Arc<str>,
    arms: Vec<(Value, Value)>,
    default: Value,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let stream = source.inner.unary(Pipeline, "ScalarWhen", move |_cap, _info| {
        let mut current_output: Option<Value> = None;
        let output_cell_id = output_cell_id.clone();
        let arms = arms.clone();
        let default = default.clone();
        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for (value, _t, diff_count) in data.iter() {
                    if *diff_count <= 0 {
                        continue;
                    }
                    let source_value = match value {
                        CellUpdate::SetValue { value, .. } => value,
                        CellUpdate::NoOp => continue,
                        other => panic!("[DD ScalarWhen] Expected SetValue, found {:?}", other),
                    };
                    let new_output = scalar_when_select(source_value, &arms, &default);
                    if current_output.as_ref().map_or(true, |current| current != &new_output) {
                        current_output = Some(new_output.clone());
                        session.give((
                            CellUpdate::set_value(output_cell_id.as_ref(), new_output),
                            time.time().clone(),
                            1isize,
                        ));
                    }
                }
            });
        }
    });
    stream.as_collection()
}

fn scalar_when_select(value: &Value, arms: &[(Value, Value)], default: &Value) -> Value {
    for (pattern, body) in arms {
        if scalar_when_matches(value, pattern) {
            return body.clone();
        }
    }
    default.clone()
}

fn scalar_when_matches(value: &Value, pattern: &Value) -> bool {
    match (value, pattern) {
        (Value::Bool(b), Value::Tagged { tag, .. }) if BoolTag::is_bool_tag(tag.as_ref()) => {
            BoolTag::matches_bool(tag.as_ref(), *b)
        }
        _ => value == pattern,
    }
}

fn build_computed_text_stream<G: timely::dataflow::Scope<Timestamp = u64>>(
    sources: &[&differential_dataflow::collection::VecCollection<G, CellUpdate>],
    output_cell_id: Arc<str>,
    parts: Vec<ComputedTextPart>,
) -> differential_dataflow::collection::VecCollection<G, CellUpdate> {
    let num_sources = sources.len();

    // Tag each source stream with its index
    let tagged_streams: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(idx, source)| (*source).clone().map(move |update| (idx, update)))
        .collect();

    // Concat all tagged streams
    let mut combined = tagged_streams[0].clone();
    for stream in &tagged_streams[1..] {
        combined = combined.concat(stream);
    }

    let stream = combined.inner.unary(Pipeline, "ComputedText", move |_cap, _info| {
        let mut cell_values: Vec<Option<Value>> = vec![None; num_sources];
        let mut current_output: Option<Value> = None;
        let output_cell_id = output_cell_id.clone();
        let parts = parts.clone();
        move |input, output| {
            input.for_each(|time, data| {
                let mut session = output.session(&time);
                for ((idx, update), _t, diff_count) in data.iter() {
                    if *diff_count <= 0 {
                        continue;
                    }
                    match update {
                        CellUpdate::SetValue { value, .. } => {
                            cell_values[*idx] = Some(value.clone());
                        }
                        CellUpdate::NoOp => continue,
                        other => panic!("[DD ComputedText] Expected SetValue, found {:?}", other),
                    }
                    // Only emit when all sources have values
                    if cell_values.iter().all(|v| v.is_some()) {
                        let formatted: String = parts
                            .iter()
                            .map(|part| match part {
                                ComputedTextPart::Static(s) => s.to_string(),
                                ComputedTextPart::CellSource(i) => {
                                    cell_values[*i].as_ref().unwrap().to_display_string()
                                }
                            })
                            .collect();
                        let new_output = Value::text(formatted);
                        if current_output.as_ref().map_or(true, |current| current != &new_output) {
                            current_output = Some(new_output.clone());
                            session.give((
                                CellUpdate::set_value(output_cell_id.as_ref(), new_output),
                                time.time().clone(),
                                1isize,
                            ));
                        }
                    }
                }
            });
        }
    });
    stream.as_collection()
}

pub(crate) fn build_collection_op_outputs<G: timely::dataflow::Scope<Timestamp = u64>>(
    scope: &mut G,
    hold_outputs: &differential_dataflow::collection::VecCollection<G, TaggedCellOutput>,
    collections: &DdCollectionConfig,
    initial_collection_items: &HashMap<CollectionId, Vec<Value>>,
    initial_cell_values: &HashMap<String, Value>,
) -> differential_dataflow::collection::VecCollection<G, TaggedCellOutput> {
    if collections.ops.is_empty() {
        return scope.new_collection::<TaggedCellOutput, isize>().1;
    }

    let mut sources_by_cell: HashMap<String, CollectionId> = HashMap::new();
    for (collection_id, cell_id) in &collections.collection_sources {
        if sources_by_cell.insert(cell_id.clone(), collection_id.clone()).is_some() {
            panic!(
                "[DD CollectionOp] Conflicting collection sources for cell '{}'",
                cell_id
            );
        }
    }

    for collection_id in collections.initial_collections.keys() {
        if !initial_collection_items.contains_key(collection_id) {
            panic!(
                "[DD CollectionOp] Missing initial items for {:?}",
                collection_id
            );
        }
    }

    let list_diffs = build_list_diff_stream(hold_outputs, Arc::new(sources_by_cell));
    let cell_updates = build_cell_update_stream(hold_outputs);
    let init_events = init_event_collection(scope);

    let empty_collection = scope.new_collection::<CellUpdate, isize>().1;

    let mut streams: HashMap<CollectionId, differential_dataflow::collection::VecCollection<G, CellUpdate>> = HashMap::new();
    for collection_id in collections.initial_collections.keys() {
        if collections.collection_sources.contains_key(collection_id) {
            let cid = collection_id.clone();
            let diffs = list_diffs
                .filter(move |(id, _)| *id == cid)
                .map(|(_, diff)| diff);
            streams.insert(cid, diffs);
        } else {
            streams.insert(collection_id.clone(), empty_collection.clone());
        }
    }

    let mut tagged_outputs: Vec<differential_dataflow::collection::VecCollection<G, TaggedCellOutput>> = Vec::new();

    for op in &collections.ops {
        let output_cell_id: Arc<str> = Arc::from(op.output_id.to_string());
        let output_stream = match &op.op {
            CollectionOp::Filter { field_filter, predicate_template } => {
                let source = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing source collection {:?}", op.source_id);
                });
                build_filter_stream(
                    source,
                    &cell_updates,
                    &init_events,
                    output_cell_id.clone(),
                    field_filter.clone(),
                    predicate_template.clone(),
                    initial_collection_items,
                    initial_cell_values,
                    &op.source_id,
                )
            }
            CollectionOp::Map { element_template } => {
                let source = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing source collection {:?}", op.source_id);
                });
                build_map_stream(
                    source,
                    &init_events,
                    output_cell_id.clone(),
                    element_template.clone(),
                    initial_collection_items,
                    &op.source_id,
                )
            }
            CollectionOp::Count => {
                let source = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing source collection {:?}", op.source_id);
                });
                build_count_stream(
                    source,
                    &init_events,
                    output_cell_id.clone(),
                    initial_collection_items,
                    &op.source_id,
                )
            }
            CollectionOp::CountWhere { filter_field, filter_value } => {
                let source = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing source collection {:?}", op.source_id);
                });
                build_count_where_stream(
                    source,
                    &cell_updates,
                    &init_events,
                    output_cell_id.clone(),
                    filter_field.clone(),
                    filter_value.clone(),
                    initial_collection_items,
                    initial_cell_values,
                    &op.source_id,
                )
            }
            CollectionOp::IsEmpty => {
                let source = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing source collection {:?}", op.source_id);
                });
                build_is_empty_stream(
                    source,
                    &init_events,
                    output_cell_id.clone(),
                    initial_collection_items,
                    &op.source_id,
                )
            }
            CollectionOp::Concat { other_source } => {
                let left = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing left source {:?}", op.source_id);
                });
                let right = streams.get(other_source).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing right source {:?}", other_source);
                });
                build_concat_stream(
                    left,
                    right,
                    &init_events,
                    output_cell_id.clone(),
                    initial_collection_items,
                    &op.source_id,
                    other_source,
                )
            }
            CollectionOp::Subtract { right_source } => {
                let left = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing left source {:?}", op.source_id);
                });
                let right = streams.get(right_source).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing right source {:?}", right_source);
                });
                build_subtract_stream(
                    left,
                    right,
                    output_cell_id.clone(),
                )
            }
            CollectionOp::GreaterThanZero => {
                let source = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing source collection {:?}", op.source_id);
                });
                build_greater_than_zero_stream(
                    source,
                    output_cell_id.clone(),
                )
            }
            CollectionOp::Equal { right_source } => {
                let left = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing left source {:?}", op.source_id);
                });
                let right = streams.get(right_source).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing right source {:?}", right_source);
                });
                build_equal_stream(
                    left,
                    right,
                    output_cell_id.clone(),
                )
            }
            CollectionOp::ScalarWhen { arms, default } => {
                let source = streams.get(&op.source_id).unwrap_or_else(|| {
                    panic!("[DD CollectionOp] Missing source {:?} for ScalarWhen", op.source_id);
                });
                build_scalar_when_stream(
                    source,
                    output_cell_id.clone(),
                    arms.clone(),
                    default.clone(),
                )
            }
            CollectionOp::ComputedText { parts, extra_sources } => {
                let mut source_streams = vec![
                    streams.get(&op.source_id).unwrap_or_else(|| {
                        panic!("[DD CollectionOp] Missing source {:?} for ComputedText", op.source_id);
                    })
                ];
                for extra in extra_sources {
                    source_streams.push(streams.get(extra).unwrap_or_else(|| {
                        panic!("[DD CollectionOp] Missing extra source {:?} for ComputedText", extra);
                    }));
                }
                build_computed_text_stream(
                    &source_streams,
                    output_cell_id.clone(),
                    parts.clone(),
                )
            }
        };

        streams.insert(op.output_id.clone(), output_stream.clone());

        let tagged = output_stream.map(move |value| TaggedCellOutput {
            cell_id: output_cell_id.to_string(),
            value,
        });
        tagged_outputs.push(tagged);
    }

    if tagged_outputs.is_empty() {
        return scope.new_collection::<TaggedCellOutput, isize>().1;
    }

    let mut merged = tagged_outputs.remove(0);
    for next in tagged_outputs {
        merged = merged.concat(&next);
    }
    merged
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

/// Merge multiple scalar cell values using LATEST semantics.
///
/// Collection/stream LATEST is handled in the evaluator via Concat ops.
pub fn merge_latest(values: &[Value]) -> Value {
    // For scalar LATEST, take the last non-undefined value
    values.iter().rev().find(|v| !v.is_undefined()).cloned().unwrap_or(Value::Unit)
}

// ============================================================================
// PERSISTENT DD WORKER
// ============================================================================

use std::cell::RefCell;
use differential_dataflow::input::InputSession;

thread_local! {
    /// Global persistent DD worker (browser is single-threaded, so no race conditions)
    static PERSISTENT_WORKER: RefCell<Option<PersistentDdWorker>> = RefCell::new(None); // ALLOWED: DD execution context
}

/// Tagged output for capture() - contains cell ID with the value
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaggedCellOutput {
    pub cell_id: String,
    pub value: CellUpdate,
}

/// Persistent Differential Dataflow worker that stays alive across event batches.
///
/// Unlike the batch-per-event model, this worker:
/// 1. Creates ONE Timely dataflow at initialization
/// 2. Keeps input handles for event injection
/// 3. Steps incrementally when events arrive
/// 4. Outputs changes via capture() (pure, no side effects)
///
/// # Pure DD Architecture
///
/// This worker uses `capture()` instead of `inspect()` to observe outputs.
/// - NO Mutex locks during dataflow execution
/// - NO side effects inside DD operators
/// - Outputs flow through message passing (mpsc channel)
pub struct PersistentDdWorker {
    /// The Timely worker (owned, stays alive)
    worker: TimelyWorker<Thread>,
    /// Input session for events (LinkId, EventValue)
    event_input: InputSession<u64, (String, EventValue), isize>,
    /// Probe for tracking progress
    probe: ProbeHandle<u64>,
    /// Current logical time
    current_time: u64,
    /// Output receiver from capture() - NO Mutex needed!
    outputs_receiver: std::sync::mpsc::Receiver<
        timely::dataflow::operators::capture::Event<u64, Vec<(TaggedCellOutput, u64, isize)>>,
    >,
    /// Cell configurations (for rebuilding dataflow if needed)
    cells: Vec<DdCellConfig>,
    /// Collection op configuration (for rebuilding dataflow if needed)
    collections: DdCollectionConfig,
    /// Config signature for change detection (hash of cell IDs and triggers)
    config_signature: u64,
}

/// Compute a signature for a cell configuration (for change detection).
fn compute_config_signature(cells: &[DdCellConfig], collections: &DdCollectionConfig) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    // Hash cell count
    cells.len().hash(&mut hasher);

    // Hash each cell's ID, triggers, filter, transform, and initial value
    for cell in cells {
        cell.id.name().hash(&mut hasher);
        for trigger in &cell.triggers {
            trigger.name().hash(&mut hasher);
        }
        cell.filter.hash(&mut hasher);
        cell.transform.hash(&mut hasher);
        cell.initial.hash(&mut hasher);
    }

    // Hash collection ops
    for op in &collections.ops {
        op.hash(&mut hasher);
    }

    // Hash collection sources (sorted for stability)
    let mut sources: Vec<_> = collections.collection_sources.iter().collect();
    sources.sort_by_key(|(id, _)| *id);
    for (collection_id, cell_id) in sources {
        collection_id.hash(&mut hasher);
        cell_id.hash(&mut hasher);
    }

    // Hash initial collections (sorted for stability)
    let mut initials: Vec<_> = collections.initial_collections.iter().collect();
    initials.sort_by_key(|(id, _)| *id);
    for (collection_id, items) in initials {
        collection_id.hash(&mut hasher);
        items.hash(&mut hasher);
    }

    hasher.finish()
}

impl PersistentDdWorker {
    /// Create a new persistent worker with the given cell configurations.
    ///
    /// # Pure DD Architecture
    ///
    /// This uses `capture()` instead of `inspect()` for output observation:
    /// - NO Mutex locks during dataflow execution
    /// - Outputs flow through mpsc channel (pure message passing)
    /// - `drain_outputs()` extracts from the channel AFTER stepping
    pub fn new(
        cells: Vec<DdCellConfig>,
        collections: DdCollectionConfig,
        initial_states: HashMap<String, Value>,
        initial_list_states: HashMap<String, ListState>,
        initial_collection_items: HashMap<CollectionId, Vec<Value>>,
    ) -> Self {
        let cells_clone = cells.clone();
        let collections_clone = collections.clone();
        let initial_states_clone = initial_states.clone();
        let initial_list_states_clone = initial_list_states.clone();
        let initial_collection_items_clone = initial_collection_items.clone();
        let list_cell_hints: HashSet<String> = initial_list_states_clone.keys().cloned().collect();
        let (list_cells, _derived_outputs) =
            list_cells_from_configs(&cells_clone, &collections_clone, &list_cell_hints);
        let list_cells_for_closure = list_cells.clone();

        // Create Timely worker
        let alloc = Thread::default();
        let mut worker = TimelyWorker::new(timely::WorkerConfig::default(), alloc, None);

        // Build the dataflow graph - returns (input_handle, probe, outputs_receiver)
        let (mut event_input, probe, outputs_receiver) = worker.dataflow::<u64, _, _>(move |scope| {
            use differential_dataflow::input::Input;
            use super::operators::hold;

            // Create input collection for events
            let (event_handle, events_collection) =
                scope.new_collection::<(String, EventValue), isize>();

            // Collect all cell outputs to merge into single capture stream
            let mut all_outputs: Vec<differential_dataflow::collection::VecCollection<_, TaggedCellOutput>> = Vec::new();

            // For each cell, create a HOLD operator
            for cell_config in &cells_clone {
                let cell_id = cell_config.id.name().to_string();
                let initial = if list_cells_for_closure.contains(&cell_id) {
                    let list_state = initial_list_states_clone.get(&cell_id).unwrap_or_else(|| {
                        panic!("[DD Persistent] Missing list state for '{}'", cell_id);
                    });
                    CellState::List(list_state.clone())
                } else {
                    let initial_value = initial_states_clone
                        .get(&cell_id)
                        .cloned()
                        .unwrap_or_else(|| cell_config.initial.clone());
                    let initial_value = validate_collection_initial(&cell_id, initial_value);
                    cell_state_from_value(
                        &cell_id,
                        initial_value,
                        &list_cells_for_closure,
                        &initial_collection_items_clone,
                    )
                };

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
                let cell_id_for_tag = cell_id.clone();
                let cell_id_for_transform = cell_id.clone();  // For O(delta) list ops

                let output = hold_with_output(initial, &triggered, move |state, event| {
                    apply_dd_transform_with_state(&transform, state, event, &cell_id_for_transform)
                }).filter(|update| !matches!(update, CellUpdate::NoOp));

                // Tag output with cell ID for identification
                let tagged_output = output.map(move |value| TaggedCellOutput {
                    cell_id: cell_id_for_tag.clone(),
                    value,
                });

                all_outputs.push(tagged_output);
            }

            // Merge all outputs from HOLD cells
            let merged = if all_outputs.is_empty() {
                scope.new_collection::<TaggedCellOutput, isize>().1
            } else {
                let first = all_outputs.remove(0);
                all_outputs.into_iter().fold(first, |acc, c| acc.concat(&c))
            };

            // Attach DD-native collection ops
            let collection_outputs = build_collection_op_outputs(
                scope,
                &merged,
                &collections_clone,
                &initial_collection_items_clone,
                &initial_states_clone,
            );

            let merged = merged.concat(&collection_outputs);

            // Use capture() for pure output observation - NO Mutex, NO side effects!
            let outputs_rx = merged.inner.capture();

            // Create probe for progress tracking
            let probe = events_collection.probe();
            (event_handle, probe, outputs_rx)
        });

        // Process init-only sources at time 0 by advancing the input frontier to 1.
        // This closes time 0 so the probe can advance, allowing probe-based stepping
        // instead of unbounded `while worker.step()` which never converges.
        event_input.advance_to(1);
        event_input.flush();
        while probe.less_than(&1) {
            worker.step();
        }

        let config_signature = compute_config_signature(&cells, &collections);

        Self {
            worker,
            event_input,
            probe,
            current_time: 1,
            outputs_receiver,
            cells,
            collections,
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

    /// Drain accumulated outputs using extract() on the capture channel.
    ///
    /// # Pure DD Architecture
    ///
    /// This extracts outputs from the mpsc receiver - NO Mutex locks!
    /// The receiver was populated during `worker.step()` via pure message passing.
    pub fn drain_outputs(&self) -> Vec<DdOutput> {
        use timely::dataflow::operators::capture::Event;

        let mut captured: Vec<(u64, Vec<(TaggedCellOutput, u64, isize)>)> = Vec::new();
        while let Ok(event) = self.outputs_receiver.try_recv() {
            if let Event::Messages(time, data) = event {
                captured.push((time, data));
            }
        }

        captured
            .into_iter()
            .flat_map(|(_time, items)| items)
            .filter(|(_, _, diff)| *diff > 0)
            .map(|(tagged, time, diff)| DdOutput {
                cell_id: CellId::new(&tagged.cell_id),
                value: tagged.value,
                time,
                diff,
            })
            .collect()
    }

    /// Get current logical time.
    pub fn current_time(&self) -> u64 {
        self.current_time
    }
}

/// Initialize the global persistent worker.
pub fn init_persistent_worker(
    cells: Vec<DdCellConfig>,
    collections: DdCollectionConfig,
    initial_states: HashMap<String, Value>,
    initial_list_states: HashMap<String, ListState>,
    initial_collection_items: HashMap<CollectionId, Vec<Value>>,
) {
    let num_cells = cells.len();
    PERSISTENT_WORKER.with(|worker| {
        *worker.borrow_mut() = Some(PersistentDdWorker::new(
            cells,
            collections,
            initial_states,
            initial_list_states,
            initial_collection_items,
        )); // ALLOWED: DD execution context
    });
    dd_log!("[DD Persistent] Worker initialized with {} cells", num_cells);
}

/// Check if persistent worker is initialized.
pub fn has_persistent_worker() -> bool {
    PERSISTENT_WORKER.with(|worker| {
        worker.borrow().is_some()
    }) // ALLOWED: DD execution context
}

/// Inject an event into the persistent worker.
pub fn inject_event_persistent(link_id: &LinkId, value: EventValue) -> Vec<DdOutput> {
    PERSISTENT_WORKER.with(|worker| {
        let mut guard = worker.borrow_mut();
        if let Some(w) = guard.as_mut() { // ALLOWED: DD execution context
            w.inject_event(link_id, value);
            w.drain_outputs()
        } else {
            panic!("[DD Persistent] Worker not initialized");
        }
    })
}

/// Drain any pending outputs from the persistent worker without injecting events.
pub fn drain_outputs_persistent() -> Vec<DdOutput> {
    PERSISTENT_WORKER.with(|worker| {
        let mut guard = worker.borrow_mut();
        if let Some(w) = guard.as_mut() { // ALLOWED: DD execution context
            w.drain_outputs()
        } else {
            panic!("[DD Persistent] Worker not initialized");
        }
    })
}

/// Shutdown the persistent worker.
pub fn shutdown_persistent_worker() {
    PERSISTENT_WORKER.with(|worker| {
        *worker.borrow_mut() = None; // ALLOWED: DD execution context
    });
    dd_log!("[DD Persistent] Worker shutdown");
}

/// Check if the current worker's config matches the given cells.
/// Returns true if worker exists and config matches, false otherwise.
pub fn config_matches(cells: &[DdCellConfig], collections: &DdCollectionConfig) -> bool {
    let new_signature = compute_config_signature(cells, collections);
    PERSISTENT_WORKER.with(|worker| {
        let guard = worker.borrow();
        if let Some(w) = guard.as_ref() { // ALLOWED: DD execution context
            w.config_signature() == new_signature
        } else {
            false
        }
    })
}

/// Reinitialize the persistent worker if config changed.
/// Returns true if worker was reinitialized, false if config was unchanged.
pub fn reinit_if_config_changed(
    cells: Vec<DdCellConfig>,
    collections: DdCollectionConfig,
    initial_states: HashMap<String, Value>,
    initial_list_states: HashMap<String, ListState>,
    initial_collection_items: HashMap<CollectionId, Vec<Value>>,
) -> bool {
    if has_persistent_worker() && config_matches(&cells, &collections) {
        false // No change needed
    } else {
        if has_persistent_worker() {
            shutdown_persistent_worker();
            dd_log!("[DD Persistent] Config changed, reinitializing worker");
        }
        init_persistent_worker(
            cells,
            collections,
            initial_states,
            initial_list_states,
            initial_collection_items,
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::value::CollectionHandle;
    use ordered_float::OrderedFloat;

    #[test]
    fn test_dd_first_increment() {
        let cells = vec![DdCellConfig {
            id: CellId::new("count"),
            initial: Value::Number(OrderedFloat(0.0)),
            triggers: vec![LinkId::new("click")],
            transform: DdTransform::Increment,
            filter: EventFilter::Any,
        }];

        let events = vec![
            (LinkId::new("click"), EventValue::Unit),
            (LinkId::new("click"), EventValue::Unit),
            (LinkId::new("click"), EventValue::Unit),
        ];

        let initial = HashMap::new();
        let collections = DdCollectionConfig {
            ops: Vec::new(),
            initial_collections: HashMap::new(),
            collection_sources: HashMap::new(),
        };
        let initial_collection_items = HashMap::new();
        let result = run_dd_first_batch(cells, collections, events, &initial, &initial_collection_items);

        assert_eq!(
            result.scalar_states.get("count"),
            Some(&Value::Number(OrderedFloat(3.0)))
        );
    }

    #[test]
    fn test_dd_first_toggle() {
        let cells = vec![DdCellConfig {
            id: CellId::new("enabled"),
            initial: Value::Bool(false),
            triggers: vec![LinkId::new("toggle")],
            transform: DdTransform::Toggle,
            filter: EventFilter::Any,
        }];

        let events = vec![
            (LinkId::new("toggle"), EventValue::Unit),
            (LinkId::new("toggle"), EventValue::Unit),
        ];

        let initial = HashMap::new();
        let collections = DdCollectionConfig {
            ops: Vec::new(),
            initial_collections: HashMap::new(),
            collection_sources: HashMap::new(),
        };
        let initial_collection_items = HashMap::new();
        let result = run_dd_first_batch(cells, collections, events, &initial, &initial_collection_items);

        // Toggle twice: false -> true -> false
        assert_eq!(result.scalar_states.get("enabled"), Some(&Value::Bool(false)));
    }

    #[test]
    fn test_dd_first_list_append() {
        let collection_id = CollectionId::new();
        let cells = vec![DdCellConfig {
            id: CellId::new("items"),
            initial: Value::List(CollectionHandle::with_id_and_cell(collection_id, "items")),
            triggers: vec![LinkId::new("add")],
            transform: DdTransform::ListAppendPrepared,
            filter: EventFilter::Any,
        }];

        let item1 = Value::object([
            ("__key", Value::text("item-1")),
            ("title", Value::text("item1")),
        ]);
        let item2 = Value::object([
            ("__key", Value::text("item-2")),
            ("title", Value::text("item2")),
        ]);

        let events = vec![
            (LinkId::new("add"), EventValue::prepared_item(item1, Vec::new())),
            (LinkId::new("add"), EventValue::prepared_item(item2, Vec::new())),
        ];

        let initial = HashMap::new();
        let collections = DdCollectionConfig {
            ops: Vec::new(),
            initial_collections: HashMap::new(),
            collection_sources: HashMap::new(),
        };
        let mut initial_collection_items = HashMap::new();
        initial_collection_items.insert(collection_id, Vec::new());
        let result = run_dd_first_batch(cells, collections, events, &initial, &initial_collection_items);

        let list_state = result.list_states.get("items").unwrap_or_else(|| {
            panic!("Expected list state for items");
        });
        assert_eq!(list_state.items().len(), 2);
    }

    #[test]
    #[should_panic(expected = "Missing collection cell_id for 'items'")]
    fn test_dd_first_list_append_requires_collection_cell_id() {
        let cells = vec![DdCellConfig {
            id: CellId::new("items"),
            initial: Value::List(CollectionHandle::new()),
            triggers: vec![LinkId::new("add")],
            transform: DdTransform::ListAppendPrepared,
            filter: EventFilter::Any,
        }];

        let item = Value::object([
            ("__key", Value::text("item-1")),
            ("title", Value::text("item1")),
        ]);
        let events = vec![(LinkId::new("add"), EventValue::prepared_item(item, Vec::new()))];

        let initial = HashMap::new();
        let collections = DdCollectionConfig {
            ops: Vec::new(),
            initial_collections: HashMap::new(),
            collection_sources: HashMap::new(),
        };
        let initial_collection_items = HashMap::new();
        let _ = run_dd_first_batch(cells, collections, events, &initial, &initial_collection_items);
    }

    #[test]
    #[should_panic(expected = "predicate_template must reference item data via Placeholder")]
    fn test_filter_predicate_template_requires_placeholder() {
        let source_id = CollectionId::new();
        let output_id = CollectionId::new();
        let item = Value::object([("__key", Value::text("a"))]);

        let mut initial_collections = HashMap::new();
        initial_collections.insert(source_id, vec![item.clone()]);

        let collections = DdCollectionConfig {
            ops: vec![CollectionOpConfig {
                output_id,
                source_id,
                op: CollectionOp::Filter {
                    field_filter: None,
                    predicate_template: Some(TemplateValue::from_value(Value::Bool(true))),
                },
            }],
            initial_collections: initial_collections.clone(),
            collection_sources: HashMap::new(),
        };

        let initial_collection_items = initial_collections;
        let initial = HashMap::new();

        let _ = run_dd_first_batch(Vec::new(), collections, Vec::new(), &initial, &initial_collection_items);
    }

    #[test]
    #[should_panic(expected = "predicate_template substitution left Placeholder")]
    fn test_filter_predicate_template_substitution_requires_full_resolution() {
        let source_id = CollectionId::new();
        let output_id = CollectionId::new();
        let item = Value::object([
            ("__key", Value::text("a")),
            ("flag", Value::Bool(true)),
        ]);

        let predicate_template = Value::object([
            ("flag", Value::Placeholder),
            ("missing", Value::Placeholder),
        ]);

        let mut initial_collections = HashMap::new();
        initial_collections.insert(source_id, vec![item]);

        let collections = DdCollectionConfig {
            ops: vec![CollectionOpConfig {
                output_id,
                source_id,
                op: CollectionOp::Filter {
                    field_filter: None,
                    predicate_template: Some(TemplateValue::from_value(predicate_template)),
                },
            }],
            initial_collections: initial_collections.clone(),
            collection_sources: HashMap::new(),
        };

        let initial_collection_items = initial_collections;
        let initial = HashMap::new();

        let _ = run_dd_first_batch(Vec::new(), collections, Vec::new(), &initial, &initial_collection_items);
    }

    #[test]
    fn test_persistent_worker_increment() {
        // Create a persistent worker (not using global, direct instantiation)
        let cells = vec![DdCellConfig {
            id: CellId::new("count"),
            initial: Value::Number(OrderedFloat(0.0)),
            triggers: vec![LinkId::new("click")],
            transform: DdTransform::Increment,
            filter: EventFilter::Any,
        }];

        let collections = DdCollectionConfig {
            ops: Vec::new(),
            initial_collections: HashMap::new(),
            collection_sources: HashMap::new(),
        };
        let initial_collection_items = HashMap::new();
        let mut worker = PersistentDdWorker::new(
            cells,
            collections,
            HashMap::new(),
            HashMap::new(),
            initial_collection_items,
        );

        // Inject events one by one (simulating real user interaction)
        worker.inject_event(&LinkId::new("click"), EventValue::Unit);
        let outputs1 = worker.drain_outputs();
        assert_eq!(outputs1.len(), 1);
        assert_eq!(
            outputs1[0].value,
            CellUpdate::set_value("count", Value::Number(OrderedFloat(1.0)))
        );

        worker.inject_event(&LinkId::new("click"), EventValue::Unit);
        let outputs2 = worker.drain_outputs();
        assert_eq!(outputs2.len(), 1);
        assert_eq!(
            outputs2[0].value,
            CellUpdate::set_value("count", Value::Number(OrderedFloat(2.0)))
        );

        worker.inject_event(&LinkId::new("click"), EventValue::Unit);
        let outputs3 = worker.drain_outputs();
        assert_eq!(outputs3.len(), 1);
        assert_eq!(
            outputs3[0].value,
            CellUpdate::set_value("count", Value::Number(OrderedFloat(3.0)))
        );

        // Verify time advances correctly
        assert_eq!(worker.current_time(), 3);
    }

    #[test]
    fn test_persistent_worker_multiple_cells() {
        // Test that multiple cells work with persistent worker
        let cells = vec![
            DdCellConfig {
                id: CellId::new("count"),
                initial: Value::Number(OrderedFloat(0.0)),
                triggers: vec![LinkId::new("inc")],
                transform: DdTransform::Increment,
                filter: EventFilter::Any,
            },
            DdCellConfig {
                id: CellId::new("enabled"),
                initial: Value::Bool(false),
                triggers: vec![LinkId::new("toggle")],
                transform: DdTransform::Toggle,
                filter: EventFilter::Any,
            },
        ];

        let collections = DdCollectionConfig {
            ops: Vec::new(),
            initial_collections: HashMap::new(),
            collection_sources: HashMap::new(),
        };
        let initial_collection_items = HashMap::new();
        let mut worker = PersistentDdWorker::new(
            cells,
            collections,
            HashMap::new(),
            HashMap::new(),
            initial_collection_items,
        );

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
        assert_eq!(
            outputs[0].value,
            CellUpdate::set_value("enabled", Value::Bool(true))
        );

        // Increment again
        worker.inject_event(&LinkId::new("inc"), EventValue::Unit);
        let outputs = worker.drain_outputs();
        assert_eq!(
            outputs[0].value,
            CellUpdate::set_value("count", Value::Number(OrderedFloat(2.0)))
        );
    }

    #[test]
    #[should_panic(expected = "List-like output applied to non-list cell")]
    fn test_apply_output_to_states_rejects_list_like_output() {
        let mut states: HashMap<String, Value> = HashMap::new();
        states.insert("count".to_string(), Value::int(0));
        let initial_by_id = states.clone();
        let output = CellUpdate::set_value("count", Value::list_with_cell("items"));
        apply_output_to_states(&mut states, &output, &initial_by_id);
    }

    #[test]
    #[should_panic(expected = "List-like output applied to non-list cell")]
    fn test_apply_output_to_states_rejects_multicell_list_like() {
        let mut states: HashMap<String, Value> = HashMap::new();
        states.insert("count".to_string(), Value::int(0));
        let initial_by_id = states.clone();
        let output = CellUpdate::multi(vec![
            CellUpdate::set_value("count", Value::list_with_cell("items")),
        ]);
        apply_output_to_states(&mut states, &output, &initial_by_id);
    }

    #[test]
    #[should_panic(expected = "Output cell 'other' does not match 'items'")]
    fn test_apply_output_to_state_maps_for_batch_rejects_wrong_list_cell() {
        let mut list_cells: HashSet<String> = HashSet::new();
        list_cells.insert("items".to_string());
        let derived_list_outputs: HashSet<String> = HashSet::new();

        let item = Value::object([("__key", Value::text("a"))]);
        let collection_id = CollectionId::new();
        let mut initial_by_id: HashMap<String, Value> = HashMap::new();
        initial_by_id.insert(
            "items".to_string(),
            Value::List(CollectionHandle::with_id_and_cell(collection_id, "items")),
        );
        let mut initial_collection_items: HashMap<CollectionId, Vec<Value>> = HashMap::new();
        initial_collection_items.insert(collection_id, vec![item.clone()]);

        let mut list_states: HashMap<String, ListState> = HashMap::new();
        let mut scalar_states: HashMap<String, Value> = HashMap::new();
        let output = CellUpdate::list_push("other", item);

        apply_output_to_state_maps_for_batch(
            &list_cells,
            &derived_list_outputs,
            &mut list_states,
            &mut scalar_states,
            "items",
            &output,
            &initial_by_id,
            &initial_collection_items,
        );
    }
}
