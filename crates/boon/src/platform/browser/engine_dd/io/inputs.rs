//! Input handling for DD events.
//!
//! This module provides the EventInjector which allows the bridge to
//! inject events (LINK fires, timer ticks) into the DD dataflow.

use super::super::core::{CellId, Event, EventValue, Input, Key, LinkId, TimerId};
#[allow(unused_imports)]
use super::super::dd_log;
use std::cell::RefCell;

// Global event dispatcher for browser environment (single-threaded)
thread_local! {
    static GLOBAL_DISPATCHER: RefCell<Option<EventInjector>> = RefCell::new(None); // ALLOWED: global dispatcher

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

/// Fire a link event via the global dispatcher.
/// Used by the bridge when button events occur.
///
/// ALL link events go to DD. No early returns.
/// DD's link_mappings handle event→action mapping.
/// IO layer may still do browser-specific things (grace periods) but doesn't prevent DD from seeing events.
pub fn fire_global_link(link_id: &str) {
    dd_log!("[DD fire_global_link] CALLED with link_id={}", link_id);

    // Fire to DD - DD's link_mappings will process via apply_link_action
    // DD has change detection, so duplicate processing is safe
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() {
            // ALLOWED: IO layer
            injector.fire_link_unit(LinkId::new(link_id));
            dd_log!("[DD Dispatcher] Fired link event: {}", link_id);
        } else {
            panic!(
                "[DD Dispatcher] Bug: No dispatcher set for link: {}",
                link_id
            );
        }
    });
}

/// Fire a link event with a text payload.
/// Used by text_input change events to propagate current text to DD.
pub fn fire_global_link_with_text(link_id: &str, text: String) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() {
            // ALLOWED: IO layer
            injector.fire_link_text(LinkId::new(link_id), text);
            dd_log!("[DD Dispatcher] Fired link with text: {}", link_id);
        } else {
            panic!(
                "[DD Dispatcher] Bug: No dispatcher set for link with text: {}",
                link_id
            );
        }
    });
}

/// Fire a link event with a boolean value.
/// Used by the bridge when hover state changes.
///
/// Always forward to DD (no dedup or IO reads).
pub fn fire_global_link_with_bool(link_id: &str, value: bool) {
    // Fire to DD - DD's link_mappings will handle via apply_link_action
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() {
            // ALLOWED: IO layer
            injector.fire_link_bool(LinkId::new(link_id), value);
            dd_log!(
                "[DD Dispatcher] Fired link with bool: {} value={}",
                link_id,
                value
            );
        } else {
            panic!(
                "[DD Dispatcher] Bug: No dispatcher set for link: {}",
                link_id
            );
        }
    });
}

/// Fire a key_down event with the key and optional text.
/// Used by text_input when a key is pressed.
/// The link_id should be for the key_down event.
pub fn fire_global_key_down(link_id: &str, key: Key, text: Option<String>) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() {
            // ALLOWED: IO layer
            injector.fire_link_key_down(LinkId::new(link_id), key.clone(), text);
            dd_log!(
                "[DD Dispatcher] Fired key_down event: {} key='{}'",
                link_id,
                key.as_str()
            );
        } else {
            panic!(
                "[DD Dispatcher] Bug: No dispatcher set for key_down: {}",
                link_id
            );
        }
    });
}

/// Fire an item-scoped link event (unit payload).
///
/// This is used for interactions originating from a concrete list item row.
pub fn fire_global_item_link(item_key: &str, action_id: &str) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() {
            injector.fire_item_link_unit(None, item_key, LinkId::new(action_id));
            dd_log!(
                "[DD Dispatcher] Fired item link event: item_key='{}' action='{}'",
                item_key,
                action_id
            );
        } else {
            panic!(
                "[DD Dispatcher] Bug: No dispatcher set for item link: item_key='{}' action='{}'",
                item_key, action_id
            );
        }
    });
}

/// Fire an item-scoped link event with text payload.
pub fn fire_global_item_link_with_text(item_key: &str, action_id: &str, text: String) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() {
            injector.fire_item_link_text(None, item_key, LinkId::new(action_id), text);
            dd_log!(
                "[DD Dispatcher] Fired item link with text: item_key='{}' action='{}'",
                item_key,
                action_id
            );
        } else {
            panic!(
                "[DD Dispatcher] Bug: No dispatcher set for item link text: item_key='{}' action='{}'",
                item_key,
                action_id
            );
        }
    });
}

/// Fire an item-scoped link event with bool payload.
pub fn fire_global_item_link_with_bool(item_key: &str, action_id: &str, value: bool) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() {
            injector.fire_item_link_bool(None, item_key, LinkId::new(action_id), value);
            dd_log!(
                "[DD Dispatcher] Fired item link with bool: item_key='{}' action='{}' value={}",
                item_key,
                action_id,
                value
            );
        } else {
            panic!(
                "[DD Dispatcher] Bug: No dispatcher set for item link bool: item_key='{}' action='{}'",
                item_key,
                action_id
            );
        }
    });
}

/// Fire an item-scoped key_down event.
pub fn fire_global_item_key_down(item_key: &str, action_id: &str, key: Key, text: Option<String>) {
    GLOBAL_DISPATCHER.with(|cell| {
        if let Some(injector) = cell.borrow().as_ref() {
            injector.fire_item_link_key_down(None, item_key, LinkId::new(action_id), key.clone(), text);
            dd_log!(
                "[DD Dispatcher] Fired item key_down: item_key='{}' action='{}' key='{}'",
                item_key,
                action_id,
                key.as_str()
            );
        } else {
            panic!(
                "[DD Dispatcher] Bug: No dispatcher set for item key_down: item_key='{}' action='{}'",
                item_key,
                action_id
            );
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

    /// Fire an item-scoped LINK event.
    pub fn fire_item_link(
        &self,
        list_cell_id: Option<CellId>,
        item_key: impl Into<std::sync::Arc<str>>,
        action_id: LinkId,
        value: EventValue,
    ) {
        self.event_input.send_or_drop(Event::ItemLink {
            list_cell_id,
            item_key: item_key.into(),
            action_id,
            value,
        });
    }

    /// Fire a LINK with unit value (most common case - button press).
    pub fn fire_link_unit(&self, id: LinkId) {
        self.fire_link(id, EventValue::Unit);
    }

    /// Fire an item-scoped LINK with unit value.
    pub fn fire_item_link_unit(
        &self,
        list_cell_id: Option<CellId>,
        item_key: impl Into<std::sync::Arc<str>>,
        action_id: LinkId,
    ) {
        self.fire_item_link(list_cell_id, item_key, action_id, EventValue::Unit);
    }

    /// Fire a LINK with text value (e.g., text input change).
    pub fn fire_link_text(&self, id: LinkId, text: impl Into<String>) {
        self.fire_link(id, EventValue::Text(text.into()));
    }

    /// Fire an item-scoped LINK with text value.
    pub fn fire_item_link_text(
        &self,
        list_cell_id: Option<CellId>,
        item_key: impl Into<std::sync::Arc<str>>,
        action_id: LinkId,
        text: impl Into<String>,
    ) {
        self.fire_item_link(
            list_cell_id,
            item_key,
            action_id,
            EventValue::Text(text.into()),
        );
    }

    /// Fire a key_down event.
    pub fn fire_link_key_down(&self, id: LinkId, key: Key, text: Option<String>) {
        self.fire_link(id, EventValue::key_down(key, text));
    }

    /// Fire an item-scoped key_down event.
    pub fn fire_item_link_key_down(
        &self,
        list_cell_id: Option<CellId>,
        item_key: impl Into<std::sync::Arc<str>>,
        action_id: LinkId,
        key: Key,
        text: Option<String>,
    ) {
        self.fire_item_link(
            list_cell_id,
            item_key,
            action_id,
            EventValue::key_down(key, text),
        );
    }

    /// Fire a LINK with boolean value (e.g., checkbox toggle).
    pub fn fire_link_bool(&self, id: LinkId, value: bool) {
        self.fire_link(id, EventValue::Bool(value));
    }

    /// Fire an item-scoped LINK with bool value.
    pub fn fire_item_link_bool(
        &self,
        list_cell_id: Option<CellId>,
        item_key: impl Into<std::sync::Arc<str>>,
        action_id: LinkId,
        value: bool,
    ) {
        self.fire_item_link(list_cell_id, item_key, action_id, EventValue::Bool(value));
    }

    /// Fire a timer tick event.
    pub fn fire_timer(&self, id: TimerId, tick: u64) {
        self.event_input.send_or_drop(Event::Timer { id, tick });
    }
}
