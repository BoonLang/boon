//! Tokio-based runtime for CLI.
//!
//! Stub implementation - the engine_v2 dependency was removed.
//! Full implementation pending engine_v2 availability.

/// Stub event loop placeholder.
pub struct EventLoop;

impl EventLoop {
    pub fn new() -> Self {
        Self
    }

    pub fn run_tick(&mut self) {}

    pub fn dirty_nodes(&self) -> &[()] {
        &[]
    }
}

/// Run the event loop until quiescent (no pending work).
pub fn run_until_quiescent(_event_loop: &mut EventLoop) {
    // Stub: no-op until engine_v2 is available
}
