//! DOM event handling for DD bridge.
//!
//! This module provides handlers that convert DOM events to DD events
//! via the io layer's EventInjector.

use super::super::io::EventInjector;
use super::super::core::{LinkId, TimerId};

/// DOM event handler that converts browser events to DD events.
///
/// This is used by rendered elements to inject events into DD.
#[derive(Clone)]
pub struct DomEventHandler {
    injector: EventInjector,
}

impl DomEventHandler {
    /// Create a new DOM event handler with the given injector.
    pub fn new(injector: EventInjector) -> Self {
        Self { injector }
    }

    /// Handle a button press event.
    pub fn on_button_press(&self, link_id: LinkId) {
        self.injector.fire_link_unit(link_id);
    }

    /// Handle a text input change event.
    pub fn on_text_change(&self, link_id: LinkId, text: String) {
        self.injector.fire_link_text(link_id, text);
    }

    /// Handle a checkbox toggle event.
    pub fn on_checkbox_toggle(&self, link_id: LinkId, checked: bool) {
        self.injector.fire_link_bool(link_id, checked);
    }

    /// Handle a timer tick.
    pub fn on_timer_tick(&self, timer_id: TimerId, tick: u64) {
        self.injector.fire_timer(timer_id, tick);
    }

    /// Handle a key press event (e.g., Enter in text input).
    pub fn on_key_press(&self, link_id: LinkId, key: &str) {
        self.injector.fire_link_text(link_id, key.to_string());
    }
}
