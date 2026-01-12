//! Rendering DD outputs to Zoon elements.
//!
//! This module provides the main DdBridge that:
//! 1. Receives DD output streams
//! 2. Converts them to Zoon signals
//! 3. Renders DdValue to Zoon elements

use super::super::io::EventInjector;
use super::events::DomEventHandler;

// Note: Zoon imports will be added when we integrate with the actual rendering.
// For now, this is a stub that shows the intended structure.

/// The main DD bridge that connects DD outputs to Zoon rendering.
///
/// # Anti-Cheat Design
///
/// The bridge ONLY receives:
/// - `OutputObserver<DdValue>` - async stream of DD outputs
/// - `EventInjector` - for sending DOM events back to DD
///
/// It has NO access to:
/// - DD internals (collections, arrangements)
/// - Synchronous state reads
/// - Mutable<T> or RefCell<T>
pub struct DdBridge {
    /// Event handler for DOM events
    event_handler: DomEventHandler,
    // Output observer will be consumed to create the render stream
    // output_observer: OutputObserver<DdValue>,
}

impl DdBridge {
    /// Create a new DD bridge.
    ///
    /// # Arguments
    ///
    /// * `injector` - Event injector for sending DOM events to DD
    ///
    /// The output observer is provided separately when rendering starts.
    pub fn new(injector: EventInjector) -> Self {
        Self {
            event_handler: DomEventHandler::new(injector),
        }
    }

    /// Get the event handler for use in rendered elements.
    pub fn event_handler(&self) -> &DomEventHandler {
        &self.event_handler
    }

    // Note: The actual rendering implementation will be added when we
    // integrate with the existing dd_bridge.rs rendering logic.
    // The key difference is that we'll use `signal::from_stream()`
    // ALLOWED: doc comment - instead of `render_signal()` with trigger_render().
    //
    // pub fn render<T>(self, output_observer: OutputObserver<DdValue>) -> impl Element {
    //     let stream = output_observer.stream();
    //     let signal = signal::from_stream(stream);
    //     El::new().child_signal(signal.map(|value| render_dd_value(&value)))
    // }
}
