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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zoon::futures_channel::mpsc;
use zoon::{Task, Timer};

use super::types::{Event, EventValue, Input, Output, CellId, LinkId, DYNAMIC_LINK_PREFIX, BoolTag, EventPayload};
use super::value::Value;
use super::super::io::{
    get_cell_value,
    load_persisted_cell_value,
    sync_cell_from_dd,
    sync_cell_from_dd_with_persist,
};
use super::super::LOG_DD_DEBUG;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};

// Persistent DD Worker (Phase 5)
use super::dataflow::{
    DdCellConfig, DdTransform, DdOutput, DdEventFilter,
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

/// Global counter for generating unique HOLD IDs for dynamic items
static DYNAMIC_CELL_COUNTER: AtomicU32 = AtomicU32::new(1000);

/// Global counter for generating unique LINK IDs for dynamic items
static DYNAMIC_LINK_COUNTER: AtomicU32 = AtomicU32::new(1000);

/// Prefix for dynamically generated HOLD IDs (used for list items)
const DYNAMIC_CELL_PREFIX: &str = "dynamic_cell_";

// NOTE: DYNAMIC_LINK_PREFIX is imported from super::types

// ============================================================================
// GENERIC CHECKBOX TOGGLE DETECTION
// ============================================================================

use super::super::io::get_checkbox_toggle_holds;

/// Find the checkbox toggle CellRef in an item object.
/// Returns (cell_id, is_completed) if a checkbox toggle hold is found.
/// This is generic - works with any field name, not just "completed".
fn find_checkbox_toggle_in_item(obj: &BTreeMap<Arc<str>, Value>) -> Option<(String, bool)> {
    let toggle_holds = get_checkbox_toggle_holds();
    for (_, value) in obj.iter() {
        match value {
            Value::CellRef(cell_id) => {
                // Check if this CellRef is a checkbox toggle
                let cell_name = cell_id.name();
                if toggle_holds.contains(&cell_name) || cell_id.is_dynamic() {
                    // Get current value from global CELL_STATES
                    // Handle both Bool and Tagged { tag: "True"/"False" } variants
                    let is_completed = super::super::io::get_cell_value(&cell_name)
                        .map(|v| match v {
                            Value::Bool(b) => b,
                            Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => BoolTag::is_true(tag.as_ref()),
                            _ => false,
                        })
                        .unwrap_or(false);
                    return Some((cell_name, is_completed));
                }
            }
            Value::Bool(b) => {
                // Direct boolean value (legacy items without CellRef)
                // We can't distinguish which bool is the "toggle" one
                // For now, return the first bool we find
                // A better approach would need metadata about which field is the toggle
                return Some((String::new(), *b));
            }
            _ => {}
        }
    }
    None
}

/// Get the checkbox toggle value for an item (true = completed).
/// Works with both CellRef and direct Bool values.
/// Task 7.2: Generic - checks ANY boolean CellRef field, not just "completed".
fn is_item_completed_generic(item: &Value) -> bool {
    match item {
        Value::Object(obj) => {
            // First try the registered checkbox toggles
            if let Some((_, is_completed)) = find_checkbox_toggle_in_item(obj) {
                return is_completed;
            }

            // Task 7.2: Generic approach - find ANY boolean CellRef field
            // Excludes "editing" fields which are UI state, not completion state
            for (field_name, field_value) in obj.iter() {
                // Skip editing-related fields (UI state, not completion state)
                if field_name.contains("edit") {
                    continue;
                }
                if let Value::CellRef(cell_id) = field_value {
                    if let Some(cell_value) = super::super::io::get_cell_value(&cell_id.name()) {
                        match cell_value {
                            Value::Bool(b) => return b,
                            Value::Tagged { tag, .. } => {
                                if BoolTag::is_bool_tag(tag.as_ref()) {
                                    return BoolTag::is_true(tag.as_ref());
                                }
                            }
                            _ => continue,
                        }
                    }
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
            let new_id = cell_id_map
                .entry(old_id.name())
                .or_insert_with(|| {
                    let counter = DYNAMIC_CELL_COUNTER.fetch_add(1, Ordering::SeqCst);
                    format!("{}{}", DYNAMIC_CELL_PREFIX, counter)
                })
                .clone();
            Value::CellRef(CellId::new(new_id))
        }
        Value::LinkRef(old_id) => {
            // Generate or reuse fresh ID for this LinkRef
            let new_id = link_id_map
                .entry(old_id.name().to_string())
                .or_insert_with(|| {
                    let counter = DYNAMIC_LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
                    format!("{}{}", DYNAMIC_LINK_PREFIX, counter)
                })
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
            Value::List(Arc::new(new_items))
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

/// Find the cell_id of a WhileRef that controls hover-based visibility.
///
/// NOTE: WhileRef variant was surgically removed in Phase 6.
/// This function now always returns None - hover handling needs DD-based replacement.
fn find_hover_while_cell_id(_items: Option<&Value>) -> Option<String> {
    // WhileRef variant was removed - return None until DD-based hover handling is implemented
    None
}

/// Find the checkbox's `checked` CellRef ID within an Element AST.
/// Used by ListRemoveCompleted to determine if an element should be removed.
fn find_checkbox_cell_id(element: &Value) -> Option<String> {
    // Element structure: Tagged { tag: "Element", fields: { items: List([checkbox, ...]) } }
    if let Value::Tagged { tag, fields } = element {
        if tag.as_ref() == "Element" {
            // Look for items list
            if let Some(Value::List(items)) = fields.get("items") {
                for item in items.iter() {
                    // Check if this item is a checkbox element
                    if let Value::Tagged { tag: item_tag, fields: item_fields } = item {
                        if item_tag.as_ref() == "Element" {
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
            let new_link_id = link_id_map
                .entry(old_link.clone())
                .or_insert_with(|| {
                    let counter = DYNAMIC_LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
                    format!("{}{}", DYNAMIC_LINK_PREFIX, counter)
                })
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
        if let (Some(old_link), Some(old_hold)) = (&hover_link_old_id, &hover_cell_old_id) {
            let new_link_id = link_id_map
                .entry(old_link.clone())
                .or_insert_with(|| {
                    let counter = DYNAMIC_LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
                    format!("{}{}", DYNAMIC_LINK_PREFIX, counter)
                })
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
            let new_link_id = link_id_map
                .entry(old_link.clone())
                .or_insert_with(|| {
                    let counter = DYNAMIC_LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
                    format!("{}{}", DYNAMIC_LINK_PREFIX, counter)
                })
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

/// Filter for WHEN pattern matching on event values.
#[derive(Clone, Debug)]
pub enum EventFilter {
    /// Accept any event value
    Any,
    /// Only accept events with this exact text value (e.g., "Enter" for key events)
    TextEquals(String),
    /// Accept events starting with this prefix (e.g., "Enter:" for Enter+text)
    TextStartsWith(String),
}

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
    /// Remove all items where completed=true (for Clear completed button)
    ListRemoveCompleted,
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

/// Configuration for the entire DD dataflow.
#[derive(Clone, Debug, Default)]
pub struct DataflowConfig {
    /// HOLD operators to create
    pub cells: Vec<CellConfig>,
    /// Link-to-cell mappings for DD-native LINK handling (Phase 8).
    /// These replace DYNAMIC_LINK_ACTIONS HashMap with DD joins.
    pub link_mappings: Vec<super::types::LinkCellMapping>,
}

impl DataflowConfig {
    /// Create a new empty dataflow config.
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            link_mappings: Vec::new(),
        }
    }

    /// Add a link-to-cell mapping (Phase 8).
    /// This replaces add_dynamic_link_action for DD-native handling.
    pub fn add_link_mapping(&mut self, mapping: super::types::LinkCellMapping) {
        self.link_mappings.push(mapping);
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
}

impl Worker {
    /// Create a new DD worker with default configuration.
    pub fn new() -> Self {
        Self {
            config: DataflowConfig::default(),
        }
    }

    /// Create a DD worker with specific configuration.
    pub fn with_config(config: DataflowConfig) -> Self {
        Self { config }
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
                StateTransform::RemoveListItem => DdTransform::Custom(Arc::new(|state, _| state.clone())),
                // Other complex transforms - use Custom fallback for now
                _ => DdTransform::Custom(Arc::new(|state, _| state.clone())),
            };

            // Build trigger list - start with explicit triggers
            let mut triggers = cell.triggered_by.clone();

            // Add timer trigger ID if this HOLD is timer-triggered
            if cell.timer_interval_ms > 0 {
                let timer_link_id = LinkId::new(&format!("__timer_{}", cell.timer_interval_ms));
                triggers.push(timer_link_id);
            }

            // Convert EventFilter to DdEventFilter
            let filter = match &cell.filter {
                EventFilter::Any => DdEventFilter::Any,
                EventFilter::TextEquals(s) => DdEventFilter::TextEquals(s.clone()),
                EventFilter::TextStartsWith(s) => DdEventFilter::TextStartsWith(s.clone()),
            };

            DdCellConfig {
                id: cell.id.clone(),
                initial: cell.initial.clone(),
                triggers,
                transform,
                filter,
            }
        }).collect()
    }

    /// Template info for pre-instantiation (O(delta) optimization).
    #[derive(Clone)]
    struct TemplateInfo {
        data_template: Value,
        element_template: Option<Value>,
        title_cell_field: String,
        cell_id: String,
        filter: EventFilter,
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
                    let title_field = template.field_initializers.iter()
                        .find(|(_, init)| matches!(init, FieldInitializer::FromEventText))
                        .map(|(path, _)| path.first().cloned().unwrap_or_default())
                        .unwrap_or_else(|| "title".to_string());
                    Some(TemplateInfo {
                        data_template: template.data_template.clone(),
                        element_template: template.element_template.clone(),
                        title_cell_field: title_field,
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
                let list_template = ListItemTemplate {
                    data_template: template.data_template.clone(),
                    element_template: template.element_template.clone(),
                    identity: ItemIdentitySpec {
                        link_ref_path: vec!["remove_button".to_string()], // Default identity path
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
    fn process_batch_with_hold(
        config: &DataflowConfig,
        events: &[Event],
        start_time: u64,
        initial_states: &HashMap<String, Value>,
    ) -> (Vec<DocumentUpdate>, u64, HashMap<String, Value>) {
        use super::operators::hold;
        use differential_dataflow::input::Input;

        let outputs = Arc::new(Mutex::new(Vec::new()));
        // Pre-populate with initial_states so transforms can resolve CellRefs from previous batches
        let final_states = Arc::new(Mutex::new(initial_states.clone()));
        let outputs_clone = outputs.clone();
        let final_states_clone = final_states.clone();

        // Clone data for the closure
        let events: Vec<Event> = events.to_vec();
        let initial_states_for_closure = initial_states.clone();
        let initial_states_for_merge = initial_states.clone();
        let config = config.clone();
        let num_events = events.len();

        Self::execute_directly_wasm(move |worker| {
            // Build dataflow for each HOLD in config
            let (mut link_input, probe) = worker.dataflow::<u64, _, _>(|scope| {
                // Create input collection for LINK events
                // Each link event is (LinkId, EventValue)
                let (link_input_handle, links) =
                    scope.new_collection::<(String, EventValue), isize>();

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

                    // Clone transform for the closure
                    let state_transform = hold_config.transform.clone();
                    // Clone final_states_clone for transforms that need to resolve CellRefs
                    let transform_states = final_states_clone.clone();

                    // Apply HOLD operator with configured transform
                    let hold_output = hold(initial, &triggered_events, move |state, event| {
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
                                // Append event text to list as plain text
                                // Event format is "Enter:text" - extract the text after ":"
                                // DEPRECATED: Use ListAppendWithTemplate for proper Element AST items
                                // This fallback is generic - no assumed object structure
                                match (state, event) {
                                    (Value::List(items), (_, EventValue::Text(text))) => {
                                        // Only process Enter key events with non-empty text
                                        let Some(item_text) = EventPayload::parse_enter_text(text) else {
                                            return state.clone();
                                        };
                                        let mut new_items: Vec<Value> = items.to_vec();
                                        // Append as plain text (generic fallback)
                                        new_items.push(Value::text(item_text));
                                        Value::List(Arc::new(new_items))
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListAppendWithTemplate { data_template, element_template, title_cell_field } => {
                                // Append item using template cloning with fresh IDs
                                // Both data and element templates use the SAME ID mapping,
                                // so the cloned element references the cloned data's HOLDs
                                match (state, event) {
                                    (Value::List(items), (_, EventValue::Text(text))) => {
                                        // Only process Enter key events with non-empty text
                                        let Some(item_text) = EventPayload::parse_enter_text(text) else {
                                            return state.clone();
                                        };

                                        // Extract field-to-hold mapping from data template
                                        // e.g., {"title" -> "hold_18", "completed" -> "hold_20", "editing" -> "hold_19"}
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

                                        // Register new HOLDs with initial values
                                        // Task 7.2: Generic - text field uses input, others use template defaults
                                        for (old_id, new_id) in &cell_id_map {
                                            let field_name = hold_to_field.get(old_id).map(|s| s.as_str());
                                            let initial_value = if field_name == Some(title_cell_field.as_str()) {
                                                // This is the text field - set to input text
                                                Value::text(item_text)
                                            } else {
                                                // Use template's default value for this HOLD
                                                // Task 7.2: Check template HOLD value type, not hardcoded field names
                                                get_cell_value(old_id).unwrap_or_else(|| {
                                                    // If template HOLD not found, check data_template for the value type
                                                    // by looking at what type of value this field has in the template
                                                    if let Value::Object(template_fields) = data_template {
                                                        if let Some(field_name) = field_name {
                                                            if let Some(template_field_value) = template_fields.get(&std::sync::Arc::from(field_name)) {
                                                                // If it's a CellRef, the type is unknown here
                                                                // Default boolean fields to false (common pattern)
                                                                if matches!(template_field_value, Value::CellRef(_)) {
                                                                    // Unknown type - try Unit first
                                                                    return Value::Unit;
                                                                }
                                                            }
                                                        }
                                                    }
                                                    Value::Unit
                                                })
                                            };
                                            if LOG_DD_DEBUG { zoon::println!("[DD Worker] Registering HOLD: {} -> {} = {:?} (field: {:?})", old_id, new_id, initial_value, field_name); }
                                            update_cell_no_persist(new_id, initial_value);
                                        }

                                        if LOG_DD_DEBUG { zoon::println!("[DD Worker] ListAppendWithTemplate: Created item with {} HOLDs, {} LINKs", cell_id_map.len(), link_id_map.len()); }

                                        // Replicate dynamic link actions from template to cloned item
                                        // For each LinkRef in the template, look up its action and register with remapped IDs
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
                                                    if LOG_DD_DEBUG { zoon::println!("[DD Worker] Replicating action {} -> {} {:?}", old_link_id, new_link_id, new_action); }
                                                    add_dynamic_link_action(new_link_id.clone(), new_action);
                                                }
                                            }
                                        }

                                        // If element template exists, clone it with SAME ID mapping
                                        // and add to "list_elements" HOLD for unified rendering
                                        if let Some(elem_tmpl) = element_template {
                                            // Find the hover link in the element template BEFORE cloning
                                            // Element template is Tagged "Element" with fields including element.hovered
                                            let hover_link_old_id = if let Value::Tagged { fields, .. } = elem_tmpl {
                                                fields.get("element")
                                                    .and_then(|e| e.get("hovered"))
                                                    .and_then(|h| match h {
                                                        Value::LinkRef(id) => Some(id.to_string()),
                                                        _ => None,
                                                    })
                                            } else {
                                                None
                                            };

                                            // Find the WhileRef for hover in the element template's items
                                            // This is the WhileRef that has arms for True (delete button) and False (NoElement)
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
                                                let new_link_id = link_id_map
                                                    .entry(old_link.clone())
                                                    .or_insert_with(|| {
                                                        let counter = DYNAMIC_LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
                                                        format!("{}{}", DYNAMIC_LINK_PREFIX, counter)
                                                    })
                                                    .clone();
                                                // Then create the hover hold mapping: hover_link_22 → hover_dynamic_link_1000
                                                let new_hover_cell = format!("hover_{}", new_link_id);
                                                cell_id_map.insert(old_hold.clone(), new_hover_cell.clone());
                                                if LOG_DD_DEBUG { zoon::println!("[DD Worker] Pre-mapped hover hold: {} -> {}", old_hold, new_hover_cell); }
                                            }

                                            // Clone element template - reuses the same ID mapping
                                            // Pass new_data_item as context to resolve PlaceholderFields (e.g., double_click LinkRefs)
                                            let new_element = clone_template_with_fresh_ids_with_context(elem_tmpl, &mut cell_id_map, &mut link_id_map, &new_data_item);

                                            // Register HoverState action if we found both the hover link and hold
                                            if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_cell_old_id) {
                                                if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), cell_id_map.get(&old_hold)) {
                                                    if LOG_DD_DEBUG { zoon::println!("[DD Worker] Registering HoverState: {} -> {}", new_link, new_hold); }
                                                    use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                                                    add_dynamic_link_action(new_link.clone(), DynamicLinkAction::HoverState(new_hold.clone()));
                                                    // Initialize hover hold to false
                                                    update_cell_no_persist(new_hold, Value::Bool(false));
                                                }
                                            }

                                            // Get current list_elements list and append
                                            let current_elements = super::super::io::get_cell_value("list_elements")
                                                .unwrap_or_else(|| Value::List(Arc::new(Vec::new())));
                                            if let Value::List(elems) = current_elements {
                                                let mut new_elems: Vec<Value> = elems.to_vec();
                                                new_elems.push(new_element);
                                                let new_count = new_elems.len();
                                                update_cell_no_persist("list_elements", Value::List(Arc::new(new_elems)));
                                                if LOG_DD_DEBUG { zoon::println!("[DD Worker] Added element to list_elements, now {} elements", new_count); }
                                            }
                                        }

                                        let mut new_items: Vec<Value> = items.to_vec();
                                        new_items.push(new_data_item);
                                        Value::List(Arc::new(new_items))
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListAppendWithClear(clear_link_id) => {
                                // Combined list operations:
                                // - "Enter:text" → append text to list
                                // - Unit event from clear_link_id → clear list to empty
                                match (state, event) {
                                    (Value::List(items), (_, EventValue::Text(text))) => {
                                        // Only process Enter key events with non-empty text
                                        let Some(item_text) = EventPayload::parse_enter_text(text) else {
                                            return state.clone();
                                        };
                                        let mut new_items: Vec<Value> = items.to_vec();
                                        new_items.push(Value::text(item_text));
                                        Value::List(Arc::new(new_items))
                                    }
                                    // Unit event from clear button → clear list
                                    // IMPORTANT: Only clear if the event is from the clear button's link,
                                    // not from other Unit events (like text input change handler)
                                    (Value::List(_), (link_id, EventValue::Unit)) if link_id == clear_link_id => {
                                        Value::List(Arc::new(Vec::new()))
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListAppendWithTemplateAndClear { data_template, element_template, title_cell_field, clear_link_id } => {
                                // Combined template append + clear button support
                                // - "Enter:text" → append using template cloning logic
                                // - Unit event from clear_link_id → clear both list_data and list_elements
                                match (state, event) {
                                    // Clear button: clear both lists
                                    (Value::List(_), (link_id, EventValue::Unit)) if link_id == clear_link_id => {
                                        // Also clear list_elements
                                        update_cell_no_persist("list_elements", Value::List(Arc::new(Vec::new())));
                                        Value::List(Arc::new(Vec::new()))
                                    }
                                    // Text input: use template cloning logic
                                    (Value::List(items), (_, EventValue::Text(text))) => {
                                        // Only process Enter key events with non-empty text
                                        let Some(item_text) = EventPayload::parse_enter_text(text) else {
                                            return state.clone();
                                        };

                                        // === Template cloning logic (same as ListAppendWithTemplate) ===

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

                                        // Register new HOLDs with initial values
                                        for (old_id, new_id) in &cell_id_map {
                                            let field_name = hold_to_field.get(old_id).map(|s| s.as_str());
                                            let initial_value = if field_name == Some(title_cell_field.as_str()) {
                                                Value::text(item_text)
                                            } else {
                                                get_cell_value(old_id).unwrap_or_else(|| {
                                                    if let Value::Object(template_fields) = data_template {
                                                        if let Some(field_name) = field_name {
                                                            if let Some(template_field_value) = template_fields.get(&std::sync::Arc::from(field_name)) {
                                                                if matches!(template_field_value, Value::CellRef(_)) {
                                                                    return Value::Unit;
                                                                }
                                                            }
                                                        }
                                                    }
                                                    Value::Unit
                                                })
                                            };
                                            if LOG_DD_DEBUG { zoon::println!("[DD Worker] ListAppendWithTemplateAndClear: Registering HOLD: {} -> {} = {:?}", old_id, new_id, initial_value); }
                                            update_cell_no_persist(new_id, initial_value);
                                        }

                                        // Replicate dynamic link actions from template to cloned item
                                        use super::super::io::{add_dynamic_link_action, get_dynamic_link_action, DynamicLinkAction};
                                        for (old_link_id, new_link_id) in &link_id_map {
                                            if let Some(action) = get_dynamic_link_action(old_link_id) {
                                                let remapped_action = match action {
                                                    DynamicLinkAction::SetTrue(old_hold) => {
                                                        cell_id_map.get(&old_hold).map(|new_hold| DynamicLinkAction::SetTrue(new_hold.clone()))
                                                    }
                                                    DynamicLinkAction::SetFalse(old_hold) => {
                                                        cell_id_map.get(&old_hold).map(|new_hold| DynamicLinkAction::SetFalse(new_hold.clone()))
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
                                                        cell_id_map.get(&old_hold).map(|new_hold| DynamicLinkAction::HoverState(new_hold.clone()))
                                                    }
                                                    DynamicLinkAction::RemoveListItem { .. } => {
                                                        Some(DynamicLinkAction::RemoveListItem { link_id: new_link_id.clone() })
                                                    }
                                                    DynamicLinkAction::SetFalseOnKeys { cell_id, keys } => {
                                                        cell_id_map.get(&cell_id).map(|new_hold| DynamicLinkAction::SetFalseOnKeys {
                                                            cell_id: new_hold.clone(),
                                                            keys: keys.clone(),
                                                        })
                                                    }
                                                    DynamicLinkAction::BoolToggle(old_hold) => {
                                                        cell_id_map.get(&old_hold).map(|new_hold| DynamicLinkAction::BoolToggle(new_hold.clone()))
                                                    }
                                                    DynamicLinkAction::ListToggleAllCompleted { list_cell_id, completed_field } => {
                                                        Some(DynamicLinkAction::ListToggleAllCompleted {
                                                            list_cell_id: list_cell_id.clone(),
                                                            completed_field: completed_field.clone(),
                                                        })
                                                    }
                                                };
                                                if let Some(new_action) = remapped_action {
                                                    if LOG_DD_DEBUG { zoon::println!("[DD Worker] ListAppendWithTemplateAndClear: Replicating action {} -> {} {:?}", old_link_id, new_link_id, new_action); }
                                                    add_dynamic_link_action(new_link_id.clone(), new_action);
                                                }
                                            }
                                        }

                                        // If element template exists, clone it with SAME ID mapping
                                        if let Some(elem_tmpl) = element_template {
                                            let hover_link_old_id = if let Value::Tagged { fields, .. } = elem_tmpl {
                                                fields.get("element").and_then(|e| e.get("hovered")).and_then(|h| match h {
                                                    Value::LinkRef(id) => Some(id.to_string()),
                                                    _ => None,
                                                })
                                            } else {
                                                None
                                            };

                                            let hover_cell_old_id = if let Value::Tagged { fields, .. } = elem_tmpl {
                                                find_hover_while_cell_id(fields.get("items"))
                                            } else {
                                                None
                                            };

                                            if let (Some(old_link), Some(old_hold)) = (&hover_link_old_id, &hover_cell_old_id) {
                                                let new_link_id = link_id_map
                                                    .entry(old_link.clone())
                                                    .or_insert_with(|| {
                                                        let counter = DYNAMIC_LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
                                                        format!("{}{}", DYNAMIC_LINK_PREFIX, counter)
                                                    })
                                                    .clone();
                                                let new_hover_cell = format!("hover_{}", new_link_id);
                                                cell_id_map.insert(old_hold.clone(), new_hover_cell.clone());
                                            }

                                            let new_element = clone_template_with_fresh_ids_with_context(elem_tmpl, &mut cell_id_map, &mut link_id_map, &new_data_item);

                                            if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_cell_old_id) {
                                                if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), cell_id_map.get(&old_hold)) {
                                                    add_dynamic_link_action(new_link.clone(), DynamicLinkAction::HoverState(new_hold.clone()));
                                                    update_cell_no_persist(new_hold, Value::Bool(false));
                                                }
                                            }

                                            let current_elements = super::super::io::get_cell_value("list_elements")
                                                .unwrap_or_else(|| Value::List(Arc::new(Vec::new())));
                                            if let Value::List(elems) = current_elements {
                                                let mut new_elems: Vec<Value> = elems.to_vec();
                                                new_elems.push(new_element);
                                                update_cell_no_persist("list_elements", Value::List(Arc::new(new_elems)));
                                            }
                                        }

                                        let mut new_items: Vec<Value> = items.to_vec();
                                        new_items.push(new_data_item);
                                        Value::List(Arc::new(new_items))
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
                            StateTransform::ListRemoveCompleted => {
                                // Remove all items where completed=true
                                // Uses generic checkbox toggle detection (no hardcoded field names)
                                let _states_map = transform_states.lock().unwrap();
                                match state {
                                    Value::List(items) => {
                                        // Use generic checkbox toggle detection (no hardcoded field names)
                                        // Filter the data list to keep only uncompleted items
                                        let filtered: Vec<Value> = items.iter()
                                            .filter(|item| !is_item_completed_generic(item))
                                            .cloned()
                                            .collect();
                                        zoon::println!("[DD Transform] ListRemoveCompleted: Filtered {} -> {} items", items.len(), filtered.len());

                                        // Also filter "list_elements" to remove corresponding elements
                                        // Uses same generic detection for consistency
                                        if let Some(Value::List(elements)) = super::super::io::get_cell_value("list_elements") {
                                            let filtered_elements: Vec<Value> = elements.iter()
                                                .filter(|element| {
                                                    // Find the checkbox's checked CellRef using generic detection
                                                    let completed_cell_id = find_checkbox_cell_id(element);
                                                    if let Some(cell_id) = completed_cell_id {
                                                        // Always read from global CELL_STATES (generic approach)
                                                        // Handle both Bool and Tagged { tag: "True"/"False" } variants
                                                        let is_completed = super::super::io::get_cell_value(&cell_id)
                                                            .map(|v| match v {
                                                                Value::Bool(b) => b,
                                                                Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => BoolTag::is_true(tag.as_ref()),
                                                                _ => false,
                                                            })
                                                            .unwrap_or(false);
                                                        zoon::println!("[DD Transform] Element hold {} is_completed={}, keeping={}", cell_id, is_completed, !is_completed);
                                                        !is_completed
                                                    } else {
                                                        true // Keep elements we can't determine
                                                    }
                                                })
                                                .cloned()
                                                .collect();
                                            zoon::println!("[DD Transform] ListRemoveCompleted: Filtered elements {} -> {}", elements.len(), filtered_elements.len());
                                            update_cell_no_persist("list_elements", Value::List(Arc::new(filtered_elements)));
                                        }

                                        Value::List(Arc::new(filtered))
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::RemoveListItem => {
                                // Remove a specific list item by link_id (matches remove_item_button LinkRef)
                                // Event format: "remove:LINK_ID" where LINK_ID identifies the item to remove
                                // HACK: list-specific - used for delete button on hover
                                zoon::println!("[DD Transform] RemoveListItem: Received event {:?}, state has {} items",
                                    event, if let Value::List(items) = state { items.len() } else { 0 });
                                match (state, event) {
                                    (Value::List(items), (_, EventValue::Text(text))) => {
                                        zoon::println!("[DD Transform] RemoveListItem: Processing text event: {}", text);
                                        if let Some(link_id_str) = EventPayload::parse_remove_link(text) {
                                            // Find the item whose remove button matches this link_id
                                            // Uses the PARSED PATH from List/remove(item, on: item.X.Y.event.press)
                                            // The path tells us exactly where the LinkRef is - no pattern matching needed
                                            let remove_path = super::super::io::get_remove_event_path();
                                            zoon::println!("[DD Transform] RemoveListItem: Using parsed path {:?} to find remove button", remove_path);

                                            let index_to_remove = items.iter().position(|item| {
                                                if remove_path.is_empty() {
                                                    // Fallback: no path parsed, can't find button
                                                    return false;
                                                }

                                                // Navigate through the parsed path to find the LinkRef
                                                // E.g., path = ["todo_elements", "remove_todo_button"]
                                                // Navigate: item -> todo_elements -> remove_todo_button -> LinkRef
                                                let mut current: Option<&Value> = Some(item);
                                                for path_segment in &remove_path {
                                                    current = current.and_then(|v| v.get(path_segment));
                                                }

                                                // Check if we found a matching LinkRef
                                                if let Some(Value::LinkRef(remove_link)) = current {
                                                    if remove_link.as_ref() == link_id_str {
                                                        return true;
                                                    }
                                                }
                                                false
                                            });

                                            if let Some(index) = index_to_remove {
                                                zoon::println!("[DD Transform] RemoveListItem: Found item with link_id {} at index {}", link_id_str, index);
                                                let mut new_items: Vec<Value> = items.to_vec();
                                                new_items.remove(index);

                                                // Also remove from "list_elements" - find by matching hover link or checkbox link
                                                // The element has a checkbox whose element.event.click LinkRef matches the item's item_checkbox
                                                // NOTE: list_elements only contains dynamic items, not initial ones
                                                if let Some(Value::List(elements)) = super::super::io::get_cell_value("list_elements") {
                                                    // For dynamic items, the link_id starts with "dynamic_link_"
                                                    // Find the element whose hover WhileRef matches our link_id pattern
                                                    let element_index = elements.iter().position(|element| {
                                                        // Check if this element's hover link matches
                                                        if let Value::Tagged { fields, .. } = element {
                                                            if let Some(Value::LinkRef(hover_link)) = fields.get("element").and_then(|e| e.get("hovered")) {
                                                                // The hover link ID pattern: e.g., "dynamic_link_1004" for an item with remove button "dynamic_link_1001"
                                                                // They share the same base, so check if our link_id starts with "dynamic_link_"
                                                                // and extract the base number
                                                                if link_id_str.starts_with(DYNAMIC_LINK_PREFIX) {
                                                                    // Extract base number from remove button link
                                                                    if let Some(base) = link_id_str.strip_prefix(DYNAMIC_LINK_PREFIX) {
                                                                        if let Ok(remove_num) = base.parse::<u32>() {
                                                                            // The hover link is usually remove_num + 3 (based on ID allocation pattern)
                                                                            // But safer to check the element's items for our specific link_id
                                                                            if let Some(Value::List(elem_items)) = fields.get("items") {
                                                                                for item in elem_items.iter() {
                                                                                    // Look for the delete button WhileRef
                                                                                    if let Value::WhileRef { arms, .. } = item {
                                                                                        for (_, body) in arms.iter() {
                                                                                            if let Value::Tagged { fields: btn_fields, tag } = body {
                                                                                                if tag.as_ref() == "Element" {
                                                                                                    if let Some(Value::Object(btn_elem)) = btn_fields.get("element") {
                                                                                                        if let Some(Value::Object(event)) = btn_elem.get("event") {
                                                                                                            if let Some(Value::LinkRef(press_link)) = event.get("press") {
                                                                                                                if press_link.as_ref() == link_id_str {
                                                                                                                    return true;
                                                                                                                }
                                                                                                            }
                                                                                                        }
                                                                                                    }
                                                                                                }
                                                                                            }
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        false
                                                    });

                                                    if let Some(elem_idx) = element_index {
                                                        let mut new_elements: Vec<Value> = elements.to_vec();
                                                        new_elements.remove(elem_idx);
                                                        zoon::println!("[DD Transform] RemoveListItem: Also removed element at index {}, now {} elements", elem_idx, new_elements.len());
                                                        update_cell_no_persist("list_elements", Value::List(Arc::new(new_elements)));
                                                    } else {
                                                        zoon::println!("[DD Transform] RemoveListItem: No matching element found in list_elements for link_id {}", link_id_str);
                                                    }
                                                }

                                                return Value::List(Arc::new(new_items));
                                            } else {
                                                zoon::println!("[DD Transform] RemoveListItem: No item found with link_id {}", link_id_str);
                                            }
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
                                match (state, event) {
                                    (Value::List(items), (_, EventValue::Text(text))) => {
                                        // Parse event format: extract link_id and optional value
                                        // Format: "action:LINK_ID" or "action:LINK_ID:value"
                                        let parts: Vec<&str> = text.splitn(3, ':').collect();
                                        if parts.len() < 2 {
                                            return state.clone();
                                        }
                                        let link_id_str = parts[1];
                                        let event_value = parts.get(2).map(|s| s.trim());

                                        // Find item by matching LinkRef at identity_path
                                        let index = items.iter().position(|item| {
                                            get_link_ref_at_path(item, identity_path)
                                                .map(|id| id == link_id_str)
                                                .unwrap_or(false)
                                        });

                                        if let Some(idx) = index {
                                            let mut new_items: Vec<Value> = items.to_vec();
                                            new_items[idx] = update_field_at_path(
                                                &new_items[idx],
                                                field_path,
                                                update,
                                                event_value,
                                            );
                                            return Value::List(Arc::new(new_items));
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }

                            StateTransform::ListItemRemoveByIdentity { identity_path, elements_hold } => {
                                // Generic: Remove a list item identified by LinkRef
                                // Event format: "remove:LINK_ID"
                                match (state, event) {
                                    (Value::List(items), (_, EventValue::Text(text))) => {
                                        if let Some(link_id_str) = EventPayload::parse_remove_link(text) {
                                            // Find item by matching LinkRef at identity_path
                                            let index = items.iter().position(|item| {
                                                get_link_ref_at_path(item, identity_path)
                                                    .map(|id| id == link_id_str)
                                                    .unwrap_or(false)
                                            });

                                            if let Some(idx) = index {
                                                let mut new_items: Vec<Value> = items.to_vec();
                                                new_items.remove(idx);

                                                // Also remove from parallel elements list if specified
                                                if let Some(cell_id) = elements_hold {
                                                    if let Some(Value::List(elements)) = super::super::io::get_cell_value(cell_id) {
                                                        // Find matching element by hover link or same identity
                                                        let elem_idx = elements.iter().position(|element| {
                                                            // Check hover link pattern for dynamic items
                                                            if let Value::Tagged { fields, .. } = element {
                                                                if let Some(Value::LinkRef(hover_link)) = fields.get("element").and_then(|e| e.get("hovered")) {
                                                                    // Dynamic items share ID ranges
                                                                    if link_id_str.starts_with(DYNAMIC_LINK_PREFIX) && hover_link.is_dynamic() {
                                                                        // Check by searching element's items for our link_id
                                                                        if let Some(Value::List(elem_items)) = fields.get("items") {
                                                                            for item in elem_items.iter() {
                                                                                if let Value::WhileRef { arms, .. } = item {
                                                                                    for (_, body) in arms.iter() {
                                                                                        if let Value::Tagged { fields: btn_fields, tag } = body {
                                                                                            if tag.as_ref() == "Element" {
                                                                                                if let Some(Value::Object(btn_elem)) = btn_fields.get("element") {
                                                                                                    if let Some(Value::Object(evt)) = btn_elem.get("event") {
                                                                                                        if let Some(Value::LinkRef(press_link)) = evt.get("press") {
                                                                                                            if press_link.as_ref() == link_id_str {
                                                                                                                return true;
                                                                                                            }
                                                                                                        }
                                                                                                    }
                                                                                                }
                                                                                            }
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            false
                                                        });

                                                        if let Some(e_idx) = elem_idx {
                                                            let mut new_elements: Vec<Value> = elements.to_vec();
                                                            new_elements.remove(e_idx);
                                                            update_cell_no_persist(cell_id, Value::List(Arc::new(new_elements)));
                                                        }
                                                    }
                                                }

                                                return Value::List(Arc::new(new_items));
                                            }
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }

                            StateTransform::ListAppendFromTemplate { template, elements_hold } => {
                                // Generic: Append item using ListItemTemplate
                                // Template's field_initializers define how each field is initialized
                                // No hardcoded field names - uses FromEventText and Constant initializers
                                match (state, event) {
                                    (Value::List(items), (_, EventValue::Text(text))) => {
                                        // Extract text from "Enter:text" format using type-safe parser
                                        let Some(input_text) = EventPayload::parse_enter_text(text) else {
                                            return state.clone();
                                        };

                                        // Instantiate template using only field_initializers
                                        // No hardcoded initial_values - let template config define everything
                                        let instantiated = instantiate_template(
                                            template,
                                            &HashMap::new(), // Empty - use field_initializers from template
                                            Some(input_text), // For FromEventText initializers
                                        );

                                        let mut new_items: Vec<Value> = items.to_vec();
                                        new_items.push(instantiated.data);

                                        // Also append to elements list if specified
                                        if let Some(cell_id) = elements_hold {
                                            if let Some(element) = instantiated.element {
                                                if let Some(Value::List(elements)) = super::super::io::get_cell_value(cell_id) {
                                                    let mut new_elements: Vec<Value> = elements.to_vec();
                                                    new_elements.push(element);
                                                    update_cell_no_persist(cell_id, Value::List(Arc::new(new_elements)));
                                                } else {
                                                    // Create new elements list
                                                    update_cell_no_persist(cell_id, Value::List(Arc::new(vec![element])));
                                                }
                                            }
                                        }

                                        Value::List(Arc::new(new_items))
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

                    // Capture outputs for this HOLD
                    let outputs = outputs_clone.clone();
                    let final_states = final_states_clone.clone();
                    let cell_id_for_inspect = hold_config.id.name().to_string();
                    let cell_id_for_state = hold_config.id.name().to_string();
                    let should_persist = hold_config.persist;

                    hold_output.inspect(move |(state, time, diff)| {
                        if *diff > 0 {
                            // Record the new state
                            final_states
                                .lock()
                                .unwrap()
                                .insert(cell_id_for_state.clone(), state.clone());

                            // Create output update
                            // - hold_updates: persisted to localStorage
                            // - hold_state_updates: UI only, not persisted
                            let mut hold_updates = HashMap::new();
                            let mut hold_state_updates = HashMap::new();
                            if should_persist {
                                hold_updates.insert(cell_id_for_inspect.clone(), state.clone());
                            } else {
                                hold_state_updates.insert(cell_id_for_inspect.clone(), state.clone());
                            }

                            outputs.lock().unwrap().push(DocumentUpdate {
                                document: state.clone(),
                                time: *time,
                                hold_updates,
                                hold_state_updates,
                            });
                        }
                    });
                }

                // If no HOLDs configured, just count events as before
                if config.cells.is_empty() {
                    use differential_dataflow::operators::Count;

                    let count = links.map(|_| ()).count();
                    let outputs = outputs_clone.clone();

                    count.inspect(move |(((), total), time, diff)| {
                        if *diff > 0 {
                            outputs.lock().unwrap().push(DocumentUpdate {
                                document: Value::int(i64::try_from(*total).unwrap_or(0)),
                                time: *time,
                                hold_updates: HashMap::new(),
                                hold_state_updates: HashMap::new(),
                            });
                        }
                    });
                }

                let probe = links.probe();
                (link_input_handle, probe)
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
        });

        let outputs = Arc::try_unwrap(outputs)
            .expect("outputs still borrowed")
            .into_inner()
            .unwrap();

        // Merge new states with initial states
        let mut new_states = initial_states_for_merge;
        let state_updates = Arc::try_unwrap(final_states)
            .expect("final_states still borrowed")
            .into_inner()
            .unwrap();
        for (k, v) in state_updates {
            new_states.insert(k, v);
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
