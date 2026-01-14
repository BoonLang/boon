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
//! - NO `.get()` - All observation through DdOutput streams

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zoon::futures_channel::mpsc;
use zoon::{Task, Timer};

use super::types::{DdEvent, DdEventValue, DdInput, DdOutput, HoldId, LinkId};
use crate::platform::browser::engine_dd::dd_value::DdValue;
use crate::platform::browser::engine_dd::io::{update_hold_state, update_hold_state_no_persist};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};

/// Global counter for generating unique HOLD IDs for dynamic items
static DYNAMIC_HOLD_COUNTER: AtomicU32 = AtomicU32::new(1000);

/// Global counter for generating unique LINK IDs for dynamic items
static DYNAMIC_LINK_COUNTER: AtomicU32 = AtomicU32::new(1000);

/// Extract field-to-hold-id mappings from a template.
/// Returns a map like {"title" -> "hold_18", "completed" -> "hold_20", ...}
fn extract_field_to_hold_map(template: &DdValue) -> HashMap<String, String> {
    let mut result = HashMap::new();
    if let DdValue::Object(fields) = template {
        for (field_name, value) in fields.iter() {
            if let DdValue::HoldRef(hold_id) = value {
                result.insert(field_name.to_string(), hold_id.to_string());
            }
        }
    }
    result
}

/// Clone a template DdValue with fresh HoldRef/LinkRef IDs.
/// Returns the cloned value and a mapping of old HoldRef IDs to new IDs.
fn clone_template_with_fresh_ids(
    template: &DdValue,
    hold_id_map: &mut HashMap<String, String>,
    link_id_map: &mut HashMap<String, String>,
) -> DdValue {
    match template {
        DdValue::HoldRef(old_id) => {
            // Generate or reuse fresh ID for this HoldRef
            let new_id = hold_id_map
                .entry(old_id.to_string())
                .or_insert_with(|| {
                    let counter = DYNAMIC_HOLD_COUNTER.fetch_add(1, Ordering::SeqCst);
                    format!("dynamic_hold_{}", counter)
                })
                .clone();
            DdValue::HoldRef(Arc::from(new_id.as_str()))
        }
        DdValue::LinkRef(old_id) => {
            // Generate or reuse fresh ID for this LinkRef
            let new_id = link_id_map
                .entry(old_id.to_string())
                .or_insert_with(|| {
                    let counter = DYNAMIC_LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
                    format!("dynamic_link_{}", counter)
                })
                .clone();
            DdValue::LinkRef(Arc::from(new_id.as_str()))
        }
        DdValue::Object(fields) => {
            let new_fields: BTreeMap<Arc<str>, DdValue> = fields
                .iter()
                .map(|(k, v)| (k.clone(), clone_template_with_fresh_ids(v, hold_id_map, link_id_map)))
                .collect();
            DdValue::Object(Arc::new(new_fields))
        }
        DdValue::List(items) => {
            let new_items: Vec<DdValue> = items
                .iter()
                .map(|v| clone_template_with_fresh_ids(v, hold_id_map, link_id_map))
                .collect();
            DdValue::List(Arc::new(new_items))
        }
        DdValue::Tagged { tag, fields } => {
            let new_fields: BTreeMap<Arc<str>, DdValue> = fields
                .iter()
                .map(|(k, v)| (k.clone(), clone_template_with_fresh_ids(v, hold_id_map, link_id_map)))
                .collect();
            DdValue::Tagged { tag: tag.clone(), fields: Arc::new(new_fields) }
        }
        DdValue::WhileRef { hold_id, computation, arms, default } => {
            // Remap the hold_id using the existing mapping
            let new_hold_id = hold_id_map
                .entry(hold_id.to_string())
                .or_insert_with(|| {
                    let counter = DYNAMIC_HOLD_COUNTER.fetch_add(1, Ordering::SeqCst);
                    format!("dynamic_hold_{}", counter)
                })
                .clone();

            // Recurse into arm patterns and bodies
            let new_arms: Vec<(DdValue, DdValue)> = arms
                .iter()
                .map(|(pattern, body)| {
                    (
                        clone_template_with_fresh_ids(pattern, hold_id_map, link_id_map),
                        clone_template_with_fresh_ids(body, hold_id_map, link_id_map),
                    )
                })
                .collect();

            // Recurse into default if present
            let new_default = default.as_ref().map(|d| {
                Arc::new(clone_template_with_fresh_ids(d.as_ref(), hold_id_map, link_id_map))
            });

            DdValue::WhileRef {
                hold_id: Arc::from(new_hold_id.as_str()),
                computation: computation.clone(), // Computation doesn't contain HoldRefs to remap
                arms: Arc::new(new_arms),
                default: new_default,
            }
        }
        // Primitive types are cloned as-is
        _ => template.clone(),
    }
}

/// Find the hold_id of a WhileRef that controls hover-based visibility.
/// Looks for a WhileRef in items that has True/False arms (for showing/hiding the delete button).
fn find_hover_while_hold_id(items: Option<&DdValue>) -> Option<String> {
    if let Some(DdValue::List(item_list)) = items {
        for item in item_list.iter() {
            // Check if this item is a WhileRef with True/False arms
            if let DdValue::WhileRef { hold_id, arms, .. } = item {
                // Check if this WhileRef has True and False arms (hover-based visibility pattern)
                let has_true_arm = arms.iter().any(|(pattern, _)| {
                    matches!(pattern, DdValue::Tagged { tag, .. } if tag.as_ref() == "True")
                });
                let has_false_arm = arms.iter().any(|(pattern, body)| {
                    matches!(pattern, DdValue::Tagged { tag, .. } if tag.as_ref() == "False") &&
                    matches!(body, DdValue::Tagged { tag, .. } if tag.as_ref() == "NoElement")
                });
                if has_true_arm && has_false_arm {
                    return Some(hold_id.to_string());
                }
            }
            // Also check nested in Tagged elements' items
            if let DdValue::Tagged { fields, .. } = item {
                if let Some(nested_items) = fields.get("items") {
                    if let Some(hold_id) = find_hover_while_hold_id(Some(nested_items)) {
                        return Some(hold_id);
                    }
                }
            }
        }
    }
    None
}

/// Find the checkbox's `checked` HoldRef ID within an Element AST.
/// Used by ListRemoveCompleted to determine if an element should be removed.
fn find_checkbox_hold_id(element: &DdValue) -> Option<String> {
    // Element structure: Tagged { tag: "Element", fields: { items: List([checkbox, ...]) } }
    if let DdValue::Tagged { tag, fields } = element {
        if tag.as_ref() == "Element" {
            // Look for items list
            if let Some(DdValue::List(items)) = fields.get("items") {
                for item in items.iter() {
                    // Check if this item is a checkbox element
                    if let DdValue::Tagged { tag: item_tag, fields: item_fields } = item {
                        if item_tag.as_ref() == "Element" {
                            if let Some(DdValue::Text(elem_type)) = item_fields.get("_element_type") {
                                if elem_type.as_ref() == "checkbox" {
                                    // Found checkbox - extract checked HoldRef
                                    if let Some(DdValue::HoldRef(hold_id)) = item_fields.get("checked") {
                                        zoon::println!("[DD Worker] find_checkbox_hold_id: found checkbox with checked={}", hold_id);
                                        return Some(hold_id.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            zoon::println!("[DD Worker] find_checkbox_hold_id: no checkbox found in element type {:?}", fields.get("_element_type"));
        }
    }
    None
}

/// Reconstruct a persisted todo item using templates.
///
/// When todos are persisted to localStorage, they lose their HoldRef/LinkRef structure.
/// This function reconstructs the full structure by:
/// 1. Cloning templates with fresh IDs
/// 2. Initializing HOLDs with persisted values (title, completed, editing)
/// 3. Registering dynamic link actions (edit, remove, hover)
/// 4. Returning the reconstructed (data_item, element) pair
///
/// This enables persisted todos to render correctly after page reload.
pub fn reconstruct_persisted_item(
    persisted_item: &DdValue,
    data_template: &DdValue,
    element_template: Option<&DdValue>,
) -> Option<(DdValue, Option<DdValue>)> {
    // Extract persisted values
    let (title, completed, editing) = if let DdValue::Object(obj) = persisted_item {
        let title = obj.get("title").and_then(|v| match v {
            DdValue::Text(t) => Some(t.to_string()),
            _ => None,
        })?;
        let completed = obj.get("completed").and_then(|v| match v {
            DdValue::Bool(b) => Some(*b),
            _ => None,
        }).unwrap_or(false);
        let editing = obj.get("editing").and_then(|v| match v {
            DdValue::Bool(b) => Some(*b),
            _ => None,
        }).unwrap_or(false);
        (title, completed, editing)
    } else {
        return None;
    };

    zoon::println!("[DD Reconstruct] Persisted item: title='{}', completed={}, editing={}", title, completed, editing);

    // Extract field-to-hold mapping from data template
    let field_to_hold = extract_field_to_hold_map(data_template);

    // Clone data template with fresh HoldRef/LinkRef IDs
    let mut hold_id_map: HashMap<String, String> = HashMap::new();
    let mut link_id_map: HashMap<String, String> = HashMap::new();
    let new_data_item = clone_template_with_fresh_ids(data_template, &mut hold_id_map, &mut link_id_map);

    // Create reverse mapping: old_hold_id -> field_name
    let hold_to_field: HashMap<String, String> = field_to_hold
        .iter()
        .map(|(field, hold)| (hold.clone(), field.clone()))
        .collect();

    // Register new HOLDs with PERSISTED values (not defaults!)
    for (old_id, new_id) in &hold_id_map {
        let field_name = hold_to_field.get(old_id).map(|s| s.as_str());
        let initial_value = match field_name {
            Some("title") => DdValue::text(title.clone()),
            Some("completed") => DdValue::Bool(completed),
            Some("editing") => DdValue::Bool(editing),
            _ => DdValue::Unit,
        };
        zoon::println!("[DD Reconstruct] Registering HOLD: {} -> {} = {:?} (field: {:?})",
            old_id, new_id, initial_value, field_name);
        update_hold_state_no_persist(new_id, initial_value);
    }

    zoon::println!("[DD Reconstruct] Created item with {} HOLDs, {} LINKs",
        hold_id_map.len(), link_id_map.len());

    // Register dynamic link actions for editing mode
    if let Some(editing_old_id) = field_to_hold.get("editing") {
        if let Some(editing_new_id) = hold_id_map.get(editing_old_id) {
            if let DdValue::Object(obj) = data_template {
                if let Some(DdValue::Object(todo_elements)) = obj.get("todo_elements") {
                    // Register double-click on todo_title_element → SetTrue(editing hold)
                    if let Some(DdValue::LinkRef(old_link_id)) = todo_elements.get("todo_title_element") {
                        if let Some(new_link_id) = link_id_map.get(old_link_id.as_ref()) {
                            zoon::println!("[DD Reconstruct] Registering SetTrue: {} -> {}", new_link_id, editing_new_id);
                            use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                            add_dynamic_link_action(new_link_id.clone(), DynamicLinkAction::SetTrue(editing_new_id.clone()));
                        }
                    }

                    // Register editing handler for editing_todo_title_element
                    if let Some(DdValue::LinkRef(old_edit_link_id)) = todo_elements.get("editing_todo_title_element") {
                        if let Some(new_edit_link_id) = link_id_map.get(old_edit_link_id.as_ref()) {
                            if let Some(title_old_id) = field_to_hold.get("title") {
                                if let Some(title_new_id) = hold_id_map.get(title_old_id) {
                                    zoon::println!("[DD Reconstruct] Registering EditingHandler: {} -> edit={}, title={}", new_edit_link_id, editing_new_id, title_new_id);
                                    use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                                    add_dynamic_link_action(new_edit_link_id.clone(), DynamicLinkAction::EditingHandler {
                                        editing_hold: editing_new_id.clone(),
                                        title_hold: title_new_id.clone(),
                                    });
                                }
                            }
                        }
                    }

                    // Register RemoveListItem action for remove_todo_button
                    if let Some(DdValue::LinkRef(old_remove_link_id)) = todo_elements.get("remove_todo_button") {
                        if let Some(new_remove_link_id) = link_id_map.get(old_remove_link_id.as_ref()) {
                            zoon::println!("[DD Reconstruct] Registering RemoveListItem: {} -> link_id={}", new_remove_link_id, new_remove_link_id);
                            use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                            add_dynamic_link_action(new_remove_link_id.clone(), DynamicLinkAction::RemoveListItem { link_id: new_remove_link_id.clone() });
                        }
                    }
                }
            }
        }
    }

    // Clone element template if provided
    let new_element = element_template.map(|elem_tmpl| {
        // Find the hover link in the element template BEFORE cloning
        let hover_link_old_id = if let DdValue::Tagged { fields, .. } = elem_tmpl {
            fields.get("element")
                .and_then(|e| e.get("hovered"))
                .and_then(|h| match h {
                    DdValue::LinkRef(id) => Some(id.to_string()),
                    _ => None,
                })
        } else {
            None
        };

        // Find the WhileRef for hover
        let hover_hold_old_id = if let DdValue::Tagged { fields, .. } = elem_tmpl {
            find_hover_while_hold_id(fields.get("items"))
        } else {
            None
        };

        // Clone element template - reuses the same ID mapping
        let cloned_element = clone_template_with_fresh_ids(elem_tmpl, &mut hold_id_map, &mut link_id_map);

        // Register HoverState action if we found both the hover link and hold
        if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_hold_old_id) {
            if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), hold_id_map.get(&old_hold)) {
                zoon::println!("[DD Reconstruct] Registering HoverState: {} -> {}", new_link, new_hold);
                use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                add_dynamic_link_action(new_link.clone(), DynamicLinkAction::HoverState(new_hold.clone()));
                // Initialize hover hold to false
                update_hold_state_no_persist(new_hold, DdValue::Bool(false));
            }
        }

        cloned_element
    });

    Some((new_data_item, new_element))
}

/// Handle to a running DD worker.
///
/// Use this to inject events and observe outputs.
pub struct DdWorkerHandle {
    /// Input channel for injecting events
    event_input: DdInput<DdEvent>,
    /// Output channel for observing document updates
    document_output: DdOutput<DocumentUpdate>,
    /// Task handle to keep the async event loop alive
    /// The underscore prefix is intentional - we don't read this, we just need to keep it alive
    _task_handle: zoon::TaskHandle,
}

impl DdWorkerHandle {
    /// Get the event input for injecting events.
    pub fn event_input(&self) -> &DdInput<DdEvent> {
        &self.event_input
    }

    /// Take the document output for observation.
    ///
    /// Note: This consumes the output. Call only once.
    pub fn take_document_output(self) -> DdOutput<DocumentUpdate> {
        self.document_output
    }

    /// Split the handle into (event_input, document_output, task_handle).
    ///
    /// This allows keeping the task alive while consuming both channels.
    pub fn split(self) -> (DdInput<DdEvent>, DdOutput<DocumentUpdate>, zoon::TaskHandle) {
        (self.event_input, self.document_output, self._task_handle)
    }
}

/// A document update from the DD worker.
#[derive(Clone, Debug)]
pub struct DocumentUpdate {
    /// The new document value as DdValue
    pub document: DdValue,
    /// Logical time of this update
    pub time: u64,
    /// HOLD state updates that SHOULD persist (hold_id -> new_value)
    pub hold_updates: HashMap<String, DdValue>,
    /// HOLD state updates for UI only, NOT persisted (hold_id -> new_value)
    /// Used for timer-driven HOLDs where persistence doesn't make sense.
    pub hold_state_updates: HashMap<String, DdValue>,
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
    /// Increment numeric value by 1
    Increment,
    /// Toggle boolean value
    BoolToggle,
    /// Set boolean to true (for editing mode on double-click)
    SetTrue,
    /// Set boolean to false (for exiting editing mode on Enter/Escape/blur)
    SetFalse,
    /// Append event text to list (creates todo objects with title/completed)
    /// DEPRECATED: Use ListAppendWithTemplate for proper Element AST items
    ListAppend,
    /// Append item using template DdValues with fresh HoldRef/LinkRef IDs.
    /// The templates are pre-evaluated structures (e.g., from new_todo() and todo_item()).
    /// At append time, both templates are cloned with the SAME ID mapping, so the element
    /// references the cloned data's HOLDs correctly.
    /// Fields:
    /// - data_template: The data object template (stored in "todos" HOLD)
    /// - element_template: The Element AST template (stored in "todos_elements" HOLD)
    /// - title_hold_field: Path to the title HoldRef field (e.g., "title")
    ListAppendWithTemplate {
        data_template: DdValue,
        element_template: Option<DdValue>,
        title_hold_field: String,
    },
    /// List with both append and clear - handles "Enter:text" for append, Unit from clear_link for clear
    /// The String parameter is the clear button's link_id - only Unit events from this link will clear.
    ListAppendWithClear(String),
    /// Clear to empty text (for text input clearing after submit)
    ClearText,
    /// Toggle completed field of a list item (event format: "toggle:N" where N is index)
    ToggleListItemCompleted,
    /// Toggle all items' completed state (for Toggle All checkbox)
    /// Sets all items to completed=true if any are uncompleted, or all to completed=false if all are completed.
    ListToggleAllCompleted,
    /// Remove all items where completed=true (for Clear completed button)
    ListRemoveCompleted,
    /// Set editing field of a list item (event format: "edit:N" or "unedit:N" where N is index)
    /// HACK: TodoMVC-specific - used for dynamic todo double-click editing
    SetListItemEditing,
    /// Update title field of a list item (event format: "save:N:new_title" where N is index)
    /// HACK: TodoMVC-specific - used for saving edited todo title
    UpdateListItemTitle,
    /// Remove a specific list item (event format: "remove:N" where N is index)
    /// HACK: TodoMVC-specific - used for delete button on hover
    RemoveListItem,
}

/// Configuration for a single HOLD operator in the dataflow.
#[derive(Clone, Debug)]
pub struct HoldConfig {
    /// Unique identifier for this HOLD
    pub id: HoldId,
    /// Initial value
    pub initial: DdValue,
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
    pub holds: Vec<HoldConfig>,
}

impl DataflowConfig {
    /// Create a new empty dataflow config.
    pub fn new() -> Self {
        Self { holds: Vec::new() }
    }

    /// Add a HOLD operator configuration.
    pub fn add_hold(mut self, id: impl Into<String>, initial: DdValue, triggers: Vec<&str>) -> Self {
        self.holds.push(HoldConfig {
            id: HoldId::new(id),
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
    pub fn add_timer_hold(mut self, id: impl Into<String>, initial: DdValue, interval_ms: u64) -> Self {
        self.holds.push(HoldConfig {
            id: HoldId::new(id),
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
        initial: DdValue,
        triggers: Vec<&str>,
        filter_text: &str,
        transform: StateTransform,
    ) -> Self {
        self.holds.push(HoldConfig {
            id: HoldId::new(id),
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
        self.holds.push(HoldConfig {
            id: HoldId::new(id),
            initial: DdValue::Bool(initial),
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
    /// Also adds a text_input_text HOLD that clears when items are added.
    pub fn add_list_append_on_enter(
        mut self,
        id: impl Into<String>,
        initial: DdValue,
        key_link_id: &str,
    ) -> Self {
        // HOLD for the list items
        self.holds.push(HoldConfig {
            id: HoldId::new(id),
            initial,
            triggered_by: vec![LinkId::new(key_link_id)],
            timer_interval_ms: 0,
            filter: EventFilter::TextStartsWith("Enter:".to_string()),
            transform: StateTransform::ListAppend,
            persist: true,
        });
        // HOLD for text input clearing - same trigger, clears to empty on successful append
        self.holds.push(HoldConfig {
            id: HoldId::new("text_input_text"),
            initial: DdValue::text(""),
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
    /// Also adds a text_input_text HOLD that clears when items are added.
    pub fn add_list_append_with_clear(
        mut self,
        id: impl Into<String>,
        initial: DdValue,
        key_link_id: &str,
        clear_link_id: &str,
    ) -> Self {
        let id_str = id.into();
        // HOLD for the list items - triggered by both Enter key AND clear button
        self.holds.push(HoldConfig {
            id: HoldId::new(&id_str),
            initial,
            triggered_by: vec![LinkId::new(key_link_id), LinkId::new(clear_link_id)],
            timer_interval_ms: 0,
            filter: EventFilter::Any, // Accept both Enter: and Unit events
            transform: StateTransform::ListAppendWithClear(clear_link_id.to_string()),
            persist: true,
        });
        // HOLD for text input clearing - only on Enter key (not clear button)
        self.holds.push(HoldConfig {
            id: HoldId::new("text_input_text"),
            initial: DdValue::text(""),
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
        Self::counter_with_initial(link_id, DdValue::int(0))
    }

    /// Create a counter configuration with a specific initial value.
    ///
    /// Used when restoring persisted state - the counter starts from
    /// the persisted value instead of 0.
    pub fn counter_with_initial(link_id: &str, initial: DdValue) -> Self {
        Self::new().add_hold("counter", initial, vec![link_id])
    }

    /// Create a counter configuration with specific HOLD ID and initial value.
    ///
    /// Allows specifying the HOLD ID to match what the evaluator generates.
    pub fn counter_with_initial_hold(link_id: &str, hold_id: &str, initial: DdValue) -> Self {
        Self::new().add_hold(hold_id, initial, vec![link_id])
    }

    /// Create a timer-driven counter configuration.
    ///
    /// Creates a HOLD that increments every interval_ms milliseconds.
    pub fn timer_counter(hold_id: &str, initial: DdValue, interval_ms: u64) -> Self {
        Self::new().add_timer_hold(hold_id, initial, interval_ms)
    }
}

/// DD Worker that processes events through Differential Dataflow.
///
/// This worker runs in an async context via `spawn_local` and processes
/// events synchronously in batches using `timely::execute_directly`.
pub struct DdWorker {
    /// Configuration for the dataflow
    config: DataflowConfig,
}

impl DdWorker {
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
    pub fn spawn(self) -> DdWorkerHandle {
        // Create channels for communication
        let (event_tx, event_rx) = mpsc::unbounded();
        let (output_tx, output_rx) = mpsc::unbounded();

        // Spawn the async event loop using Zoon's Task abstraction
        // This works on both WASM and non-WASM targets
        let task_handle = Task::start_droppable(Self::event_loop(self.config, event_rx, output_tx));

        DdWorkerHandle {
            event_input: DdInput::new(event_tx),
            document_output: DdOutput::new(output_rx),
            _task_handle: task_handle,
        }
    }

    /// The main event loop that processes DD events.
    async fn event_loop(
        config: DataflowConfig,
        mut event_rx: mpsc::UnboundedReceiver<DdEvent>,
        output_tx: mpsc::UnboundedSender<DocumentUpdate>,
    ) {
        // State maintained across batches - each HOLD has its current value
        let mut current_time: u64 = 0;
        let mut hold_states: HashMap<String, DdValue> = config
            .holds
            .iter()
            .map(|h| (h.id.name().to_string(), h.initial.clone()))
            .collect();

        loop {
            // Yield to browser to prevent blocking
            // Timer::sleep(0) yields to the event loop on WASM, or sleeps briefly on other platforms
            Timer::sleep(0).await;

            // Collect all pending events (non-blocking)
            let mut events = Vec::new();
            loop {
                match event_rx.try_next() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => {
                        // Channel closed (all senders dropped) - exit event loop
                        zoon::println!("[DdWorker] Channel closed, exiting event loop");
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

            zoon::println!("[DdWorker] Processing {} events", events.len());

            // Process events through DD
            // timely 0.25+ has WASM support (PR #663)
            let (outputs, new_time, new_states) = Self::process_batch_with_hold(
                &config,
                &events,
                current_time,
                &hold_states,
            );

            current_time = new_time;
            hold_states = new_states;

            zoon::println!("[DdWorker] Produced {} outputs, new states: {:?}", outputs.len(), hold_states);

            // Send outputs
            for output in &outputs {
                zoon::println!("[DdWorker] Output: counter = {:?}", output.document);
            }
            for output in outputs {
                if output_tx.unbounded_send(output).is_err() {
                    // Output channel closed, stop processing
                    return;
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

    /// Process a batch of events through Differential Dataflow using HOLD operator.
    ///
    /// This uses the actual `hold()` function from dd_runtime for proper DD semantics.
    /// Uses WASM-compatible execute_directly that doesn't require std::time::Instant.
    fn process_batch_with_hold(
        config: &DataflowConfig,
        events: &[DdEvent],
        start_time: u64,
        initial_states: &HashMap<String, DdValue>,
    ) -> (Vec<DocumentUpdate>, u64, HashMap<String, DdValue>) {
        use crate::platform::browser::engine_dd::dd_runtime::hold;
        use differential_dataflow::input::Input;

        let outputs = Arc::new(Mutex::new(Vec::new()));
        // Pre-populate with initial_states so transforms can resolve HoldRefs from previous batches
        let final_states = Arc::new(Mutex::new(initial_states.clone()));
        let outputs_clone = outputs.clone();
        let final_states_clone = final_states.clone();

        // Clone data for the closure
        let events: Vec<DdEvent> = events.to_vec();
        let initial_states_for_closure = initial_states.clone();
        let initial_states_for_merge = initial_states.clone();
        let config = config.clone();
        let num_events = events.len();

        Self::execute_directly_wasm(move |worker| {
            // Build dataflow for each HOLD in config
            let (mut link_input, probe) = worker.dataflow::<u64, _, _>(|scope| {
                // Create input collection for LINK events
                // Each link event is (LinkId, DdEventValue)
                let (link_input_handle, links) =
                    scope.new_collection::<(String, DdEventValue), isize>();

                // For each HOLD config, create a HOLD operator
                for hold_config in &config.holds {
                    let hold_id = hold_config.id.name().to_string();
                    let initial = initial_states_for_closure
                        .get(&hold_id)
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
                        zoon::println!("[DD Worker] HOLD {} listening for timer events", hold_id);
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
                                    matches!(event_value, DdEventValue::Text(t) if t == pattern)
                                }
                                EventFilter::TextStartsWith(prefix) => {
                                    matches!(event_value, DdEventValue::Text(t) if t.starts_with(prefix))
                                }
                            }
                        });

                    // Clone transform for the closure
                    let state_transform = hold_config.transform.clone();
                    // Clone final_states_clone for transforms that need to resolve HoldRefs
                    let transform_states = final_states_clone.clone();

                    // Apply HOLD operator with configured transform
                    let hold_output = hold(initial, &triggered_events, move |state, event| {
                        match &state_transform {
                            StateTransform::Increment => {
                                // Increment if numeric
                                match state {
                                    DdValue::Number(n) => DdValue::float(n.0 + 1.0),
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::BoolToggle => {
                                // Toggle boolean value
                                // Handle both Bool and Tagged { tag: "True"/"False" } variants
                                match state {
                                    DdValue::Bool(b) => DdValue::Bool(!*b),
                                    DdValue::Tagged { tag, .. } if tag.as_ref() == "True" => DdValue::Bool(false),
                                    DdValue::Tagged { tag, .. } if tag.as_ref() == "False" => DdValue::Bool(true),
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::SetTrue => {
                                // Set to true (for entering editing mode on double-click)
                                DdValue::Bool(true)
                            }
                            StateTransform::SetFalse => {
                                // Set to false (for exiting editing mode on Enter/Escape/blur)
                                DdValue::Bool(false)
                            }
                            StateTransform::ListAppend => {
                                // Append event text to list as a proper todo object
                                // Event format is "Enter:text" - extract the text after ":"
                                // DEPRECATED: Use ListAppendWithTemplate for proper Element AST items
                                match (state, event) {
                                    (DdValue::List(items), (_, DdEventValue::Text(text))) => {
                                        // Extract text after "Enter:" prefix
                                        let item_text = if let Some(stripped) = text.strip_prefix("Enter:") {
                                            stripped.trim()
                                        } else {
                                            text.trim()
                                        };
                                        // Only append non-empty text
                                        if item_text.is_empty() {
                                            return state.clone();
                                        }
                                        let mut new_items: Vec<DdValue> = items.to_vec();
                                        // Create proper todo object with title, completed, and editing fields
                                        let todo = DdValue::object([
                                            ("title", DdValue::text(item_text)),
                                            ("completed", DdValue::Bool(false)),
                                            ("editing", DdValue::Bool(false)),
                                        ]);
                                        new_items.push(todo);
                                        DdValue::List(Arc::new(new_items))
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListAppendWithTemplate { data_template, element_template, title_hold_field } => {
                                // Append item using template cloning with fresh IDs
                                // Both data and element templates use the SAME ID mapping,
                                // so the cloned element references the cloned data's HOLDs
                                match (state, event) {
                                    (DdValue::List(items), (_, DdEventValue::Text(text))) => {
                                        // Extract text after "Enter:" prefix
                                        let item_text = if let Some(stripped) = text.strip_prefix("Enter:") {
                                            stripped.trim()
                                        } else {
                                            text.trim()
                                        };
                                        // Only append non-empty text
                                        if item_text.is_empty() {
                                            return state.clone();
                                        }

                                        // Extract field-to-hold mapping from data template
                                        // e.g., {"title" -> "hold_18", "completed" -> "hold_20", "editing" -> "hold_19"}
                                        let field_to_hold = extract_field_to_hold_map(data_template);

                                        // Clone data template with fresh HoldRef/LinkRef IDs
                                        let mut hold_id_map: HashMap<String, String> = HashMap::new();
                                        let mut link_id_map: HashMap<String, String> = HashMap::new();
                                        let new_data_item = clone_template_with_fresh_ids(data_template, &mut hold_id_map, &mut link_id_map);

                                        // Create reverse mapping: old_hold_id -> field_name
                                        let hold_to_field: HashMap<String, String> = field_to_hold
                                            .iter()
                                            .map(|(field, hold)| (hold.clone(), field.clone()))
                                            .collect();

                                        // Register new HOLDs with initial values
                                        // Use field_name to determine what value to set
                                        for (old_id, new_id) in &hold_id_map {
                                            let field_name = hold_to_field.get(old_id).map(|s| s.as_str());
                                            let initial_value = match field_name {
                                                Some(name) if name == title_hold_field || name == "title" => {
                                                    // This is the title HOLD - set to input text
                                                    DdValue::text(item_text)
                                                }
                                                Some("completed") => {
                                                    // Completed defaults to false
                                                    DdValue::Bool(false)
                                                }
                                                Some("editing") => {
                                                    // Editing defaults to false
                                                    DdValue::Bool(false)
                                                }
                                                _ => {
                                                    // Unknown HOLD - default to Unit
                                                    DdValue::Unit
                                                }
                                            };
                                            zoon::println!("[DD Worker] Registering HOLD: {} -> {} = {:?} (field: {:?})",
                                                old_id, new_id, initial_value, field_name);
                                            update_hold_state_no_persist(new_id, initial_value);
                                        }

                                        zoon::println!("[DD Worker] ListAppendWithTemplate: Created item with {} HOLDs, {} LINKs",
                                            hold_id_map.len(), link_id_map.len());

                                        // Register dynamic link actions for editing mode
                                        // Find editing hold ID and todo_title_element link ID, then wire them
                                        zoon::println!("[DD Worker] Checking dynamic link registration: field_to_hold={:?}", field_to_hold);
                                        if let Some(editing_old_id) = field_to_hold.get("editing") {
                                            zoon::println!("[DD Worker] Found editing hold: {}", editing_old_id);
                                            if let Some(editing_new_id) = hold_id_map.get(editing_old_id) {
                                                zoon::println!("[DD Worker] Mapped to new editing hold: {}", editing_new_id);
                                                // Find todo_title_element link from data template's todo_elements
                                                if let DdValue::Object(obj) = data_template {
                                                    zoon::println!("[DD Worker] data_template is Object with keys: {:?}", obj.keys().collect::<Vec<_>>());
                                                    if let Some(DdValue::Object(todo_elements)) = obj.get("todo_elements") {
                                                        zoon::println!("[DD Worker] Found todo_elements with keys: {:?}", todo_elements.keys().collect::<Vec<_>>());
                                                        // Register double-click on todo_title_element → SetTrue(editing hold)
                                                        if let Some(DdValue::LinkRef(old_link_id)) = todo_elements.get("todo_title_element") {
                                                            zoon::println!("[DD Worker] Found todo_title_element link: {}", old_link_id);
                                                            if let Some(new_link_id) = link_id_map.get(old_link_id.as_ref()) {
                                                                zoon::println!("[DD Worker] Registering dynamic link: {} -> SetTrue({})", new_link_id, editing_new_id);
                                                                use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                                                                add_dynamic_link_action(new_link_id.clone(), DynamicLinkAction::SetTrue(editing_new_id.clone()));
                                                            } else {
                                                                zoon::println!("[DD Worker] ERROR: Could not find new link ID for {}", old_link_id);
                                                            }
                                                        } else {
                                                            zoon::println!("[DD Worker] ERROR: todo_title_element not found or not LinkRef");
                                                        }

                                                        // Register editing handler for editing_todo_title_element
                                                        // Handles: Enter → save title + exit, Escape → exit (no save), blur → exit (no save)
                                                        zoon::println!("[DD Worker] CHECKPOINT: About to check editing_todo_title_element");
                                                        if let Some(DdValue::LinkRef(old_edit_link_id)) = todo_elements.get("editing_todo_title_element") {
                                                            zoon::println!("[DD Worker] Found editing_todo_title_element link: {}", old_edit_link_id);
                                                            if let Some(new_edit_link_id) = link_id_map.get(old_edit_link_id.as_ref()) {
                                                                // Also need the title HOLD ID for saving
                                                                if let Some(title_old_id) = field_to_hold.get("title") {
                                                                    if let Some(title_new_id) = hold_id_map.get(title_old_id) {
                                                                        zoon::println!("[DD Worker] Registering EditingHandler: {} -> edit={}, title={}", new_edit_link_id, editing_new_id, title_new_id);
                                                                        use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                                                                        add_dynamic_link_action(new_edit_link_id.clone(), DynamicLinkAction::EditingHandler {
                                                                            editing_hold: editing_new_id.clone(),
                                                                            title_hold: title_new_id.clone(),
                                                                        });
                                                                    }
                                                                }
                                                            }
                                                        }

                                                        // Register RemoveListItem action for remove_todo_button
                                                        // Handles: click → remove this todo from the list
                                                        if let Some(DdValue::LinkRef(old_remove_link_id)) = todo_elements.get("remove_todo_button") {
                                                            zoon::println!("[DD Worker] Found remove_todo_button link: {}", old_remove_link_id);
                                                            if let Some(new_remove_link_id) = link_id_map.get(old_remove_link_id.as_ref()) {
                                                                // Use link_id instead of index (indices shift when items are removed)
                                                                zoon::println!("[DD Worker] Registering RemoveListItem: {} -> link_id={}", new_remove_link_id, new_remove_link_id);
                                                                use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                                                                add_dynamic_link_action(new_remove_link_id.clone(), DynamicLinkAction::RemoveListItem { link_id: new_remove_link_id.clone() });
                                                            }
                                                        }
                                                    } else {
                                                        zoon::println!("[DD Worker] ERROR: todo_elements not found or not Object");
                                                    }
                                                } else {
                                                    zoon::println!("[DD Worker] ERROR: data_template is not Object: {:?}", data_template);
                                                }
                                            } else {
                                                zoon::println!("[DD Worker] ERROR: Could not map editing hold {} to new ID", editing_old_id);
                                            }
                                        } else {
                                            zoon::println!("[DD Worker] No editing field in field_to_hold");
                                        }

                                        // If element template exists, clone it with SAME ID mapping
                                        // and add to "todos_elements" HOLD for unified rendering
                                        if let Some(elem_tmpl) = element_template {
                                            // Find the hover link in the element template BEFORE cloning
                                            // Element template is Tagged "Element" with fields including element.hovered
                                            let hover_link_old_id = if let DdValue::Tagged { fields, .. } = elem_tmpl {
                                                fields.get("element")
                                                    .and_then(|e| e.get("hovered"))
                                                    .and_then(|h| match h {
                                                        DdValue::LinkRef(id) => Some(id.to_string()),
                                                        _ => None,
                                                    })
                                            } else {
                                                None
                                            };

                                            // Find the WhileRef for hover in the element template's items
                                            // This is the WhileRef that has arms for True (delete button) and False (NoElement)
                                            let hover_hold_old_id = if let DdValue::Tagged { fields, .. } = elem_tmpl {
                                                find_hover_while_hold_id(fields.get("items"))
                                            } else {
                                                None
                                            };

                                            // Clone element template - reuses the same ID mapping
                                            let new_element = clone_template_with_fresh_ids(elem_tmpl, &mut hold_id_map, &mut link_id_map);

                                            // Register HoverState action if we found both the hover link and hold
                                            if let (Some(old_link), Some(old_hold)) = (hover_link_old_id, hover_hold_old_id) {
                                                if let (Some(new_link), Some(new_hold)) = (link_id_map.get(&old_link), hold_id_map.get(&old_hold)) {
                                                    zoon::println!("[DD Worker] Registering HoverState: {} -> {}", new_link, new_hold);
                                                    use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                                                    add_dynamic_link_action(new_link.clone(), DynamicLinkAction::HoverState(new_hold.clone()));
                                                    // Initialize hover hold to false
                                                    update_hold_state_no_persist(new_hold, DdValue::Bool(false));
                                                }
                                            }

                                            // Get current todos_elements list and append
                                            let current_elements = super::super::io::get_hold_value("todos_elements")
                                                .unwrap_or_else(|| DdValue::List(Arc::new(Vec::new())));
                                            if let DdValue::List(elems) = current_elements {
                                                let mut new_elems: Vec<DdValue> = elems.to_vec();
                                                new_elems.push(new_element);
                                                let new_count = new_elems.len();
                                                update_hold_state_no_persist("todos_elements", DdValue::List(Arc::new(new_elems)));
                                                zoon::println!("[DD Worker] Added element to todos_elements, now {} elements", new_count);
                                            }
                                        }

                                        let mut new_items: Vec<DdValue> = items.to_vec();
                                        new_items.push(new_data_item);
                                        DdValue::List(Arc::new(new_items))
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListAppendWithClear(clear_link_id) => {
                                // Combined list operations:
                                // - "Enter:text" → append text to list
                                // - Unit event from clear_link_id → clear list to empty
                                match (state, event) {
                                    (DdValue::List(items), (_, DdEventValue::Text(text))) => {
                                        // Handle append (Enter:text format)
                                        let item_text = if let Some(stripped) = text.strip_prefix("Enter:") {
                                            stripped.trim()
                                        } else {
                                            // Not an Enter: event - ignore
                                            return state.clone();
                                        };
                                        // Only append non-empty text
                                        if item_text.is_empty() {
                                            return state.clone();
                                        }
                                        let mut new_items: Vec<DdValue> = items.to_vec();
                                        new_items.push(DdValue::text(item_text));
                                        DdValue::List(Arc::new(new_items))
                                    }
                                    // Unit event from clear button → clear list
                                    // IMPORTANT: Only clear if the event is from the clear button's link,
                                    // not from other Unit events (like text input change handler)
                                    (DdValue::List(_), (link_id, DdEventValue::Unit)) if link_id == clear_link_id => {
                                        DdValue::List(Arc::new(Vec::new()))
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ClearText => {
                                // Clear to empty text when triggered
                                // Only clear if the event has valid text (non-empty Enter:xxx)
                                match event {
                                    (_, DdEventValue::Text(text)) => {
                                        let item_text = if let Some(stripped) = text.strip_prefix("Enter:") {
                                            stripped.trim()
                                        } else {
                                            ""
                                        };
                                        // Only clear if there's actual text to add
                                        if item_text.is_empty() {
                                            return state.clone();
                                        }
                                        DdValue::text("")
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ToggleListItemCompleted => {
                                // Toggle completed field of a specific list item
                                // Event format: "toggle:N" where N is the index
                                // Handles both text items (converts to object) and object items
                                match (state, event) {
                                    (DdValue::List(items), (_, DdEventValue::Text(text))) => {
                                        if let Some(index_str) = text.strip_prefix("toggle:") {
                                            if let Ok(index) = index_str.parse::<usize>() {
                                                if index < items.len() {
                                                    let mut new_items: Vec<DdValue> = items.to_vec();
                                                    new_items[index] = match &new_items[index] {
                                                        // Text item: convert to object with completed=true
                                                        DdValue::Text(title) => {
                                                            DdValue::todo_object(title, true)
                                                        }
                                                        // Object item: toggle completed field
                                                        DdValue::Object(obj) => {
                                                            let mut new_obj = (**obj).clone();
                                                            let completed = new_obj.get("completed")
                                                                .map(|c| matches!(c, DdValue::Bool(true)))
                                                                .unwrap_or(false);
                                                            new_obj.insert(Arc::from("completed"), DdValue::Bool(!completed));
                                                            DdValue::Object(Arc::new(new_obj))
                                                        }
                                                        // Other types: no change
                                                        _ => new_items[index].clone(),
                                                    };
                                                    return DdValue::List(Arc::new(new_items));
                                                }
                                            }
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListToggleAllCompleted => {
                                // Toggle All: if any item is uncompleted, set all to completed=true
                                // If all items are completed, set all to completed=false
                                match state {
                                    DdValue::List(items) => {
                                        if items.is_empty() {
                                            return state.clone();
                                        }
                                        // Helper to check if an item's completed value is true
                                        // Must resolve HoldRefs to get actual boolean values
                                        // NOTE: Always read from global HOLD_STATES since update_hold_state writes there
                                        let is_item_completed = |obj: &std::collections::BTreeMap<Arc<str>, DdValue>| -> bool {
                                            match obj.get("completed") {
                                                Some(DdValue::Bool(true)) => true,
                                                Some(DdValue::HoldRef(hold_id)) => {
                                                    // All HoldRefs are read from global HOLD_STATES
                                                    // (update_hold_state writes there, not to worker's states_map)
                                                    super::super::io::get_hold_value(hold_id.as_ref())
                                                        .map(|v| matches!(v, DdValue::Bool(true)))
                                                        .unwrap_or(false)
                                                }
                                                _ => false,
                                            }
                                        };
                                        // Check if all items are completed
                                        let all_completed = items.iter().all(|item| {
                                            match item {
                                                DdValue::Object(obj) => is_item_completed(obj),
                                                // Text items are considered uncompleted
                                                _ => false,
                                            }
                                        });
                                        // Toggle: if all completed, set all to uncompleted; otherwise set all to completed
                                        let new_completed = !all_completed;
                                        zoon::println!("[DD Transform] ListToggleAllCompleted: all_completed={}, setting to {}", all_completed, new_completed);
                                        let new_items: Vec<DdValue> = items.iter().map(|item| {
                                            match item {
                                                DdValue::Object(obj) => {
                                                    match obj.get("completed") {
                                                        Some(DdValue::HoldRef(hold_id)) => {
                                                            // For HoldRef: update the underlying hold, keep the ref
                                                            zoon::println!("[DD Transform] Updating hold {} to {}", hold_id, new_completed);
                                                            update_hold_state(hold_id.as_ref(), DdValue::Bool(new_completed));
                                                            // Keep the object unchanged (HoldRef stays intact)
                                                            item.clone()
                                                        }
                                                        _ => {
                                                            // For Bool or missing: set directly
                                                            let mut new_obj = (**obj).clone();
                                                            new_obj.insert(Arc::from("completed"), DdValue::Bool(new_completed));
                                                            DdValue::Object(Arc::new(new_obj))
                                                        }
                                                    }
                                                }
                                                // Convert text to object with completed state
                                                DdValue::Text(title) => {
                                                    DdValue::todo_object(title, new_completed)
                                                }
                                                // Other types: keep as-is
                                                _ => item.clone(),
                                            }
                                        }).collect();
                                        DdValue::List(Arc::new(new_items))
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::ListRemoveCompleted => {
                                // Remove all items where completed=true
                                // Need to resolve HoldRefs to get actual boolean values
                                let states_map = transform_states.lock().unwrap();
                                match state {
                                    DdValue::List(items) => {
                                        // Helper to check if an item is completed
                                        // NOTE: Dynamic holds (from toggle_hold_bool) are in global HOLD_STATES
                                        let is_item_completed = |item: &DdValue| -> bool {
                                            match item {
                                                DdValue::Object(obj) => {
                                                    match obj.get("completed") {
                                                        Some(DdValue::Bool(true)) => true,
                                                        Some(DdValue::HoldRef(hold_id)) => {
                                                            // Dynamic holds are updated in global HOLD_STATES
                                                            if hold_id.starts_with("dynamic_") {
                                                                super::super::io::get_hold_value(hold_id.as_ref())
                                                                    .map(|v| matches!(v, DdValue::Bool(true)))
                                                                    .unwrap_or(false)
                                                            } else {
                                                                states_map.get(hold_id.as_ref())
                                                                    .map(|v| matches!(v, DdValue::Bool(true)))
                                                                    .unwrap_or(false)
                                                            }
                                                        }
                                                        _ => false,
                                                    }
                                                }
                                                _ => false,
                                            }
                                        };

                                        // Filter the "todos" data list
                                        let filtered: Vec<DdValue> = items.iter()
                                            .filter(|item| !is_item_completed(item))
                                            .cloned()
                                            .collect();
                                        zoon::println!("[DD Transform] ListRemoveCompleted: Filtered {} -> {} items", items.len(), filtered.len());

                                        // Also filter "todos_elements" to remove corresponding elements
                                        // Each element has a checkbox with checked: HoldRef pointing to a completed hold
                                        // NOTE: Use global HOLD_STATES for dynamic holds since worker states_map may be stale
                                        if let Some(DdValue::List(elements)) = super::super::io::get_hold_value("todos_elements") {
                                            let filtered_elements: Vec<DdValue> = elements.iter()
                                                .filter(|element| {
                                                    // Find the checkbox's checked HoldRef
                                                    let completed_hold_id = find_checkbox_hold_id(element);
                                                    if let Some(hold_id) = completed_hold_id {
                                                        // For dynamic holds (toggle_hold_bool updates global HOLD_STATES),
                                                        // read from global state, not worker's stale states_map
                                                        let is_completed = if hold_id.starts_with("dynamic_") {
                                                            super::super::io::get_hold_value(&hold_id)
                                                                .map(|v| matches!(v, DdValue::Bool(true)))
                                                                .unwrap_or(false)
                                                        } else {
                                                            states_map.get(&hold_id)
                                                                .map(|v| matches!(v, DdValue::Bool(true)))
                                                                .unwrap_or(false)
                                                        };
                                                        zoon::println!("[DD Transform] Element hold {} is_completed={}, keeping={}", hold_id, is_completed, !is_completed);
                                                        !is_completed
                                                    } else {
                                                        true // Keep elements we can't determine
                                                    }
                                                })
                                                .cloned()
                                                .collect();
                                            zoon::println!("[DD Transform] ListRemoveCompleted: Filtered elements {} -> {}", elements.len(), filtered_elements.len());
                                            update_hold_state_no_persist("todos_elements", DdValue::List(Arc::new(filtered_elements)));
                                        }

                                        DdValue::List(Arc::new(filtered))
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::SetListItemEditing => {
                                // Set editing field of a list item
                                // Event format: "edit:N" (set editing=true) or "unedit:N" (set editing=false)
                                // HACK: TodoMVC-specific - used for dynamic todo double-click editing
                                match (state, event) {
                                    (DdValue::List(items), (_, DdEventValue::Text(text))) => {
                                        let (set_to_true, index_str) = if let Some(s) = text.strip_prefix("edit:") {
                                            (true, s)
                                        } else if let Some(s) = text.strip_prefix("unedit:") {
                                            (false, s)
                                        } else {
                                            return state.clone();
                                        };

                                        if let Ok(index) = index_str.parse::<usize>() {
                                            if index < items.len() {
                                                let mut new_items: Vec<DdValue> = items.to_vec();
                                                new_items[index] = match &new_items[index] {
                                                    DdValue::Object(obj) => {
                                                        let mut new_obj = (**obj).clone();
                                                        new_obj.insert(Arc::from("editing"), DdValue::Bool(set_to_true));
                                                        DdValue::Object(Arc::new(new_obj))
                                                    }
                                                    _ => new_items[index].clone(),
                                                };
                                                return DdValue::List(Arc::new(new_items));
                                            }
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::UpdateListItemTitle => {
                                // Update title field of a list item
                                // Event format: "save:N:new_title" where N is index
                                // HACK: TodoMVC-specific - used for saving edited todo title
                                match (state, event) {
                                    (DdValue::List(items), (_, DdEventValue::Text(text))) => {
                                        if let Some(rest) = text.strip_prefix("save:") {
                                            if let Some(colon_pos) = rest.find(':') {
                                                let index_str = &rest[..colon_pos];
                                                let new_title = rest[colon_pos + 1..].trim();

                                                if !new_title.is_empty() {
                                                    if let Ok(index) = index_str.parse::<usize>() {
                                                        if index < items.len() {
                                                            let mut new_items: Vec<DdValue> = items.to_vec();
                                                            new_items[index] = match &new_items[index] {
                                                                DdValue::Object(obj) => {
                                                                    let mut new_obj = (**obj).clone();
                                                                    new_obj.insert(Arc::from("title"), DdValue::text(new_title));
                                                                    // Also exit editing mode
                                                                    new_obj.insert(Arc::from("editing"), DdValue::Bool(false));
                                                                    DdValue::Object(Arc::new(new_obj))
                                                                }
                                                                _ => new_items[index].clone(),
                                                            };
                                                            return DdValue::List(Arc::new(new_items));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }
                            StateTransform::RemoveListItem => {
                                // Remove a specific list item by link_id (matches remove_todo_button LinkRef)
                                // Event format: "remove:LINK_ID" where LINK_ID identifies the item to remove
                                // HACK: TodoMVC-specific - used for delete button on hover
                                zoon::println!("[DD Transform] RemoveListItem: Received event {:?}, state has {} items",
                                    event, if let DdValue::List(items) = state { items.len() } else { 0 });
                                match (state, event) {
                                    (DdValue::List(items), (_, DdEventValue::Text(text))) => {
                                        zoon::println!("[DD Transform] RemoveListItem: Processing text event: {}", text);
                                        if let Some(link_id_str) = text.strip_prefix("remove:") {
                                            // Find the item whose remove_todo_button matches this link_id
                                            let index_to_remove = items.iter().position(|item| {
                                                if let DdValue::Object(obj) = item {
                                                    if let Some(DdValue::Object(todo_elements)) = obj.get("todo_elements") {
                                                        if let Some(DdValue::LinkRef(remove_link)) = todo_elements.get("remove_todo_button") {
                                                            return remove_link.as_ref() == link_id_str;
                                                        }
                                                    }
                                                }
                                                false
                                            });

                                            if let Some(index) = index_to_remove {
                                                zoon::println!("[DD Transform] RemoveListItem: Found item with link_id {} at index {}", link_id_str, index);
                                                let mut new_items: Vec<DdValue> = items.to_vec();
                                                new_items.remove(index);

                                                // Also remove from "todos_elements" - find by matching hover link or checkbox link
                                                // The element has a checkbox whose element.event.click LinkRef matches the todo's todo_checkbox
                                                // NOTE: todos_elements only contains dynamic todos, not initial ones
                                                if let Some(DdValue::List(elements)) = super::super::io::get_hold_value("todos_elements") {
                                                    // For dynamic todos, the link_id starts with "dynamic_link_"
                                                    // Find the element whose hover WhileRef matches our link_id pattern
                                                    let element_index = elements.iter().position(|element| {
                                                        // Check if this element's hover link matches
                                                        if let DdValue::Tagged { fields, .. } = element {
                                                            if let Some(DdValue::LinkRef(hover_link)) = fields.get("element").and_then(|e| e.get("hovered")) {
                                                                // The hover link ID pattern: e.g., "dynamic_link_1004" for a todo with remove button "dynamic_link_1001"
                                                                // They share the same base, so check if our link_id starts with "dynamic_link_"
                                                                // and extract the base number
                                                                if link_id_str.starts_with("dynamic_link_") {
                                                                    // Extract base number from remove button link
                                                                    if let Some(base) = link_id_str.strip_prefix("dynamic_link_") {
                                                                        if let Ok(remove_num) = base.parse::<u32>() {
                                                                            // The hover link is usually remove_num + 3 (based on ID allocation pattern)
                                                                            // But safer to check the element's items for our specific link_id
                                                                            if let Some(DdValue::List(elem_items)) = fields.get("items") {
                                                                                for item in elem_items.iter() {
                                                                                    // Look for the delete button WhileRef
                                                                                    if let DdValue::WhileRef { arms, .. } = item {
                                                                                        for (_, body) in arms.iter() {
                                                                                            if let DdValue::Tagged { fields: btn_fields, tag } = body {
                                                                                                if tag.as_ref() == "Element" {
                                                                                                    if let Some(DdValue::Object(btn_elem)) = btn_fields.get("element") {
                                                                                                        if let Some(DdValue::Object(event)) = btn_elem.get("event") {
                                                                                                            if let Some(DdValue::LinkRef(press_link)) = event.get("press") {
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
                                                        let mut new_elements: Vec<DdValue> = elements.to_vec();
                                                        new_elements.remove(elem_idx);
                                                        zoon::println!("[DD Transform] RemoveListItem: Also removed element at index {}, now {} elements", elem_idx, new_elements.len());
                                                        update_hold_state_no_persist("todos_elements", DdValue::List(Arc::new(new_elements)));
                                                    } else {
                                                        zoon::println!("[DD Transform] RemoveListItem: No matching element found in todos_elements for link_id {}", link_id_str);
                                                    }
                                                }

                                                return DdValue::List(Arc::new(new_items));
                                            } else {
                                                zoon::println!("[DD Transform] RemoveListItem: No item found with link_id {}", link_id_str);
                                            }
                                        }
                                        state.clone()
                                    }
                                    _ => state.clone(),
                                }
                            }
                        }
                    });

                    // Capture outputs for this HOLD
                    let outputs = outputs_clone.clone();
                    let final_states = final_states_clone.clone();
                    let hold_id_for_inspect = hold_config.id.name().to_string();
                    let hold_id_for_state = hold_config.id.name().to_string();
                    let should_persist = hold_config.persist;

                    hold_output.inspect(move |(state, time, diff)| {
                        if *diff > 0 {
                            // Record the new state
                            final_states
                                .lock()
                                .unwrap()
                                .insert(hold_id_for_state.clone(), state.clone());

                            // Create output update
                            // - hold_updates: persisted to localStorage
                            // - hold_state_updates: UI only, not persisted
                            let mut hold_updates = HashMap::new();
                            let mut hold_state_updates = HashMap::new();
                            if should_persist {
                                hold_updates.insert(hold_id_for_inspect.clone(), state.clone());
                            } else {
                                hold_state_updates.insert(hold_id_for_inspect.clone(), state.clone());
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
                if config.holds.is_empty() {
                    use differential_dataflow::operators::Count;

                    let count = links.map(|_| ()).count();
                    let outputs = outputs_clone.clone();

                    count.inspect(move |(((), total), time, diff)| {
                        if *diff > 0 {
                            outputs.lock().unwrap().push(DocumentUpdate {
                                document: DdValue::int(i64::try_from(*total).unwrap_or(0)),
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
                    DdEvent::Link { id, value } => {
                        link_input.insert((id.name().to_string(), value));
                    }
                    DdEvent::Timer { id, tick } => {
                        // Timer events are treated like link events with a special ID
                        // This allows timer-triggered HOLDs to work with the same logic
                        let timer_link_id = format!("__timer_{}", id.name());
                        link_input.insert((timer_link_id, DdEventValue::Unit));
                        zoon::println!("[DD Worker] Timer {} tick {}", id.name(), tick);
                    }
                    DdEvent::External { name: _, value: _ } => {
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

impl Default for DdWorker {
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
        assert_eq!(config.holds.len(), 1);
        assert_eq!(config.holds[0].id.name(), "counter");
        assert_eq!(config.holds[0].initial, DdValue::int(0));
        assert_eq!(config.holds[0].triggered_by.len(), 1);
        assert_eq!(config.holds[0].triggered_by[0].name(), "button.press");
    }

    #[test]
    fn test_dataflow_config_multiple_holds() {
        let config = DataflowConfig::new()
            .add_hold("counter1", DdValue::int(0), vec!["click1"])
            .add_hold("counter2", DdValue::int(10), vec!["click2", "click3"]);

        assert_eq!(config.holds.len(), 2);
        assert_eq!(config.holds[0].id.name(), "counter1");
        assert_eq!(config.holds[1].id.name(), "counter2");
        assert_eq!(config.holds[1].initial, DdValue::int(10));
        assert_eq!(config.holds[1].triggered_by.len(), 2);
    }

    #[test]
    fn test_process_batch_with_hold_simple() {
        // Test the synchronous batch processing directly (no async runtime needed)
        let config = DataflowConfig::counter("click");
        let events = vec![
            DdEvent::Link {
                id: LinkId::new("click"),
                value: DdEventValue::Unit,
            },
            DdEvent::Link {
                id: LinkId::new("click"),
                value: DdEventValue::Unit,
            },
        ];
        let initial_states = [("counter".to_string(), DdValue::int(0))]
            .into_iter()
            .collect();

        let (outputs, new_time, new_states) =
            DdWorker::process_batch_with_hold(&config, &events, 0, &initial_states);

        // Should have 2 outputs (one per click)
        assert_eq!(outputs.len(), 2);
        assert_eq!(new_time, 2);

        // Final state should be 2
        assert_eq!(new_states.get("counter"), Some(&DdValue::int(2)));
    }

    #[test]
    fn test_process_batch_filters_by_link_id() {
        // Test that HOLD only responds to its configured triggers
        let config = DataflowConfig::counter("button");
        let events = vec![
            DdEvent::Link {
                id: LinkId::new("button"),
                value: DdEventValue::Unit,
            },
            DdEvent::Link {
                id: LinkId::new("other"),  // Not "button", should be ignored
                value: DdEventValue::Unit,
            },
            DdEvent::Link {
                id: LinkId::new("button"),
                value: DdEventValue::Unit,
            },
        ];
        let initial_states = [("counter".to_string(), DdValue::int(0))]
            .into_iter()
            .collect();

        let (outputs, _, new_states) =
            DdWorker::process_batch_with_hold(&config, &events, 0, &initial_states);

        // Should only have 2 outputs (the "button" clicks)
        assert_eq!(outputs.len(), 2);
        // Final state should be 2, not 3
        assert_eq!(new_states.get("counter"), Some(&DdValue::int(2)));
    }
}
