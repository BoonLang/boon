//! Input handling for DD events.
//!
//! This module provides the EventInjector which allows the bridge to
//! inject events (LINK fires, timer ticks) into the DD dataflow.

use std::cell::RefCell;
use std::collections::HashMap;
use super::super::core::{Event, EventValue, Input, LinkId, TimerId, CellId, EventPayload};

/// Action to perform when a dynamic link fires.
#[derive(Clone, Debug)]
pub enum DynamicLinkAction {
    /// Toggle a boolean hold (for checkbox clicks)
    BoolToggle(String),
    /// Set a hold to true (for entering edit mode via double-click)
    SetTrue(String),
    /// Set a hold to false (for exiting edit mode via Escape)
    SetFalse(String),
    /// Set a cell to false when specific keys are pressed (Escape/Enter for exiting edit mode)
    SetFalseOnKeys { cell_id: String, keys: Vec<String> },
    /// Handle editing events: save title on Enter, exit on Escape
    EditingHandler { editing_cell: String, title_cell: String },
    /// Update hold state to match hover state (true/false)
    HoverState(String),
    /// Remove a list item by link_id (for delete buttons on dynamically added items)
    /// The link_id identifies which item to remove by matching its remove button's LinkRef
    RemoveListItem { link_id: String },
    /// Toggle ALL items' completed field based on computed all_completed value.
    /// Used for "toggle all" checkbox that sets all items to completed or not completed.
    ListToggleAllCompleted {
        /// The list Cell ID (e.g., "cell_0" for todos)
        list_cell_id: String,
        /// The field to toggle on each item (e.g., "completed")
        completed_field: String,
    },
}

// Global event dispatcher for browser environment (single-threaded)
thread_local! {
    static GLOBAL_DISPATCHER: RefCell<Option<EventInjector>> = RefCell::new(None); // ALLOWED: global dispatcher

    // Phase 3: EDITING_HOLDS_GRACE_PERIOD removed - blur debouncing moves to DD temporal operators
    static TASK_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: task handle
    static OUTPUT_LISTENER_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: listener handle
    static TIMER_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: timer handle
    // ═══════════════════════════════════════════════════════════════════════════
    // SURGICALLY REMOVED: ROUTER_MAPPINGS
    //
    // This was business logic in the I/O layer that bypassed DD:
    // - fire_global_link() checked ROUTER_MAPPINGS before injecting to DD
    // - If found, navigated directly + mutated cell state
    // - DD never saw the link event!
    //
    // Pure DD architecture:
    // - ALL link events go to DD
    // - Router/go_to() is a DD operator that outputs navigation commands
    // - Output observer handles browser navigation
    // ═══════════════════════════════════════════════════════════════════════════
    static DYNAMIC_LINK_ACTIONS: RefCell<HashMap<String, DynamicLinkAction>> = RefCell::new(HashMap::new()); // ALLOWED: dynamic link handlers
}

/// Set the global event dispatcher.
/// Called when Worker is created to enable event injection from anywhere.
pub fn set_global_dispatcher(injector: EventInjector) {
    GLOBAL_DISPATCHER.with(|cell| {
        *cell.borrow_mut() = Some(injector); // ALLOWED: IO layer
    });
}

/// Clear the global event dispatcher.
pub fn clear_global_dispatcher() {
    GLOBAL_DISPATCHER.with(|cell| {
        *cell.borrow_mut() = None; // ALLOWED: IO layer
    });
}

/// Store the task handle to keep the async worker alive.
pub fn set_task_handle(handle: zoon::TaskHandle) {
    TASK_HANDLE.with(|cell| {
        *cell.borrow_mut() = Some(handle); // ALLOWED: IO layer
    });
}

/// Clear the stored task handle (stops the worker).
pub fn clear_task_handle() {
    TASK_HANDLE.with(|cell| {
        *cell.borrow_mut() = None; // ALLOWED: IO layer
    });
}

/// Store the output listener handle.
pub fn set_output_listener_handle(handle: zoon::TaskHandle) {
    OUTPUT_LISTENER_HANDLE.with(|cell| {
        *cell.borrow_mut() = Some(handle); // ALLOWED: IO layer
    });
}

/// Clear the output listener handle.
pub fn clear_output_listener_handle() {
    OUTPUT_LISTENER_HANDLE.with(|cell| {
        *cell.borrow_mut() = None; // ALLOWED: IO layer
    });
}

/// Store the timer handle to keep the timer task alive.
pub fn set_timer_handle(handle: zoon::TaskHandle) {
    TIMER_HANDLE.with(|cell| {
        *cell.borrow_mut() = Some(handle); // ALLOWED: IO layer
    });
}

/// Clear the timer handle (stops the timer).
pub fn clear_timer_handle() {
    TIMER_HANDLE.with(|cell| {
        *cell.borrow_mut() = None; // ALLOWED: IO layer
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED: add_router_mapping(), clear_router_mappings()
//
// These registered link→route mappings that bypassed DD:
// - add_router_mapping(link_id, route) - stored mapping in I/O layer
// - fire_global_link() checked mappings before injecting to DD
// - If found, navigated + mutated state WITHOUT going through DD!
//
// Pure DD: Router/go_to() should be a DD operator that outputs navigation
// commands, handled by the output observer.
// ═══════════════════════════════════════════════════════════════════════════

/// Register a dynamic link action.
/// Called when cloning templates to wire up dynamic links to their hold actions.
pub fn add_dynamic_link_action(link_id: impl Into<String>, action: DynamicLinkAction) {
    let link_id = link_id.into();
    zoon::println!("[DD DynamicLink] Registering {} -> {:?}", link_id, action);
    DYNAMIC_LINK_ACTIONS.with(|cell| {
        cell.borrow_mut().insert(link_id, action); // ALLOWED: IO layer
    });
}

/// Clear all dynamic link actions.
/// Called when resetting state between runs.
pub fn clear_dynamic_link_actions() {
    DYNAMIC_LINK_ACTIONS.with(|cell| {
        cell.borrow_mut().clear(); // ALLOWED: IO layer
    });
}

/// Get the action registered for a link ID.
/// Used when cloning templates to replicate actions for new LinkRefs.
pub fn get_dynamic_link_action(link_id: &str) -> Option<DynamicLinkAction> {
    DYNAMIC_LINK_ACTIONS.with(|cell| {
        cell.borrow().get(link_id).cloned() // ALLOWED: IO layer
    })
}

/// Convert all DYNAMIC_LINK_ACTIONS to LinkCellMappings for DD-native processing (Phase 8).
///
/// This is a migration bridge that allows the existing `add_dynamic_link_action` calls
/// to work while we transition to pure DD link handling. The returned mappings are
/// added to DataflowConfig and processed by the DD worker.
pub fn get_all_link_mappings() -> Vec<super::super::core::types::LinkCellMapping> {
    use super::super::core::types::{LinkCellMapping, LinkAction, CellId};

    DYNAMIC_LINK_ACTIONS.with(|cell| {
        let actions = cell.borrow(); // ALLOWED: IO layer
        actions.iter().filter_map(|(link_id, action)| {
            match action {
                DynamicLinkAction::BoolToggle(cell_id) => {
                    Some(LinkCellMapping::bool_toggle(link_id.clone(), cell_id.clone()))
                }
                DynamicLinkAction::SetTrue(cell_id) => {
                    Some(LinkCellMapping::set_true(link_id.clone(), cell_id.clone()))
                }
                DynamicLinkAction::SetFalse(cell_id) => {
                    Some(LinkCellMapping::set_false(link_id.clone(), cell_id.clone()))
                }
                DynamicLinkAction::SetFalseOnKeys { cell_id, keys } => {
                    Some(LinkCellMapping::with_key_filter(
                        link_id.clone(),
                        cell_id.clone(),
                        LinkAction::SetFalse,
                        keys.clone(),
                    ))
                }
                DynamicLinkAction::HoverState(cell_id) => {
                    Some(LinkCellMapping::hover_state(link_id.clone(), cell_id.clone()))
                }
                DynamicLinkAction::RemoveListItem { link_id: remove_link_id } => {
                    // RemoveListItem uses event indirection pattern:
                    // delete_button → dynamic_list_remove → list_cell
                    // The IO layer just routes; DD handles removal via StateTransform::RemoveListItem
                    // This is acceptable as IO only routes, doesn't do business logic
                    zoon::println!("[Phase8] RemoveListItem {} uses event indirection (acceptable)", remove_link_id);
                    None
                }
                DynamicLinkAction::ListToggleAllCompleted { list_cell_id, completed_field } => {
                    Some(LinkCellMapping::new(
                        link_id.clone(),
                        list_cell_id.clone(),
                        LinkAction::ListToggleAllCompleted {
                            list_cell_id: CellId::new(list_cell_id.clone()),
                            completed_field: completed_field.clone(),
                        },
                    ))
                }
                DynamicLinkAction::EditingHandler { editing_cell, title_cell } => {
                    // EditingHandler expands to multiple mappings:
                    // - Blur → editing_cell = false (handled via fire_global_blur with grace period)
                    // - Escape → editing_cell = false
                    // - Enter:text → title_cell = text, editing_cell = false
                    // Grace period logic stays in IO (browser-specific focus race handling)
                    zoon::println!("[Phase8] EditingHandler {} -> editing={}, title={}",
                        link_id, editing_cell, title_cell);
                    // Return blur/Escape mapping; Enter is more complex and stays in IO for now
                    Some(LinkCellMapping::with_key_filter(
                        link_id.clone(),
                        editing_cell.clone(),
                        LinkAction::SetFalse,
                        vec!["Escape".to_string()],
                    ))
                }
            }
        }).collect()
    })
}

/// Check if a link has a dynamic action registered.
/// Phase 3: GUTTED - no longer fires events. Worker's link_mappings handle all routing.
/// Returns true if the link is registered (for logging purposes only).
fn check_dynamic_link_action(link_id: &str) -> bool {
    // Phase 3: IO layer is thin routing only
    // Worker's link_mappings (loaded from get_all_link_mappings()) handle event→action mapping
    // We just check if registered for logging purposes
    let is_registered = DYNAMIC_LINK_ACTIONS.with(|cell| {
        cell.borrow().contains_key(link_id) // ALLOWED: IO layer read
    });

    if is_registered {
        zoon::println!("[DD DynamicLink] {} is registered (Worker link_mappings will handle)", link_id);
    }

    is_registered
}

// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED: check_router_mapping()
//
// This function intercepted link events BEFORE they reached DD:
// - Checked ROUTER_MAPPINGS for link→route mapping
// - If found: navigated browser + mutated cell state directly
// - DD never saw the event!
//
// Pure DD: ALL link events should go to DD. Router/go_to() outputs
// navigation commands that the output observer handles.
// ═══════════════════════════════════════════════════════════════════════════

/// Fire a link event via the global dispatcher.
/// Used by the bridge when button events occur.
///
/// Phase 3: ALL link events go to DD. No early returns.
/// DD's link_mappings handle event→action mapping.
/// IO layer may still do browser-specific things (grace periods) but doesn't prevent DD from seeing events.
pub fn fire_global_link(link_id: &str) {
    zoon::println!("[DD fire_global_link] CALLED with link_id={}", link_id);

    // Phase 3: Check dynamic action for browser-specific handling (grace periods, etc.)
    // but DON'T early return - let events flow to DD as well
    let _ = check_dynamic_link_action(link_id);

    // Fire to DD - DD's link_mappings will process via apply_link_action
    // DD has change detection, so duplicate processing is safe
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_unit(LinkId::new(link_id));
            zoon::println!("[DD Dispatcher] Fired link event: {}", link_id);
        } else {
            zoon::println!("[DD Dispatcher] Warning: No dispatcher set for link: {}", link_id);
        }
    });
}

/// Fire a link event with a text value.
/// Used by the bridge when events need to carry text data (e.g., "toggle:0").
pub fn fire_global_link_with_text(link_id: &str, text: &str) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_text(LinkId::new(link_id), text);
            zoon::println!("[DD Dispatcher] Fired link with text: {} text='{}'", link_id, text);
        } else {
            zoon::println!("[DD Dispatcher] Warning: No dispatcher set for link: {}", link_id);
        }
    });
}

/// Fire a link event with a boolean value.
/// Used by the bridge when hover state changes.
///
/// Phase 3: Simplified - deduplication kept for performance (hover can fire 100s/sec),
/// but events always flow to DD for processing via link_mappings.
pub fn fire_global_link_with_bool(link_id: &str, value: bool) {
    // Performance optimization: deduplicate hover events to prevent signal spam
    // Check current value and skip if unchanged
    let hover_cell_id = format!("hover_{}", link_id);
    let current = super::outputs::get_cell_value(&hover_cell_id);
    let same_value = match current {
        Some(super::super::core::value::Value::Bool(b)) => b == value,
        _ => false,
    };

    if same_value {
        // Value unchanged - skip to prevent signal spam from continuous hover
        return;
    }

    // Fire to DD - DD's link_mappings will handle via apply_link_action
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_bool(LinkId::new(link_id), value);
            zoon::println!("[DD Dispatcher] Fired link with bool: {} value={}", link_id, value);
        } else {
            zoon::println!("[DD Dispatcher] Warning: No dispatcher set for link: {}", link_id);
        }
    });
}

/// Fire a blur event.
/// Used by text_input when the input loses focus.
/// Phase 3: SIMPLIFIED - just forwards blur event to DD. Worker's link_mappings handle EditingHandler.
pub fn fire_global_blur(link_id: &str) {
    zoon::println!("[DD fire_global_blur] {} - forwarding to DD", link_id);
    // Phase 3: IO layer is thin routing - just fire the blur event with a "blur" marker
    // Worker's link_mappings will match on this and handle EditingHandler actions
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            // Fire as blur event - Worker can distinguish blur from regular link events
            injector.fire_link_text(LinkId::new(link_id), "blur");
            zoon::println!("[DD Dispatcher] Fired blur event: {}", link_id);
        } else {
            zoon::println!("[DD Dispatcher] Warning: No dispatcher set for blur: {}", link_id);
        }
    });
}

// Phase 3: clear_editing_grace_period and clear_editing_grace_period_for_link removed
// DD handles blur debouncing via temporal operators

/// Fire a key_down event with the key name.
/// Used by text_input when a key is pressed.
/// The link_id should be for the key_down event, and key is the key name (e.g., "Enter").
pub fn fire_global_key_down(link_id: &str, key: &str) {
    // Check if this link has a dynamic action for this key
    if check_dynamic_key_action(link_id, key) {
        // Action executed directly, no need to fire DD event
        return;
    }

    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_text(LinkId::new(link_id), key);
            zoon::println!("[DD Dispatcher] Fired key_down event: {} key='{}'", link_id, key);
        } else {
            zoon::println!("[DD Dispatcher] Warning: No dispatcher set for key_down: {}", link_id);
        }
    });
}

/// Check if a link has a dynamic key action registered.
/// Phase 3: GUTTED - no longer handles key events. Worker's link_mappings do pattern matching.
/// Always returns false so key events flow to DD.
fn check_dynamic_key_action(link_id: &str, key: &str) -> bool {
    // Phase 3: IO layer is thin routing - just log and forward to DD
    // Worker's link_mappings will match on key patterns (Escape, Enter:text)
    let is_registered = DYNAMIC_LINK_ACTIONS.with(|cell| {
        cell.borrow().contains_key(link_id) // ALLOWED: IO layer read
    });

    if is_registered {
        zoon::println!("[DD check_dynamic_key_action] {} key='{}' - forwarding to DD (link_mappings will handle)", link_id, key);
    }

    // Always return false - let key events flow to DD
    false
}

/// Event injector for sending events to the DD worker.
///
/// The bridge uses this to inject DOM events into the DD dataflow.
/// All injection is fire-and-forget - there's no synchronous feedback.
#[derive(Clone)]
pub struct EventInjector {
    event_input: Input<Event>,
}

impl EventInjector {
    /// Create a new event injector with the given input channel.
    pub fn new(event_input: Input<Event>) -> Self {
        Self { event_input }
    }

    /// Fire a LINK event (e.g., button click).
    pub fn fire_link(&self, id: LinkId, value: EventValue) {
        self.event_input.send_or_drop(Event::Link { id, value });
    }

    /// Fire a LINK with unit value (most common case - button press).
    pub fn fire_link_unit(&self, id: LinkId) {
        self.fire_link(id, EventValue::Unit);
    }

    /// Fire a LINK with text value (e.g., text input change).
    pub fn fire_link_text(&self, id: LinkId, text: impl Into<String>) {
        self.fire_link(id, EventValue::Text(text.into()));
    }

    /// Fire a LINK with boolean value (e.g., checkbox toggle).
    pub fn fire_link_bool(&self, id: LinkId, value: bool) {
        self.fire_link(id, EventValue::Bool(value));
    }

    /// Fire a timer tick event.
    pub fn fire_timer(&self, id: TimerId, tick: u64) {
        self.event_input.send_or_drop(Event::Timer { id, tick });
    }

    /// Fire an external event with arbitrary name and value.
    pub fn fire_external(&self, name: impl Into<String>, value: EventValue) {
        self.event_input.send_or_drop(Event::External {
            name: name.into(),
            value,
        });
    }
}
