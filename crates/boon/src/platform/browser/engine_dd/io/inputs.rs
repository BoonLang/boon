//! Input handling for DD events.
//!
//! This module provides the EventInjector which allows the bridge to
//! inject events (LINK fires, timer ticks) into the DD dataflow.

use std::cell::RefCell;
use super::super::core::{Event, EventValue, Input, Key, LinkId, TimerId};

// Global event dispatcher for browser environment (single-threaded)
thread_local! {
    static GLOBAL_DISPATCHER: RefCell<Option<EventInjector>> = RefCell::new(None); // ALLOWED: global dispatcher

    // Phase 3: EDITING_HOLDS_GRACE_PERIOD removed - blur debouncing moves to DD temporal operators
    static TASK_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: task handle
    static OUTPUT_LISTENER_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: listener handle
    static TIMER_HANDLE: RefCell<Option<zoon::TaskHandle>> = RefCell::new(None); // ALLOWED: timer handle
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

    // Fire to DD - DD's link_mappings will process via apply_link_action
    // DD has change detection, so duplicate processing is safe
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_unit(LinkId::new(link_id));
            zoon::println!("[DD Dispatcher] Fired link event: {}", link_id);
        } else {
            panic!("[DD Dispatcher] Bug: No dispatcher set for link: {}", link_id);
        }
    });
}

/// Fire a link event with a text payload.
/// Used by text_input change events to propagate current text to DD.
pub fn fire_global_link_with_text(link_id: &str, text: String) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_text(LinkId::new(link_id), text);
            zoon::println!("[DD Dispatcher] Fired link with text: {}", link_id);
        } else {
            panic!("[DD Dispatcher] Bug: No dispatcher set for link with text: {}", link_id);
        }
    });
}

/// Fire a link event with a boolean value.
/// Used by the bridge when hover state changes.
///
/// Phase 3: Simplified - always forward to DD (no dedup or IO reads).
pub fn fire_global_link_with_bool(link_id: &str, value: bool) {
    // Fire to DD - DD's link_mappings will handle via apply_link_action
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_bool(LinkId::new(link_id), value);
            zoon::println!("[DD Dispatcher] Fired link with bool: {} value={}", link_id, value);
        } else {
            panic!("[DD Dispatcher] Bug: No dispatcher set for link: {}", link_id);
        }
    });
}

/// Fire a blur event.
/// Used by text_input when the input loses focus.
/// Phase 3: SIMPLIFIED - just forwards blur event to DD. Worker's link_mappings handle edit actions.
pub fn fire_global_blur(link_id: &str) {
    zoon::println!("[DD fire_global_blur] {} - forwarding to DD", link_id);
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_unit(LinkId::new(link_id));
            zoon::println!("[DD Dispatcher] Fired blur event: {}", link_id);
        } else {
            panic!("[DD Dispatcher] Bug: No dispatcher set for blur: {}", link_id);
        }
    });
}

// Phase 3: clear_editing_grace_period and clear_editing_grace_period_for_link removed
// DD handles blur debouncing via temporal operators

/// Fire a key_down event with the key and optional text.
/// Used by text_input when a key is pressed.
/// The link_id should be for the key_down event.
pub fn fire_global_key_down(link_id: &str, key: Key, text: Option<String>) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() { // ALLOWED: IO layer
            injector.fire_link_key_down(LinkId::new(link_id), key.clone(), text);
            zoon::println!("[DD Dispatcher] Fired key_down event: {} key='{}'", link_id, key.as_str());
        } else {
            panic!("[DD Dispatcher] Bug: No dispatcher set for key_down: {}", link_id);
        }
    });
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

    /// Fire a key_down event.
    pub fn fire_link_key_down(&self, id: LinkId, key: Key, text: Option<String>) {
        self.fire_link(id, EventValue::key_down(key, text));
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
