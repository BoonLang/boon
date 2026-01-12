//! Input handling for DD events.
//!
//! This module provides the EventInjector which allows the bridge to
//! inject events (LINK fires, timer ticks) into the DD dataflow.

use std::cell::RefCell;
use std::collections::HashMap;
use super::super::core::{DdEvent, DdEventValue, DdInput, LinkId, TimerId};

// Global event dispatcher for browser environment (single-threaded)
thread_local! {
    static GLOBAL_DISPATCHER: RefCell<Option<EventInjector>> = RefCell::new(None); // ALLOWED: global dispatcher
    static TASK_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: task handle
    static OUTPUT_LISTENER_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: listener handle
    static TIMER_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: timer handle
    static ROUTER_MAPPINGS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new()); // ALLOWED: router state
}

/// Set the global event dispatcher.
/// Called when DdWorker is created to enable event injection from anywhere.
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

/// Check if a link has a router mapping and navigate if so.
/// Returns true if navigation occurred.
fn check_router_mapping(link_id: &str) -> bool {
    ROUTER_MAPPINGS.with(|cell| {
        if let Some(route) = cell.borrow().get(link_id) { // ALLOWED: IO layer
            // Update filter state based on route (for reactive todo list filtering)
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
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_bool(LinkId::new(link_id), value);
            zoon::println!("[DD Dispatcher] Fired link with bool: {} value={}", link_id, value);
        } else {
            zoon::println!("[DD Dispatcher] Warning: No dispatcher set for link: {}", link_id);
        }
    });
}

/// Fire a key_down event with the key name.
/// Used by text_input when a key is pressed.
/// The link_id should be for the key_down event, and key is the key name (e.g., "Enter").
pub fn fire_global_key_down(link_id: &str, key: &str) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_text(LinkId::new(link_id), key);
            zoon::println!("[DD Dispatcher] Fired key_down event: {} key='{}'", link_id, key);
        } else {
            zoon::println!("[DD Dispatcher] Warning: No dispatcher set for key_down: {}", link_id);
        }
    });
}

/// Event injector for sending events to the DD worker.
///
/// The bridge uses this to inject DOM events into the DD dataflow.
/// All injection is fire-and-forget - there's no synchronous feedback.
#[derive(Clone)]
pub struct EventInjector {
    event_input: DdInput<DdEvent>,
}

impl EventInjector {
    /// Create a new event injector with the given input channel.
    pub fn new(event_input: DdInput<DdEvent>) -> Self {
        Self { event_input }
    }

    /// Fire a LINK event (e.g., button click).
    pub fn fire_link(&self, id: LinkId, value: DdEventValue) {
        self.event_input.send_or_drop(DdEvent::Link { id, value });
    }

    /// Fire a LINK with unit value (most common case - button press).
    pub fn fire_link_unit(&self, id: LinkId) {
        self.fire_link(id, DdEventValue::Unit);
    }

    /// Fire a LINK with text value (e.g., text input change).
    pub fn fire_link_text(&self, id: LinkId, text: impl Into<String>) {
        self.fire_link(id, DdEventValue::Text(text.into()));
    }

    /// Fire a LINK with boolean value (e.g., checkbox toggle).
    pub fn fire_link_bool(&self, id: LinkId, value: bool) {
        self.fire_link(id, DdEventValue::Bool(value));
    }

    /// Fire a timer tick event.
    pub fn fire_timer(&self, id: TimerId, tick: u64) {
        self.event_input.send_or_drop(DdEvent::Timer { id, tick });
    }

    /// Fire an external event with arbitrary name and value.
    pub fn fire_external(&self, name: impl Into<String>, value: DdEventValue) {
        self.event_input.send_or_drop(DdEvent::External {
            name: name.into(),
            value,
        });
    }
}
