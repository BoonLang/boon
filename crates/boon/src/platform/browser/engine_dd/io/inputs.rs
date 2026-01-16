//! Input handling for DD events.
//!
//! This module provides the EventInjector which allows the bridge to
//! inject events (LINK fires, timer ticks) into the DD dataflow.

use std::cell::RefCell;
use std::collections::HashMap;
use super::super::core::{Event, EventValue, Input, LinkId, TimerId};

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

    // Track editing holds that were just enabled - blur events should be ignored for these
    // until the input has been properly focused. This prevents spurious blur events during
    // the WhileRef arm switch when transitioning from label to text_input.
    static EDITING_HOLDS_GRACE_PERIOD: RefCell<std::collections::HashSet<String>> = RefCell::new(std::collections::HashSet::new()); // ALLOWED: global state
    static TASK_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: task handle
    static OUTPUT_LISTENER_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: listener handle
    static TIMER_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: timer handle
    static ROUTER_MAPPINGS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new()); // ALLOWED: router state
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

/// Add a router mapping (link_id → route).
/// Used for LATEST → Router/go_to patterns.
pub fn add_router_mapping(link_id: impl Into<String>, route: impl Into<String>) {
    let link_id = link_id.into();
    let route = route.into();
    zoon::println!("[DD Router] Mapping {} -> {}", link_id, route);
    ROUTER_MAPPINGS.with(|cell| {
        cell.borrow_mut().insert(link_id, route); // ALLOWED: IO layer
    });
}

/// Clear all router mappings.
pub fn clear_router_mappings() {
    ROUTER_MAPPINGS.with(|cell| {
        cell.borrow_mut().clear(); // ALLOWED: IO layer
    });
}

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
    use super::super::core::types::{LinkCellMapping, LinkAction, CellId, LinkId};

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

/// Check if a link has a dynamic action and execute it.
/// Returns true if the action was handled.
fn check_dynamic_link_action(link_id: &str) -> bool {
    DYNAMIC_LINK_ACTIONS.with(|cell| {
        if let Some(action) = cell.borrow().get(link_id).cloned() { // ALLOWED: IO layer
            match action {
                DynamicLinkAction::SetTrue(cell_id) => {
                    zoon::println!("[DD DynamicLink] {} -> SetTrue({})", link_id, cell_id);
                    // Add to grace period - blur events for this editing_cell should be ignored
                    // until the focus race settles (use timeout instead of relying on focus event)
                    let cell_id_for_timeout = cell_id.clone();
                    EDITING_HOLDS_GRACE_PERIOD.with(|cell| {
                        cell.borrow_mut().insert(cell_id.clone()); // ALLOWED: IO layer
                    });
                    // NOTE: Grace period is cleared ONLY by key events (Enter/Escape) in check_dynamic_key_action.
                    let _ = cell_id_for_timeout; // Mark as used
                    // Phase 7: Fire to DD instead of direct update
                    // Format: "cell_set_true:{cell_id}" - DD worker processes this
                    GLOBAL_DISPATCHER.with(|disp_cell| {
                        if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                            injector.fire_link_text(LinkId::new("dd_cell_update"), format!("set_true:{}", cell_id));
                            zoon::println!("[DD Phase7] Fired dd_cell_update with set_true:{}", cell_id);
                        }
                    });
                }
                DynamicLinkAction::SetFalse(cell_id) => {
                    zoon::println!("[DD DynamicLink] {} -> SetFalse({})", link_id, cell_id);
                    // Phase 7: Fire to DD instead of direct update
                    GLOBAL_DISPATCHER.with(|disp_cell| {
                        if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                            injector.fire_link_text(LinkId::new("dd_cell_update"), format!("set_false:{}", cell_id));
                            zoon::println!("[DD Phase7] Fired dd_cell_update with set_false:{}", cell_id);
                        }
                    });
                }
                DynamicLinkAction::BoolToggle(cell_id) => {
                    zoon::println!("[DD DynamicLink] {} -> BoolToggle({})", link_id, cell_id);
                    // Phase 7: Fire to DD instead of direct update
                    GLOBAL_DISPATCHER.with(|disp_cell| {
                        if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                            injector.fire_link_text(LinkId::new("dd_cell_update"), format!("bool_toggle:{}", cell_id));
                            zoon::println!("[DD Phase7] Fired dd_cell_update with bool_toggle:{}", cell_id);
                        }
                    });
                }
                DynamicLinkAction::SetFalseOnKeys { .. } => {
                    // SetFalseOnKeys is handled by check_dynamic_key_action, not here
                    // Return false to let the event propagate to DD worker
                    return false;
                }
                DynamicLinkAction::EditingHandler { .. } => {
                    // EditingHandler is only handled by:
                    // - fire_global_blur() for blur events
                    // - check_dynamic_key_action() for key events
                    // Change events should propagate to DD worker, not exit edit mode
                    return false;
                }
                DynamicLinkAction::HoverState(_) => {
                    // HoverState is handled by fire_global_link_with_bool, not here
                    return false;
                }
                DynamicLinkAction::RemoveListItem { link_id: remove_link_id } => {
                    // Fire the link-id based remove event to trigger list item removal
                    zoon::println!("[DD DynamicLink] {} -> RemoveListItem(link_id={})", link_id, remove_link_id);
                    // Fire via global dispatcher with link_id format (not index, since indices shift)
                    GLOBAL_DISPATCHER.with(|disp_cell| {
                        if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                            injector.fire_link_text(LinkId::new("dynamic_list_remove"), format!("remove:{}", remove_link_id));
                            zoon::println!("[DD Dispatcher] Fired dynamic_list_remove with remove:{}", remove_link_id);
                        }
                    });
                }
                DynamicLinkAction::ListToggleAllCompleted { list_cell_id, completed_field } => {
                    // Toggle ALL items' completed field - fire DD event
                    zoon::println!("[DD DynamicLink] {} -> ListToggleAllCompleted(list={}, field={})", link_id, list_cell_id, completed_field);
                    // Phase 7: Fire to DD instead of direct mutation
                    GLOBAL_DISPATCHER.with(|disp_cell| {
                        if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                            injector.fire_link_text(LinkId::new("dd_cell_update"), format!("list_toggle_all:{}:{}", list_cell_id, completed_field));
                            zoon::println!("[DD Phase7] Fired dd_cell_update with list_toggle_all:{}:{}", list_cell_id, completed_field);
                        }
                    });
                }
            }
            true
        } else {
            false
        }
    })
}

/// Check if a link has a router mapping and navigate if so.
/// Returns true if navigation occurred.
fn check_router_mapping(link_id: &str) -> bool {
    ROUTER_MAPPINGS.with(|cell| {
        if let Some(route) = cell.borrow().get(link_id) { // ALLOWED: IO layer
            // Update filter state based on route (for reactive list filtering)
            super::outputs::set_filter_from_route(route);

            #[cfg(target_arch = "wasm32")]
            {
                use zoon::*;
                if let Ok(history) = window().history() {
                    let _ = history.push_state_with_url(&zoon::JsValue::NULL, "", Some(route));
                    zoon::println!("[DD Router] Navigated to {}", route);
                    // Fire popstate event to trigger Router/route() updates
                    // Use regular Event since PopStateEvent may not be exported from web_sys
                    if let Ok(event) = zoon::web_sys::Event::new("popstate") {
                        let _ = window().dispatch_event(&event);
                    }
                }
            }
            true
        } else {
            false
        }
    })
}

/// Fire a link event via the global dispatcher.
/// Used by the bridge when button events occur.
pub fn fire_global_link(link_id: &str) {
    zoon::println!("[DD fire_global_link] CALLED with link_id={}", link_id);
    // Check if this link has a dynamic action (for dynamic list item editing)
    if check_dynamic_link_action(link_id) {
        // Action executed directly, no need to fire DD event
        return;
    }

    // Check if this link has a router mapping (LATEST → Router/go_to pattern)
    if check_router_mapping(link_id) {
        // Navigation occurred, no need to fire DD event
        return;
    }

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
pub fn fire_global_link_with_bool(link_id: &str, value: bool) {
    // Check if this link has a HoverState action (for dynamic list item hover)
    let handled = DYNAMIC_LINK_ACTIONS.with(|cell| {
        if let Some(action) = cell.borrow().get(link_id).cloned() { // ALLOWED: IO layer
            if let DynamicLinkAction::HoverState(cell_id) = action {
                // Skip update if value hasn't changed - prevents signal spam on continuous hover
                let current = super::outputs::get_cell_value(&cell_id);
                let same_value = match current {
                    Some(super::super::core::value::Value::Bool(b)) => b == value,
                    _ => false,
                };
                if same_value {
                    return true; // Already handled, skip redundant update
                }
                zoon::println!("[DD Hover] {} -> update {} to {}", link_id, cell_id, value);
                // Phase 7: Fire to DD instead of direct mutation
                GLOBAL_DISPATCHER.with(|disp_cell| {
                    if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                        injector.fire_link_text(LinkId::new("dd_cell_update"), format!("hover_set:{}:{}", cell_id, value));
                        zoon::println!("[DD Phase7] Fired dd_cell_update with hover_set:{}:{}", cell_id, value);
                    }
                });
                return true;
            }
        }
        false
    });

    if handled {
        return;
    }

    // Also update synthetic hover hold for statically-defined hover WhileRefs
    // The evaluator creates holds named "hover_{link_id}" for element.hovered |> WHILE patterns
    let hover_cell_id = format!("hover_{}", link_id);
    // Skip update if value hasn't changed - prevents signal spam on continuous hover
    let current = super::outputs::get_cell_value(&hover_cell_id);
    let same_value = match current {
        Some(super::super::core::value::Value::Bool(b)) => b == value,
        _ => false,
    };
    if !same_value {
        // Phase 7: Fire to DD instead of direct mutation
        GLOBAL_DISPATCHER.with(|disp_cell| {
            if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                injector.fire_link_text(LinkId::new("dd_cell_update"), format!("hover_set:{}:{}", hover_cell_id, value));
                zoon::println!("[DD Phase7] Fired dd_cell_update with hover_set:{}:{}", hover_cell_id, value);
            }
        });
        zoon::println!("[DD Hover] {} -> synthetic hold {} = {}", link_id, hover_cell_id, value);

        // Only fire to dispatcher when value actually changed
        GLOBAL_DISPATCHER.with(|cell| {
            if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
                injector.fire_link_bool(LinkId::new(link_id), value);
                zoon::println!("[DD Dispatcher] Fired link with bool: {} value={}", link_id, value);
            } else {
                zoon::println!("[DD Dispatcher] Warning: No dispatcher set for link: {}", link_id);
            }
        });
    }
}

/// Fire a blur event.
/// Used by text_input when the input loses focus.
/// This specifically handles EditingHandler to exit edit mode on blur.
pub fn fire_global_blur(link_id: &str) {
    // Check if this link has an EditingHandler
    DYNAMIC_LINK_ACTIONS.with(|cell| {
        if let Some(action) = cell.borrow().get(link_id).cloned() { // ALLOWED: IO layer
            if let DynamicLinkAction::EditingHandler { editing_cell, .. } = action {
                // Check if this editing_cell is in grace period (just enabled, input hasn't been focused yet)
                let in_grace_period = EDITING_HOLDS_GRACE_PERIOD.with(|grace| {
                    grace.borrow().contains(&editing_cell) // ALLOWED: IO layer
                });
                if in_grace_period {
                    // Ignore spurious blur during WhileRef arm switch
                    zoon::println!("[DD Blur] {} -> IGNORED (grace period for {})", link_id, editing_cell);
                    return;
                }
                // Exit edit mode without saving on blur
                // IMPORTANT: Boon uses Tagged booleans, not Rust bools
                zoon::println!("[DD Blur] {} -> exit edit (no save)", link_id);
                // Phase 7: Fire to DD instead of direct mutation
                GLOBAL_DISPATCHER.with(|disp_cell| {
                    if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                        injector.fire_link_text(LinkId::new("dd_cell_update"), format!("set_false:{}", editing_cell));
                        zoon::println!("[DD Phase7] Fired dd_cell_update with set_false:{}", editing_cell);
                    }
                });
                return;
            }
        }
        // For non-EditingHandler links, fire as a regular link event
        fire_global_link(link_id);
    });
}

/// Clear the grace period for an editing hold (call when input receives focus)
pub fn clear_editing_grace_period(editing_cell: &str) {
    EDITING_HOLDS_GRACE_PERIOD.with(|cell| {
        cell.borrow_mut().remove(editing_cell); // ALLOWED: IO layer
    });
    zoon::println!("[DD Focus] Cleared grace period for {}", editing_cell);
}

/// Clear the grace period for a link's editing hold (call when input receives focus)
/// This looks up the EditingHandler for the given blur_link_id and clears its grace period.
pub fn clear_editing_grace_period_for_link(blur_link_id: &str) {
    DYNAMIC_LINK_ACTIONS.with(|cell| {
        if let Some(DynamicLinkAction::EditingHandler { editing_cell, .. }) = cell.borrow().get(blur_link_id).cloned() { // ALLOWED: IO layer
            EDITING_HOLDS_GRACE_PERIOD.with(|grace| {
                grace.borrow_mut().remove(&editing_cell); // ALLOWED: IO layer
            });
            zoon::println!("[DD Focus] Cleared grace period for {} (via link {})", editing_cell, blur_link_id);
        }
    });
}

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

/// Check if a link has a dynamic key action and execute it.
/// Returns true if the action was handled.
fn check_dynamic_key_action(link_id: &str, key: &str) -> bool {
    zoon::println!("[DD check_dynamic_key_action] link_id='{}', key='{}'", link_id, key);
    DYNAMIC_LINK_ACTIONS.with(|cell| {
        let actions = cell.borrow(); // ALLOWED: IO layer
        zoon::println!("[DD check_dynamic_key_action] DYNAMIC_LINK_ACTIONS has {} entries", actions.len());
        if let Some(action) = actions.get(link_id).cloned() {
            zoon::println!("[DD check_dynamic_key_action] Found action for {}: {:?}", link_id, action);
            match action {
                DynamicLinkAction::SetFalseOnKeys { cell_id, keys } => {
                    if keys.iter().any(|k| k == key) {
                        zoon::println!("[DD DynamicLink] {} key='{}' -> SetFalse({})", link_id, key, cell_id);
                        // Phase 7: Fire to DD instead of direct mutation
                        GLOBAL_DISPATCHER.with(|disp_cell| {
                            if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                                injector.fire_link_text(LinkId::new("dd_cell_update"), format!("set_false:{}", cell_id));
                                zoon::println!("[DD Phase7] Fired dd_cell_update with set_false:{}", cell_id);
                            }
                        });
                        return true;
                    }
                }
                DynamicLinkAction::EditingHandler { editing_cell, title_cell } => {
                    // Handle editing key events
                    if key == "Escape" {
                        // Exit edit mode without saving
                        // Clear grace period first (user is explicitly exiting)
                        EDITING_HOLDS_GRACE_PERIOD.with(|cell| {
                            cell.borrow_mut().remove(&editing_cell); // ALLOWED: IO layer
                        });
                        zoon::println!("[DD DynamicLink] {} key='Escape' -> exit edit (no save)", link_id);
                        // Phase 7: Fire to DD instead of direct mutation
                        GLOBAL_DISPATCHER.with(|disp_cell| {
                            if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                                injector.fire_link_text(LinkId::new("dd_cell_update"), format!("set_false:{}", editing_cell));
                                zoon::println!("[DD Phase7] Fired dd_cell_update with set_false:{}", editing_cell);
                            }
                        });
                        return true;
                    } else if let Some(text) = key.strip_prefix("Enter:") {
                        // Save title and exit edit mode
                        // Clear grace period first (user is explicitly saving)
                        EDITING_HOLDS_GRACE_PERIOD.with(|cell| {
                            cell.borrow_mut().remove(&editing_cell); // ALLOWED: IO layer
                        });
                        zoon::println!("[DD DynamicLink] {} key='Enter' -> save '{}' to {}, exit edit", link_id, text, title_cell);
                        // Phase 7: Fire to DD instead of direct mutation
                        // Fire text update first, then set editing to false
                        GLOBAL_DISPATCHER.with(|disp_cell| {
                            if let Some(injector) = disp_cell.borrow().as_ref() { // ALLOWED: IO layer
                                // Encode text with base64 to avoid delimiter issues
                                let encoded_text = text.replace(':', "::"); // Escape colons
                                injector.fire_link_text(LinkId::new("dd_cell_update"), format!("set_text:{}:{}", title_cell, encoded_text));
                                zoon::println!("[DD Phase7] Fired dd_cell_update with set_text:{}:{}", title_cell, text);
                                injector.fire_link_text(LinkId::new("dd_cell_update"), format!("set_false:{}", editing_cell));
                                zoon::println!("[DD Phase7] Fired dd_cell_update with set_false:{}", editing_cell);
                            }
                        });
                        return true;
                    }
                }
                // Other action types don't handle key events
                _ => {}
            }
        }
        false
    })
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
