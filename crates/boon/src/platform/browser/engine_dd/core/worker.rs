//! DD Worker - Differential Dataflow worker with async event loop.
//!
//! This module provides the core DD worker that processes events through
//! Differential Dataflow in an async-friendly way for browser execution.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     spawn_local() async                         │
//! │  ┌───────────────────────────────────────────────────────────┐  │
//! │  │                    Event Loop                             │  │
//! │  │                                                           │  │
//! │  │  1. yield_to_browser().await                             │  │
//! │  │  2. collect_pending_events()                             │  │
//! │  │  3. execute_directly() { process events }                │  │
//! │  │  4. send outputs via channel                             │  │
//! │  │  5. goto 1                                               │  │
//! │  └───────────────────────────────────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Anti-Cheat Design
//!
//! - NO `Mutable<T>` - State lives in DD or is passed through channels
//! - NO `RefCell<T>` - No shared mutable state
//! - NO `.get()` - All observation through Output streams

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
#[allow(unused_imports)]
use super::super::dd_log;
use zoon::futures_channel::mpsc;
use zoon::{Task, Timer};

use super::types::{Event, EventValue, Input, CellId, LinkId, BoolTag, EventFilter};
use super::value::{
    attach_item_key, attach_or_validate_item_key, contains_placeholder,
    ensure_unique_item_keys, extract_item_key,
    CellUpdate, CollectionHandle, CollectionId, TemplateValue, Value,
};
use super::collection_ops::{CollectionOp, CollectionOpConfig, ComputedTextPart};
use super::super::io::{
    load_persisted_cell_value_with_collections,
    sync_cell_from_dd,
    sync_cell_from_dd_with_persist,
    sync_list_state_from_dd,
    sync_list_state_from_dd_with_persist,
    navigate_to_route,
};
use std::collections::BTreeMap;
// Persistent DD Worker
use super::dataflow::{
    DdCellConfig, DdCollectionConfig, DdTransform,
    inject_event_persistent, drain_outputs_persistent, reinit_if_config_changed,
};

// ============================================================================
// PHASE 5: DETERMINISTIC ID COUNTERS
// ============================================================================
//
// ID counters are reset to 1000 when a worker is created. This gives:
// - Determinism: reset_id_counters() resets both to 1000 for each Worker session
// - Replayability: same inputs always produce same IDs
//
// Worker calls reset_id_counters() in new() to start each session fresh.

/// Counter for generating unique HOLD IDs for dynamic items.
///
/// Global atomic because `allocate_dynamic_cell_id()` is called from helper
/// functions that don't have a `&mut Worker` reference. Reset per session
/// via `reset_id_counters()` in `Worker::new()`.
static DYNAMIC_CELL_COUNTER: AtomicU32 = AtomicU32::new(1000);

/// Counter for generating unique LINK IDs for dynamic items.
///
/// Global atomic because `allocate_dynamic_link_id()` is called from helper
/// functions that don't have a `&mut Worker` reference. Reset per session
/// via `reset_id_counters()` in `Worker::new()`.
static DYNAMIC_LINK_COUNTER: AtomicU32 = AtomicU32::new(1000);

/// Reset ID counters to starting values (1000).
/// Called by Worker::new() to ensure deterministic ID generation per session.
pub fn reset_id_counters() {
    DYNAMIC_CELL_COUNTER.store(1000, Ordering::SeqCst);
    DYNAMIC_LINK_COUNTER.store(1000, Ordering::SeqCst);
}

/// Allocate a new dynamic cell ID.
fn allocate_dynamic_cell_id() -> String {
    let id = DYNAMIC_CELL_COUNTER.fetch_add(1, Ordering::SeqCst);
    CellId::dynamic(id).name()
}

/// Allocate a new dynamic link ID.
fn allocate_dynamic_link_id() -> String {
    let id = DYNAMIC_LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
    LinkId::dynamic(id).name().to_string()
}

// NOTE: Dynamic ID string formats are centralized in CellId/LinkId.

fn list_cell_ids(config: &DataflowConfig) -> HashSet<String> {
    let mut ids: HashSet<String> = HashSet::new();
    for cell in &config.cells {
        if matches!(
            cell.transform,
            StateTransform::ListAppendWithTemplate { .. }
                | StateTransform::ListAppendWithTemplateAndClear { .. }
                | StateTransform::ListAppendFromTemplate { .. }
                | StateTransform::ListAppendSimple
                | StateTransform::ListAppendSimpleWithClear { .. }
        ) {
            ids.insert(cell.id.name());
        }
        if matches!(cell.initial, Value::List(_)) {
            ids.insert(cell.id.name());
        }
    }
    for binding in &config.list_append_bindings {
        ids.insert(binding.list_cell_id.clone());
    }
    for (cell_id, init) in &config.cell_initializations {
        if matches!(init.value, Value::List(_)) {
            ids.insert(cell_id.clone());
        }
    }
    for (cell_id, _) in &config.remove_event_paths {
        ids.insert(cell_id.clone());
    }
    for binding in &config.bulk_remove_bindings {
        ids.insert(binding.list_cell_id.clone());
    }
    for (cell_id, _) in &config.list_item_templates {
        ids.insert(cell_id.clone());
    }
    for (cell_id, _) in &config.list_element_templates {
        ids.insert(cell_id.clone());
    }
    for (_collection_id, cell_id) in &config.collection_sources {
        ids.insert(cell_id.clone());
    }
    for op in &config.collection_ops {
        if matches!(op.op, CollectionOp::Filter { .. } | CollectionOp::Map { .. } | CollectionOp::Concat { .. }) {
            ids.insert(op.output_id.to_string());
        }
    }
    ids
}

fn derived_list_outputs(config: &DataflowConfig) -> HashSet<String> {
    let mut outputs = HashSet::new();
    for op in &config.collection_ops {
        if matches!(op.op, CollectionOp::Filter { .. } | CollectionOp::Map { .. } | CollectionOp::Concat { .. }) {
            outputs.insert(op.output_id.to_string());
        }
    }
    outputs
}

fn collection_id_for_cell(config: &DataflowConfig, cell_id: &str) -> CollectionId {
    // Check source collections first (e.g., "todos" → CollectionId)
    config.collection_sources
        .iter()
        .find_map(|(collection_id, source_cell)| {
            if source_cell == cell_id { Some(*collection_id) } else { None }
        })
        // Also check collection op outputs (e.g., "collection_33" from Filter/Map/Concat)
        .or_else(|| {
            config.collection_ops.iter().find_map(|op| {
                if op.output_id.to_string() == cell_id { Some(op.output_id) } else { None }
            })
        })
        .unwrap_or_else(|| {
            panic!("[DD Worker] Missing collection id for list cell '{}'", cell_id);
        })
}

fn validate_cell_state_value(
    cell_id: &str,
    value: Value,
    list_cells: &HashSet<String>,
) -> Value {
    if !list_cells.contains(cell_id) {
        if matches!(value, Value::List(_)) {
            panic!(
                "[DD Worker] Non-list cell '{}' cannot store list-like value {:?}",
                cell_id, value
            );
        }
        return value;
    }
    match value {
        Value::List(handle) => {
            let existing = handle.cell_id.as_deref().unwrap_or_else(|| {
                panic!("[DD Worker] Missing collection cell_id for '{}'", cell_id);
            });
            if existing != cell_id {
                panic!("[DD Worker] Collection cell_id mismatch: expected '{}', found '{}'", cell_id, existing);
            }
            Value::List(handle)
        }
        Value::Unit => {
            // Forward reference not yet resolved — return Unit; DD will handle it
            dd_log!("[DD Worker] List cell '{}' has Unit initial value (forward ref?), skipping validation", cell_id);
            Value::Unit
        }
        other => {
            panic!(
                "[DD Worker] List cell '{}' must be List, found {:?}",
                cell_id, other
            );
        }
    }
}

fn build_list_states(
    list_cells: &HashSet<String>,
    derived_list_outputs: &HashSet<String>,
    list_items_by_cell: &HashMap<String, Vec<Value>>,
) -> HashMap<String, super::dataflow::ListState> {
    let mut list_states = HashMap::new();
    for cell_id in list_cells {
        if let Some(items) = list_items_by_cell.get(cell_id) {
            list_states.insert(
                cell_id.clone(),
                super::dataflow::ListState::new(items.clone(), cell_id),
            );
        } else if derived_list_outputs.contains(cell_id) {
            list_states.insert(
                cell_id.clone(),
                super::dataflow::ListState::new(Vec::new(), cell_id),
            );
        } else {
            panic!("[DD Worker] Missing list items for '{}'", cell_id);
        }
    }
    list_states
}

fn ensure_list_states(
    list_cells: &HashSet<String>,
    derived_list_outputs: &HashSet<String>,
    list_states: &mut HashMap<String, super::dataflow::ListState>,
    initial_list_states: &HashMap<String, super::dataflow::ListState>,
) {
    for cell_id in list_cells {
        if list_states.contains_key(cell_id) {
            continue;
        }
        if let Some(state) = initial_list_states.get(cell_id) {
            list_states.insert(cell_id.clone(), state.clone());
        } else if derived_list_outputs.contains(cell_id) {
            list_states.insert(
                cell_id.clone(),
                super::dataflow::ListState::new(Vec::new(), cell_id),
            );
        } else {
            panic!("[DD Worker] Missing list state for '{}'", cell_id);
        }
    }
}

fn strip_list_cells(cell_states: &mut HashMap<String, Value>, list_cells: &HashSet<String>) {
    for cell_id in list_cells {
        cell_states.remove(cell_id);
    }
}

fn apply_output_to_state_maps(
    list_cells: &HashSet<String>,
    list_states: &mut HashMap<String, super::dataflow::ListState>,
    cell_states: &mut HashMap<String, Value>,
    output: &CellUpdate,
    initial_by_id: &HashMap<String, Value>,
) {
    match output {
        CellUpdate::Multi(updates) => {
            for update in updates.iter() {
                apply_output_to_state_maps(
                    list_cells,
                    list_states,
                    cell_states,
                    update,
                    initial_by_id,
                );
            }
        }
        CellUpdate::NoOp => {}
        _ => {
            let cell_id = output.cell_id().unwrap_or_else(|| {
                panic!("[DD Worker] Missing cell id for update {:?}", output);
            });
            if list_cells.contains(cell_id) {
                apply_list_update_to_state(list_states, cell_id, output);
                if matches!(output, CellUpdate::SetValue { .. }) {
                    panic!(
                        "[DD Worker] List cell '{}' received non-diff output {:?}",
                        cell_id, output
                    );
                }
                return;
            }

            match output {
                CellUpdate::SetValue { value, .. } => {
                    cell_states.insert(cell_id.to_string(), value.clone());
                }
                _ => {
                    panic!(
                        "[DD Worker] List diff applied to non-list cell '{}': {:?}",
                        cell_id, output
                    );
                }
            }
        }
    }
}

fn apply_list_update_to_state(
    list_states: &mut HashMap<String, super::dataflow::ListState>,
    cell_id: &str,
    output: &CellUpdate,
) {
    let list_state = list_states.get_mut(cell_id).unwrap_or_else(|| {
        panic!("[DD Worker] Missing list state for '{}'", cell_id);
    });

    match output {
        CellUpdate::ListPush { cell_id: diff_cell_id, item } => {
            if diff_cell_id.as_ref() != cell_id {
                panic!(
                    "[DD Worker] ListPush diff for '{}' applied to '{}'",
                    diff_cell_id, cell_id
                );
            }
            list_state.push(item.clone(), "list push");
        }
        CellUpdate::ListInsertAt { cell_id: diff_cell_id, index, item } => {
            if diff_cell_id.as_ref() != cell_id {
                panic!(
                    "[DD Worker] ListInsertAt diff for '{}' applied to '{}'",
                    diff_cell_id, cell_id
                );
            }
            list_state.insert(*index, item.clone(), "list insert");
        }
        CellUpdate::ListRemoveAt { cell_id: diff_cell_id, index } => {
            if diff_cell_id.as_ref() != cell_id {
                panic!(
                    "[DD Worker] ListRemoveAt diff for '{}' applied to '{}'",
                    diff_cell_id, cell_id
                );
            }
            list_state.remove_at(*index, "list remove");
        }
        CellUpdate::ListRemoveByKey { cell_id: diff_cell_id, key } => {
            if diff_cell_id.as_ref() != cell_id {
                panic!(
                    "[DD Worker] ListRemoveByKey diff for '{}' applied to '{}'",
                    diff_cell_id, cell_id
                );
            }
            list_state.remove_by_key(key.as_ref(), "list remove by key");
        }
        CellUpdate::ListRemoveBatch { cell_id: diff_cell_id, keys } => {
            if diff_cell_id.as_ref() != cell_id {
                panic!(
                    "[DD Worker] ListRemoveBatch diff for '{}' applied to '{}'",
                    diff_cell_id, cell_id
                );
            }
            list_state.remove_batch(keys, "list remove batch");
        }
        CellUpdate::ListClear { cell_id: diff_cell_id } => {
            if diff_cell_id.as_ref() != cell_id {
                panic!(
                    "[DD Worker] ListClear diff for '{}' applied to '{}'",
                    diff_cell_id, cell_id
                );
            }
            list_state.clear();
        }
        CellUpdate::ListItemUpdate { cell_id: diff_cell_id, key, field_path, new_value } => {
            if diff_cell_id.as_ref() != cell_id {
                panic!(
                    "[DD Worker] ListItemUpdate diff for '{}' applied to '{}'",
                    diff_cell_id, cell_id
                );
            }
            list_state.update_field(key.as_ref(), field_path, new_value, "list item update");
        }
        CellUpdate::SetValue { .. } => {
            panic!(
                "[DD Worker] List cell '{}' received non-diff output {:?}",
                cell_id, output
            );
        }
        CellUpdate::Multi(_) => {
            panic!("[DD Worker] Multi update must be expanded before list state update");
        }
        CellUpdate::NoOp => {}
        other => {
            panic!(
                "[DD Worker] List cell '{}' received non-diff output {:?}",
                cell_id, other
            );
        }
    }
}


// ============================================================================
// GENERIC TEMPLATE SYSTEM
// ============================================================================
// These types replace hardcoded item-specific logic with configurable templates.

/// Path to a field within a nested Value structure.
/// Example: `["item_elements", "remove_item_button"]` accesses `item.item_elements.remove_item_button`
pub type FieldPath = Vec<String>;

/// Specifies how to identify a list item by matching a LinkRef field.
/// Used for stable identity routing (LinkRef IDs don't change when items are reordered).
#[derive(Clone, Debug)]
pub struct ItemIdentitySpec {
    /// Path to the LinkRef field that uniquely identifies the item.
    /// Example: `["item_elements", "remove_item_button"]` for items
    pub link_ref_path: FieldPath,
}

/// Specifies how to initialize a field when creating a new list item from a template.
#[derive(Clone, Debug)]
pub enum FieldInitializer {
    /// Initialize from the event text (e.g., Enter key text)
    FromEventText,
    /// Initialize to a constant value
    Constant(Value),
}

/// Specifies an action to register for a LINK when cloning a template.
#[derive(Clone, Debug)]
pub enum LinkActionSpec {
    /// Set a HOLD to true when this link fires (e.g., double-click to enter edit mode)
    SetHoldTrue { hold_path: FieldPath },
    /// Set a HOLD to false when this link fires (e.g., Escape to exit edit mode)
    SetHoldFalse { hold_path: FieldPath },
    /// Track hover state (true on mouseenter, false on mouseleave)
    HoverState { hold_path: FieldPath },
}

/// Configuration for registering a link action during template cloning.
#[derive(Clone, Debug)]
pub struct LinkActionConfig {
    /// Path to the LinkRef in the data template
    pub link_path: FieldPath,
    /// What action to perform when this link fires
    pub action: LinkActionSpec,
}

/// Template for dynamically creating list items with fresh HOLD/LINK IDs.
///
/// This replaces hardcoded item-specific logic with a declarative configuration.
/// The same template system works for any list type (items, shopping items, messages, etc.)
#[derive(Clone, Debug)]
pub struct ListItemTemplate {
    /// The data object template (contains CellRef/LinkRef placeholders)
    pub data_template: TemplateValue,
    /// Optional element AST template for rendering
    pub element_template: Option<TemplateValue>,
    /// How to identify this item uniquely (for stable event routing)
    pub identity: ItemIdentitySpec,
    /// Fields to initialize from input when creating new items
    pub field_initializers: Vec<(FieldPath, FieldInitializer)>,
    /// Link actions to register when cloning the template
    pub link_actions: Vec<LinkActionConfig>,
    /// Mappings of field names to their CellRef paths for persisted value reconstruction
    /// Example: [("title", ["title"]), ("completed", ["completed"])]
    pub persisted_fields: Vec<(String, FieldPath)>,
}

impl ListItemTemplate {
    /// Create a new template with required fields.
    pub fn new(data_template: TemplateValue, identity_path: FieldPath) -> Self {
        Self {
            data_template,
            element_template: None,
            identity: ItemIdentitySpec { link_ref_path: identity_path },
            field_initializers: Vec::new(),
            link_actions: Vec::new(),
            persisted_fields: Vec::new(),
        }
    }

    /// Set the element template for rendering.
    pub fn with_element_template(mut self, template: TemplateValue) -> Self {
        self.element_template = Some(template);
        self
    }

    /// Add a field initializer.
    pub fn with_field_initializer(mut self, path: FieldPath, initializer: FieldInitializer) -> Self {
        self.field_initializers.push((path, initializer));
        self
    }

    /// Add a link action.
    pub fn with_link_action(mut self, link_path: FieldPath, action: LinkActionSpec) -> Self {
        self.link_actions.push(LinkActionConfig { link_path, action });
        self
    }

    /// Add a persisted field mapping.
    pub fn with_persisted_field(mut self, name: &str, path: FieldPath) -> Self {
        self.persisted_fields.push((name.to_string(), path));
        self
    }
}

/// Get a value at a field path within a Value.
pub fn get_at_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }
    match value {
        Value::Object(obj) => {
            obj.get(path[0].as_str()).and_then(|v| get_at_path(v, &path[1..]))
        }
        Value::Tagged { fields, .. } => {
            fields.get(path[0].as_str()).and_then(|v| get_at_path(v, &path[1..]))
        }
        _ => None,
    }
}

/// Get the LinkRef ID at a field path.
pub fn get_link_ref_at_path(value: &Value, path: &[String]) -> Option<String> {
    get_at_path(value, path).and_then(|v| match v {
        Value::LinkRef(id) => Some(id.name().to_string()),
        _ => None,
    })
}

/// Get the CellRef ID at a field path.
pub fn get_hold_ref_at_path(value: &Value, path: &[String]) -> Option<String> {
    get_at_path(value, path).and_then(|v| match v {
        Value::CellRef(id) => Some(id.name()),
        _ => None,
    })
}

/// Update a field at the given path within a Value.
/// Returns a new Value with the updated field.
pub fn update_field_at_path(value: &Value, path: &[String], update: &FieldUpdate) -> Value {
    if path.is_empty() {
        // Apply update to the value directly
        return match update {
            FieldUpdate::Constant(v) => v.clone(),
            FieldUpdate::Toggle => match value {
                Value::Bool(b) => Value::Bool(!b),
                Value::Tagged { tag, .. } if BoolTag::is_true(tag.as_ref()) => Value::Bool(false),
                Value::Tagged { tag, .. } if BoolTag::is_false(tag.as_ref()) => Value::Bool(true),
                other => panic!("[DD] FieldUpdate::Toggle expected Bool, found {:?}", other),
            },
            FieldUpdate::SetTrue => Value::Bool(true),
            FieldUpdate::SetFalse => Value::Bool(false),
        };
    }

    match value {
        Value::Object(obj) => {
            let mut new_obj = (**obj).clone();
            let field_value = obj.get(path[0].as_str()).unwrap_or_else(|| {
                panic!("[DD] update_field_at_path missing field '{}' in Object", path[0]);
            });
            new_obj.insert(
                Arc::from(path[0].as_str()),
                update_field_at_path(field_value, &path[1..], update),
            );
            Value::Object(Arc::new(new_obj))
        }
        Value::Tagged { tag, fields } => {
            let mut new_fields = (**fields).clone();
            let field_value = fields.get(path[0].as_str()).unwrap_or_else(|| {
                panic!("[DD] update_field_at_path missing field '{}' in Tagged", path[0]);
            });
            new_fields.insert(
                Arc::from(path[0].as_str()),
                update_field_at_path(field_value, &path[1..], update),
            );
            Value::Tagged { tag: tag.clone(), fields: Arc::new(new_fields) }
        }
        other => panic!("[DD] update_field_at_path expected Object/Tagged, found {:?}", other),
    }
}

// ============================================================================
// END GENERIC TEMPLATE SYSTEM
// ============================================================================

/// Extract field-to-hold-id mappings from a template.
/// Returns a map like {"title" -> "hold_18", "completed" -> "hold_20", ...}
fn extract_field_to_hold_map(template: &Value) -> HashMap<String, String> {
    let mut result = HashMap::new();
    if let Value::Object(fields) = template {
        for (field_name, value) in fields.iter() {
            if let Value::CellRef(cell_id) = value {
                result.insert(field_name.to_string(), cell_id.name());
            }
        }
    }
    result
}

/// Clone a template Value with fresh CellRef/LinkRef IDs.
/// Returns the cloned value and a mapping of old CellRef IDs to new IDs.
/// The optional `data_context` parameter is used to resolve PlaceholderFields
/// (deferred field accesses) to actual LinkRefs from the cloned data item.
fn clone_template_with_fresh_ids(
    template: &Value,
    cell_id_map: &mut HashMap<String, String>,
    link_id_map: &mut HashMap<String, String>,
    collection_id_map: &mut HashMap<CollectionId, CollectionId>,
    initial_collections: &mut HashMap<CollectionId, Vec<Value>>,
) -> Value {
    clone_template_with_fresh_ids_impl(
        template,
        cell_id_map,
        link_id_map,
        collection_id_map,
        initial_collections,
        None,
    )
}

/// Clone a template Value with fresh IDs and optional data context for PlaceholderField resolution.
fn clone_template_with_fresh_ids_with_context(
    template: &Value,
    cell_id_map: &mut HashMap<String, String>,
    link_id_map: &mut HashMap<String, String>,
    collection_id_map: &mut HashMap<CollectionId, CollectionId>,
    initial_collections: &mut HashMap<CollectionId, Vec<Value>>,
    data_context: &Value,
) -> Value {
    clone_template_with_fresh_ids_impl(
        template,
        cell_id_map,
        link_id_map,
        collection_id_map,
        initial_collections,
        Some(data_context),
    )
}

fn clone_template_with_fresh_ids_impl(
    template: &Value,
    cell_id_map: &mut HashMap<String, String>,
    link_id_map: &mut HashMap<String, String>,
    collection_id_map: &mut HashMap<CollectionId, CollectionId>,
    initial_collections: &mut HashMap<CollectionId, Vec<Value>>,
    data_context: Option<&Value>,
) -> Value {
    match template {
        Value::CellRef(old_id) => {
            // Generate or reuse fresh ID for this CellRef
            // Use thread-local allocator for deterministic IDs
            let new_id = cell_id_map
                .entry(old_id.name())
                .or_insert_with(allocate_dynamic_cell_id)
                .clone();
            Value::CellRef(CellId::new(new_id))
        }
        Value::LinkRef(old_id) => {
            // Generate or reuse fresh ID for this LinkRef
            // Use thread-local allocator for deterministic IDs
            let new_id = link_id_map
                .entry(old_id.name().to_string())
                .or_insert_with(allocate_dynamic_link_id)
                .clone();
            Value::LinkRef(LinkId::new(new_id))
        }
        Value::Object(fields) => {
            let new_fields: BTreeMap<Arc<str>, Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), clone_template_with_fresh_ids_impl(
                    v,
                    cell_id_map,
                    link_id_map,
                    collection_id_map,
                    initial_collections,
                    data_context,
                )))
                .collect();
            Value::Object(Arc::new(new_fields))
        }
        Value::PlaceholderField(_) | Value::PlaceholderWhile(_) => {
            let data_context = data_context.unwrap_or_else(|| {
                panic!("[DD Worker] PlaceholderField requires data_context during template clone");
            });
            let resolved = template.substitute_placeholders(data_context);
            clone_template_with_fresh_ids_impl(
                &resolved,
                cell_id_map,
                link_id_map,
                collection_id_map,
                initial_collections,
                Some(data_context),
            )
        }
        Value::WhileConfig(config) => {
            let new_cell_id = cell_id_map
                .entry(config.cell_id.name())
                .or_insert_with(allocate_dynamic_cell_id)
                .clone();
            let arms: Vec<super::value::WhileArm> = config
                .arms
                .iter()
                .map(|arm| super::value::WhileArm {
                    pattern: clone_template_with_fresh_ids_impl(
                        &arm.pattern,
                        cell_id_map,
                        link_id_map,
                        collection_id_map,
                        initial_collections,
                        data_context,
                    ),
                    body: clone_template_with_fresh_ids_impl(
                        &arm.body,
                        cell_id_map,
                        link_id_map,
                        collection_id_map,
                        initial_collections,
                        data_context,
                    ),
                })
                .collect();
            let default = clone_template_with_fresh_ids_impl(
                &config.default,
                cell_id_map,
                link_id_map,
                collection_id_map,
                initial_collections,
                data_context,
            );
            Value::WhileConfig(Arc::new(super::value::WhileConfig {
                cell_id: CellId::new(new_cell_id),
                arms: Arc::new(arms),
                default: Box::new(default),
            }))
        }
        Value::List(handle) => {
            if handle.cell_id.is_some() {
                return Value::List(handle.clone());
            }
            let new_id = if let Some(existing) = collection_id_map.get(&handle.id) {
                *existing
            } else {
                let original_items = initial_collections.get(&handle.id).unwrap_or_else(|| {
                    panic!(
                        "[DD Worker] Missing initial items for collection {:?} during template clone",
                        handle.id
                    );
                }).clone();
                let cloned_items: Vec<Value> = original_items.iter()
                    .map(|item| clone_template_with_fresh_ids_impl(
                        item,
                        cell_id_map,
                        link_id_map,
                        collection_id_map,
                        initial_collections,
                        None,
                    ))
                    .collect();
                let new_id = CollectionId::new();
                initial_collections.insert(new_id, cloned_items);
                collection_id_map.insert(handle.id, new_id);
                new_id
            };
            Value::List(CollectionHandle::new_with_id(new_id))
        }
        Value::Tagged { tag, fields } => {
            let new_fields: BTreeMap<Arc<str>, Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), clone_template_with_fresh_ids_impl(
                    v,
                    cell_id_map,
                    link_id_map,
                    collection_id_map,
                    initial_collections,
                    None,
                )))
                .collect();
            Value::Tagged { tag: tag.clone(), fields: Arc::new(new_fields) }
        }
        // Primitive types and removed symbolic refs are cloned as-is
        _ => template.clone(),
    }
}

/// Find the cell_id of a WHILE config value that controls hover-based visibility.
fn find_hover_while_cell_id(
    items: Option<&Value>,
    initial_collections: &HashMap<CollectionId, Vec<Value>>,
) -> Option<String> {
    // Search for WhileConfig in items
    let items = items?;
    let iter: Box<dyn Iterator<Item = &Value>> = match items {
        Value::List(handle) => {
            let items = initial_collections.get(&handle.id).unwrap_or_else(|| {
                panic!(
                    "[DD Worker] Missing initial items for collection {:?} in hover scan",
                    handle.id
                );
            });
            Box::new(items.iter())
        }
        _ => return None,
    };
    for item in iter {
        if let Value::WhileConfig(config) = item {
            return Some(config.cell_id.name().to_string());
        }
    }
    None
}

/// Extract hover link/cell IDs from an element template and pre-populate ID mappings.
/// This ensures hover WHILE holds get fresh cell IDs before template cloning.
/// Returns (hover_link_old_id, hover_cell_old_id) for downstream initialization.
fn pre_populate_hover_mappings(
    elem: &Value,
    cell_id_map: &mut HashMap<String, String>,
    link_id_map: &mut HashMap<String, String>,
    initial_collections: &HashMap<CollectionId, Vec<Value>>,
) -> (Option<String>, Option<String>) {
    let hover_link_old_id = if let Value::Tagged { fields, .. } = elem {
        fields.get("element")
            .and_then(|e| e.get("hovered"))
            .and_then(|h| match h {
                Value::LinkRef(id) => Some(id.name().to_string()),
                _ => None,
            })
    } else {
        None
    };

    let hover_cell_old_id = if let Value::Tagged { fields, .. } = elem {
        find_hover_while_cell_id(fields.get("items"), initial_collections)
    } else {
        None
    };

    if let (Some(old_link), Some(old_hold)) = (&hover_link_old_id, &hover_cell_old_id) {
        link_id_map
            .entry(old_link.clone())
            .or_insert_with(allocate_dynamic_link_id);
        let new_hover_cell = allocate_dynamic_cell_id();
        cell_id_map.insert(old_hold.clone(), new_hover_cell.clone());
        dd_log!("[DD Worker] Pre-mapped hover hold: {} -> {}", old_hold, new_hover_cell);
    }

    (hover_link_old_id, hover_cell_old_id)
}

/// Build a stable item key from an optional identity link id.
/// Fail-fast if identity_path is missing or doesn't resolve.
fn build_item_key(identity_link_id: Option<&str>, identity_path: &[String], context: &str) -> String {
    if let Some(link_id) = identity_link_id {
        format!("link:{}", link_id)
    } else if identity_path.is_empty() {
        panic!("Bug: missing identity path in {}", context);
    } else {
        panic!("Bug: identity path {:?} did not resolve to LinkRef in {}", identity_path, context);
    }
}

/// Check if a persisted value is a stripped LinkRef structure that should not be overlaid.
/// Returns true for Unit values or Objects where all values are Unit (stripped LinkRef objects).
fn is_stripped_linkref_value(value: &Value) -> bool {
    match value {
        Value::Unit => true,
        Value::Object(fields) => !fields.is_empty() && fields.values().all(|v| matches!(v, Value::Unit)),
        _ => false,
    }
}

fn bool_from_value(value: &Value, cell_states: &HashMap<String, Value>, context: &str) -> bool {
    let resolved = match value {
        Value::CellRef(cell_id) => {
            let cell_name = cell_id.name();
            cell_states.get(&cell_name).unwrap_or_else(|| {
                panic!(
                    "[DD CollectionOp] missing cell state '{}' for {}",
                    cell_name, context
                );
            })
        }
        other => other,
    };
    match resolved {
        Value::Bool(b) => *b,
        Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => {
            BoolTag::is_true(tag.as_ref())
        }
        other => panic!(
            "[DD CollectionOp] {} must evaluate to Bool/BoolTag, found {:?}",
            context, other
        ),
    }
}

fn is_item_field_equal(
    item: &Value,
    field_name: &str,
    expected: &Value,
    cell_states: &HashMap<String, Value>,
) -> bool {
    fn bool_value(value: &Value) -> Option<bool> {
        match value {
            Value::Bool(b) => Some(*b),
            Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => {
                Some(BoolTag::is_true(tag.as_ref()))
            }
            _ => None,
        }
    }

    let fields = match item {
        Value::Object(obj) => obj,
        Value::Tagged { fields, .. } => fields,
        other => panic!("[DD CollectionOp] expected Object/Tagged item, found {:?}", other),
    };
    let field_value = fields.get(field_name)
        .unwrap_or_else(|| panic!("[DD CollectionOp] missing '{}' field on item", field_name));
    let actual = match field_value {
        Value::CellRef(cell_id) => {
            let cell_name = cell_id.name();
            cell_states.get(&cell_name).unwrap_or_else(|| {
                panic!(
                    "[DD CollectionOp] missing cell state '{}' for field '{}'",
                    cell_name, field_name
                );
            })
        }
        other => other,
    };
    let expected_value = match expected {
        Value::CellRef(cell_id) => {
            let cell_name = cell_id.name();
            cell_states.get(&cell_name).unwrap_or_else(|| {
                panic!(
                    "[DD CollectionOp] missing cell state '{}' for expected value",
                    cell_name
                );
            })
        }
        other => other,
    };
    match (bool_value(actual), bool_value(expected_value)) {
        (Some(actual_bool), Some(expected_bool)) => actual_bool == expected_bool,
        _ => actual == expected_value,
    }
}

fn compute_collection_items(
    config: &DataflowConfig,
    list_states: &HashMap<String, super::dataflow::ListState>,
) -> HashMap<CollectionId, Vec<Value>> {
    if config.collection_ops.is_empty() {
        return HashMap::new();
    }

    let mut items_by_id: HashMap<CollectionId, Vec<Value>> = HashMap::new();

    for collection_id in config.collection_sources.keys() {
        if !config.initial_collections.contains_key(collection_id) {
            panic!(
                "[DD CollectionOp] Missing initial collection entry for source {:?}",
                collection_id
            );
        }
    }

    for (collection_id, items) in &config.initial_collections {
        if let Some(source_cell_id) = config.collection_sources.get(collection_id) {
            let list_state = list_states.get(source_cell_id).unwrap_or_else(|| {
                panic!(
                    "[DD CollectionOp] Missing list state for source cell '{}' (collection {:?})",
                    source_cell_id, collection_id
                );
            });
            items_by_id.insert(collection_id.clone(), list_state.items().to_vec());
        } else {
            ensure_unique_item_keys(items, "initial collection");
            items_by_id.insert(collection_id.clone(), items.clone());
        }
    }

    items_by_id
}

fn compute_bulk_remove_diff(
    binding: &BulkRemoveBinding,
    cell_states: &HashMap<String, Value>,
    list_states: &HashMap<String, super::dataflow::ListState>,
) -> Option<CellUpdate> {
    let list_state = list_states.get(&binding.list_cell_id).unwrap_or_else(|| {
        panic!(
            "[DD BulkRemove] missing list state '{}' for bulk remove",
            binding.list_cell_id
        );
    });
    let items = list_state.items();

    let mut keys: Vec<Arc<str>> = Vec::new();
    for item in items {
        let matches = match (binding.field_filter.as_ref(), binding.predicate_template.as_ref()) {
            (Some((field_name, field_value)), None) => {
                is_item_field_equal(item, field_name.as_ref(), field_value, cell_states)
            }
            (None, Some(template)) => {
                if !template.contains_placeholder() {
                    panic!("[DD BulkRemove] predicate_template must reference item data via Placeholder");
                }
                let resolved_item = super::dataflow::resolve_item_cellrefs(item, cell_states);
                let substituted = template.substitute_placeholders(&resolved_item);
                if contains_placeholder(&substituted) {
                    panic!(
                        "[DD BulkRemove] predicate_template substitution left Placeholder in {:?}",
                        substituted
                    );
                }
                bool_from_value(&substituted, cell_states, "bulk remove predicate")
            }
            _ => {
                panic!("[DD BulkRemove] invalid bulk remove binding configuration");
            }
        };
        if matches {
            keys.push(extract_item_key(item, "bulk remove"));
        }
    }

    if keys.is_empty() {
        return None;
    }

    let mut seen: HashSet<Arc<str>> = HashSet::new();
    for key in &keys {
        if !seen.insert(key.clone()) {
            panic!(
                "[DD BulkRemove] duplicate __key '{}' in list '{}'",
                key, binding.list_cell_id
            );
        }
    }

    Some(CellUpdate::ListRemoveBatch {
        cell_id: Arc::from(binding.list_cell_id.as_str()),
        keys,
    })
}

fn apply_bulk_remove_for_link(
    config: &DataflowConfig,
    link_id: &super::types::LinkId,
    time: u64,
    initial_by_id: &HashMap<String, Value>,
    list_cells: &HashSet<String>,
    list_states: &mut HashMap<String, super::dataflow::ListState>,
    new_states: &mut HashMap<String, Value>,
    outputs: &mut Vec<DocumentUpdate>,
) {
    if config.bulk_remove_bindings.is_empty() {
        return;
    }

    for binding in &config.bulk_remove_bindings {
        if binding.link_id != *link_id {
            continue;
        }
        let Some(diff) = compute_bulk_remove_diff(binding, new_states, list_states) else {
            continue;
        };

        apply_output_to_state_maps(
            list_cells,
            list_states,
            new_states,
            &diff,
            initial_by_id,
        );

        let mut hold_updates = HashMap::new();
        let mut hold_state_updates = HashMap::new();
        if should_persist_cell(config, &binding.list_cell_id) {
            hold_updates.insert(binding.list_cell_id.clone(), diff.clone());
        } else {
            hold_state_updates.insert(binding.list_cell_id.clone(), diff.clone());
        }

        outputs.push(DocumentUpdate {
            document: diff,
            time,
            hold_updates,
            hold_state_updates,
        });
    }
}

fn apply_dd_outputs_for_worker(
    config: &DataflowConfig,
    dd_outputs: Vec<super::dataflow::DdOutput>,
    list_cells: &HashSet<String>,
    list_states: &mut HashMap<String, super::dataflow::ListState>,
    new_states: &mut HashMap<String, Value>,
    initial_by_id: &HashMap<String, Value>,
    outputs: &mut Vec<DocumentUpdate>,
) {
    for dd_output in dd_outputs {
        let cell_id = dd_output.cell_id.name().to_string();
        let update = dd_output.value;
        if let Some(update_cell_id) = update.cell_id() {
            if update_cell_id != cell_id {
                panic!(
                    "[DD Worker] Output cell '{}' does not match '{}': {:?}",
                    update_cell_id, cell_id, update
                );
            }
        }
        apply_output_to_state_maps(
            list_cells,
            list_states,
            new_states,
            &update,
            initial_by_id,
        );

        let mut hold_updates = HashMap::new();
        let mut hold_state_updates = HashMap::new();
        match &update {
            CellUpdate::Multi(updates) => {
                for update in updates {
                    let update_cell_id = update.cell_id().unwrap_or_else(|| {
                        panic!("[DD Worker] Missing cell id for update {:?}", update);
                    });
                    if should_persist_cell(config, update_cell_id) {
                        hold_updates.insert(update_cell_id.to_string(), update.clone());
                    } else {
                        hold_state_updates.insert(update_cell_id.to_string(), update.clone());
                    }
                }
            }
            CellUpdate::NoOp => {}
            _ => {
                if should_persist_cell(config, &cell_id) {
                    hold_updates.insert(cell_id.clone(), update.clone());
                } else {
                    hold_state_updates.insert(cell_id.clone(), update.clone());
                }
            }
        }

        outputs.push(DocumentUpdate {
            document: update,
            time: dd_output.time,
            hold_updates,
            hold_state_updates,
        });
    }
}

/// Reconstruct a persisted item using templates.
///
/// When list items are persisted to localStorage, they lose their CellRef/LinkRef structure.
/// This function reconstructs the full structure by:
/// 1. Cloning templates with fresh IDs
/// 2. Initializing HOLDs with persisted values (using generic field detection)
/// 3. Initializing hover HOLDs (if present)
/// 4. Returning the reconstructed item with initializations
///
/// This enables persisted items to render correctly after page reload.
pub fn reconstruct_persisted_item(
    persisted_item: &Value,
    data_template: &TemplateValue,
    element_template: Option<&TemplateValue>,
    identity_path: &[String],
    initial_collections: &mut HashMap<CollectionId, Vec<Value>>,
) -> Option<InstantiatedItem> {
    let data_template = data_template.as_value();
    // Extract persisted values as a generic map (no hardcoded field names)
    let persisted_fields = if let Value::Object(obj) = persisted_item {
        obj.clone()
    } else {
        return None;
    };

    dd_log!("[DD Reconstruct] Persisted item fields: {:?}", persisted_fields.keys().collect::<Vec<_>>());

    // Extract field-to-hold mapping from data template
    let field_to_hold = extract_field_to_hold_map(data_template);

    // Clone data template with fresh CellRef/LinkRef IDs
    let mut cell_id_map: HashMap<String, String> = HashMap::new();
    let mut link_id_map: HashMap<String, String> = HashMap::new();
    let mut collection_id_map: HashMap<CollectionId, CollectionId> = HashMap::new();
    let mut data_item = clone_template_with_fresh_ids(
        data_template,
        &mut cell_id_map,
        &mut link_id_map,
        &mut collection_id_map,
        initial_collections,
    );
    let mut initializations: Vec<(String, Value)> = Vec::new();

    // Create reverse mapping: old_cell_id -> field_name
    let hold_to_field: HashMap<String, String> = field_to_hold
        .iter()
        .map(|(field, hold)| (hold.clone(), field.clone()))
        .collect();

    // Register new HOLDs with PERSISTED values (generic - no field name assumptions)
    for (old_id, new_id) in &cell_id_map {
        // Find which field this HOLD corresponds to
        let field_name = hold_to_field.get(old_id);
        // Get the persisted value for this field (if any)
        let field_name = field_name.unwrap_or_else(|| {
            panic!("[DD Reconstruct] Bug: missing field mapping for hold {}", old_id);
        });
        let initial_value = persisted_fields
            .get(field_name.as_str())
            .cloned()
            .unwrap_or_else(|| {
                panic!("[DD Reconstruct] Missing persisted field '{}' for hold {}", field_name, old_id);
            });
        dd_log!("[DD Reconstruct] Registering HOLD: {} -> {} = {:?} (field: {:?})",
            old_id, new_id, initial_value, field_name);
        initializations.push((new_id.clone(), initial_value));
    }

    // Overlay persisted values for non-HOLD fields (e.g., nested collections).
    // Skip fields that are:
    // - HOLD fields (handled via initializations above)
    // - __key field (gets rewritten by attach_item_key)
    // - Stripped LinkRef structures (Unit or Object of Units from already-instantiated items)
    data_item = match data_item {
        Value::Object(fields) => {
            let mut new_fields = (*fields).clone();
            for (field_name, persisted_value) in persisted_fields.iter() {
                if field_to_hold.contains_key(field_name.as_ref()) {
                    continue;
                }
                if field_name.as_ref() == super::types::ITEM_KEY_FIELD {
                    continue;
                }
                if is_stripped_linkref_value(persisted_value) {
                    dd_log!("[DD Reconstruct] Skipping stripped field '{}' (keeping template LinkRefs)", field_name);
                    continue;
                }
                new_fields.insert(field_name.clone(), persisted_value.clone());
            }
            Value::Object(Arc::new(new_fields))
        }
        Value::Tagged { tag, fields } => {
            let mut new_fields = (*fields).clone();
            for (field_name, persisted_value) in persisted_fields.iter() {
                if field_to_hold.contains_key(field_name.as_ref()) {
                    continue;
                }
                if field_name.as_ref() == super::types::ITEM_KEY_FIELD {
                    continue;
                }
                if is_stripped_linkref_value(persisted_value) {
                    dd_log!("[DD Reconstruct] Skipping stripped field '{}' (keeping template LinkRefs)", field_name);
                    continue;
                }
                new_fields.insert(field_name.clone(), persisted_value.clone());
            }
            Value::Tagged { tag, fields: Arc::new(new_fields) }
        }
        other => {
            panic!(
                "[DD Reconstruct] Bug: expected Object/Tagged template, found {:?}",
                other
            );
        }
    };

    dd_log!("[DD Reconstruct] Created item with {} HOLDs, {} LINKs",
        cell_id_map.len(), link_id_map.len());

    // Clone element template if provided
    let new_element = element_template.map(|elem_tmpl| {
        let elem_tmpl = elem_tmpl.as_value();
        let (hover_link_old_id, hover_cell_old_id) = pre_populate_hover_mappings(
            elem_tmpl, &mut cell_id_map, &mut link_id_map, initial_collections,
        );

        // Clone element template - reuses the same ID mapping
        // Pass data_item as context to resolve PlaceholderFields (e.g., double_click LinkRefs)
        let cloned_element = clone_template_with_fresh_ids_with_context(
            elem_tmpl,
            &mut cell_id_map,
            &mut link_id_map,
            &mut collection_id_map,
            initial_collections,
            &data_item,
        );

        // Initialize hover hold to false if present.
        if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_cell_old_id) {
            if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), cell_id_map.get(&old_hold)) {
                dd_log!("[DD Reconstruct] Initializing HoverState: {} -> {}", new_link, new_hold);
                initializations.push((new_hold.clone(), Value::Bool(false)));
            }
        }

        cloned_element
    });

    // Resolve identity link and attach __key to both data and element items
    let identity_link_id = get_link_ref_at_path(data_template, identity_path)
        .or_else(|| element_template.and_then(|elem| get_link_ref_at_path(elem.as_value(), identity_path)))
        .and_then(|old_id| link_id_map.get(&old_id).cloned());
    let item_key = build_item_key(identity_link_id.as_deref(), identity_path, "reconstruct_persisted_item");

    let new_data_item = attach_item_key(data_item, &item_key);
    let new_element = new_element.map(|elem| attach_item_key(elem, &item_key));

    Some(InstantiatedItem {
        data: new_data_item,
        element: new_element,
        cell_id_map,
        link_id_map,
        identity_link_id,
        link_mappings: Vec::new(),
        initializations,
    })
}

/// Instantiate a fresh item from the Boon code with unique IDs.
///
/// Fresh items have the original template CellRef/LinkRef IDs (e.g., hold_12, link_22).
/// These IDs are shared across all items from the same template, which causes bugs
/// (e.g., hovering one item shows delete button on all items).
///
/// This function clones the item with fresh unique IDs so each item is independent.
/// Initial HOLD values are provided explicitly via `initial_cell_values` (no IO reads).
///
/// Returns the instantiated item with fresh IDs and initializations.
pub fn instantiate_fresh_item(
    fresh_item: &Value,
    element_template: Option<&TemplateValue>,
    identity_path: &[String],
    initial_cell_values: &HashMap<String, Value>,
    initial_collections: &mut HashMap<CollectionId, Vec<Value>>,
    per_item_cell_values: Option<&HashMap<String, Value>>,
) -> Option<InstantiatedItem> {
    dd_log!("[DD Worker] instantiate_fresh_item: identity_path={:?}, fresh_item={:?}", identity_path, fresh_item);
    dd_log!("[DD Worker] instantiate_fresh_item: element_template={:?}", element_template.map(|t| t.as_value()));
    let mut cell_id_map: HashMap<String, String> = HashMap::new();
    let mut link_id_map: HashMap<String, String> = HashMap::new();
    let mut collection_id_map: HashMap<CollectionId, CollectionId> = HashMap::new();

    // Clone the data item with fresh IDs
    let data_item = clone_template_with_fresh_ids(
        fresh_item,
        &mut cell_id_map,
        &mut link_id_map,
        &mut collection_id_map,
        initial_collections,
    );
    let mut initializations: Vec<(String, Value)> = Vec::new();

    // Initialize HOLDs from the fresh item's values.
    // Per-item cell values (if available) take priority over global initial_cell_values
    // because multiple items from the same template share cell IDs but may have
    // different initial values (e.g., different titles in a todo list).
    if let Value::Object(obj) = fresh_item {
        for (field_name, value) in obj.iter() {
            match value {
                Value::CellRef(old_cell_id) => {
                    let old_cell_name = old_cell_id.name();
                    let new_cell_id = cell_id_map.get(&old_cell_name).unwrap_or_else(|| {
                        panic!("[DD Worker] Bug: missing fresh cell id for {}", old_cell_name);
                    });
                    // Prefer per-item snapshot over global values
                    let current_value = per_item_cell_values
                        .and_then(|piv| piv.get(&old_cell_name))
                        .or_else(|| initial_cell_values.get(&old_cell_name))
                        .unwrap_or_else(|| {
                            panic!("[DD Worker] Bug: missing initial value for template hold {}", old_cell_name);
                        });
                    dd_log!("[DD Worker] instantiate_fresh_item: field {} hold {} -> {}, value={:?}", field_name, old_cell_name, new_cell_id, current_value);
                    initializations.push((new_cell_id.clone(), current_value.clone()));
                }
                Value::Object(_) => {}
                _ => {}
            }
        }
    }

    // Clone element template if provided
    let new_element = element_template.map(|elem_tmpl| {
        let elem_tmpl = elem_tmpl.as_value();
        let (hover_link_old_id, hover_cell_old_id) = pre_populate_hover_mappings(
            elem_tmpl, &mut cell_id_map, &mut link_id_map, initial_collections,
        );

        // Clone element template with same ID mappings
        let cloned_element = clone_template_with_fresh_ids_with_context(
            elem_tmpl,
            &mut cell_id_map,
            &mut link_id_map,
            &mut collection_id_map,
            initial_collections,
            &data_item,
        );

        // Initialize hover hold to false if present.
        if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_cell_old_id) {
            if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), cell_id_map.get(&old_hold)) {
                dd_log!("[DD Worker] instantiate_fresh_item: Initializing HoverState: {} -> {}", new_link, new_hold);
                initializations.push((new_hold.clone(), Value::Bool(false)));
            }
        }

        cloned_element
    });

    // Resolve identity link and attach __key to both data and element items
    let identity_link_id = get_link_ref_at_path(fresh_item, identity_path)
        .or_else(|| element_template.and_then(|elem| get_link_ref_at_path(elem.as_value(), identity_path)))
        .and_then(|old_id| link_id_map.get(&old_id).cloned());
    let item_key = build_item_key(identity_link_id.as_deref(), identity_path, "instantiate_fresh_item");

    let new_data_item = attach_item_key(data_item, &item_key);
    let new_element = new_element.map(|elem| attach_item_key(elem, &item_key));

    Some(InstantiatedItem {
        data: new_data_item,
        element: new_element,
        cell_id_map,
        link_id_map,
        identity_link_id,
        link_mappings: Vec::new(),
        initializations,
    })
}

/// Result of instantiating a template with fresh IDs and cell initializations.
#[derive(Clone, Debug)]
pub struct InstantiatedItem {
    /// The cloned data object with fresh CellRef/LinkRef IDs
    pub data: Value,
    /// The cloned element with fresh IDs (if element template was provided)
    pub element: Option<Value>,
    /// Mapping from old CellRef IDs to new IDs
    pub cell_id_map: HashMap<String, String>,
    /// Mapping from old LinkRef IDs to new IDs
    pub link_id_map: HashMap<String, String>,
    /// The identity LinkRef ID for this item (used for stable event routing)
    pub identity_link_id: Option<String>,
    /// Link mappings generated for this item (DD-native link handling)
    pub link_mappings: Vec<super::types::LinkCellMapping>,
    /// Initial values for any new cells created during instantiation.
    /// Each entry is (cell_id, initial_value).
    pub initializations: Vec<(String, Value)>,
}

/// Instantiate a list item from a template with fresh HOLD/LINK IDs.
///
/// This is the generic replacement for hardcoded item-specific template cloning.
/// It:
/// 1. Clones data and element templates with fresh IDs
/// 2. Registers link actions as specified in the template
/// 3. Initializes HOLD values from the provided values map
/// 4. Returns the instantiated item with ID mappings
///
/// # Arguments
/// * `template` - The template configuration
/// * `initial_values` - Map of field name to initial value (for title, completed, etc.)
/// * `event_text` - Optional event text (for FromEventText initializers)
pub fn instantiate_template(
    template: &ListItemTemplate,
    initial_values: &HashMap<String, Value>,
    event_text: Option<&str>,
    initial_collections: &mut HashMap<CollectionId, Vec<Value>>,
) -> InstantiatedItem {
    let mut cell_id_map: HashMap<String, String> = HashMap::new();
    let mut link_id_map: HashMap<String, String> = HashMap::new();
    let mut collection_id_map: HashMap<CollectionId, CollectionId> = HashMap::new();
    let mut initializations: Vec<(String, Value)> = Vec::new();
    let mut initialized_cells: HashSet<String> = HashSet::new();

    // Clone data template with fresh IDs
    let data = clone_template_with_fresh_ids(
        template.data_template.as_value(),
        &mut cell_id_map,
        &mut link_id_map,
        &mut collection_id_map,
        initial_collections,
    );

    // Clone element template if present
    let element = template.element_template.as_ref().map(|elem| {
        let elem = elem.as_value();
        let (hover_link_old_id, hover_cell_old_id) = pre_populate_hover_mappings(
            elem, &mut cell_id_map, &mut link_id_map, initial_collections,
        );

        // Pass cloned data as context to resolve PlaceholderFields (e.g., double_click LinkRefs)
        let cloned = clone_template_with_fresh_ids_with_context(
            elem,
            &mut cell_id_map,
            &mut link_id_map,
            &mut collection_id_map,
            initial_collections,
            &data,
        );

        // Initialize hover hold to false if present.
        if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_cell_old_id) {
            if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), cell_id_map.get(&old_hold)) {
                initializations.push((new_hold.clone(), Value::Bool(false)));
                initialized_cells.insert(new_hold.clone());
            }
        }

        cloned
    });

    // Extract identity link ID (data template first, then element template)
    let identity_link_id = get_link_ref_at_path(template.data_template.as_value(), &template.identity.link_ref_path)
        .or_else(|| {
            template.element_template
                .as_ref()
                .and_then(|elem| get_link_ref_at_path(elem.as_value(), &template.identity.link_ref_path))
        })
        .and_then(|old_id| link_id_map.get(&old_id).cloned());

    let item_key = build_item_key(
        identity_link_id.as_deref(),
        &template.identity.link_ref_path,
        "instantiate_template",
    );
    let data = attach_item_key(data, &item_key);
    let element = element.map(|elem| attach_item_key(elem, &item_key));

    // Initialize HOLDs with values from initial_values or field_initializers
    for (old_id, new_id) in &cell_id_map {
        if initialized_cells.contains(new_id) {
            continue;
        }
        // Check if this hold corresponds to a persisted field
        let field_value = template.persisted_fields.iter()
            .find(|(_, path)| {
                let hold_id = get_hold_ref_at_path(template.data_template.as_value(), path).unwrap_or_else(|| {
                    panic!("[DD Instantiate] Bug: persisted field path {:?} did not resolve in template", path);
                });
                hold_id == *old_id
            })
            .and_then(|(name, _)| initial_values.get(name).cloned());

        // Or check field_initializers
        let initializer_value = template.field_initializers.iter()
            .find(|(path, _)| {
                let hold_id = get_hold_ref_at_path(template.data_template.as_value(), path).unwrap_or_else(|| {
                    panic!("[DD Instantiate] Bug: initializer field path {:?} did not resolve in template", path);
                });
                hold_id == *old_id
            })
            .and_then(|(_, init)| match init {
                FieldInitializer::FromEventText => Some(
                    event_text
                        .map(|t| Value::text(t))
                        .unwrap_or_else(|| {
                            panic!("[DD] FieldInitializer::FromEventText missing event text for hold {}", old_id);
                        }),
                ),
                FieldInitializer::Constant(v) => Some(v.clone()),
            });

        let value = field_value.or(initializer_value).unwrap_or_else(|| {
            panic!("[DD] Missing initialization for hold {}", old_id);
        });
        initializations.push((new_id.clone(), value));
        initialized_cells.insert(new_id.clone());
    }

    // Register link actions (DD-native link mappings)
    let mut link_mappings: Vec<super::types::LinkCellMapping> = Vec::new();
    for action_config in &template.link_actions {
        let old_id = get_link_ref_at_path(template.data_template.as_value(), &action_config.link_path)
            .unwrap_or_else(|| {
                panic!(
                    "[DD Instantiate] Bug: link path {:?} did not resolve in template",
                    action_config.link_path
                );
            });
        let new_link_id = link_id_map.get(&old_id).unwrap_or_else(|| {
            panic!(
                "[DD Instantiate] Bug: missing new link id for {}",
                old_id
            );
        });
        link_mappings.extend(register_link_action(
            new_link_id,
            &action_config.action,
            template.data_template.as_value(),
            &cell_id_map,
        ));
    }

    // Seed list state for any new collections created during template cloning (runtime path).
    if !collection_id_map.is_empty() {
        let mut seeded: HashSet<CollectionId> = HashSet::new();
        for new_id in collection_id_map.values() {
            if !seeded.insert(*new_id) {
                continue;
            }
            let items = initial_collections.get(new_id).unwrap_or_else(|| {
                panic!(
                    "[DD Instantiate] Missing initial items for cloned collection {:?}",
                    new_id
                );
            }).clone();
            sync_list_state_from_dd(new_id.to_string(), items);
        }
    }

    InstantiatedItem {
        data,
        element,
        cell_id_map,
        link_id_map,
        identity_link_id,
        link_mappings,
        initializations,
    }
}

/// Register a link action based on the action spec.
fn register_link_action(
    new_link_id: &str,
    action: &LinkActionSpec,
    data_template: &Value,
    cell_id_map: &HashMap<String, String>,
) -> Vec<super::types::LinkCellMapping> {
    let mut mappings = Vec::new();
    match action {
        LinkActionSpec::SetHoldTrue { hold_path } => {
            let old_cell_id = get_hold_ref_at_path(data_template, hold_path).unwrap_or_else(|| {
                panic!("[DD Instantiate] Bug: hold path {:?} did not resolve in template", hold_path);
            });
            let new_cell_id = cell_id_map.get(&old_cell_id).unwrap_or_else(|| {
                panic!("[DD Instantiate] Bug: missing new cell id for {}", old_cell_id);
            });
            mappings.push(super::types::LinkCellMapping::set_true(
                new_link_id.to_string(),
                new_cell_id.clone(),
            ));
        }
        LinkActionSpec::SetHoldFalse { hold_path } => {
            let old_cell_id = get_hold_ref_at_path(data_template, hold_path).unwrap_or_else(|| {
                panic!("[DD Instantiate] Bug: hold path {:?} did not resolve in template", hold_path);
            });
            let new_cell_id = cell_id_map.get(&old_cell_id).unwrap_or_else(|| {
                panic!("[DD Instantiate] Bug: missing new cell id for {}", old_cell_id);
            });
            mappings.push(super::types::LinkCellMapping::set_false(
                new_link_id.to_string(),
                new_cell_id.clone(),
            ));
        }
        LinkActionSpec::HoverState { hold_path } => {
            let old_cell_id = get_hold_ref_at_path(data_template, hold_path).unwrap_or_else(|| {
                panic!("[DD Instantiate] Bug: hold path {:?} did not resolve in template", hold_path);
            });
            let new_cell_id = cell_id_map.get(&old_cell_id).unwrap_or_else(|| {
                panic!("[DD Instantiate] Bug: missing new cell id for {}", old_cell_id);
            });
            mappings.push(super::types::LinkCellMapping::hover_state(
                new_link_id.to_string(),
                new_cell_id.clone(),
            ));
        }
    }
    mappings
}

/// Remap template link mappings to new link/cell IDs for a cloned item.
pub fn remap_link_mappings_for_item(
    mappings: &[super::types::LinkCellMapping],
    link_id_map: &HashMap<String, String>,
    cell_id_map: &HashMap<String, String>,
) -> Vec<super::types::LinkCellMapping> {
    let mut remapped = Vec::new();
    for mapping in mappings {
        let old_link_id = mapping.link_id.name();
        let Some(new_link_id) = link_id_map.get(old_link_id) else {
            continue;
        };

        let mut new_mapping = mapping.clone();
        new_mapping.link_id = super::types::LinkId::new(new_link_id);

        match &mut new_mapping.action {
            super::types::LinkAction::RemoveListItem { list_cell_id, .. } => {
                // List cell ids are global; do not remap unless explicitly present.
                if let Some(new_id) = cell_id_map.get(&list_cell_id.name()) {
                    *list_cell_id = super::types::CellId::new(new_id);
                }
            }
            _ => {
                // Remap cell id if present in map; otherwise keep the original
                // (global cells like LATEST-derived IDs don't need remapping)
                if let Some(new_cell_id) = cell_id_map.get(&mapping.cell_id.name()) {
                    new_mapping.cell_id = super::types::CellId::new(new_cell_id);
                }
            }
        }

        remapped.push(new_mapping);
    }
    remapped
}

/// Handle to a running DD worker.
///
/// Use this to inject events. The worker writes directly to CELL_STATES (Phase 6).
pub struct WorkerHandle {
    /// Input channel for injecting events
    event_input: Input<Event>,
    /// Task handle to keep the async event loop alive
    task_handle: zoon::TaskHandle,
}

impl WorkerHandle {
    /// Get the event input for injecting events.
    pub fn event_input(&self) -> &Input<Event> {
        &self.event_input
    }

    /// Split the handle into (event_input, task_handle).
    ///
    pub fn split(self) -> (Input<Event>, zoon::TaskHandle) {
        (self.event_input, self.task_handle)
    }
}

fn should_persist_cell(config: &DataflowConfig, cell_id: &str) -> bool {
    if config.is_collection_output_cell_id(cell_id) {
        return false;
    }
    config
        .cells
        .iter()
        .find(|cell| cell.id.name() == cell_id)
        .map(|cell| cell.persist)
        .or_else(|| config.get_cell_initialization(cell_id).map(|init| init.persist))
        .unwrap_or_else(|| panic!("[DD Worker] Bug: missing cell '{}' for persistence lookup", cell_id))
}

/// A document update from the DD worker.
#[derive(Clone, Debug)]
pub struct DocumentUpdate {
    /// The new document update
    pub document: CellUpdate,
    /// Logical time of this update
    pub time: u64,
    /// HOLD state updates that SHOULD persist (cell_id -> new_value)
    pub hold_updates: HashMap<String, CellUpdate>,
    /// HOLD state updates for UI only, NOT persisted (cell_id -> new_value)
    /// Used for timer-driven HOLDs where persistence doesn't make sense.
    pub hold_state_updates: HashMap<String, CellUpdate>,
}

// Note: EventFilter is now defined in types.rs and re-exported via mod.rs

/// Type of state transformation to apply.
#[derive(Clone, Debug)]
pub enum StateTransform {
    /// Identity transform - pass through the event value as-is
    /// Used when the transform type cannot be determined during evaluation
    Identity,
    /// Increment numeric value by 1
    Increment,
    /// Toggle boolean value
    BoolToggle,
    /// Set boolean to true
    SetTrue,
    /// Set boolean to false
    SetFalse,
    /// Append item using a ListItemTemplate with full configuration.
    /// The template is pre-evaluated and contains field initializers and identity.
    ListAppendWithTemplate {
        template: ListItemTemplate,
    },
    /// Combined template + clear: ListAppendWithTemplate functionality with clear button support.
    /// Handles both Enter key events (append from template) and Unit from clear_link_id (clear).
    ListAppendWithTemplateAndClear {
        template: ListItemTemplate,
        clear_link_id: String,
    },
    // ListRemoveByFieldValue removed: bulk removal must be expressed via DD collection ops.

    // ========================================================================
    // GENERIC LIST OPERATIONS WITH STABLE IDENTITY
    // ========================================================================
    // These transforms use LinkRef IDs for stable identity instead of array indices.
    // When items are removed/reordered, LinkRef IDs remain constant.

    /// Generic: Append item using ListItemTemplate with full configuration.
    /// Replaces ListAppendWithTemplate with more flexibility.
    ListAppendFromTemplate {
        /// The template to instantiate
        template: ListItemTemplate,
    },

    /// Append raw event text value as a simple list item (no template).
    /// For lists with only append+clear and no per-item reactivity.
    ListAppendSimple,
    /// Append raw event text value or clear on unit event (no template).
    ListAppendSimpleWithClear {
        clear_link_id: String,
    },
}

/// Specifies how to update a field value.
#[derive(Clone, Debug)]
pub enum FieldUpdate {
    /// Set to a constant value
    Constant(Value),
    /// Toggle boolean value
    Toggle,
    /// Set to true
    SetTrue,
    /// Set to false
    SetFalse,
}

/// Configuration for a single HOLD operator in the dataflow.
#[derive(Clone, Debug)]
pub struct CellConfig {
    /// Unique identifier for this HOLD
    pub id: CellId,
    /// Initial value
    pub initial: Value,
    /// Which LINK events trigger this HOLD
    pub triggered_by: Vec<LinkId>,
    /// Timer interval in ms (0 = no timer trigger)
    pub timer_interval_ms: u64,
    /// Event filter for WHEN pattern matching
    pub filter: EventFilter,
    /// State transformation to apply
    pub transform: StateTransform,
    /// Whether to persist values to localStorage (false for timer-driven HOLDs)
    pub persist: bool,
}

/// Initialization for non-reactive cells (e.g., current_route).
#[derive(Clone, Debug)]
pub struct CellInitialization {
    pub value: Value,
    pub persist: bool,
}

/// Binding information for List/append and List/clear operations.
#[derive(Clone, Debug)]
pub struct ListAppendBinding {
    pub list_cell_id: String,
    pub append_link_ids: Vec<String>,
    pub clear_link_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct BulkRemoveBinding {
    pub link_id: super::types::LinkId,
    pub list_cell_id: String,
    pub field_filter: Option<(Arc<str>, Value)>,
    pub predicate_template: Option<TemplateValue>,
}

impl BulkRemoveBinding {
    pub fn new(
        link_id: super::types::LinkId,
        list_cell_id: impl Into<String>,
        field_filter: Option<(Arc<str>, Value)>,
        predicate_template: Option<TemplateValue>,
    ) -> Self {
        if field_filter.is_some() && predicate_template.is_some() {
            panic!("[DD Config] BulkRemoveBinding cannot use both field_filter and predicate_template");
        }
        if field_filter.is_none() && predicate_template.is_none() {
            panic!("[DD Config] BulkRemoveBinding requires field_filter or predicate_template");
        }
        Self {
            link_id,
            list_cell_id: list_cell_id.into(),
            field_filter,
            predicate_template,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DataflowConfig {
    /// HOLD operators to create
    pub cells: Vec<CellConfig>,
    /// Initial values for non-HOLD cells (populated before DD starts).
    pub cell_initializations: HashMap<String, CellInitialization>,
    /// Link-to-cell mappings for DD-native LINK handling (Phase 8).
    /// These replace DYNAMIC_LINK_ACTIONS HashMap with DD joins.
    pub link_mappings: Vec<super::types::LinkCellMapping>,
    /// Collection operations (Phase 4).
    /// Replaces symbolic reference variants with DD dataflow operators.
    pub collection_ops: Vec<CollectionOpConfig>,
    /// Initial collections (list literals evaluated at startup)
    pub initial_collections: HashMap<CollectionId, Vec<Value>>,
    /// Mapping from collection id to source list cell id (for reactive list sources).
    pub collection_sources: HashMap<CollectionId, String>,

    // ========================================================================
    // Registry fields (replace the thread_local registries in outputs.rs)
    // ========================================================================

    /// Route cells that should be initialized from the browser path.
    pub route_cells: HashSet<String>,

    /// Cells whose updates trigger browser navigation (Router/go_to).
    /// When these cells change, the worker pushes the new URL and fires ROUTE_CHANGE_LINK_ID.
    pub go_to_cells: HashSet<String>,

    /// Remove event paths per list cell: path from item to LinkRef that triggers removal.
    /// Parsed from List/remove(item, on: item.X.Y.event.press) → ["X", "Y"]
    /// Replaces REMOVE_EVENT_PATH registry.
    pub remove_event_paths: HashMap<String, Vec<String>>,

    /// Bulk remove bindings (global remove buttons with explicit predicate).
    /// Parsed from List/remove(item, on: elements.X.event.press |> THEN {...})
    pub bulk_remove_bindings: Vec<BulkRemoveBinding>,

    /// Explicit bindings for List/append/List/clear (parsed from Boon code).
    pub list_append_bindings: Vec<ListAppendBinding>,

    /// Explicit list item templates keyed by list cell id.
    /// Built by evaluator from List/append item expressions (no inference in interpreter).
    pub list_item_templates: HashMap<String, ListItemTemplate>,

    /// Element templates for list rendering keyed by list cell id.
    /// Populated when List/map registers element templates.
    pub list_element_templates: HashMap<String, TemplateValue>,

    /// Per-item cell initial values for list collections.
    /// When a list literal evaluates function calls that create HOLDs with shared cell IDs,
    /// each item may have different initial values for those cells. This stores per-item
    /// snapshots so `instantiate_fresh_item` can use the correct value for each item.
    pub per_item_cell_values: HashMap<CollectionId, Vec<HashMap<String, Value>>>,
}

/// Template info for pre-instantiation (O(delta) optimization).
/// Moved outside impl block to satisfy Rust's grammar rules.
#[derive(Clone)]
struct TemplateInfo {
    template: ListItemTemplate,
    cell_id: String,
    filter: EventFilter,
}

impl DataflowConfig {
    /// Create a new empty dataflow config.
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            cell_initializations: HashMap::new(),
            link_mappings: Vec::new(),
            collection_ops: Vec::new(),
            initial_collections: HashMap::new(),
            collection_sources: HashMap::new(),
            // Registry fields
            route_cells: HashSet::new(),
            go_to_cells: HashSet::new(),
            remove_event_paths: HashMap::new(),
            bulk_remove_bindings: Vec::new(),
            list_append_bindings: Vec::new(),
            list_item_templates: HashMap::new(),
            list_element_templates: HashMap::new(),
            per_item_cell_values: HashMap::new(),
        }
    }

    /// Merge another DataflowConfig into this one.
    /// Used when a forked runtime (e.g., WHILE arm pre-evaluation) creates
    /// cells, collections, or link mappings that must be registered in the
    /// parent's config to be processed by the worker.
    pub fn merge_from(&mut self, other: DataflowConfig) {
        self.cells.extend(other.cells);
        self.cell_initializations.extend(other.cell_initializations);
        self.link_mappings.extend(other.link_mappings);
        self.collection_ops.extend(other.collection_ops);
        for (coll_id, items) in other.initial_collections {
            self.initial_collections.entry(coll_id).or_insert(items);
        }
        self.collection_sources.extend(other.collection_sources);
        self.route_cells.extend(other.route_cells);
        self.go_to_cells.extend(other.go_to_cells);
        self.remove_event_paths.extend(other.remove_event_paths);
        self.bulk_remove_bindings.extend(other.bulk_remove_bindings);
        self.list_append_bindings.extend(other.list_append_bindings);
        self.list_item_templates.extend(other.list_item_templates);
        self.list_element_templates.extend(other.list_element_templates);
        self.per_item_cell_values.extend(other.per_item_cell_values);
    }

    // ========================================================================
    // Registry field setters (replace the set_* functions in outputs.rs)
    // ========================================================================

    /// Register a route cell to initialize from current browser path.
    pub fn add_route_cell(&mut self, cell_id: impl Into<String>) {
        self.route_cells.insert(cell_id.into());
    }

    /// Register a go_to cell (Router/go_to input).
    pub fn add_go_to_cell(&mut self, cell_id: impl Into<String>) {
        self.go_to_cells.insert(cell_id.into());
    }

    /// Register a non-HOLD cell initialization.
    /// Allows overwrite for two-pass evaluation (pass 2 overwrites pass 1).
    pub fn add_cell_initialization(
        &mut self,
        cell_id: impl Into<String>,
        value: Value,
        persist: bool,
    ) {
        let cell_id = cell_id.into();
        self.cell_initializations.insert(
            cell_id,
            CellInitialization { value, persist },
        );
    }

    /// Get an initial value for a cell if registered.
    pub fn get_cell_initialization(&self, cell_id: &str) -> Option<&CellInitialization> {
        self.cell_initializations.get(cell_id)
    }

    /// Register a list item template for a list cell.
    /// Allows overwrite for two-pass evaluation (pass 2 overwrites pass 1).
    pub fn set_list_item_template(&mut self, list_cell_id: impl Into<String>, template: ListItemTemplate) {
        let list_cell_id = list_cell_id.into();
        self.list_item_templates.insert(list_cell_id, template);
    }

    /// Get a list item template for a list cell.
    pub fn get_list_item_template(&self, list_cell_id: &str) -> Option<&ListItemTemplate> {
        self.list_item_templates.get(list_cell_id)
    }

    /// Register an element template for a list cell.
    /// Allows overwrite for two-pass evaluation (pass 2 overwrites pass 1).
    pub fn set_list_element_template(&mut self, list_cell_id: impl Into<String>, template: TemplateValue) {
        let list_cell_id = list_cell_id.into();
        self.list_element_templates.insert(list_cell_id, template);
    }

    /// Attach any known element templates to list item templates.
    /// Skips entries where the element template is provided through collection_ops
    /// (e.g., chained retain → map operations).
    pub fn attach_list_element_templates(&mut self) {
        for (list_cell_id, template) in self.list_item_templates.iter_mut() {
            if template.element_template.is_some() {
                continue;
            }
            if let Some(element_template) = self.list_element_templates.get(list_cell_id) {
                template.element_template = Some(element_template.clone());
            }
        }
    }

    /// Set the remove event path for a list cell.
    pub fn set_remove_event_path(&mut self, list_cell_id: impl Into<String>, path: Vec<String>) {
        if path.is_empty() {
            panic!("[DD Config] remove_event_path must not be empty");
        }
        let list_cell_id = list_cell_id.into();
        if let Some(existing) = self.remove_event_paths.get(&list_cell_id) {
            if existing != &path {
                panic!(
                    "[DD Config] Conflicting remove_event_path for '{}': {:?} vs {:?}",
                    list_cell_id, existing, path
                );
            }
            return;
        }
        self.remove_event_paths.insert(list_cell_id, path);
    }

    /// Register a bulk remove binding (global remove link + predicate).
    pub fn add_bulk_remove_binding(
        &mut self,
        link_id: super::types::LinkId,
        list_cell_id: impl Into<String>,
        field_filter: Option<(Arc<str>, Value)>,
        predicate_template: Option<TemplateValue>,
    ) {
        let list_cell_id = list_cell_id.into();
        if let Some(existing) = self.bulk_remove_bindings.iter().find(|binding| {
            binding.link_id == link_id && binding.list_cell_id == list_cell_id
        }) {
            if existing.field_filter != field_filter || existing.predicate_template != predicate_template {
                panic!(
                    "[DD Config] Conflicting bulk remove binding for link '{}' list '{}'",
                    link_id.name(),
                    list_cell_id
                );
            }
            return;
        }
        self.bulk_remove_bindings.push(BulkRemoveBinding::new(
            link_id,
            list_cell_id,
            field_filter,
            predicate_template,
        ));
    }

    /// Add or merge a List/append/List/clear binding.
    pub fn add_list_append_binding(&mut self, binding: ListAppendBinding) {
        if let Some(existing) = self.list_append_bindings.iter_mut()
            .find(|b| b.list_cell_id == binding.list_cell_id)
        {
            for link in binding.append_link_ids {
                if !existing.append_link_ids.contains(&link) {
                    existing.append_link_ids.push(link);
                }
            }
            for link in binding.clear_link_ids {
                if !existing.clear_link_ids.contains(&link) {
                    existing.clear_link_ids.push(link);
                }
            }
            return;
        }
        self.list_append_bindings.push(binding);
    }

    /// Add a link-to-cell mapping (Phase 8).
    /// This replaces add_dynamic_link_action for DD-native handling.
    pub fn add_link_mapping(&mut self, mapping: super::types::LinkCellMapping) {
        for existing in &self.link_mappings {
            if existing.link_id == mapping.link_id && existing.cell_id == mapping.cell_id {
                if existing.action == mapping.action && existing.key_filter == mapping.key_filter {
                    // Idempotent add - mapping already present.
                    return;
                }
                if existing.key_filter.is_none() != mapping.key_filter.is_none() {
                    // One mapping has a key_filter and the other doesn't — they handle
                    // different event types from the same element LINK:
                    // - key_down with Enter/Escape filter + blur/press without filter → both valid
                    // - SetText (text events) + key-filtered mapping → both valid
                    continue;
                }
                if existing.key_filter.is_none() && mapping.key_filter.is_none()
                    && existing.action != mapping.action
                {
                    panic!(
                        "[DD Config] Conflicting link mappings for link '{}', cell '{}': {:?} vs {:?}",
                        mapping.link_id.name(),
                        mapping.cell_id.name(),
                        existing.action,
                        mapping.action
                    );
                }
                if let (Some(existing_keys), Some(new_keys)) = (&existing.key_filter, &mapping.key_filter) {
                    if existing_keys.iter().any(|k| new_keys.contains(k)) {
                        panic!(
                            "[DD Config] Overlapping key filters for link '{}', cell '{}': {:?} vs {:?}",
                            mapping.link_id.name(),
                            mapping.cell_id.name(),
                            existing_keys,
                            new_keys
                        );
                    }
                }
            }
        }
        self.link_mappings.push(mapping);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Collection operation builders
    // ══════════════════════════════════════════════════════════════════════════

    /// Register an initial collection (list literal).
    /// Returns the CollectionId for referencing in operations.
    pub fn add_initial_collection(&mut self, items: Vec<Value>) -> CollectionId {
        let id = CollectionId::new();
        self.add_initial_collection_with_id(id, items);
        id
    }

    /// Register an initial collection with a specific ID.
    pub fn add_initial_collection_with_id(
        &mut self,
        id: CollectionId,
        items: Vec<Value>,
    ) -> CollectionId {
        ensure_unique_item_keys(&items, "initial collection");
        if let Some(existing) = self.initial_collections.get(&id) {
            if existing != &items {
                panic!(
                    "[DD Config] Conflicting initial collection items for {:?}: {:?} vs {:?}",
                    id, existing, items
                );
            }
            return id;
        }
        self.initial_collections.insert(id, items);
        id
    }

    /// Register a source list cell for a collection id.
    pub fn add_collection_source(&mut self, collection_id: CollectionId, cell_id: impl Into<String>) {
        let cell_id = cell_id.into();
        if !self.initial_collections.contains_key(&collection_id) {
            panic!(
                "[DD Config] Collection source {:?} missing initial collection entry",
                collection_id
            );
        }
        if let Some(existing) = self.collection_sources.get(&collection_id) {
            if existing != &cell_id {
                panic!(
                    "[DD Config] Conflicting collection source for {:?}: '{}' vs '{}'",
                    collection_id, existing, cell_id
                );
            }
            return;
        }
        self.collection_sources.insert(collection_id, cell_id);
    }

    /// Check if a cell id corresponds to a collection op output.
    pub fn is_collection_output_cell_id(&self, cell_id: &str) -> bool {
        self.collection_ops.iter().any(|op| op.output_id.to_string() == cell_id)
    }

    /// Register non-persistent cells for collection op outputs.
    pub fn register_collection_cells(&mut self) {
        let output_cell_ids: Vec<String> = self
            .collection_ops
            .iter()
            .map(|op| op.output_id.to_string())
            .collect();

        for cell_id in output_cell_ids {
            if self.cells.iter().any(|cell| cell.id.name() == cell_id) {
                panic!(
                    "[DD Config] Collection output '{}' conflicts with existing cell",
                    cell_id
                );
            }
            if self.cell_initializations.contains_key(&cell_id) {
                continue;
            }
            self.add_cell_initialization(cell_id, Value::Unit, false);
        }
    }

    /// Add a filter operation.
    /// Replaces: FilteredListRef, FilteredListRefWithPredicate
    pub fn add_filter(
        &mut self,
        source_id: CollectionId,
        field_filter: Option<(Arc<str>, Value)>,
        predicate_template: Option<TemplateValue>,
    ) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id,
            op: CollectionOp::Filter {
                field_filter,
                predicate_template,
            },
        });
        output_id
    }

    /// Add a map operation.
    /// Replaces: MappedListRef
    pub fn add_map(&mut self, source_id: CollectionId, element_template: TemplateValue) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id,
            op: CollectionOp::Map { element_template },
        });
        output_id
    }

    /// Add a count operation.
    /// Replaces: ComputedRef::ListCount (DD collection op only)
    pub fn add_count(&mut self, source_id: CollectionId) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id,
            op: CollectionOp::Count,
        });
        output_id
    }

    /// Add a count-where operation.
    /// Replaces: ComputedRef::ListCountWhere (DD collection op only)
    pub fn add_count_where(
        &mut self,
        source_id: CollectionId,
        filter_field: Arc<str>,
        filter_value: Value,
    ) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id,
            op: CollectionOp::CountWhere {
                filter_field,
                filter_value,
            },
        });
        output_id
    }

    /// Add an is-empty operation.
    pub fn add_is_empty(&mut self, source_id: CollectionId) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id,
            op: CollectionOp::IsEmpty,
        });
        output_id
    }

    /// Add a concat operation (left + right).
    /// Replaces: Collection concat semantics for LATEST/list streams.
    pub fn add_concat(&mut self, left_source: CollectionId, right_source: CollectionId) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id: left_source,
            op: CollectionOp::Concat { other_source: right_source },
        });
        output_id
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Arithmetic/Comparison Operation Helpers
    // ══════════════════════════════════════════════════════════════════════════

    /// Add a subtract operation (left - right).
    /// Replaces: ComputedRef::Subtract
    /// Used for: active_list_count = list_count - completed_list_count
    pub fn add_subtract(&mut self, left_source: CollectionId, right_source: CollectionId) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id: left_source,
            op: CollectionOp::Subtract { right_source },
        });
        output_id
    }

    /// Add a greater-than-zero check.
    /// Replaces: ComputedRef::GreaterThanZero
    /// Used for: show_clear_completed = completed_list_count > 0
    pub fn add_greater_than_zero(&mut self, source_id: CollectionId) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id,
            op: CollectionOp::GreaterThanZero,
        });
        output_id
    }

    /// Add an equality comparison.
    /// Replaces: ComputedRef::Equal
    /// Used for: all_completed = completed_list_count == list_count
    pub fn add_equal(&mut self, left_source: CollectionId, right_source: CollectionId) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id: left_source,
            op: CollectionOp::Equal { right_source },
        });
        output_id
    }

    /// Add a scalar pattern-match operation.
    /// Used for reactive WHEN on scalar cells (e.g., count |> WHEN { 1 => "", __ => "s" }).
    pub fn add_scalar_when(
        &mut self,
        source_id: CollectionId,
        arms: Vec<(Value, Value)>,
        default: Value,
    ) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id,
            op: CollectionOp::ScalarWhen { arms, default },
        });
        output_id
    }

    /// Add a computed text operation (reactive TEXT interpolation).
    /// Sources are ordered: index 0 = source_id, 1+ = extra_sources.
    pub fn add_computed_text(
        &mut self,
        sources: Vec<CollectionId>,
        parts: Vec<ComputedTextPart>,
    ) -> CollectionId {
        assert!(!sources.is_empty(), "[DD DataflowConfig] ComputedText requires at least one source");
        let output_id = CollectionId::new();
        let source_id = sources[0].clone();
        let extra_sources = sources[1..].to_vec();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id,
            op: CollectionOp::ComputedText { parts, extra_sources },
        });
        output_id
    }

    /// Add a HOLD operator configuration.
    pub fn add_hold(mut self, id: impl Into<String>, initial: Value, triggers: Vec<&str>) -> Self {
        self.cells.push(CellConfig {
            id: CellId::new(id),
            initial,
            triggered_by: triggers.into_iter().map(LinkId::new).collect(),
            timer_interval_ms: 0,
            filter: EventFilter::Any,
            transform: StateTransform::Increment,
            persist: true,
        });
        self
    }

    /// Add a timer-triggered HOLD operator.
    /// Timer values are NOT persisted - they're time-based, not user data.
    pub fn add_timer_hold(mut self, id: impl Into<String>, initial: Value, interval_ms: u64) -> Self {
        self.cells.push(CellConfig {
            id: CellId::new(id),
            initial,
            triggered_by: Vec::new(),
            timer_interval_ms: interval_ms,
            filter: EventFilter::Any,
            transform: StateTransform::Increment,
            persist: false,
        });
        self
    }

    /// Add a HOLD with WHEN-style text filter.
    ///
    /// Only triggers when event value matches the specified text.
    pub fn add_filtered_hold(
        mut self,
        id: impl Into<String>,
        initial: Value,
        triggers: Vec<&str>,
        filter_text: &str,
        transform: StateTransform,
    ) -> Self {
        self.cells.push(CellConfig {
            id: CellId::new(id),
            initial,
            triggered_by: triggers.into_iter().map(LinkId::new).collect(),
            timer_interval_ms: 0,
            filter: EventFilter::TextEquals(filter_text.to_string()),
            transform,
            persist: true,
        });
        self
    }

    /// Add a boolean toggle HOLD.
    ///
    /// Toggles boolean value on each trigger event.
    pub fn add_bool_toggle(mut self, id: impl Into<String>, initial: bool, triggers: Vec<&str>) -> Self {
        self.cells.push(CellConfig {
            id: CellId::new(id),
            initial: Value::Bool(initial),
            triggered_by: triggers.into_iter().map(LinkId::new).collect(),
            timer_interval_ms: 0,
            filter: EventFilter::Any,
            transform: StateTransform::BoolToggle,
            persist: true,
        });
        self
    }

}

/// DD Worker that processes events through Differential Dataflow.
///
/// This worker runs in an async context via `spawn_local` and processes
/// events synchronously in batches using `timely::execute_directly`.
pub struct Worker {
    /// Configuration for the dataflow
    config: DataflowConfig,
    // ID counters moved to thread-locals (allocate_dynamic_cell_id/link_id).
    // Worker calls reset_id_counters() on creation for deterministic sessions.
}

impl Worker {
    /// Create a new DD worker with default configuration.
    ///
    /// Resets thread-local ID counters for deterministic ID generation.
    /// Each Worker session starts fresh at ID 1000.
    pub fn new() -> Self {
        // Reset thread-local counters for deterministic replays
        reset_id_counters();
        Self {
            config: DataflowConfig::default(),
        }
    }

    /// Create a DD worker with specific configuration.
    ///
    /// Resets thread-local ID counters for deterministic ID generation.
    pub fn with_config(config: DataflowConfig) -> Self {
        // Reset thread-local counters for deterministic replays
        reset_id_counters();
        Self {
            config,
        }
    }

    /// Spawn the DD worker and return a handle for interaction.
    ///
    /// The worker runs in an async context via `wasm_bindgen_futures::spawn_local`.
    /// Events are processed synchronously in batches using Timely/DD.
    ///
    /// # Phase 6: Single State Authority
    /// - Initialization happens SYNCHRONOUSLY in spawn() before returning
    /// - This ensures CELL_STATES is populated before bridge tries to render
    /// - Event loop handles runtime updates only
    pub fn spawn(mut self) -> WorkerHandle {
        // ═══════════════════════════════════════════════════════════════════════
        // PHASE 6: SYNCHRONOUS INITIALIZATION
        // Initialize CELL_STATES before returning, so bridge can render immediately
        // ═══════════════════════════════════════════════════════════════════════
        let mut initial_cell_states: HashMap<String, Value> = HashMap::new();
        let list_cells = list_cell_ids(&self.config);
        let derived_list_outputs = derived_list_outputs(&self.config);
        let mut list_items_by_cell: HashMap<String, Vec<Value>> = HashMap::new();

        for cell_config in &self.config.cells {
            let cell_id = cell_config.id.name();
            let initial_value = if list_cells.contains(&cell_id) {
                cell_config.initial.clone()
            } else if cell_config.persist {
                // Try to load persisted value, fall back to config default
                if let Some(persisted) = load_persisted_cell_value_with_collections(&cell_id) {
                    for (collection_id, items) in persisted.collections {
                        if self.config.initial_collections.insert(collection_id, items).is_some() {
                            panic!(
                                "[DD Worker] Duplicate nested collection id {:?} during persisted load",
                                collection_id
                            );
                        }
                    }
                    persisted.value
                } else {
                    cell_config.initial.clone()
                }
            } else {
                cell_config.initial.clone()
            };

            if list_cells.contains(&cell_id) {
                // Unit means forward reference not yet resolved — skip list processing
                if matches!(initial_value, Value::Unit) {
                    dd_log!("[DD Worker] List cell '{}' has Unit initial (forward ref?), skipping", cell_id);
                    initial_cell_states.insert(cell_id.to_string(), Value::Unit);
                    continue;
                }
                let collection_id = collection_id_for_cell(&self.config, &cell_id);
                let (items, handle) = match initial_value {
                    Value::List(handle) => {
                        if handle.id != collection_id {
                            panic!(
                                "[DD Worker] Collection id mismatch for '{}': expected {:?}, found {:?}",
                                cell_id, collection_id, handle.id
                            );
                        }
                        let existing = handle.cell_id.as_deref().unwrap_or_else(|| {
                            panic!("[DD Worker] Missing collection cell_id for '{}'", cell_id);
                        });
                        if existing != cell_id {
                            panic!(
                                "[DD Worker] Collection cell_id mismatch: expected '{}', found '{}'",
                                cell_id, existing
                            );
                        }
                        let items = self.config.initial_collections.get(&collection_id).cloned().unwrap_or_else(|| {
                            panic!("[DD Worker] Missing initial items for list cell '{}'", cell_id);
                        });
                        (items, handle)
                    }
                    other => {
                        panic!(
                            "[DD Worker] List cell '{}' must be Collection, found {:?}",
                            cell_id, other
                        );
                    }
                };

                if cell_config.persist {
                    sync_list_state_from_dd_with_persist(&cell_id, items.clone());
                } else {
                    sync_list_state_from_dd(&cell_id, items.clone());
                }
                list_items_by_cell.insert(cell_id.to_string(), items);
                initial_cell_states.insert(cell_id.to_string(), Value::List(handle));
                continue;
            }

            // Skip template cells — their initial values contain Placeholder markers
            // from List/map template evaluation. These cells will be cloned with fresh IDs
            // and real values when list items are instantiated.
            if contains_placeholder(&initial_value) {
                dd_log!("[DD Worker] Skipping template cell '{}' (contains placeholder)", cell_id);
                continue;
            }

            let initial_value = validate_cell_state_value(&cell_id, initial_value, &list_cells);
            // Write to global CELL_STATES synchronously
            if cell_config.persist {
                sync_cell_from_dd_with_persist(CellUpdate::set_value(cell_id.as_str(), initial_value.clone()));
            } else {
                sync_cell_from_dd(CellUpdate::set_value(cell_id.as_str(), initial_value.clone()));
            }
            // Also build local copy for DD processing
            initial_cell_states.insert(cell_id.to_string(), initial_value);
        }

        // Initialize non-HOLD cells registered in config (e.g., current_route).
        for (cell_id, init) in &self.config.cell_initializations {
            if initial_cell_states.contains_key(cell_id) {
                // Avoid overriding cells already initialized from CellConfig.
                continue;
            }
            if derived_list_outputs.contains(cell_id) {
                // Derived collection outputs (Filter/Map/Concat) are computed
                // by process_with_persistent_worker below, not initialized here.
                continue;
            }
            let initial_value = if list_cells.contains(cell_id) {
                init.value.clone()
            } else if init.persist {
                if let Some(persisted) = load_persisted_cell_value_with_collections(cell_id) {
                    for (collection_id, items) in persisted.collections {
                        if self.config.initial_collections.insert(collection_id, items).is_some() {
                            panic!(
                                "[DD Worker] Duplicate nested collection id {:?} during persisted load",
                                collection_id
                            );
                        }
                    }
                    persisted.value
                } else {
                    init.value.clone()
                }
            } else {
                init.value.clone()
            };

            if list_cells.contains(cell_id) {
                // Unit means forward reference not yet resolved — skip list processing
                if matches!(initial_value, Value::Unit) {
                    dd_log!("[DD Worker] List cell '{}' has Unit initial (forward ref?), skipping", cell_id);
                    initial_cell_states.insert(cell_id.clone(), Value::Unit);
                    continue;
                }
                let collection_id = collection_id_for_cell(&self.config, cell_id);
                let (items, handle) = match initial_value {
                    Value::List(handle) => {
                        if handle.id != collection_id {
                            panic!(
                                "[DD Worker] Collection id mismatch for '{}': expected {:?}, found {:?}",
                                cell_id, collection_id, handle.id
                            );
                        }
                        let existing = handle.cell_id.as_deref().unwrap_or_else(|| {
                            panic!("[DD Worker] Missing collection cell_id for '{}'", cell_id);
                        });
                        if existing != cell_id {
                            panic!(
                                "[DD Worker] Collection cell_id mismatch: expected '{}', found '{}'",
                                cell_id, existing
                            );
                        }
                        let items = self.config.initial_collections.get(&collection_id).cloned().unwrap_or_else(|| {
                            panic!("[DD Worker] Missing initial items for list cell '{}'", cell_id);
                        });
                        (items, handle)
                    }
                    other => {
                        panic!(
                            "[DD Worker] List cell '{}' must be Collection, found {:?}",
                            cell_id, other
                        );
                    }
                };

                if init.persist {
                    sync_list_state_from_dd_with_persist(cell_id, items.clone());
                } else {
                    sync_list_state_from_dd(cell_id, items.clone());
                }
                list_items_by_cell.insert(cell_id.clone(), items);
                initial_cell_states.insert(cell_id.clone(), Value::List(handle));
                continue;
            }

            // Skip template cells (same as above for CellConfig loop).
            if contains_placeholder(&initial_value) {
                dd_log!("[DD Worker] Skipping template cell init '{}' (contains placeholder)", cell_id);
                continue;
            }

            let initial_value = validate_cell_state_value(cell_id, initial_value, &list_cells);
            if init.persist {
                sync_cell_from_dd_with_persist(CellUpdate::set_value(cell_id.as_str(), initial_value.clone()));
            } else {
                sync_cell_from_dd(CellUpdate::set_value(cell_id.as_str(), initial_value.clone()));
            }
            initial_cell_states.insert(cell_id.clone(), initial_value);
        }

        // Initialize static collections (list literals not tied to list cells) for rendering.
        for (collection_id, items) in &self.config.initial_collections {
            if self.config.collection_sources.contains_key(collection_id) {
                continue;
            }
            let cell_id = collection_id.to_string();
            sync_list_state_from_dd(cell_id, items.clone());
        }

        let mut initial_list_states_override: Option<HashMap<String, super::dataflow::ListState>> = None;
        let mut initial_scalar_states_override: Option<HashMap<String, Value>> = None;

        if !self.config.collection_ops.is_empty() {
            let list_states = build_list_states(&list_cells, &derived_list_outputs, &list_items_by_cell);
            let mut scalar_states = initial_cell_states.clone();
            strip_list_cells(&mut scalar_states, &list_cells);
            let (outputs, _time, new_states, new_list_states) = Self::process_with_persistent_worker(
                &mut self.config,
                Vec::new(),
                0,
                &scalar_states,
                &list_states,
            );

            for output in outputs {
                for (cell_id, value) in output.hold_updates {
                    let update_cell_id = value.cell_id().unwrap_or_else(|| {
                        panic!("[DD Worker] Missing cell id for update {:?}", value);
                    });
                    if update_cell_id != cell_id.as_str() {
                        panic!(
                            "[DD Worker] hold_updates key '{}' does not match update cell '{}'",
                            cell_id, update_cell_id
                        );
                    }
                    sync_cell_from_dd_with_persist(value);
                }
                for (cell_id, value) in output.hold_state_updates {
                    let update_cell_id = value.cell_id().unwrap_or_else(|| {
                        panic!("[DD Worker] Missing cell id for update {:?}", value);
                    });
                    if update_cell_id != cell_id.as_str() {
                        panic!(
                            "[DD Worker] hold_state_updates key '{}' does not match update cell '{}'",
                            cell_id, update_cell_id
                        );
                    }
                    sync_cell_from_dd(value);
                }
            }

            initial_scalar_states_override = Some(new_states);
            initial_list_states_override = Some(new_list_states);
        }

        dd_log!("[Worker Phase 6] Initialized {} cells synchronously", self.config.cells.len());

        let list_states = initial_list_states_override
            .unwrap_or_else(|| build_list_states(&list_cells, &derived_list_outputs, &list_items_by_cell));
        let mut scalar_states = initial_scalar_states_override
            .unwrap_or(initial_cell_states);
        strip_list_cells(&mut scalar_states, &list_cells);

        // Create event channel for injecting events
        let (event_tx, event_rx) = mpsc::unbounded();

        // Spawn the async event loop (initialization already done)
        let task_handle = Task::start_droppable(Self::event_loop(
            self.config,
            event_rx,
            scalar_states,
            list_states,
        ));

        WorkerHandle {
            event_input: Input::new(event_tx),
            task_handle,
        }
    }

    /// The main event loop that processes DD events.
    ///
    /// # Phase 6: Single State Authority
    /// - Initialization is done in spawn() before this runs
    /// - This loop handles runtime event processing only
    async fn event_loop(
        mut config: DataflowConfig,
        mut event_rx: mpsc::UnboundedReceiver<Event>,
        initial_cell_states: HashMap<String, Value>,
        initial_list_states: HashMap<String, super::dataflow::ListState>,
    ) {
        let mut current_time: u64 = 0;
        let mut cell_states = initial_cell_states;
        let list_cells = list_cell_ids(&config);
        let mut list_states = initial_list_states;

        // ═══════════════════════════════════════════════════════════════════════
        // EVENT LOOP
        // ═══════════════════════════════════════════════════════════════════════
        loop {
            // Yield to browser to prevent blocking
            Timer::sleep(0).await;

            // Collect all pending events (non-blocking)
            let mut events = Vec::new();
            loop {
                match event_rx.try_next() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => {
                        // Channel closed (all senders dropped) - exit event loop
                        dd_log!("[Worker] Channel closed, exiting event loop");
                        return;
                    }
                    Err(_) => {
                        // No items ready yet, but channel still open - stop collecting
                        break;
                    }
                }
            }

            if events.is_empty() {
                continue;
            }

            dd_log!("[Worker] Processing {} events", events.len());

            // Process events through DD
            let (outputs, new_time, new_states, new_list_states) =
                Self::process_with_persistent_worker(
                    &mut config,
                    events,
                    current_time,
                    &cell_states,
                    &list_states,
                );

            current_time = new_time;
            cell_states = new_states;
            list_states = new_list_states;

            dd_log!("[Worker] Produced {} outputs", outputs.len());

            // ═══════════════════════════════════════════════════════════════════
            // PHASE 6: Write directly to CELL_STATES instead of via channel
            // ═══════════════════════════════════════════════════════════════════
            let mut nav_path: Option<String> = None;
            for output in outputs {
                // Write hold_updates (with persistence)
                for (cell_id, value) in output.hold_updates {
                    let update_cell_id = value.cell_id().unwrap_or_else(|| {
                        panic!("[DD Worker] Missing cell id for update {:?}", value);
                    });
                    if update_cell_id != cell_id.as_str() {
                        panic!(
                            "[DD Worker] hold_updates key '{}' does not match update cell '{}'",
                            cell_id, update_cell_id
                        );
                    }
                    sync_cell_from_dd_with_persist(value);
                }
                // Write hold_state_updates (no persistence)
                for (cell_id, value) in output.hold_state_updates {
                    // Check if this is a Router/go_to navigation cell
                    if config.go_to_cells.contains(&cell_id) {
                        if let Some(path) = cell_states.get(&cell_id) {
                            if let Value::Text(p) = path {
                                nav_path = Some(p.to_string());
                            }
                        }
                    }
                    let update_cell_id = value.cell_id().unwrap_or_else(|| {
                        panic!("[DD Worker] Missing cell id for update {:?}", value);
                    });
                    if update_cell_id != cell_id.as_str() {
                        panic!(
                            "[DD Worker] hold_state_updates key '{}' does not match update cell '{}'",
                            cell_id, update_cell_id
                        );
                    }
                    sync_cell_from_dd(value);
                }
            }
            // Handle Router/go_to navigation
            if let Some(path) = nav_path {
                navigate_to_route(&path);
                // Update all route cells so Router/route() reactive watchers see the change
                for route_cell in &config.route_cells {
                    sync_cell_from_dd(CellUpdate::set_value(route_cell.as_str(), Value::text(path.as_str())));
                }
            }

        }
    }

    /// Process events through the persistent DD worker (Phase 5).
    ///
    /// Uses a SINGLE long-lived Timely worker
    /// that persists across all event batches. This gives true O(delta) complexity
    /// because DD arrangements are not rebuilt on each batch.
    ///
    /// Key optimization: Templates are pre-instantiated BEFORE DD injection.
    /// This moves side effects (ID generation, HOLD registration) outside DD,
    /// allowing DD to use pure transforms.
    fn process_with_persistent_worker(
        config: &mut DataflowConfig,
        events: Vec<Event>,
        start_time: u64,
        initial_states: &HashMap<String, Value>,
        initial_list_states: &HashMap<String, super::dataflow::ListState>,
    ) -> (Vec<DocumentUpdate>, u64, HashMap<String, Value>, HashMap<String, super::dataflow::ListState>) {
        // Initialize or reinitialize persistent worker if config changed
        let mut list_cells = list_cell_ids(config);
        let mut derived_outputs = derived_list_outputs(config);
        let dd_cells = Self::convert_config_to_dd_cells(config);
        let dd_collections = Self::convert_config_to_dd_collections(config);
        let initial_collection_items = compute_collection_items(config, initial_list_states);
        let reinitialized = reinit_if_config_changed(
            dd_cells,
            dd_collections,
            initial_states.clone(),
            initial_list_states.clone(),
            initial_collection_items,
        );
        if reinitialized {
            dd_log!("[Worker] (Re)initialized persistent DD worker with {} cells", config.cells.len());
        }

        // Build template info map for O(delta) pre-instantiation
        let template_cells = Self::build_template_cell_map(config);
        let mut list_states = initial_list_states.clone();

        let mut outputs = Vec::new();
        let mut new_states = initial_states.clone();
        strip_list_cells(&mut new_states, &list_cells);
        let mut current_time = start_time;
        let mut initial_by_id: HashMap<String, Value> = config
            .cells
            .iter()
            .map(|cell| {
                let cell_id = cell.id.name();
                let initial = validate_cell_state_value(&cell_id, cell.initial.clone(), &list_cells);
                (cell_id.to_string(), initial)
            })
            .collect();
        for (cell_id, init) in &config.cell_initializations {
            initial_by_id.entry(cell_id.clone()).or_insert_with(|| {
                validate_cell_state_value(cell_id, init.value.clone(), &list_cells)
            });
        }
        // Derived outputs initialize from DD output stream on first emission.
        ensure_list_states(&list_cells, &derived_outputs, &mut list_states, initial_list_states);

        if reinitialized {
            let init_outputs = drain_outputs_persistent();
            apply_dd_outputs_for_worker(
                config,
                init_outputs,
                &list_cells,
                &mut list_states,
                &mut new_states,
                &initial_by_id,
                &mut outputs,
            );
        }

        // Process each event through the persistent worker
        for event in events {
            let (link_id, event_value) = match event {
                Event::Link { id, value } => (id, value),
                Event::Timer { id, .. } => (LinkId::new(&format!("__timer_{}", id)), EventValue::Unit),
                Event::External { name, .. } => {
                    panic!("[DD Worker] External events not supported: {}", name);
                }
            };

            // O(delta) optimization: Pre-instantiate templates BEFORE DD injection.
            // This moves side effects (ID generation, HOLD registration) outside DD.
            let (event_value, config_changed) = Self::maybe_pre_instantiate(&link_id, event_value, &template_cells, config);
            if config_changed {
                list_cells = list_cell_ids(config);
                derived_outputs = derived_list_outputs(config);
                initial_by_id = config
                    .cells
                    .iter()
                    .map(|cell| {
                        let cell_id = cell.id.name();
                        let initial = validate_cell_state_value(&cell_id, cell.initial.clone(), &list_cells);
                        (cell_id.to_string(), initial)
                    })
                    .collect();
                for (cell_id, init) in &config.cell_initializations {
                    initial_by_id.entry(cell_id.clone()).or_insert_with(|| {
                        validate_cell_state_value(cell_id, init.value.clone(), &list_cells)
                    });
                }
                ensure_list_states(&list_cells, &derived_outputs, &mut list_states, initial_list_states);
                let dd_cells = Self::convert_config_to_dd_cells(config);
                let dd_collections = Self::convert_config_to_dd_collections(config);
                let initial_collection_items = compute_collection_items(config, &list_states);
                let reinitialized = reinit_if_config_changed(
                    dd_cells,
                    dd_collections,
                    new_states.clone(),
                    list_states.clone(),
                    initial_collection_items,
                );
                if reinitialized {
                    dd_log!("[Worker] Reinitialized persistent worker after config change");
                }
                // Derived outputs initialize from DD output stream on first emission.
                if reinitialized {
                    let init_outputs = drain_outputs_persistent();
                    apply_dd_outputs_for_worker(
                        config,
                        init_outputs,
                        &list_cells,
                        &mut list_states,
                        &mut new_states,
                        &initial_by_id,
                        &mut outputs,
                    );
                }
            }

            // Inject event and get outputs
            let dd_outputs = inject_event_persistent(&link_id, event_value);
            current_time += 1;

            // Convert DD outputs to DocumentUpdates and update state
            apply_dd_outputs_for_worker(
                config,
                dd_outputs,
                &list_cells,
                &mut list_states,
                &mut new_states,
                &initial_by_id,
                &mut outputs,
            );

            apply_bulk_remove_for_link(
                config,
                &link_id,
                current_time,
                &initial_by_id,
                &list_cells,
                &mut list_states,
                &mut new_states,
                &mut outputs,
            );
        }

        (outputs, current_time, new_states, list_states)
    }

    /// Convert DataflowConfig cells to DdCellConfig for the persistent worker.
    fn convert_config_to_dd_cells(config: &DataflowConfig) -> Vec<DdCellConfig> {
        config.cells.iter().map(|cell| {
            // Map StateTransform to base DdTransform
            // Template-based transforms use ListAppendPrepared (pure, O(delta))
            // Pre-instantiation happens BEFORE DD injection in process_with_persistent_worker
            let base_transform = match &cell.transform {
                StateTransform::Increment => DdTransform::Increment,
                StateTransform::BoolToggle => DdTransform::Toggle,
                StateTransform::SetTrue => DdTransform::SetValue(Value::Bool(true)),
                StateTransform::SetFalse => DdTransform::SetValue(Value::Bool(false)),
                StateTransform::Identity => DdTransform::Identity,
                // Template-based transforms: pre-instantiation happens before DD, so use pure append
                StateTransform::ListAppendWithTemplate { .. } => DdTransform::ListAppendPrepared,
                StateTransform::ListAppendWithTemplateAndClear { clear_link_id, .. } => {
                    DdTransform::ListAppendPreparedWithClear { clear_link_id: clear_link_id.clone() }
                }
                StateTransform::ListAppendFromTemplate { .. } => DdTransform::ListAppendPrepared,
                StateTransform::ListAppendSimple => DdTransform::ListAppendSimple,
                StateTransform::ListAppendSimpleWithClear { clear_link_id } => {
                    DdTransform::ListAppendSimpleWithClear { clear_link_id: clear_link_id.clone() }
                }
            };

            // Build trigger list - start with explicit triggers
            let mut triggers = cell.triggered_by.clone();

            // Add timer trigger ID if this HOLD is timer-triggered
            if cell.timer_interval_ms > 0 {
                let timer_link_id = LinkId::new(&format!("__timer_{}", cell.timer_interval_ms));
                triggers.push(timer_link_id);
            }

            // Capture base triggers/filter before adding link mappings
            let base_triggers = triggers.clone();
            let base_filter = cell.filter.clone();

            // Attach link mappings for this cell (pure DD link handling)
            let mappings: Vec<super::types::LinkCellMapping> = config
                .link_mappings
                .iter()
                .filter(|m| m.cell_id == cell.id)
                .cloned()
                .collect();

            let (transform, filter) = if mappings.is_empty() {
                (base_transform, base_filter)
            } else {
                // Add mapping link IDs to triggers (dedup)
                for mapping in &mappings {
                    if !triggers.iter().any(|id| id == &mapping.link_id) {
                        triggers.push(mapping.link_id.clone());
                    }
                }
                (
                    DdTransform::WithLinkMappings {
                        base: Box::new(base_transform),
                        base_triggers,
                        base_filter: base_filter.clone(),
                        mappings,
                    },
                    EventFilter::Any,
                )
            };

            // EventFilter is now unified - no conversion needed
            DdCellConfig {
                id: cell.id.clone(),
                initial: cell.initial.clone(),
                triggers,
                transform,
                filter,
            }
        }).collect()
    }

    /// Convert DataflowConfig collections to DD collection config.
    fn convert_config_to_dd_collections(config: &DataflowConfig) -> DdCollectionConfig {
        DdCollectionConfig {
            ops: config.collection_ops.clone(),
            initial_collections: config.initial_collections.clone(),
            collection_sources: config.collection_sources.clone(),
        }
    }

    /// Build a map of link_id -> template info for cells that use template-based transforms.
    /// This enables O(delta) pre-instantiation before DD injection.
    fn build_template_cell_map(config: &DataflowConfig) -> HashMap<String, Vec<TemplateInfo>> {
        let mut map: HashMap<String, Vec<TemplateInfo>> = HashMap::new();

        for cell in &config.cells {
            let template_info = match &cell.transform {
                StateTransform::ListAppendWithTemplate { template } => {
                    Some(TemplateInfo {
                        template: template.clone(),
                        cell_id: cell.id.name(),
                        filter: cell.filter.clone(),
                    })
                }
                StateTransform::ListAppendWithTemplateAndClear { template, .. } => {
                    Some(TemplateInfo {
                        template: template.clone(),
                        cell_id: cell.id.name(),
                        filter: cell.filter.clone(),
                    })
                }
                StateTransform::ListAppendFromTemplate { template, .. } => {
                    Some(TemplateInfo {
                        template: template.clone(),
                        cell_id: cell.id.name(),
                        filter: cell.filter.clone(),
                    })
                }
                _ => None,
            };

            if let Some(info) = template_info {
                // Add to each trigger link
                for trigger in &cell.triggered_by {
                    map.entry(trigger.name().to_string())
                        .or_default()
                        .push(info.clone());
                }
            }
        }

        map
    }

    /// Pre-instantiate template if this event triggers a templated cell.
    /// Returns PreparedItem event value with the instantiated item + initializations.
    fn maybe_pre_instantiate(
        link_id: &LinkId,
        event_value: EventValue,
        template_cells: &HashMap<String, Vec<TemplateInfo>>,
        config: &mut DataflowConfig,
    ) -> (EventValue, bool) {
        if matches!(event_value, EventValue::PreparedItem { .. }) {
            return (event_value, false);
        }
        // Only pre-instantiate for Enter key events with text
        let Some(item_text) = event_value.enter_text() else {
            return (event_value, false);
        };
        let (source_key, source_text) = match &event_value {
            EventValue::KeyDown { key, text } => (Some(key.clone()), text.clone()),
            EventValue::Text(text) => (None, Some(text.clone())),
            _ => (None, None),
        };

        // Check if any templated cell is triggered by this link
        let Some(templates) = template_cells.get(link_id.name()) else {
            return (event_value, false);
        };

        // Find first template whose filter matches
        for template in templates {
            if template.filter.matches(&event_value) {

                dd_log!("[Worker] Pre-instantiating template for cell {} with text '{}'", template.cell_id, item_text);

                // Use the explicit ListItemTemplate (no reconstruction).
                let list_template = template.template.clone();

                // Pre-instantiate IDs and initial values BEFORE DD injection.
                // This keeps DD transforms pure while avoiding IO-side mutations.
                let instantiated = instantiate_template(
                    &list_template,
                    &HashMap::new(),
                    Some(item_text),
                    &mut config.initial_collections,
                );

                let mut config_changed = false;
                // Register dynamic cell configs for new item holds (pure DD).
                for (cell_id, value) in &instantiated.initializations {
                    if config.cells.iter().any(|cell| cell.id.name() == *cell_id) {
                        panic!("[DD Worker] Bug: duplicate cell config for '{}'", cell_id);
                    }
                    config.cells.push(CellConfig {
                        id: CellId::new(cell_id),
                        initial: value.clone(),
                        triggered_by: Vec::new(),
                        timer_interval_ms: 0,
                        filter: EventFilter::Any,
                        transform: StateTransform::Identity,
                        persist: false,
                    });
                    config_changed = true;
                }

                // Remap template link mappings to this item's fresh IDs.
                let mut new_mappings = remap_link_mappings_for_item(
                    &config.link_mappings,
                    &instantiated.link_id_map,
                    &instantiated.cell_id_map,
                );
                // Include mappings explicitly generated by instantiate_template.
                new_mappings.extend(instantiated.link_mappings.clone());
                if !new_mappings.is_empty() {
                    for mapping in new_mappings {
                        config.add_link_mapping(mapping);
                    }
                    config_changed = true;
                }

                dd_log!("[Worker] Pre-instantiated item: {:?}", instantiated.data);

                // Return PreparedItem containing the pre-instantiated data
                return (
                    EventValue::prepared_item_with_source(
                    instantiated.data,
                    instantiated.initializations,
                    source_key,
                    source_text,
                    ),
                    config_changed,
                );
            }
        }

        (event_value, false)
    }

    // Note: List item templates are fully specified in config; no runtime lookups.
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn test_dataflow_config_builder() {
        // Test that we can build a simple counter config
        let config = DataflowConfig::new().add_hold("counter", Value::int(0), vec!["button.press"]);
        assert_eq!(config.cells.len(), 1);
        assert_eq!(config.cells[0].id.name(), "counter");
        assert_eq!(config.cells[0].initial, Value::int(0));
        assert_eq!(config.cells[0].triggered_by.len(), 1);
        assert_eq!(config.cells[0].triggered_by[0].name(), "button.press");
    }

    #[test]
    fn test_dataflow_config_multiple_holds() {
        let config = DataflowConfig::new()
            .add_hold("counter1", Value::int(0), vec!["click1"])
            .add_hold("counter2", Value::int(10), vec!["click2", "click3"]);

        assert_eq!(config.cells.len(), 2);
        assert_eq!(config.cells[0].id.name(), "counter1");
        assert_eq!(config.cells[1].id.name(), "counter2");
        assert_eq!(config.cells[1].initial, Value::int(10));
        assert_eq!(config.cells[1].triggered_by.len(), 2);
    }

    #[test]
    #[should_panic(expected = "Duplicate __key")]
    fn test_initial_collection_duplicate_keys_panics() {
        let mut config = DataflowConfig::new();
        let collection_id = CollectionId::new();
        let item1 = Value::object([("__key", Value::text("a"))]);
        let item2 = Value::object([("__key", Value::text("a"))]);
        config
            .initial_collections
            .insert(collection_id, vec![item1, item2]);

        let list_states: HashMap<String, super::super::dataflow::ListState> = HashMap::new();
        let _ = compute_collection_items(&config, &list_states);
    }

    #[test]
    #[should_panic(expected = "missing __key")]
    fn test_initial_collection_missing_key_panics() {
        let mut config = DataflowConfig::new();
        let collection_id = CollectionId::new();
        let item = Value::object([("title", Value::text("todo"))]);
        config
            .initial_collections
            .insert(collection_id, vec![item]);

        let list_states: HashMap<String, super::super::dataflow::ListState> = HashMap::new();
        let _ = compute_collection_items(&config, &list_states);
    }

    #[test]
    #[should_panic(expected = "List cell 'todos' must be List")]
    fn test_list_cell_rejects_non_list() {
        let mut list_cells: HashSet<String> = HashSet::new();
        list_cells.insert("todos".to_string());
        let _ = validate_cell_state_value("todos", Value::text("not a list"), &list_cells);
    }

    #[test]
    #[should_panic(expected = "Missing collection cell_id for 'todos'")]
    fn test_list_cell_missing_collection_cell_id_panics() {
        let mut list_cells: HashSet<String> = HashSet::new();
        list_cells.insert("todos".to_string());
        let value = Value::List(CollectionHandle::new());
        let _ = validate_cell_state_value("todos", value, &list_cells);
    }

    #[test]
    #[should_panic(expected = "ListPush diff for 'other' applied to 'items'")]
    fn test_list_diff_wrong_cell_id_panics() {
        let mut list_states: HashMap<String, super::super::dataflow::ListState> = HashMap::new();
        let item = Value::object([("__key", Value::text("a"))]);
        list_states.insert(
            "items".to_string(),
            super::super::dataflow::ListState::new(vec![item.clone()], "init"),
        );
        let output = CellUpdate::ListPush {
            cell_id: Arc::from("other"),
            item,
        };
        apply_list_update_to_state(&mut list_states, "items", &output);
    }

    #[test]
    #[should_panic(expected = "Output cell 'other' does not match 'count'")]
    fn test_apply_dd_outputs_rejects_mismatched_update_cell_id() {
        let config = DataflowConfig::new().add_hold("counter", Value::int(0), vec!["click"]);
        let dd_outputs = vec![super::super::dataflow::DdOutput {
            cell_id: CellId::new("count"),
            value: CellUpdate::set_value("other", Value::int(1)),
            time: 1,
            diff: 1,
        }];
        let list_cells: HashSet<String> = HashSet::new();
        let mut list_states: HashMap<String, super::super::dataflow::ListState> = HashMap::new();
        let mut new_states: HashMap<String, Value> = HashMap::new();
        let initial_by_id = HashMap::from([(String::from("count"), Value::int(0))]);
        let mut outputs: Vec<DocumentUpdate> = Vec::new();

        apply_dd_outputs_for_worker(
            &config,
            dd_outputs,
            &list_cells,
            &mut list_states,
            &mut new_states,
            &initial_by_id,
            &mut outputs,
        );
    }
}
