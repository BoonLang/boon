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
use std::cell::RefCell;
// Phase 10: Mutex removed - outputs captured via pure DD channels

use zoon::futures_channel::mpsc;
use zoon::{Task, Timer};

use super::types::{Event, EventValue, Input, CellId, LinkId, DYNAMIC_LINK_PREFIX, BoolTag, ElementTag, EventPayload, EventFilter};
use super::value::Value;
use super::super::io::{
    get_cell_value,
    load_persisted_cell_value,
    sync_cell_from_dd,
    sync_cell_from_dd_with_persist,
    update_cell_no_persist,
};
use super::super::LOG_DD_DEBUG;
use std::collections::BTreeMap;

// Persistent DD Worker (Phase 5)
use super::dataflow::{
    DdCellConfig, DdTransform,
    inject_event_persistent, reinit_if_config_changed,
};

/// Enable persistent DD worker mode (Phase 5).
/// When true, uses a single long-lived Timely worker instead of batch-per-event.
/// This is the key optimization for true O(delta) complexity.
///
/// Template-based transforms (ListAppendWithTemplate, etc.) are now supported via
/// pre-instantiation BEFORE DD injection. Templates are instantiated with fresh IDs
/// outside DD, then passed as PreparedItem events for pure DD append.
const USE_PERSISTENT_WORKER: bool = true;

// ============================================================================
// PHASE 5: THREAD-LOCAL ID COUNTERS (Deterministic Generation)
// ============================================================================
//
// ID counters are thread_local instead of global static. This gives:
// - Determinism: reset_id_counters() resets both to 1000 for each Worker session
// - Replayability: same inputs always produce same IDs
// - No global mutation: counters scoped to current thread
//
// Worker calls reset_id_counters() in new() to start each session fresh.

thread_local! {
    /// Counter for generating unique HOLD IDs for dynamic items
    static DYNAMIC_CELL_COUNTER: RefCell<u32> = RefCell::new(1000);

    /// Counter for generating unique LINK IDs for dynamic items
    static DYNAMIC_LINK_COUNTER: RefCell<u32> = RefCell::new(1000);

    /// Active DataflowConfig for the current Worker session.
    /// Set by Worker::with_config() and read by outputs.rs consumers.
    /// This consolidates all registry state into one config object.
    static ACTIVE_CONFIG: RefCell<Option<DataflowConfig>> = RefCell::new(None);
}

/// Set the active config for this session.
/// Called by Worker::with_config() when starting.
pub fn set_active_config(config: DataflowConfig) {
    ACTIVE_CONFIG.with(|c| *c.borrow_mut() = Some(config));
}

/// Get a reference to the active config.
/// Returns None if no Worker is running.
pub fn with_active_config<R>(f: impl FnOnce(&DataflowConfig) -> R) -> Option<R> {
    ACTIVE_CONFIG.with(|c| {
        c.borrow().as_ref().map(f)
    })
}

/// Check if a cell ID is a text-clear cell.
pub fn is_text_clear_cell(cell_id: &str) -> bool {
    with_active_config(|config| config.text_clear_cells.contains(cell_id)).unwrap_or(false)
}

/// Get the remove event path from the active config.
pub fn get_remove_event_path() -> Vec<String> {
    with_active_config(|config| config.remove_event_path.clone()).unwrap_or_default()
}

/// Get the bulk remove event path from the active config.
pub fn get_bulk_remove_event_path() -> Vec<String> {
    with_active_config(|config| config.bulk_remove_event_path.clone()).unwrap_or_default()
}

/// Get the editing bindings from the active config.
pub fn get_editing_bindings() -> Vec<EditingBinding> {
    with_active_config(|config| config.editing_bindings.clone()).unwrap_or_default()
}

/// Get the toggle bindings from the active config.
pub fn get_toggle_bindings() -> Vec<ToggleBinding> {
    with_active_config(|config| config.toggle_bindings.clone()).unwrap_or_default()
}

/// Get the global toggle bindings from the active config.
pub fn get_global_toggle_bindings() -> Vec<GlobalToggleBinding> {
    with_active_config(|config| config.global_toggle_bindings.clone()).unwrap_or_default()
}

/// Clear the active config.
/// Called when clearing state between examples.
pub fn clear_active_config() {
    ACTIVE_CONFIG.with(|c| *c.borrow_mut() = None);
}

/// Reset ID counters to starting values (1000).
/// Called by Worker::new() to ensure deterministic ID generation per session.
pub fn reset_id_counters() {
    DYNAMIC_CELL_COUNTER.with(|c| *c.borrow_mut() = 1000);
    DYNAMIC_LINK_COUNTER.with(|c| *c.borrow_mut() = 1000);
}

/// Allocate a new dynamic cell ID from thread-local counter.
fn allocate_dynamic_cell_id() -> String {
    DYNAMIC_CELL_COUNTER.with(|c| {
        let mut counter = c.borrow_mut();
        let id = *counter;
        *counter += 1;
        format!("{}{}", DYNAMIC_CELL_PREFIX, id)
    })
}

/// Allocate a new dynamic link ID from thread-local counter.
fn allocate_dynamic_link_id() -> String {
    DYNAMIC_LINK_COUNTER.with(|c| {
        let mut counter = c.borrow_mut();
        let id = *counter;
        *counter += 1;
        format!("{}{}", DYNAMIC_LINK_PREFIX, id)
    })
}

/// Prefix for dynamically generated HOLD IDs (used for list items)
const DYNAMIC_CELL_PREFIX: &str = "dynamic_cell_";

// NOTE: DYNAMIC_LINK_PREFIX is imported from super::types

// ============================================================================
// PURE FIELD CHECKING
// ============================================================================

/// Check if a specific boolean field on an item is true.
/// PURE: Does not read from IO layer - checks item data directly.
///
/// Parameters:
/// - item: The list item Value (expected to be an Object)
/// - field_name: The name of the boolean field to check (e.g., "completed", "done", "checked")
///
/// Returns true if the field exists and is a true boolean value.
/// For CellRef fields, this reads the referenced cell from IO layer (still has IO dependency).
fn is_item_field_true(item: &Value, field_name: &str) -> bool {
    match item {
        Value::Object(obj) => {
            if let Some(field_value) = obj.get(field_name) {
                match field_value {
                    // Direct boolean value
                    Value::Bool(b) => return *b,
                    // Tagged boolean (True/False)
                    Value::Tagged { tag, .. } => {
                        if BoolTag::is_bool_tag(tag.as_ref()) {
                            return BoolTag::is_true(tag.as_ref());
                        }
                    }
                    // CellRef - need to look up the cell value (still has IO dependency)
                    // TODO: In a fully pure implementation, item data would embed the value directly
                    Value::CellRef(cell_id) => {
                        if let Some(cell_value) = super::super::io::get_cell_value(&cell_id.name()) {
                            match cell_value {
                                Value::Bool(b) => return b,
                                Value::Tagged { tag, .. } => {
                                    if BoolTag::is_bool_tag(tag.as_ref()) {
                                        return BoolTag::is_true(tag.as_ref());
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            false
        }
        _ => false,
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
    /// Initialize from the event text (e.g., Enter:text → "text")
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
    /// Handle editing: save on Enter, cancel on Escape
    EditingHandler { editing_cell_path: FieldPath, title_cell_path: FieldPath },
    /// Track hover state (true on mouseenter, false on mouseleave)
    HoverState { hold_path: FieldPath },
    /// Remove this item from the list
    RemoveItem,
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
    pub data_template: Value,
    /// Optional element AST template for rendering
    pub element_template: Option<Value>,
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
    pub fn new(data_template: Value, identity_path: FieldPath) -> Self {
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
    pub fn with_element_template(mut self, template: Value) -> Self {
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
pub fn update_field_at_path(value: &Value, path: &[String], update: &FieldUpdate, event_value: Option<&str>) -> Value {
    if path.is_empty() {
        // Apply update to the value directly
        return match update {
            FieldUpdate::Constant(v) => v.clone(),
            FieldUpdate::Toggle => match value {
                Value::Bool(b) => Value::Bool(!b),
                _ => value.clone(),
            },
            FieldUpdate::SetTrue => Value::Bool(true),
            FieldUpdate::SetFalse => Value::Bool(false),
            FieldUpdate::FromEventTextAfterIdentity => {
                event_value.map(|s| Value::text(s)).unwrap_or_else(|| value.clone())
            }
        };
    }

    match value {
        Value::Object(obj) => {
            let mut new_obj = (**obj).clone();
            if let Some(field_value) = obj.get(path[0].as_str()) {
                new_obj.insert(
                    Arc::from(path[0].as_str()),
                    update_field_at_path(field_value, &path[1..], update, event_value),
                );
            }
            Value::Object(Arc::new(new_obj))
        }
        Value::Tagged { tag, fields } => {
            let mut new_fields = (**fields).clone();
            if let Some(field_value) = fields.get(path[0].as_str()) {
                new_fields.insert(
                    Arc::from(path[0].as_str()),
                    update_field_at_path(field_value, &path[1..], update, event_value),
                );
            }
            Value::Tagged { tag: tag.clone(), fields: Arc::new(new_fields) }
        }
        _ => value.clone(),
    }
}

/// Compute the new field value for a ListItemUpdate diff.
/// Given an item, field path, and update type, returns just the new field value.
/// Phase 4: Used by ListItemSetFieldByIdentity to emit O(1) diffs instead of cloning lists.
fn compute_field_update(item: &Value, path: &[String], update: &FieldUpdate, event_value: Option<&str>) -> Value {
    // Navigate to the field at the path
    let mut current = item;
    for segment in path.iter().take(path.len().saturating_sub(1)) {
        match current {
            Value::Object(fields) => {
                if let Some(next) = fields.get(segment.as_str()) {
                    current = next;
                } else {
                    return Value::Unit; // Field not found
                }
            }
            Value::Tagged { fields, .. } => {
                if let Some(next) = fields.get(segment.as_str()) {
                    current = next;
                } else {
                    return Value::Unit;
                }
            }
            _ => return Value::Unit,
        }
    }

    // Get the current value of the final field
    let final_field = path.last().map(|s| s.as_str()).unwrap_or("");
    let current_value = match current {
        Value::Object(fields) => fields.get(final_field).cloned().unwrap_or(Value::Unit),
        Value::Tagged { fields, .. } => fields.get(final_field).cloned().unwrap_or(Value::Unit),
        _ => Value::Unit,
    };

    // Apply the update to compute the new value
    match update {
        FieldUpdate::Constant(v) => v.clone(),
        FieldUpdate::Toggle => match &current_value {
            Value::Bool(b) => Value::Bool(!b),
            _ => current_value,
        },
        FieldUpdate::SetTrue => Value::Bool(true),
        FieldUpdate::SetFalse => Value::Bool(false),
        FieldUpdate::FromEventTextAfterIdentity => {
            event_value.map(|s| Value::text(s)).unwrap_or(current_value)
        }
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
) -> Value {
    clone_template_with_fresh_ids_impl(template, cell_id_map, link_id_map, None)
}

/// Clone a template Value with fresh IDs and optional data context for PlaceholderField resolution.
fn clone_template_with_fresh_ids_with_context(
    template: &Value,
    cell_id_map: &mut HashMap<String, String>,
    link_id_map: &mut HashMap<String, String>,
    data_context: &Value,
) -> Value {
    clone_template_with_fresh_ids_impl(template, cell_id_map, link_id_map, Some(data_context))
}

fn clone_template_with_fresh_ids_impl(
    template: &Value,
    cell_id_map: &mut HashMap<String, String>,
    link_id_map: &mut HashMap<String, String>,
    data_context: Option<&Value>,
) -> Value {
    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 7 NOTE: Removed match arms for surgically removed Value variants:
    //   - PlaceholderField, WhileRef, PlaceholderWhileRef
    // These were template substitution patterns that should be replaced with
    // DD list_map operator for incremental list rendering.
    // ═══════════════════════════════════════════════════════════════════════════
    match template {
        Value::CellRef(old_id) => {
            // Generate or reuse fresh ID for this CellRef
            // Phase 5: Use thread-local allocator for deterministic IDs
            let new_id = cell_id_map
                .entry(old_id.name())
                .or_insert_with(allocate_dynamic_cell_id)
                .clone();
            Value::CellRef(CellId::new(new_id))
        }
        Value::LinkRef(old_id) => {
            // Generate or reuse fresh ID for this LinkRef
            // Phase 5: Use thread-local allocator for deterministic IDs
            let new_id = link_id_map
                .entry(old_id.name().to_string())
                .or_insert_with(allocate_dynamic_link_id)
                .clone();
            Value::LinkRef(LinkId::new(new_id))
        }
        Value::Object(fields) => {
            let new_fields: BTreeMap<Arc<str>, Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), clone_template_with_fresh_ids_impl(v, cell_id_map, link_id_map, data_context)))
                .collect();
            Value::Object(Arc::new(new_fields))
        }
        Value::List(items) => {
            let new_items: Vec<Value> = items
                .iter()
                .map(|v| clone_template_with_fresh_ids_impl(v, cell_id_map, link_id_map, None))
                .collect();
            Value::collection(new_items)
        }
        Value::Tagged { tag, fields } => {
            let new_fields: BTreeMap<Arc<str>, Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), clone_template_with_fresh_ids_impl(v, cell_id_map, link_id_map, None)))
                .collect();
            Value::Tagged { tag: tag.clone(), fields: Arc::new(new_fields) }
        }
        // Primitive types and removed symbolic refs are cloned as-is
        _ => template.clone(),
    }
}

/// Find the cell_id of a __while_config__ Tagged value that controls hover-based visibility.
///
/// Pure DD: WhileRef variant was replaced with __while_config__ Tagged value in Phase 7.
/// This function now searches for the Tagged pattern.
fn find_hover_while_cell_id(items: Option<&Value>) -> Option<String> {
    // Search for __while_config__ Tagged value in items
    let items = items?;
    if let Value::List(item_list) = items {
        for item in item_list.iter() {
            if let Value::Tagged { tag, fields, .. } = item {
                if tag.as_ref() == "__while_config__" {
                    // Extract cell_id from the Tagged value
                    if let Some(Value::Text(cell_id)) = fields.get("cell_id") {
                        return Some(cell_id.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Extract a unique key from a Value for O(1) lookup.
/// Looks for CellRef or LinkRef IDs which are guaranteed unique per item.
/// Falls back to display string for simple values.
/// Used by ListRemoveCompleted to emit batch removal keys.
fn extract_item_key_for_removal(value: &Value) -> Arc<str> {
    match value {
        Value::CellRef(cell_id) => Arc::from(format!("hold:{}", cell_id.name())),
        Value::LinkRef(link_id) => Arc::from(format!("link:{}", link_id.name())),
        Value::Object(fields) => {
            // For objects, find first CellRef or LinkRef field (they're unique per item)
            for (_, field_value) in fields.iter() {
                match field_value {
                    Value::CellRef(cell_id) => return Arc::from(format!("hold:{}", cell_id.name())),
                    Value::LinkRef(link_id) => return Arc::from(format!("link:{}", link_id.name())),
                    Value::Object(inner) => {
                        // Check nested objects (e.g., todo_elements)
                        for (_, inner_value) in inner.iter() {
                            match inner_value {
                                Value::CellRef(cell_id) => return Arc::from(format!("hold:{}", cell_id.name())),
                                Value::LinkRef(link_id) => return Arc::from(format!("link:{}", link_id.name())),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Fallback to display string
            Arc::from(value.to_display_string())
        }
        _ => Arc::from(value.to_display_string()),
    }
}

/// Find the checkbox's `checked` CellRef ID within an Element AST.
/// Used by ListRemoveCompleted to determine if an element should be removed.
fn find_checkbox_cell_id(element: &Value) -> Option<String> {
    // Element structure: Tagged { tag: "Element", fields: { items: List([checkbox, ...]) } }
    if let Value::Tagged { tag, fields } = element {
        if ElementTag::is_element(tag.as_ref()) {
            // Look for items list
            if let Some(Value::List(items)) = fields.get("items") {
                for item in items.iter() {
                    // Check if this item is a checkbox element
                    if let Value::Tagged { tag: item_tag, fields: item_fields } = item {
                        if ElementTag::is_element(item_tag.as_ref()) {
                            if let Some(Value::Text(elem_type)) = item_fields.get("_element_type") {
                                if elem_type.as_ref() == "checkbox" {
                                    // Found checkbox - extract checked CellRef
                                    if let Some(Value::CellRef(cell_id)) = item_fields.get("checked") {
                                        if LOG_DD_DEBUG { zoon::println!("[DD Worker] find_checkbox_cell_id: found checkbox with checked={}", cell_id.name()); }
                                        return Some(cell_id.name());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if LOG_DD_DEBUG { zoon::println!("[DD Worker] find_checkbox_cell_id: no checkbox found in element type {:?}", fields.get("_element_type")); }
        }
    }
    None
}

/// Reconstruct a persisted item using templates.
///
/// When list items are persisted to localStorage, they lose their CellRef/LinkRef structure.
/// This function reconstructs the full structure by:
/// 1. Cloning templates with fresh IDs
/// 2. Initializing HOLDs with persisted values (using generic field detection)
/// 3. Registering dynamic link actions (edit, remove, hover)
/// 4. Returning the reconstructed (data_item, element) pair
///
/// This enables persisted items to render correctly after page reload.
pub fn reconstruct_persisted_item(
    persisted_item: &Value,
    data_template: &Value,
    element_template: Option<&Value>,
) -> Option<(Value, Option<Value>)> {
    // Extract persisted values as a generic map (no hardcoded field names)
    let persisted_fields = if let Value::Object(obj) = persisted_item {
        obj.clone()
    } else {
        return None;
    };

    zoon::println!("[DD Reconstruct] Persisted item fields: {:?}", persisted_fields.keys().collect::<Vec<_>>());

    // Extract field-to-hold mapping from data template
    let field_to_hold = extract_field_to_hold_map(data_template);

    // Clone data template with fresh CellRef/LinkRef IDs
    let mut cell_id_map: HashMap<String, String> = HashMap::new();
    let mut link_id_map: HashMap<String, String> = HashMap::new();
    let new_data_item = clone_template_with_fresh_ids(data_template, &mut cell_id_map, &mut link_id_map);

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
        // Task 7.2: Use original HOLD's type to determine default, not hardcoded field names
        let initial_value = field_name
            .and_then(|name| persisted_fields.get(name.as_str()))
            .cloned()
            .unwrap_or_else(|| {
                // Task 7.2: Check original HOLD's value to determine type, not field name
                if let Some(template_value) = get_cell_value(old_id) {
                    match template_value {
                        Value::Bool(_) => Value::Bool(false),
                        Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => {
                            Value::Bool(false)
                        }
                        _ => Value::Unit
                    }
                } else {
                    Value::Unit
                }
            });
        zoon::println!("[DD Reconstruct] Registering HOLD: {} -> {} = {:?} (field: {:?})",
            old_id, new_id, initial_value, field_name);
        update_cell_no_persist(new_id, initial_value);
    }

    zoon::println!("[DD Reconstruct] Created item with {} HOLDs, {} LINKs",
        cell_id_map.len(), link_id_map.len());

    // Replicate dynamic link actions from template to cloned item
    // For each LinkRef in the template, look up its action and register the same action
    // (with remapped hold IDs) for the new LinkRef
    use super::super::io::{add_dynamic_link_action, get_dynamic_link_action, DynamicLinkAction};
    for (old_link_id, new_link_id) in &link_id_map {
        if let Some(action) = get_dynamic_link_action(old_link_id) {
            // Remap hold IDs in the action
            let remapped_action = match action {
                DynamicLinkAction::SetTrue(old_hold) => {
                    cell_id_map.get(&old_hold)
                        .map(|new_hold| DynamicLinkAction::SetTrue(new_hold.clone()))
                }
                DynamicLinkAction::SetFalse(old_hold) => {
                    cell_id_map.get(&old_hold)
                        .map(|new_hold| DynamicLinkAction::SetFalse(new_hold.clone()))
                }
                DynamicLinkAction::EditingHandler { editing_cell, title_cell } => {
                    match (cell_id_map.get(&editing_cell), cell_id_map.get(&title_cell)) {
                        (Some(new_editing), Some(new_title)) => {
                            Some(DynamicLinkAction::EditingHandler {
                                editing_cell: new_editing.clone(),
                                title_cell: new_title.clone(),
                            })
                        }
                        _ => None,
                    }
                }
                DynamicLinkAction::HoverState(old_hold) => {
                    cell_id_map.get(&old_hold)
                        .map(|new_hold| DynamicLinkAction::HoverState(new_hold.clone()))
                }
                DynamicLinkAction::RemoveListItem { .. } => {
                    // RemoveListItem uses the new link_id to identify which item to remove
                    Some(DynamicLinkAction::RemoveListItem { link_id: new_link_id.clone() })
                }
                DynamicLinkAction::SetFalseOnKeys { cell_id, keys } => {
                    cell_id_map.get(&cell_id)
                        .map(|new_hold| DynamicLinkAction::SetFalseOnKeys {
                            cell_id: new_hold.clone(),
                            keys: keys.clone(),
                        })
                }
                DynamicLinkAction::BoolToggle(old_hold) => {
                    cell_id_map.get(&old_hold)
                        .map(|new_hold| DynamicLinkAction::BoolToggle(new_hold.clone()))
                }
                DynamicLinkAction::ListToggleAllCompleted { list_cell_id, completed_field } => {
                    // ListToggleAllCompleted operates on the whole list, not per-item
                    // Keep the same action (list_cell_id doesn't change per item)
                    Some(DynamicLinkAction::ListToggleAllCompleted {
                        list_cell_id: list_cell_id.clone(),
                        completed_field: completed_field.clone(),
                    })
                }
            };
            if let Some(new_action) = remapped_action {
                zoon::println!("[DD Reconstruct] Replicating action {} -> {} {:?}", old_link_id, new_link_id, new_action);
                add_dynamic_link_action(new_link_id.clone(), new_action);
            }
        }
    }

    // Clone element template if provided
    let new_element = element_template.map(|elem_tmpl| {
        // Find the hover link in the element template BEFORE cloning
        let hover_link_old_id = if let Value::Tagged { fields, .. } = elem_tmpl {
            fields.get("element")
                .and_then(|e| e.get("hovered"))
                .and_then(|h| match h {
                    Value::LinkRef(id) => Some(id.name().to_string()),
                    _ => None,
                })
        } else {
            None
        };

        // Find the WhileRef for hover
        let hover_cell_old_id = if let Value::Tagged { fields, .. } = elem_tmpl {
            find_hover_while_cell_id(fields.get("items"))
        } else {
            None
        };

        // Pre-populate hover hold mapping BEFORE cloning
        // The hover WhileRef uses cell_id like "hover_link_22", which is a synthetic hold
        // We need to map it to "hover_{new_link_id}" before clone processes the WhileRef
        if let (Some(old_link), Some(old_hold)) = (&hover_link_old_id, &hover_cell_old_id) {
            // First ensure the link gets a new ID
            // Phase 5: Use thread-local allocator for deterministic IDs
            let new_link_id = link_id_map
                .entry(old_link.clone())
                .or_insert_with(allocate_dynamic_link_id)
                .clone();
            // Then create the hover hold mapping: hover_link_22 → hover_dynamic_link_1000
            let new_hover_cell = format!("hover_{}", new_link_id);
            cell_id_map.insert(old_hold.clone(), new_hover_cell.clone());
            zoon::println!("[DD Reconstruct Persisted] Pre-mapped hover hold: {} -> {}", old_hold, new_hover_cell);
        }

        // Clone element template - reuses the same ID mapping
        // Pass new_data_item as context to resolve PlaceholderFields (e.g., double_click LinkRefs)
        let cloned_element = clone_template_with_fresh_ids_with_context(elem_tmpl, &mut cell_id_map, &mut link_id_map, &new_data_item);

        // Register HoverState action if we found both the hover link and hold
        if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_cell_old_id) {
            if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), cell_id_map.get(&old_hold)) {
                zoon::println!("[DD Reconstruct] Registering HoverState: {} -> {}", new_link, new_hold);
                use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                add_dynamic_link_action(new_link.clone(), DynamicLinkAction::HoverState(new_hold.clone()));
                // Initialize hover hold to false
                update_cell_no_persist(new_hold, Value::Bool(false));
            }
        }

        cloned_element
    });

    Some((new_data_item, new_element))
}

/// Instantiate a fresh item from the Boon code with unique IDs.
///
/// Fresh items have the original template CellRef/LinkRef IDs (e.g., hold_12, link_22).
/// These IDs are shared across all items from the same template, which causes bugs
/// (e.g., hovering one item shows delete button on all items).
///
/// This function clones the item with fresh unique IDs so each item is independent.
///
/// Returns (new_data_item, new_element) where both have fresh IDs.
pub fn instantiate_fresh_item(
    fresh_item: &Value,
    element_template: Option<&Value>,
) -> Option<(Value, Option<Value>)> {
    let mut cell_id_map: HashMap<String, String> = HashMap::new();
    let mut link_id_map: HashMap<String, String> = HashMap::new();

    // Clone the data item with fresh IDs
    let new_data_item = clone_template_with_fresh_ids(fresh_item, &mut cell_id_map, &mut link_id_map);

    // Initialize HOLDs from the fresh item's values
    // For fresh items, we can extract values directly from CellRefs in the original
    if let Value::Object(obj) = fresh_item {
        for (field_name, value) in obj.iter() {
            match value {
                Value::CellRef(old_cell_id) => {
                    // Get current value of this hold and initialize the new hold with it
                    let old_cell_name = old_cell_id.name();
                    if let Some(new_cell_id) = cell_id_map.get(&old_cell_name) {
                        if let Some(current_value) = super::super::io::get_cell_value(&old_cell_name) {
                            if LOG_DD_DEBUG { zoon::println!("[DD Worker] instantiate_fresh_item: field {} hold {} -> {}, value={:?}", field_name, old_cell_name, new_cell_id, current_value); }
                            update_cell_no_persist(new_cell_id, current_value);
                        }
                    }
                }
                Value::Object(inner_obj) => {
                    // Handle nested objects (like todo_elements containing LinkRefs)
                    for (inner_name, inner_value) in inner_obj.iter() {
                        if let Value::LinkRef(old_link_id) = inner_value {
                            // Replicate link actions for each LinkRef in nested objects
                            let old_link_name = old_link_id.name();
                            if let Some(new_link_id) = link_id_map.get(old_link_name) {
                                if let Some(action) = super::super::io::get_dynamic_link_action(old_link_name) {
                                    // Remap the action's hold references
                                    let remapped_action = match action {
                                        super::super::io::DynamicLinkAction::SetTrue(old_hold) => {
                                            cell_id_map.get(&old_hold)
                                                .map(|new_hold| super::super::io::DynamicLinkAction::SetTrue(new_hold.clone()))
                                        }
                                        super::super::io::DynamicLinkAction::SetFalse(old_hold) => {
                                            cell_id_map.get(&old_hold)
                                                .map(|new_hold| super::super::io::DynamicLinkAction::SetFalse(new_hold.clone()))
                                        }
                                        super::super::io::DynamicLinkAction::BoolToggle(old_hold) => {
                                            cell_id_map.get(&old_hold)
                                                .map(|new_hold| super::super::io::DynamicLinkAction::BoolToggle(new_hold.clone()))
                                        }
                                        super::super::io::DynamicLinkAction::EditingHandler { editing_cell, title_cell } => {
                                            match (cell_id_map.get(&editing_cell), cell_id_map.get(&title_cell)) {
                                                (Some(new_editing), Some(new_title)) => {
                                                    Some(super::super::io::DynamicLinkAction::EditingHandler {
                                                        editing_cell: new_editing.clone(),
                                                        title_cell: new_title.clone(),
                                                    })
                                                }
                                                _ => None,
                                            }
                                        }
                                        super::super::io::DynamicLinkAction::HoverState(old_hold) => {
                                            cell_id_map.get(&old_hold)
                                                .map(|new_hold| super::super::io::DynamicLinkAction::HoverState(new_hold.clone()))
                                        }
                                        super::super::io::DynamicLinkAction::RemoveListItem { .. } => {
                                            Some(super::super::io::DynamicLinkAction::RemoveListItem { link_id: new_link_id.clone() })
                                        }
                                        super::super::io::DynamicLinkAction::SetFalseOnKeys { cell_id, keys } => {
                                            cell_id_map.get(&cell_id)
                                                .map(|new_hold| super::super::io::DynamicLinkAction::SetFalseOnKeys {
                                                    cell_id: new_hold.clone(),
                                                    keys: keys.clone(),
                                                })
                                        }
                                        super::super::io::DynamicLinkAction::ListToggleAllCompleted { list_cell_id, completed_field } => {
                                            Some(super::super::io::DynamicLinkAction::ListToggleAllCompleted {
                                                list_cell_id: list_cell_id.clone(),
                                                completed_field: completed_field.clone(),
                                            })
                                        }
                                    };
                                    if let Some(new_action) = remapped_action {
                                        if LOG_DD_DEBUG { zoon::println!("[DD Worker] instantiate_fresh_item: Replicating action {} -> {} {:?}", old_link_name, new_link_id, new_action); }
                                        super::super::io::add_dynamic_link_action(new_link_id.clone(), new_action);
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Clone element template if provided
    let new_element = element_template.map(|elem_tmpl| {
        // Find the hover link in the element template BEFORE cloning
        let hover_link_old_id = if let Value::Tagged { fields, .. } = elem_tmpl {
            fields.get("element")
                .and_then(|e| e.get("hovered"))
                .and_then(|h| match h {
                    Value::LinkRef(id) => Some(id.name().to_string()),
                    _ => None,
                })
        } else {
            None
        };

        // Find the WhileRef for hover
        let hover_cell_old_id = if let Value::Tagged { fields, .. } = elem_tmpl {
            find_hover_while_cell_id(fields.get("items"))
        } else {
            None
        };

        // Pre-populate hover hold mapping BEFORE cloning
        // Phase 5: Use thread-local allocator for deterministic IDs
        if let (Some(old_link), Some(old_hold)) = (&hover_link_old_id, &hover_cell_old_id) {
            let new_link_id = link_id_map
                .entry(old_link.clone())
                .or_insert_with(allocate_dynamic_link_id)
                .clone();
            let new_hover_cell = format!("hover_{}", new_link_id);
            cell_id_map.insert(old_hold.clone(), new_hover_cell.clone());
            if LOG_DD_DEBUG { zoon::println!("[DD Worker] instantiate_fresh_item: Pre-mapped hover hold: {} -> {}", old_hold, new_hover_cell); }
        }

        // Clone element template with same ID mappings
        let cloned_element = clone_template_with_fresh_ids_with_context(elem_tmpl, &mut cell_id_map, &mut link_id_map, &new_data_item);

        // Register HoverState action
        if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_cell_old_id) {
            if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), cell_id_map.get(&old_hold)) {
                if LOG_DD_DEBUG { zoon::println!("[DD Worker] instantiate_fresh_item: Registering HoverState: {} -> {}", new_link, new_hold); }
                super::super::io::add_dynamic_link_action(new_link.clone(), super::super::io::DynamicLinkAction::HoverState(new_hold.clone()));
                update_cell_no_persist(new_hold, Value::Bool(false));
            }
        }

        cloned_element
    });

    Some((new_data_item, new_element))
}

/// Result of instantiating a template with fresh IDs.
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
) -> InstantiatedItem {
    let mut cell_id_map: HashMap<String, String> = HashMap::new();
    let mut link_id_map: HashMap<String, String> = HashMap::new();

    // Clone data template with fresh IDs
    let data = clone_template_with_fresh_ids(&template.data_template, &mut cell_id_map, &mut link_id_map);

    // Clone element template if present
    let element = template.element_template.as_ref().map(|elem| {
        // Find hover link/hold before cloning (needed for HoverState action)
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
            find_hover_while_cell_id(fields.get("items"))
        } else {
            None
        };

        // Pre-populate hover hold mapping BEFORE cloning
        // The hover WhileRef uses cell_id like "hover_link_22", which is a synthetic hold
        // We need to map it to "hover_{new_link_id}" before clone processes the WhileRef
        if let (Some(old_link), Some(old_hold)) = (&hover_link_old_id, &hover_cell_old_id) {
            // First ensure the link gets a new ID
            // Phase 5: Use thread-local allocator for deterministic IDs
            let new_link_id = link_id_map
                .entry(old_link.clone())
                .or_insert_with(allocate_dynamic_link_id)
                .clone();
            // Then create the hover hold mapping: hover_link_22 → hover_dynamic_link_1000
            let new_hover_cell = format!("hover_{}", new_link_id);
            cell_id_map.insert(old_hold.clone(), new_hover_cell.clone());
            zoon::println!("[DD Instantiate] Pre-mapped hover hold: {} -> {}", old_hold, new_hover_cell);
        }

        // Pass cloned data as context to resolve PlaceholderFields (e.g., double_click LinkRefs)
        let cloned = clone_template_with_fresh_ids_with_context(elem, &mut cell_id_map, &mut link_id_map, &data);

        // Register HoverState if found
        if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_cell_old_id) {
            if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), cell_id_map.get(&old_hold)) {
                use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                add_dynamic_link_action(new_link.clone(), DynamicLinkAction::HoverState(new_hold.clone()));
                update_cell_no_persist(new_hold, Value::Bool(false));
            }
        }

        cloned
    });

    // Extract identity link ID
    let identity_link_id = get_link_ref_at_path(&template.data_template, &template.identity.link_ref_path)
        .and_then(|old_id| link_id_map.get(&old_id).cloned());

    // Initialize HOLDs with values from initial_values or field_initializers
    for (old_id, new_id) in &cell_id_map {
        // Check if this hold corresponds to a persisted field
        let field_value = template.persisted_fields.iter()
            .find(|(_, path)| {
                get_hold_ref_at_path(&template.data_template, path)
                    .map(|id| id == *old_id)
                    .unwrap_or(false)
            })
            .and_then(|(name, _)| initial_values.get(name).cloned());

        // Or check field_initializers
        let initializer_value = template.field_initializers.iter()
            .find(|(path, _)| {
                get_hold_ref_at_path(&template.data_template, path)
                    .map(|id| id == *old_id)
                    .unwrap_or(false)
            })
            .and_then(|(_, init)| match init {
                FieldInitializer::FromEventText => event_text.map(|t| Value::text(t)),
                FieldInitializer::Constant(v) => Some(v.clone()),
            });

        let value = field_value.or(initializer_value).unwrap_or(Value::Unit);
        update_cell_no_persist(new_id, value);
    }

    // Register link actions
    for action_config in &template.link_actions {
        let old_link_id = get_link_ref_at_path(&template.data_template, &action_config.link_path);
        if let Some(old_id) = old_link_id {
            if let Some(new_link_id) = link_id_map.get(&old_id) {
                register_link_action(
                    new_link_id,
                    &action_config.action,
                    &template.data_template,
                    &cell_id_map,
                    &link_id_map,
                );
            }
        }
    }

    InstantiatedItem {
        data,
        element,
        cell_id_map,
        link_id_map,
        identity_link_id,
    }
}

/// Register a link action based on the action spec.
fn register_link_action(
    new_link_id: &str,
    action: &LinkActionSpec,
    data_template: &Value,
    cell_id_map: &HashMap<String, String>,
    link_id_map: &HashMap<String, String>,
) {
    use super::super::io::{add_dynamic_link_action, DynamicLinkAction};

    match action {
        LinkActionSpec::SetHoldTrue { hold_path } => {
            if let Some(old_cell_id) = get_hold_ref_at_path(data_template, hold_path) {
                if let Some(new_cell_id) = cell_id_map.get(&old_cell_id) {
                    add_dynamic_link_action(new_link_id.to_string(), DynamicLinkAction::SetTrue(new_cell_id.clone()));
                }
            }
        }
        LinkActionSpec::SetHoldFalse { hold_path } => {
            if let Some(old_cell_id) = get_hold_ref_at_path(data_template, hold_path) {
                if let Some(new_cell_id) = cell_id_map.get(&old_cell_id) {
                    add_dynamic_link_action(new_link_id.to_string(), DynamicLinkAction::SetFalse(new_cell_id.clone()));
                }
            }
        }
        LinkActionSpec::EditingHandler { editing_cell_path, title_cell_path } => {
            let editing_old = get_hold_ref_at_path(data_template, editing_cell_path);
            let title_old = get_hold_ref_at_path(data_template, title_cell_path);
            if let (Some(edit_old), Some(title_old)) = (editing_old, title_old) {
                if let (Some(edit_new), Some(title_new)) = (cell_id_map.get(&edit_old), cell_id_map.get(&title_old)) {
                    add_dynamic_link_action(new_link_id.to_string(), DynamicLinkAction::EditingHandler {
                        editing_cell: edit_new.clone(),
                        title_cell: title_new.clone(),
                    });
                }
            }
        }
        LinkActionSpec::HoverState { hold_path } => {
            if let Some(old_cell_id) = get_hold_ref_at_path(data_template, hold_path) {
                if let Some(new_cell_id) = cell_id_map.get(&old_cell_id) {
                    add_dynamic_link_action(new_link_id.to_string(), DynamicLinkAction::HoverState(new_cell_id.clone()));
                }
            }
        }
        LinkActionSpec::RemoveItem => {
            add_dynamic_link_action(new_link_id.to_string(), DynamicLinkAction::RemoveListItem {
                link_id: new_link_id.to_string(),
            });
        }
    }
}

/// Handle to a running DD worker.
///
/// Use this to inject events. The worker writes directly to CELL_STATES (Phase 6).
pub struct WorkerHandle {
    /// Input channel for injecting events
    event_input: Input<Event>,
    /// Task handle to keep the async event loop alive
    _task_handle: zoon::TaskHandle,
}

impl WorkerHandle {
    /// Get the event input for injecting events.
    pub fn event_input(&self) -> &Input<Event> {
        &self.event_input
    }

    /// Split the handle into (event_input, task_handle).
    ///
    /// Phase 6: Output channel removed - worker writes directly to CELL_STATES.
    pub fn split(self) -> (Input<Event>, zoon::TaskHandle) {
        (self.event_input, self._task_handle)
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Phase 10: PURE DD OUTPUT TYPE
// ══════════════════════════════════════════════════════════════════════════════
//
// TaggedCellOutput flows through DD operators and is captured at the end using
// timely's built-in capture() mechanism. This avoids any shared mutable state
// (Mutex/RefCell) inside the dataflow - pure DD!
//
// See docs/plans/pure_dd.md Phase 10 for details.
// ══════════════════════════════════════════════════════════════════════════════

/// A tagged cell output that flows through DD and is captured via timely::Capture.
/// This replaces the inspect() + Mutex pattern with pure DD message passing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaggedCellOutput {
    /// The cell id this output belongs to
    pub cell_id: Arc<str>,
    /// The new state value
    pub state: Value,
    /// Whether this cell should persist to localStorage
    pub should_persist: bool,
    /// Logical time of this update
    pub time: u64,
}

impl TaggedCellOutput {
    pub fn new(cell_id: impl Into<Arc<str>>, state: Value, should_persist: bool, time: u64) -> Self {
        Self {
            cell_id: cell_id.into(),
            state,
            should_persist,
            time,
        }
    }
}

/// A document update from the DD worker.
#[derive(Clone, Debug)]
pub struct DocumentUpdate {
    /// The new document value as Value
    pub document: Value,
    /// Logical time of this update
    pub time: u64,
    /// HOLD state updates that SHOULD persist (cell_id -> new_value)
    pub hold_updates: HashMap<String, Value>,
    /// HOLD state updates for UI only, NOT persisted (cell_id -> new_value)
    /// Used for timer-driven HOLDs where persistence doesn't make sense.
    pub hold_state_updates: HashMap<String, Value>,
}

// Note: EventFilter is now defined in types.rs and re-exported via mod.rs
// Phase 3.3: Consolidated with DdEventFilter

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
    /// Set boolean to true (for editing mode on double-click)
    SetTrue,
    /// Set boolean to false (for exiting editing mode on Enter/Escape/blur)
    SetFalse,
    /// Append event text to list (creates list item objects with title/completed)
    /// DEPRECATED: Use ListAppendWithTemplate for proper Element AST items
    ListAppend,
    /// Append item using template Values with fresh CellRef/LinkRef IDs.
    /// The templates are pre-evaluated structures (e.g., from new_list_item() and list_item()).
    /// At append time, both templates are cloned with the SAME ID mapping, so the element
    /// references the cloned data's HOLDs correctly.
    /// Fields:
    /// - data_template: The data object template (stored in "list_data" HOLD)
    /// - element_template: The Element AST template (stored in "list_elements" HOLD)
    /// - title_cell_field: Path to the title CellRef field (e.g., "title")
    ListAppendWithTemplate {
        data_template: Value,
        element_template: Option<Value>,
        title_cell_field: String,
    },
    /// List with both append and clear - handles "Enter:text" for append, Unit from clear_link for clear
    /// The String parameter is the clear button's link_id - only Unit events from this link will clear.
    ListAppendWithClear(String),
    /// Combined template + clear: ListAppendWithTemplate functionality with clear button support.
    /// Handles both "Enter:text" events (append from template) and Unit from clear_link_id (clear).
    ListAppendWithTemplateAndClear {
        data_template: Value,
        element_template: Option<Value>,
        title_cell_field: String,
        clear_link_id: String,
    },
    /// Clear to empty text (for text input clearing after submit)
    ClearText,
    /// Remove all items where the specified boolean field is true (for Clear completed button)
    /// The completed_field parameter specifies which field to check (e.g., "completed", "done", "checked")
    /// NO HARDCODED FIELD NAMES: This must be explicitly configured, not inferred
    ListRemoveCompleted {
        /// The field name to check for completion (e.g., "completed")
        completed_field: String,
    },
    /// Remove a specific list item by LinkRef identity (event format: "remove:LINK_ID")
    RemoveListItem,

    // ========================================================================
    // GENERIC LIST OPERATIONS WITH STABLE IDENTITY
    // ========================================================================
    // These transforms use LinkRef IDs for stable identity instead of array indices.
    // When items are removed/reordered, LinkRef IDs remain constant.

    /// Generic: Set a field on a list item identified by LinkRef.
    /// Event format: "action:LINK_ID" or "action:LINK_ID:value"
    /// - identity_path: path to the LinkRef that identifies the item (e.g., ["item_elements", "remove_item_button"])
    /// - field_path: path to the field to update (e.g., ["editing"])
    /// - update: how to compute the new value
    ListItemSetFieldByIdentity {
        /// Path to the identifying LinkRef within each list item
        identity_path: FieldPath,
        /// Path to the field to update
        field_path: FieldPath,
        /// How to update the field
        update: FieldUpdate,
    },

    /// Generic: Remove a list item identified by LinkRef.
    /// Event format: "remove:LINK_ID"
    /// - identity_path: path to the LinkRef that identifies the item
    /// - elements_hold: optional parallel elements list to also update
    ListItemRemoveByIdentity {
        /// Path to the identifying LinkRef within each list item
        identity_path: FieldPath,
        /// Optional: also remove from this parallel elements HOLD
        elements_hold: Option<String>,
    },

    /// Generic: Append item using ListItemTemplate with full configuration.
    /// Replaces ListAppendWithTemplate with more flexibility.
    ListAppendFromTemplate {
        /// The template to instantiate
        template: ListItemTemplate,
        /// Optional: also append to this parallel elements HOLD
        elements_hold: Option<String>,
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
    /// Set from event text after prefix (e.g., "save:LINK_ID:value" → "value")
    FromEventTextAfterIdentity,
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

// ══════════════════════════════════════════════════════════════════════════════
// Phase 4: Collection Operation Configuration
//
// These types configure DD collection operators (filter, map, count) that replace
// the surgically removed symbolic reference variants (FilteredListRef, MappedListRef, etc.).
//
// Instead of storing "deferred evaluation recipes" that the bridge evaluates
// imperatively, the evaluator now registers collection operations here.
// The worker builds actual DD dataflow operators from this config.
// ══════════════════════════════════════════════════════════════════════════════

use super::value::CollectionId;

/// Type of collection operation.
#[derive(Clone, Debug)]
pub enum CollectionOp {
    /// Filter items by predicate.
    /// Replaces: FilteredListRef, FilteredListRefWithPredicate, ReactiveFilteredList
    Filter {
        /// For simple field equality: Some((field_name, expected_value))
        /// For complex predicates: None (use predicate_template)
        field_filter: Option<(Arc<str>, Value)>,
        /// For complex predicates: evaluated template with Placeholder
        predicate_template: Option<Value>,
    },
    /// Map/transform items.
    /// Replaces: MappedListRef, FilteredMappedListRef
    Map {
        /// Element template with Placeholder for item substitution
        element_template: Value,
    },
    /// Count items.
    /// Replaces: ComputedRef::ListCount
    Count,
    /// Count items matching filter.
    /// Replaces: ComputedRef::ListCountWhere, ComputedRef::ListCountWhereHold
    CountWhere {
        /// Field to filter on
        filter_field: Arc<str>,
        /// Value to match
        filter_value: Value,
    },
    /// Check if list is empty.
    IsEmpty,
    /// Concatenate two collections.
    Concat {
        /// Second collection to concatenate
        other_source: CollectionId,
    },
    // ══════════════════════════════════════════════════════════════════════════
    // Phase 4: Arithmetic/Comparison Operations
    // Replaces: ComputedRef::Subtract, ComputedRef::GreaterThanZero, ComputedRef::Equal
    // These operate on Count/CountWhere outputs to produce derived values.
    // ══════════════════════════════════════════════════════════════════════════
    /// Subtract one count from another (produces Number).
    /// Used for: active_list_count = list_count - completed_list_count
    Subtract {
        /// Second operand (subtracted from source)
        right_source: CollectionId,
    },
    /// Check if a count is greater than zero (produces Bool).
    /// Used for: show_clear_completed = completed_list_count > 0
    GreaterThanZero,
    /// Compare two values for equality (produces Bool).
    /// Used for: all_completed = completed_list_count == list_count
    Equal {
        /// Second operand to compare against source
        right_source: CollectionId,
    },
}

/// Configuration for a collection operation in the DD dataflow.
///
/// Each operation takes one (or more) input collections and produces
/// an output collection. The worker builds a DAG of DD operators from these.
#[derive(Clone, Debug)]
pub struct CollectionOpConfig {
    /// Unique ID for the output collection
    pub output_id: CollectionId,
    /// Source collection ID (input to this operation)
    pub source_id: CollectionId,
    /// The operation to perform
    pub op: CollectionOp,
}

/// Configuration for the entire DD dataflow.
/// Editing event bindings parsed from HOLD body.
/// Contains paths to LinkRefs that control editing state.
#[derive(Clone, Debug, Default)]
pub struct EditingBinding {
    /// The Cell ID that these bindings control (e.g., "cell_5")
    pub cell_id: Option<String>,
    /// Path to LinkRef whose double_click triggers edit mode
    pub edit_trigger_path: Vec<String>,
    /// Actual LinkRef ID for edit trigger (resolved during evaluation)
    pub edit_trigger_link_id: Option<String>,
    /// Path to LinkRef whose key_down exits edit mode on Enter/Escape
    pub exit_key_path: Vec<String>,
    /// Actual LinkRef ID for exit key (resolved during evaluation)
    pub exit_key_link_id: Option<String>,
    /// Path to LinkRef whose blur exits edit mode
    pub exit_blur_path: Vec<String>,
    /// Actual LinkRef ID for exit blur (resolved during evaluation)
    pub exit_blur_link_id: Option<String>,
}

/// Toggle event binding parsed from HOLD body.
/// Contains the path to a LinkRef whose click event toggles a boolean HOLD.
#[derive(Clone, Debug)]
pub struct ToggleBinding {
    /// The Cell ID that this toggle affects
    pub cell_id: String,
    /// Path to LinkRef whose click triggers toggle (e.g., ["todo_elements", "todo_checkbox"])
    pub event_path: Vec<String>,
    /// Event type (usually "click")
    pub event_type: String,
    /// Actual LinkRef ID if available (resolved during evaluation)
    /// When present, this takes precedence over event_path resolution
    pub link_id: Option<String>,
}

/// Global toggle event binding for "toggle all" patterns.
/// Contains the path to a LinkRef whose click toggles ALL items in a list.
/// Pattern: `store.elements.toggle_all.event.click |> THEN { store.all_completed |> Bool/not() }`
#[derive(Clone, Debug)]
pub struct GlobalToggleBinding {
    /// The Cell ID that this toggle affects (the list cell like "todos")
    pub list_cell_id: String,
    /// Path to LinkRef whose click triggers toggle (e.g., ["store", "elements", "toggle_all_checkbox"])
    pub event_path: Vec<String>,
    /// Event type (usually "click")
    pub event_type: String,
    /// Path to the global computed value (e.g., ["store", "all_completed"])
    pub value_path: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DataflowConfig {
    /// HOLD operators to create
    pub cells: Vec<CellConfig>,
    /// Link-to-cell mappings for DD-native LINK handling (Phase 8).
    /// These replace DYNAMIC_LINK_ACTIONS HashMap with DD joins.
    pub link_mappings: Vec<super::types::LinkCellMapping>,
    /// Collection operations (Phase 4).
    /// Replaces symbolic reference variants with DD dataflow operators.
    pub collection_ops: Vec<CollectionOpConfig>,
    /// Initial collections (list literals evaluated at startup)
    pub initial_collections: HashMap<CollectionId, Vec<Value>>,

    // ========================================================================
    // Phase 7.3: Registry fields moved to DataflowConfig
    // These replace the thread_local registries in outputs.rs
    // ========================================================================

    /// Checkbox toggle field names for reactive "items left" count.
    /// Replaces CHECKBOX_TOGGLE_HOLDS registry.
    pub toggle_fields: Vec<String>,

    /// Cell IDs that should be cleared on text submit (Enter key).
    /// Replaces TEXT_CLEAR_HOLDS registry.
    pub text_clear_cells: HashSet<String>,

    /// Remove event path: path from item to LinkRef that triggers removal.
    /// Parsed from List/remove(item, on: item.X.Y.event.press) → ["X", "Y"]
    /// Replaces REMOVE_EVENT_PATH registry.
    pub remove_event_path: Vec<String>,

    /// Bulk remove event path: path to global LinkRef that triggers bulk removal.
    /// Parsed from List/remove(item, on: elements.X.event.press |> THEN {...})
    /// Replaces BULK_REMOVE_EVENT_PATH registry.
    pub bulk_remove_event_path: Vec<String>,

    /// Editing event bindings for edit mode control.
    /// Replaces EDITING_EVENT_BINDINGS registry.
    pub editing_bindings: Vec<EditingBinding>,

    /// Toggle event bindings for checkbox-like toggling.
    /// Replaces TOGGLE_EVENT_BINDINGS registry.
    pub toggle_bindings: Vec<ToggleBinding>,

    /// Global toggle bindings for "toggle all" patterns.
    /// Replaces GLOBAL_TOGGLE_BINDINGS registry.
    pub global_toggle_bindings: Vec<GlobalToggleBinding>,
}

/// Template info for pre-instantiation (O(delta) optimization).
/// Moved outside impl block to satisfy Rust's grammar rules.
#[derive(Clone)]
struct TemplateInfo {
    data_template: Value,
    element_template: Option<Value>,
    title_cell_field: String,
    cell_id: String,
    filter: EventFilter,
}

impl DataflowConfig {
    /// Create a new empty dataflow config.
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            link_mappings: Vec::new(),
            collection_ops: Vec::new(),
            initial_collections: HashMap::new(),
            // Phase 7.3: Registry fields
            toggle_fields: Vec::new(),
            text_clear_cells: HashSet::new(),
            remove_event_path: Vec::new(),
            bulk_remove_event_path: Vec::new(),
            editing_bindings: Vec::new(),
            toggle_bindings: Vec::new(),
            global_toggle_bindings: Vec::new(),
        }
    }

    // ========================================================================
    // Phase 7.3: Registry field setters
    // These replace the set_* functions in outputs.rs
    // ========================================================================

    /// Add a toggle field name (for reactive "items left" count).
    pub fn add_toggle_field(&mut self, field: impl Into<String>) {
        self.toggle_fields.push(field.into());
    }

    /// Add a text clear cell (cleared on Enter key).
    pub fn add_text_clear_cell(&mut self, cell_id: impl Into<String>) {
        self.text_clear_cells.insert(cell_id.into());
    }

    /// Set the remove event path.
    pub fn set_remove_event_path(&mut self, path: Vec<String>) {
        self.remove_event_path = path;
    }

    /// Set the bulk remove event path.
    pub fn set_bulk_remove_event_path(&mut self, path: Vec<String>) {
        self.bulk_remove_event_path = path;
    }

    /// Set the editing bindings.
    pub fn set_editing_bindings(&mut self, binding: EditingBinding) {
        zoon::println!("[DD Config] Setting editing bindings: {:?}", binding);
        self.editing_bindings.push(binding);
    }

    /// Add a toggle binding.
    pub fn add_toggle_binding(&mut self, binding: ToggleBinding) {
        zoon::println!("[DD Config] Adding toggle binding: {:?}", binding);
        self.toggle_bindings.push(binding);
    }

    /// Add a global toggle binding.
    pub fn add_global_toggle_binding(&mut self, binding: GlobalToggleBinding) {
        zoon::println!("[DD Config] Adding global toggle binding: {:?}", binding);
        self.global_toggle_bindings.push(binding);
    }

    /// Add a link-to-cell mapping (Phase 8).
    /// This replaces add_dynamic_link_action for DD-native handling.
    pub fn add_link_mapping(&mut self, mapping: super::types::LinkCellMapping) {
        self.link_mappings.push(mapping);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Phase 4: Collection operation builders
    // ══════════════════════════════════════════════════════════════════════════

    /// Register an initial collection (list literal).
    /// Returns the CollectionId for referencing in operations.
    pub fn add_initial_collection(&mut self, items: Vec<Value>) -> CollectionId {
        let id = CollectionId::new();
        self.initial_collections.insert(id.clone(), items);
        id
    }

    /// Add a filter operation.
    /// Replaces: FilteredListRef, FilteredListRefWithPredicate
    pub fn add_filter(
        &mut self,
        source_id: CollectionId,
        field_filter: Option<(Arc<str>, Value)>,
        predicate_template: Option<Value>,
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
    pub fn add_map(&mut self, source_id: CollectionId, element_template: Value) -> CollectionId {
        let output_id = CollectionId::new();
        self.collection_ops.push(CollectionOpConfig {
            output_id: output_id.clone(),
            source_id,
            op: CollectionOp::Map { element_template },
        });
        output_id
    }

    /// Add a count operation.
    /// Replaces: ComputedRef::ListCount
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
    /// Replaces: ComputedRef::ListCountWhere
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

    // ══════════════════════════════════════════════════════════════════════════
    // Phase 4: Arithmetic/Comparison Operation Helpers
    // Replaces: ComputedRef::Subtract, ComputedRef::GreaterThanZero, ComputedRef::Equal
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
    /// Only triggers when event value matches the specified text (e.g., "Enter").
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

    /// Add a list that appends items on Enter key.
    ///
    /// Used for shopping_list style patterns: key_down |> WHEN { Enter => append }
    /// Events are "Enter:text" format - text after colon is appended.
    /// Also adds a text-clear HOLD that clears when items are added.
    /// Task 7.1: text_clear_cell_id is now passed from interpreter (derived from link ID).
    pub fn add_list_append_on_enter(
        mut self,
        id: impl Into<String>,
        initial: Value,
        key_link_id: &str,
        text_clear_cell_id: &str,
    ) -> Self {
        // HOLD for the list items
        self.cells.push(CellConfig {
            id: CellId::new(id),
            initial,
            triggered_by: vec![LinkId::new(key_link_id)],
            timer_interval_ms: 0,
            filter: EventFilter::TextStartsWith("Enter:".to_string()),
            transform: StateTransform::ListAppend,
            persist: true,
        });
        // HOLD for text input clearing - same trigger, clears to empty on successful append
        self.cells.push(CellConfig {
            id: CellId::new(text_clear_cell_id),
            initial: Value::text(""),
            triggered_by: vec![LinkId::new(key_link_id)],
            timer_interval_ms: 0,
            filter: EventFilter::TextStartsWith("Enter:".to_string()),
            transform: StateTransform::ClearText,
            persist: false, // UI-only, not persisted
        });
        // Phase 7.3: Register in text_clear_cells set
        self.text_clear_cells.insert(text_clear_cell_id.to_string());
        self
    }

    /// Add a list with both append (Enter key) and clear (button press) operations.
    ///
    /// Used for shopping_list patterns: List/append() |> List/clear(on: button.press)
    /// Events:
    /// - "Enter:text" format from key_link_id → append text to list
    /// - Unit event from clear_link_id → clear list to empty
    /// Also adds a text-clear HOLD that clears when items are added.
    /// Task 7.1: text_clear_cell_id is now passed from interpreter (derived from link ID).
    pub fn add_list_append_with_clear(
        mut self,
        id: impl Into<String>,
        initial: Value,
        key_link_id: &str,
        clear_link_id: &str,
        text_clear_cell_id: &str,
    ) -> Self {
        let id_str = id.into();
        // HOLD for the list items - triggered by both Enter key AND clear button
        self.cells.push(CellConfig {
            id: CellId::new(&id_str),
            initial,
            triggered_by: vec![LinkId::new(key_link_id), LinkId::new(clear_link_id)],
            timer_interval_ms: 0,
            filter: EventFilter::Any, // Accept both Enter: and Unit events
            transform: StateTransform::ListAppendWithClear(clear_link_id.to_string()),
            persist: true,
        });
        // HOLD for text input clearing - only on Enter key (not clear button)
        self.cells.push(CellConfig {
            id: CellId::new(text_clear_cell_id),
            initial: Value::text(""),
            triggered_by: vec![LinkId::new(key_link_id)],
            timer_interval_ms: 0,
            filter: EventFilter::TextStartsWith("Enter:".to_string()),
            transform: StateTransform::ClearText,
            persist: false, // UI-only, not persisted
        });
        // Phase 7.3: Register in text_clear_cells set
        self.text_clear_cells.insert(text_clear_cell_id.to_string());
        self
    }

    /// Create a simple counter configuration.
    ///
    /// Creates a HOLD that counts link events.
    pub fn counter(link_id: &str) -> Self {
        Self::counter_with_initial(link_id, Value::int(0))
    }

    /// Create a counter configuration with a specific initial value.
    ///
    /// Used when restoring persisted state - the counter starts from
    /// the persisted value instead of 0.
    pub fn counter_with_initial(link_id: &str, initial: Value) -> Self {
        Self::new().add_hold("counter", initial, vec![link_id])
    }

    /// Create a counter configuration with specific HOLD ID and initial value.
    ///
    /// Allows specifying the HOLD ID to match what the evaluator generates.
    pub fn counter_with_initial_hold(link_id: &str, cell_id: &str, initial: Value) -> Self {
        Self::new().add_hold(cell_id, initial, vec![link_id])
    }

    /// Create a timer-driven counter configuration.
    ///
    /// Creates a HOLD that increments every interval_ms milliseconds.
    pub fn timer_counter(cell_id: &str, initial: Value, interval_ms: u64) -> Self {
        Self::new().add_timer_hold(cell_id, initial, interval_ms)
    }
}

/// DD Worker that processes events through Differential Dataflow.
///
/// This worker runs in an async context via `spawn_local` and processes
/// events synchronously in batches using `timely::execute_directly`.
pub struct Worker {
    /// Configuration for the dataflow
    config: DataflowConfig,
    // Phase 5: ID counters moved to thread-locals (allocate_dynamic_cell_id/link_id)
    // Worker calls reset_id_counters() on creation for deterministic sessions.
}

impl Worker {
    /// Create a new DD worker with default configuration.
    ///
    /// Phase 5: Resets thread-local ID counters for deterministic ID generation.
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
    /// Phase 5: Resets thread-local ID counters for deterministic ID generation.
    /// Phase 7.3: Sets active config for outputs.rs consumers to read.
    pub fn with_config(config: DataflowConfig) -> Self {
        // Reset thread-local counters for deterministic replays
        reset_id_counters();
        // Set active config for outputs.rs to read
        set_active_config(config.clone());
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
        // PHASE 8: DD-NATIVE LINK HANDLING
        // Convert DYNAMIC_LINK_ACTIONS to link_mappings before processing
        // ═══════════════════════════════════════════════════════════════════════
        let link_mappings = super::super::io::get_all_link_mappings();
        if !link_mappings.is_empty() {
            zoon::println!("[Worker Phase 8] Loaded {} link mappings from DYNAMIC_LINK_ACTIONS", link_mappings.len());
            self.config.link_mappings = link_mappings;
        }

        // ═══════════════════════════════════════════════════════════════════════
        // PHASE 6: SYNCHRONOUS INITIALIZATION
        // Initialize CELL_STATES before returning, so bridge can render immediately
        // ═══════════════════════════════════════════════════════════════════════
        let mut initial_cell_states: HashMap<String, Value> = HashMap::new();

        for cell_config in &self.config.cells {
            let cell_id = cell_config.id.name();
            let initial_value = if cell_config.persist {
                // Try to load persisted value, fall back to config default
                load_persisted_cell_value(&cell_id).unwrap_or_else(|| cell_config.initial.clone())
            } else {
                cell_config.initial.clone()
            };

            // Write to global CELL_STATES synchronously
            if cell_config.persist {
                sync_cell_from_dd_with_persist(&cell_id, initial_value.clone());
            } else {
                sync_cell_from_dd(&cell_id, initial_value.clone());
            }

            // Also build local copy for DD processing
            initial_cell_states.insert(cell_id.to_string(), initial_value);
        }

        if LOG_DD_DEBUG {
            zoon::println!("[Worker Phase 6] Initialized {} cells synchronously", self.config.cells.len());
        }

        // Create event channel for injecting events
        let (event_tx, event_rx) = mpsc::unbounded();

        // Spawn the async event loop (initialization already done)
        let task_handle = Task::start_droppable(Self::event_loop(self.config, event_rx, initial_cell_states));

        WorkerHandle {
            event_input: Input::new(event_tx),
            _task_handle: task_handle,
        }
    }

    /// The main event loop that processes DD events.
    ///
    /// # Phase 6: Single State Authority
    /// - Initialization is done in spawn() before this runs
    /// - This loop handles runtime event processing only
    async fn event_loop(
        config: DataflowConfig,
        mut event_rx: mpsc::UnboundedReceiver<Event>,
        initial_cell_states: HashMap<String, Value>,
    ) {
        let mut current_time: u64 = 0;
        let mut cell_states = initial_cell_states;

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
                        zoon::println!("[Worker] Channel closed, exiting event loop");
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

            zoon::println!("[Worker] Processing {} events", events.len());

            // Process events through DD
            let (outputs, new_time, new_states) = if USE_PERSISTENT_WORKER {
                Self::process_with_persistent_worker(&config, &events, current_time, &cell_states)
            } else {
                Self::process_batch_with_hold(&config, &events, current_time, &cell_states)
            };

            current_time = new_time;
            cell_states = new_states;

            zoon::println!("[Worker] Produced {} outputs", outputs.len());

            // ═══════════════════════════════════════════════════════════════════
            // PHASE 6: Write directly to CELL_STATES instead of via channel
            // ═══════════════════════════════════════════════════════════════════
            for output in outputs {
                // Write hold_updates (with persistence)
                for (cell_id, value) in output.hold_updates {
                    sync_cell_from_dd_with_persist(&cell_id, value);
                }
                // Write hold_state_updates (no persistence)
                for (cell_id, value) in output.hold_state_updates {
                    sync_cell_from_dd(&cell_id, value);
                }
            }
        }
    }

    /// Execute a dataflow directly without spawning threads.
    ///
    /// This is a WASM-compatible version of `timely::execute_directly` that
    /// passes `None` for the timestamp to avoid `std::time::Instant::now()`.
    fn execute_directly_wasm<T, F>(func: F) -> T
    where
        F: FnOnce(&mut timely::worker::Worker<timely::communication::allocator::thread::Thread>) -> T,
    {
        let alloc = timely::communication::allocator::thread::Thread::default();
        // Pass None for timestamp to avoid std::time::Instant::now() on WASM
        let mut worker = timely::worker::Worker::new(
            timely::WorkerConfig::default(),
            alloc,
            None,  // No timestamp - WASM compatible!
        );
        let result = func(&mut worker);
        // Step the worker until all dataflows complete
        while worker.has_dataflows() {
            worker.step_or_park(None);
        }
        result
    }

    /// Process events through the persistent DD worker (Phase 5).
    ///
    /// Unlike `process_batch_with_hold`, this uses a SINGLE long-lived Timely worker
    /// that persists across all event batches. This gives true O(delta) complexity
    /// because DD arrangements are not rebuilt on each batch.
    ///
    /// Key optimization: Templates are pre-instantiated BEFORE DD injection.
    /// This moves side effects (ID generation, HOLD registration) outside DD,
    /// allowing DD to use pure transforms.
    fn process_with_persistent_worker(
        config: &DataflowConfig,
        events: &[Event],
        start_time: u64,
        initial_states: &HashMap<String, Value>,
    ) -> (Vec<DocumentUpdate>, u64, HashMap<String, Value>) {
        // Initialize or reinitialize persistent worker if config changed
        let dd_cells = Self::convert_config_to_dd_cells(config);
        let reinitialized = reinit_if_config_changed(dd_cells, initial_states.clone());
        if reinitialized {
            zoon::println!("[Worker] (Re)initialized persistent DD worker with {} cells", config.cells.len());
        }

        // Build template info map for O(delta) pre-instantiation
        let template_cells = Self::build_template_cell_map(config);

        let mut outputs = Vec::new();
        let mut new_states = initial_states.clone();
        let mut current_time = start_time;

        // Process each event through the persistent worker
        for event in events {
            let (link_id, event_value) = match event {
                Event::Link { id, value } => (id.clone(), value.clone()),
                Event::Timer { id, .. } => (LinkId::new(&format!("__timer_{}", id)), EventValue::Unit),
                Event::External { .. } => continue, // Skip external events for now
            };

            // O(delta) optimization: Pre-instantiate templates BEFORE DD injection.
            // This moves side effects (ID generation, HOLD registration) outside DD.
            let event_value = Self::maybe_pre_instantiate(&link_id, event_value, &template_cells);

            // Inject event and get outputs
            let dd_outputs = inject_event_persistent(&link_id, event_value.clone());
            current_time += 1;

            // Phase 8: Process link mappings (DD-native LINK handling)
            // This replaces the IO layer's check_dynamic_link_action
            for mapping in &config.link_mappings {
                if super::dataflow::mapping_matches_event(mapping, link_id.name(), &event_value) {
                    // Get current value of target cell
                    if let Some(current_value) = new_states.get(&mapping.cell_id.name()) {
                        // Apply the action
                        let new_value = super::dataflow::apply_link_action(
                            &mapping.action,
                            current_value,
                            &event_value,
                        );

                        // Only produce output if value changed
                        if new_value != *current_value {
                            // Update state
                            let cell_id = mapping.cell_id.name();
                            new_states.insert(cell_id.clone(), new_value.clone());

                            // Create DocumentUpdate for rendering
                            let mut hold_updates = HashMap::new();
                            hold_updates.insert(cell_id.clone(), new_value.clone());

                            outputs.push(DocumentUpdate {
                                document: new_value.clone(),
                                time: current_time,
                                hold_updates,
                                hold_state_updates: HashMap::new(),
                            });

                            zoon::println!("[Phase8] Link mapping applied: {} -> {} {:?}",
                                link_id.name(), mapping.cell_id.name(), mapping.action);
                        }
                    }
                }
            }

            // Convert DD outputs to DocumentUpdates and update state
            for dd_output in dd_outputs {
                let cell_id = dd_output.cell_id.name().to_string();
                new_states.insert(cell_id.clone(), dd_output.value.clone());

                // Create DocumentUpdate for rendering
                let mut hold_updates = HashMap::new();
                hold_updates.insert(cell_id.clone(), dd_output.value.clone());

                outputs.push(DocumentUpdate {
                    document: dd_output.value.clone(),
                    time: dd_output.time,
                    hold_updates,
                    hold_state_updates: HashMap::new(),
                });
            }
        }

        (outputs, current_time, new_states)
    }

    /// Convert DataflowConfig cells to DdCellConfig for the persistent worker.
    fn convert_config_to_dd_cells(config: &DataflowConfig) -> Vec<DdCellConfig> {
        config.cells.iter().map(|cell| {
            // Map StateTransform to DdTransform
            // Template-based transforms use ListAppendPrepared (pure, O(delta))
            // Pre-instantiation happens BEFORE DD injection in process_with_persistent_worker
            let transform = match &cell.transform {
                StateTransform::Increment => DdTransform::Increment,
                StateTransform::BoolToggle => DdTransform::Toggle,
                StateTransform::SetTrue => DdTransform::SetValue(Value::Bool(true)),
                StateTransform::SetFalse => DdTransform::SetValue(Value::Bool(false)),
                StateTransform::Identity => DdTransform::SetValue(Value::Unit),
                StateTransform::ListAppend => DdTransform::ListAppend,
                StateTransform::ClearText => DdTransform::SetValue(Value::text("")),
                StateTransform::ListAppendWithClear(_) => DdTransform::ListAppend,
                // Template-based transforms: pre-instantiation happens before DD, so use pure append
                StateTransform::ListAppendWithTemplate { .. } => DdTransform::ListAppendPrepared,
                StateTransform::ListAppendWithTemplateAndClear { .. } => DdTransform::ListAppendPrepared,
                StateTransform::ListAppendFromTemplate { .. } => DdTransform::ListAppendPrepared,
                StateTransform::RemoveListItem => DdTransform::Identity,
                // Phase 7: Use Identity for transforms handled in HOLD closure
                // The actual transform logic is in the HOLD closure (below), not here.
                _ => DdTransform::Identity,
            };

            // Build trigger list - start with explicit triggers
            let mut triggers = cell.triggered_by.clone();

            // Add timer trigger ID if this HOLD is timer-triggered
            if cell.timer_interval_ms > 0 {
                let timer_link_id = LinkId::new(&format!("__timer_{}", cell.timer_interval_ms));
                triggers.push(timer_link_id);
            }

            // Phase 3.3: EventFilter is now unified - no conversion needed
            DdCellConfig {
                id: cell.id.clone(),
                initial: cell.initial.clone(),
                triggers,
                transform,
                filter: cell.filter.clone(),
            }
        }).collect()
    }

    /// Build a map of link_id -> template info for cells that use template-based transforms.
    /// This enables O(delta) pre-instantiation before DD injection.
    fn build_template_cell_map(config: &DataflowConfig) -> HashMap<String, Vec<TemplateInfo>> {
        let mut map: HashMap<String, Vec<TemplateInfo>> = HashMap::new();

        for cell in &config.cells {
            let template_info = match &cell.transform {
                StateTransform::ListAppendWithTemplate { data_template, element_template, title_cell_field } => {
                    Some(TemplateInfo {
                        data_template: data_template.clone(),
                        element_template: element_template.clone(),
                        title_cell_field: title_cell_field.clone(),
                        cell_id: cell.id.name(),
                        filter: cell.filter.clone(),
                    })
                }
                StateTransform::ListAppendWithTemplateAndClear { data_template, element_template, title_cell_field, .. } => {
                    Some(TemplateInfo {
                        data_template: data_template.clone(),
                        element_template: element_template.clone(),
                        title_cell_field: title_cell_field.clone(),
                        cell_id: cell.id.name(),
                        filter: cell.filter.clone(),
                    })
                }
                StateTransform::ListAppendFromTemplate { template, .. } => {
                    // Extract title field from template initializers
                    // NO FALLBACKS: Field MUST be found in template, no silent "title" default
                    let title_field = template.field_initializers.iter()
                        .find(|(_, init)| matches!(init, FieldInitializer::FromEventText))
                        .and_then(|(path, _)| path.first().cloned());

                    match title_field {
                        Some(field) if !field.is_empty() => {
                            Some(TemplateInfo {
                                data_template: template.data_template.clone(),
                                element_template: template.element_template.clone(),
                                title_cell_field: field,
                                cell_id: cell.id.name(),
                                filter: cell.filter.clone(),
                            })
                        }
                        _ => {
                            zoon::println!("[DD Worker] WARNING: ListAppendFromTemplate has no FromEventText field. \
                                Skipping template info (no silent 'title' fallback).");
                            None
                        }
                    }
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
    /// Returns PreparedItem event value with the instantiated item.
    fn maybe_pre_instantiate(
        link_id: &LinkId,
        event_value: EventValue,
        template_cells: &HashMap<String, Vec<TemplateInfo>>,
    ) -> EventValue {
        // Only pre-instantiate for Text events (Enter:text format)
        let text = match &event_value {
            EventValue::Text(t) => t,
            _ => return event_value,
        };

        // Check if any templated cell is triggered by this link
        let Some(templates) = template_cells.get(link_id.name()) else {
            return event_value;
        };

        // Find first template whose filter matches
        for template in templates {
            let matches = match &template.filter {
                EventFilter::Any => true,
                EventFilter::TextEquals(pattern) => text == pattern,
                EventFilter::TextStartsWith(prefix) => text.starts_with(prefix),
            };

            if matches {
                // Extract item text from "Enter:text" format
                let Some(item_text) = EventPayload::parse_enter_text(text) else {
                    return event_value;
                };

                if LOG_DD_DEBUG {
                    zoon::println!("[Worker] Pre-instantiating template for cell {} with text '{}'", template.cell_id, item_text);
                }

                // Build initial values map with the title field set to item_text
                let mut initial_values: HashMap<String, Value> = HashMap::new();
                initial_values.insert(template.title_cell_field.clone(), Value::text(item_text));

                // Build ListItemTemplate from template info
                // TODO: identity.link_ref_path should come from template config, not be hardcoded!
                // Currently hardcoded to "remove_button" which assumes todo_mvc structure.
                // FIX NEEDED: Add identity_path to TemplateInfo and populate from evaluator
                let identity_path = vec!["remove_button".to_string()]; // HARDCODED - see TODO above
                zoon::println!("[DD Worker] WARNING: Using hardcoded identity path {:?} for template. \
                    This should be configured in the template, not hardcoded.", identity_path);

                let list_template = ListItemTemplate {
                    data_template: template.data_template.clone(),
                    element_template: template.element_template.clone(),
                    identity: ItemIdentitySpec {
                        link_ref_path: identity_path,
                    },
                    field_initializers: vec![(
                        vec![template.title_cell_field.clone()],
                        FieldInitializer::FromEventText,
                    )],
                    link_actions: Vec::new(),
                    persisted_fields: Vec::new(),
                };

                // Pre-instantiate with side effects (ID generation, HOLD registration)
                // This happens BEFORE DD injection, so DD stays pure
                let instantiated = instantiate_template(&list_template, &initial_values, Some(item_text));

                if LOG_DD_DEBUG {
                    zoon::println!("[Worker] Pre-instantiated item: {:?}", instantiated.data);
                }

                // Return PreparedItem containing the pre-instantiated data
                return EventValue::PreparedItem(instantiated.data);
            }
        }

        event_value
    }

    /// Process a batch of events through Differential Dataflow using HOLD operator.
    ///
    /// This uses the actual `hold()` function from dd_runtime for proper DD semantics.
    /// Uses WASM-compatible execute_directly that doesn't require std::time::Instant.
    ///
    /// # Phase 10: Pure DD Output Capture
    ///
    /// This function uses timely's built-in `capture()` mechanism instead of
    /// `inspect()` with Mutex. All outputs flow through DD operators as pure
    /// data and are captured at the end using message passing.
    ///
    /// See docs/plans/pure_dd.md Phase 10 for details.
    fn process_batch_with_hold(
        config: &DataflowConfig,
        events: &[Event],
        start_time: u64,
        initial_states: &HashMap<String, Value>,
    ) -> (Vec<DocumentUpdate>, u64, HashMap<String, Value>) {
        use super::operators::hold;
        use differential_dataflow::input::Input;
        use timely::dataflow::operators::{Capture, Concatenate};
        use timely::dataflow::operators::capture::Extract;

        // ══════════════════════════════════════════════════════════════════════════════
        // Phase 10: PURE DD OUTPUT CAPTURE
        // ══════════════════════════════════════════════════════════════════════════════
        //
        // NO Mutex, NO RefCell - outputs flow through DD operators:
        // 1. Each HOLD output is tagged with (cell_id, state, should_persist, time)
        // 2. All outputs are concatenated into a single collection
        // 3. The collection's inner stream is captured via timely::Capture
        // 4. After stepping, we extract() to get all outputs
        //
        // This is pure DD - no shared mutable state anywhere in the dataflow.
        // ══════════════════════════════════════════════════════════════════════════════

        // Clone data for the closure
        let events: Vec<Event> = events.to_vec();
        let initial_states_for_closure = initial_states.clone();
        let initial_states_for_merge = initial_states.clone();
        let config = config.clone();
        let num_events = events.len();

        // Phase 10: execute_directly_wasm returns the captured outputs receiver
        let outputs_rx = Self::execute_directly_wasm(move |worker| {
            // Build dataflow for each HOLD in config
            let (mut link_input, probe, outputs_rx) = worker.dataflow::<u64, _, _>(|scope| {
                // Create input collection for LINK events
                // Each link event is (LinkId, EventValue)
                let (link_input_handle, links) =
                    scope.new_collection::<(String, EventValue), isize>();

                // Phase 10: Collect all tagged output streams for pure DD capture
                use timely::dataflow::Stream;
                let mut tagged_outputs: Vec<Stream<_, (TaggedCellOutput, u64, isize)>> = Vec::new();

                // For each HOLD config, create a HOLD operator
                for hold_config in &config.cells {
                    let cell_id = hold_config.id.name().to_string();
                    let initial = initial_states_for_closure
                        .get(&cell_id)
                        .cloned()
                        .unwrap_or_else(|| hold_config.initial.clone());
                    let mut trigger_ids: Vec<String> = hold_config
                        .triggered_by
                        .iter()
                        .map(|l| l.name().to_string())
                        .collect();

                    // Add timer trigger ID if this HOLD is timer-triggered
                    if hold_config.timer_interval_ms > 0 {
                        let timer_id = format!("__timer_{}", hold_config.timer_interval_ms);
                        trigger_ids.push(timer_id);
                        if LOG_DD_DEBUG { zoon::println!("[DD Worker] HOLD {} listening for timer events", cell_id); }
                    }

                    // Clone filter for the closure
                    let event_filter = hold_config.filter.clone();

                    // Filter links to only those that trigger this HOLD
                    // AND match the event filter (WHEN pattern)
                    let triggered_events = links
                        .filter(move |(link_id, event_value)| {
                            // First check if link_id matches
                            if !trigger_ids.contains(link_id) {
                                return false;
                            }
                            // Then apply event value filter (WHEN pattern matching)
                            match &event_filter {
                                EventFilter::Any => true,
                                EventFilter::TextEquals(pattern) => {
                                    matches!(event_value, EventValue::Text(t) if t == pattern)
                                }
                                EventFilter::TextStartsWith(prefix) => {
                                    matches!(event_value, EventValue::Text(t) if t.starts_with(prefix))
                                }
                            }
                        });

                    // Clone transform and cell_id for the closure
                    let state_transform = hold_config.transform.clone();
                    let cell_id_for_transform = cell_id.clone();

                    // Apply HOLD operator with configured transform
                    let hold_output = hold(initial, &triggered_events, move |state, event| {
                        // Phase 2.1: cell_id_for_transform is available for ListDiff variants
                        let _ = &cell_id_for_transform; // Silence unused warning when not using ListDiff
                        match &state_transform {
                            StateTransform::Increment => {
                                // Increment if numeric
                                match state {
                                    Value::Number(n) => Value::float(n.0 + 1.0),
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::BoolToggle => {
                                // Toggle boolean value
                                // Handle both Bool and Tagged { tag: "True"/"False" } variants
                                match state {
                                    Value::Bool(b) => Value::Bool(!*b),
                                    Value::Tagged { tag, .. } if BoolTag::is_true(tag.as_ref()) => Value::Bool(false),
                                    Value::Tagged { tag, .. } if BoolTag::is_false(tag.as_ref()) => Value::Bool(true),
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::SetTrue => {
                                // Set to true (for entering editing mode on double-click)
                                Value::Bool(true)
                            }
                            StateTransform::SetFalse => {
                                // Set to false (for exiting editing mode on Enter/Escape/blur)
                                Value::Bool(false)
                            }
                            StateTransform::ListAppend => {
                                // Phase 2.1: Emit ListPush diff instead of cloning full list
                                // O(1) operation - no list clone needed
                                if state.is_list_like() {
                                    if let (_, EventValue::Text(text)) = event {
                                        // Only process Enter key events with non-empty text
                                        let Some(item_text) = EventPayload::parse_enter_text(text) else {
                                            return state.clone();
                                        };
                                        // Return ListPush diff - applied by sync_cell_from_dd()
                                        Value::list_push(cell_id_for_transform.clone(), Value::text(item_text))
                                    } else {
                                        state.clone()
                                    }
                                } else {
                                    state.clone()
                                }
                            }
                            StateTransform::ListAppendWithTemplate { data_template: _, element_template: _, title_cell_field: _ } => {
                                // Phase 2.1: Emit ListPush diff instead of cloning full list
                                // Template handling moved to pre-instantiation (Phase 1.2)
                                match event {
                                    (_, EventValue::PreparedItem(prepared_data)) => {
                                        // Pure DD: Emit ListPush diff - O(1) operation
                                        if !state.is_list_like() {
                                            return state.clone();
                                        }
                                        Value::list_push(cell_id_for_transform.clone(), prepared_data.clone())
                                    }
                                    (_, EventValue::Text(text)) => {
                                        // Text events should have been pre-instantiated.
                                        if LOG_DD_DEBUG {
                                            zoon::println!("[DD Worker] WARNING: ListAppendWithTemplate received Text event '{}' - should have been pre-instantiated", text);
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListAppendWithClear(clear_link_id) => {
                                // Phase 2.1: Emit ListPush/ListClear diffs - O(1) operations
                                match (state, event) {
                                    (state, (_, EventValue::Text(text))) if state.is_list_like() => {
                                        // Only process Enter key events with non-empty text
                                        let Some(item_text) = EventPayload::parse_enter_text(text) else {
                                            return state.clone();
                                        };
                                        Value::list_push(cell_id_for_transform.clone(), Value::text(item_text))
                                    }
                                    // Unit event from clear button → emit ListClear diff
                                    (state, (link_id, EventValue::Unit)) if state.is_list_like() && link_id == clear_link_id => {
                                        Value::list_clear(cell_id_for_transform.clone())
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListAppendWithTemplateAndClear { data_template: _, element_template: _, title_cell_field: _, clear_link_id } => {
                                // Phase 2.1: Emit ListPush/ListClear diffs - O(1) operations
                                match event {
                                    // Clear button: emit MultiCellUpdate to clear both lists atomically
                                    (link_id, EventValue::Unit) if state.is_list_like() && link_id == &clear_link_id => {
                                        // Phase 2: Return MultiCellUpdate instead of side effect
                                        Value::multi_cell_update(vec![
                                            ("list_elements", Value::list_clear("list_elements")),
                                            (cell_id_for_transform.as_str(), Value::list_clear(cell_id_for_transform.clone())),
                                        ])
                                    }
                                    // PreparedItem: Emit ListPush diff - O(1) operation
                                    (_, EventValue::PreparedItem(prepared_data)) => {
                                        if !state.is_list_like() {
                                            return state.clone();
                                        }
                                        Value::list_push(cell_id_for_transform.clone(), prepared_data.clone())
                                    }
                                    // Text events should have been pre-instantiated
                                    (_, EventValue::Text(text)) => {
                                        if LOG_DD_DEBUG {
                                            zoon::println!("[DD Worker] WARNING: ListAppendWithTemplateAndClear received Text event '{}' - should have been pre-instantiated", text);
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ClearText => {
                                // Clear to empty text when triggered
                                // Only clear if the event has valid text (non-empty Enter:xxx)
                                match event {
                                    (_, EventValue::Text(text)) => {
                                        // Only clear if there's actual text to add
                                        if EventPayload::parse_enter_text(text).is_none() {
                                            return state.clone();
                                        }
                                        Value::text("")
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListRemoveCompleted { completed_field } => {
                                // Phase 7.3: Remove all items where the specified boolean field is true
                                // Uses ListRemoveBatch diffs instead of filtering + full list clone
                                //
                                // PURE IMPLEMENTATION: Checks item data directly via is_item_field_true()
                                // with the explicit completed_field parameter from config. No IO layer reads.
                                //
                                // Key improvement: We emit batch removal keys instead of cloned filtered lists.
                                // This gives O(k) diff application instead of O(n) list clone.
                                if let Some(items) = state.as_list_items() {
                                    // Collect keys of completed items using explicit field name
                                    let completed_keys: Vec<Arc<str>> = items.iter()
                                        .filter(|item| is_item_field_true(item, completed_field))
                                        .map(|item| extract_item_key_for_removal(item))
                                        .collect();

                                    if completed_keys.is_empty() {
                                        zoon::println!("[DD Transform] ListRemoveCompleted: No completed items to remove");
                                        return state.clone();
                                    }

                                    zoon::println!("[DD Transform] ListRemoveCompleted: Emitting batch removal for {} keys", completed_keys.len());

                                    // Emit MultiCellUpdate with ListRemoveBatch for both lists
                                    // The output observer will apply the batch removal to both lists
                                    Value::multi_cell_update(vec![
                                        ("list_elements", Value::list_remove_batch("list_elements", completed_keys.clone())),
                                        (cell_id_for_transform.as_str(), Value::list_remove_batch(cell_id_for_transform.clone(), completed_keys)),
                                    ])
                                } else {
                                    state.clone()
                                }
                            }
                            StateTransform::RemoveListItem => {
                                // Phase 2.1: Emit ListRemoveByKey diff - O(1) lookup
                                // The key is the link_id from the event
                                zoon::println!("[DD Transform] RemoveListItem: Received event {:?}", event);
                                match (state, event) {
                                    (state, (_, EventValue::Text(text))) if state.is_list_like() => {
                                        zoon::println!("[DD Transform] RemoveListItem: Processing text event: {}", text);
                                        if let Some(link_id_str) = EventPayload::parse_remove_link(text) {
                                            // Phase 2.1: Emit ListRemoveByKey diff with link_id as key
                                            // The sync_cell_from_dd() will apply this and also handle list_elements
                                            zoon::println!("[DD Transform] RemoveListItem: Emitting ListRemoveByKey for link:{}", link_id_str);
                                            return Value::list_remove_by_key(cell_id_for_transform.clone(), format!("link:{}", link_id_str));
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }

                            // ================================================================
                            // GENERIC LIST OPERATIONS WITH STABLE IDENTITY
                            // ================================================================

                            StateTransform::ListItemSetFieldByIdentity { identity_path, field_path, update } => {
                                // Generic: Set a field on a list item identified by LinkRef
                                // Event format: "action:LINK_ID" or "action:LINK_ID:value"
                                // Phase 4: Use ListItemUpdate diff for O(1) updates (no list clone)
                                match (state, event) {
                                    (state, (_, EventValue::Text(text))) if state.is_list_like() => {
                                        let items = state.as_list_items().unwrap();
                                        // Parse event format: extract link_id and optional value
                                        // Format: "action:LINK_ID" or "action:LINK_ID:value"
                                        let parts: Vec<&str> = text.splitn(3, ':').collect();
                                        if parts.len() < 2 {
                                            return state.clone();
                                        }
                                        let link_id_str = parts[1];
                                        let event_value = parts.get(2).map(|s| s.trim());

                                        // Find item to get the key and compute new value
                                        let item_opt = items.iter().find(|item| {
                                            get_link_ref_at_path(item, identity_path)
                                                .map(|id| id == link_id_str)
                                                .unwrap_or(false)
                                        });

                                        if let Some(item) = item_opt {
                                            // Compute the new field value using the update function
                                            let new_field_value = compute_field_update(item, field_path, update, event_value);

                                            // Phase 4: Return ListItemUpdate diff - O(1) key lookup in output observer
                                            let key = format!("link:{}", link_id_str);
                                            let field_path_arc: Vec<Arc<str>> = field_path.iter().map(|s| Arc::from(s.as_str())).collect();
                                            return Value::list_item_update(
                                                cell_id_for_transform.clone(),
                                                key,
                                                field_path_arc,
                                                new_field_value,
                                            );
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }

                            StateTransform::ListItemRemoveByIdentity { identity_path: _, elements_hold } => {
                                // Phase 2.1: Emit ListRemoveByKey diff - O(1) lookup
                                // Note: identity_path is no longer needed as we use the link_id directly as key
                                match (state, event) {
                                    (state, (_, EventValue::Text(text))) if state.is_list_like() => {
                                        if let Some(link_id_str) = EventPayload::parse_remove_link(text) {
                                            let key = format!("link:{}", link_id_str);
                                            // Phase 2: Use MultiCellUpdate if we need to update both lists
                                            if let Some(elements_cell_id) = elements_hold {
                                                // Return MultiCellUpdate for both lists
                                                return Value::multi_cell_update(vec![
                                                    (elements_cell_id.as_str(), Value::list_remove_by_key(elements_cell_id.clone(), key.clone())),
                                                    (cell_id_for_transform.as_str(), Value::list_remove_by_key(cell_id_for_transform.clone(), key)),
                                                ]);
                                            }
                                            // Single list removal
                                            return Value::list_remove_by_key(cell_id_for_transform.clone(), key);
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }

                            StateTransform::ListAppendFromTemplate { template: _, elements_hold: _ } => {
                                // Phase 2.1: Emit ListPush diff - O(1) operation
                                match event {
                                    (_, EventValue::PreparedItem(prepared_data)) => {
                                        // Emit ListPush diff instead of cloning list
                                        if !state.is_list_like() {
                                            return state.clone();
                                        }
                                        Value::list_push(cell_id_for_transform.clone(), prepared_data.clone())
                                    }
                                    (_, EventValue::Text(text)) => {
                                        // Text events should have been pre-instantiated
                                        if LOG_DD_DEBUG {
                                            zoon::println!("[DD Worker] WARNING: ListAppendFromTemplate received Text event '{}' - should have been pre-instantiated", text);
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }

                            StateTransform::Identity => {
                                // Identity transform - pass through unchanged
                                // This is a placeholder for transforms that couldn't be determined
                                state.clone()
                            }
                        }
                    });

                    // Phase 10: Tag outputs for pure DD capture (no Mutex!)
                    let cell_id_for_tag: Arc<str> = Arc::from(hold_config.id.name());
                    let should_persist = hold_config.persist;

                    // Map hold_output to TaggedCellOutput - flows through DD as pure data
                    let tagged = hold_output.inner.map(move |(state, time, diff)| {
                        (TaggedCellOutput::new(
                            cell_id_for_tag.clone(),
                            state,
                            should_persist,
                            time,
                        ), time, diff)
                    });

                    tagged_outputs.push(tagged);
                }

                // If no HOLDs configured, just count events as before
                if config.cells.is_empty() {
                    use differential_dataflow::operators::Count;

                    let count = links.map(|_| ()).count();

                    // Phase 10: Tag count output for pure DD capture
                    let count_tagged = count.inner.map(|(((), total), time, diff)| {
                        (TaggedCellOutput::new(
                            "__count__",
                            Value::int(i64::try_from(total).unwrap_or(0)),
                            false,
                            time,
                        ), time, diff)
                    });
                    tagged_outputs.push(count_tagged);
                }

                // Phase 10: Concatenate all tagged outputs and capture
                // This is pure DD - no Mutex, no RefCell, just message passing!
                use timely::dataflow::operators::Concatenate;
                let combined = scope.concatenate(tagged_outputs);
                let outputs_rx = combined.capture();

                let probe = links.probe();
                (link_input_handle, probe, outputs_rx)
            });

            // Process events
            let mut time = start_time;
            for event in events {
                match event {
                    Event::Link { id, value } => {
                        link_input.insert((id.name().to_string(), value));
                    }
                    Event::Timer { id, tick } => {
                        // Timer events are treated like link events with a special ID
                        // This allows timer-triggered HOLDs to work with the same logic
                        let timer_link_id = format!("__timer_{}", id.name());
                        link_input.insert((timer_link_id, EventValue::Unit));
                        if LOG_DD_DEBUG { zoon::println!("[DD Worker] Timer {} tick {}", id.name(), tick); }
                    }
                    Event::External { name: _, value: _ } => {
                        // TODO: External events
                    }
                }

                time += 1;
                link_input.advance_to(time);
                link_input.flush();

                while probe.less_than(&time) {
                    worker.step();
                }
            }

            // Phase 10: Return the captured outputs receiver
            outputs_rx
        });

        // Phase 10: Extract captured outputs using timely's Extract trait
        // This is pure DD - no Mutex unwrap, just channel extraction!
        // Note: Extract trait is imported at the top of this function
        let captured = outputs_rx.extract();

        // Build DocumentUpdates and final_states from captured TaggedCellOutputs
        let mut outputs = Vec::new();
        let mut new_states = initial_states_for_merge;

        for (_time, batch) in captured {
            for (tagged, _t, diff) in batch {
                if diff > 0 {
                    // Record the new state
                    new_states.insert(tagged.cell_id.to_string(), tagged.state.clone());

                    // Create output update
                    let mut hold_updates = HashMap::new();
                    let mut hold_state_updates = HashMap::new();
                    if tagged.should_persist {
                        hold_updates.insert(tagged.cell_id.to_string(), tagged.state.clone());
                    } else {
                        hold_state_updates.insert(tagged.cell_id.to_string(), tagged.state.clone());
                    }

                    outputs.push(DocumentUpdate {
                        document: tagged.state,
                        time: tagged.time,
                        hold_updates,
                        hold_state_updates,
                    });
                }
            }
        }

        (outputs, start_time + num_events as u64, new_states)
    }
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dataflow_config_builder() {
        // Test that we can build a simple counter config
        let config = DataflowConfig::counter("button.press");
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
    fn test_process_batch_with_hold_simple() {
        // Test the synchronous batch processing directly (no async runtime needed)
        let config = DataflowConfig::counter("click");
        let events = vec![
            Event::Link {
                id: LinkId::new("click"),
                value: EventValue::Unit,
            },
            Event::Link {
                id: LinkId::new("click"),
                value: EventValue::Unit,
            },
        ];
        let initial_states = [("counter".to_string(), Value::int(0))]
            .into_iter()
            .collect();

        let (outputs, new_time, new_states) =
            Worker::process_batch_with_hold(&config, &events, 0, &initial_states);

        // Should have 2 outputs (one per click)
        assert_eq!(outputs.len(), 2);
        assert_eq!(new_time, 2);

        // Final state should be 2
        assert_eq!(new_states.get("counter"), Some(&Value::int(2)));
    }

    #[test]
    fn test_process_batch_filters_by_link_id() {
        // Test that HOLD only responds to its configured triggers
        let config = DataflowConfig::counter("button");
        let events = vec![
            Event::Link {
                id: LinkId::new("button"),
                value: EventValue::Unit,
            },
            Event::Link {
                id: LinkId::new("other"),  // Not "button", should be ignored
                value: EventValue::Unit,
            },
            Event::Link {
                id: LinkId::new("button"),
                value: EventValue::Unit,
            },
        ];
        let initial_states = [("counter".to_string(), Value::int(0))]
            .into_iter()
            .collect();

        let (outputs, _, new_states) =
            Worker::process_batch_with_hold(&config, &events, 0, &initial_states);

        // Should only have 2 outputs (the "button" clicks)
        assert_eq!(outputs.len(), 2);
        // Final state should be 2, not 3
        assert_eq!(new_states.get("counter"), Some(&Value::int(2)));
    }
}
