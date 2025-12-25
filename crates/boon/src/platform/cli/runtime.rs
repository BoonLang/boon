//! Tokio-based runtime for CLI.

use crate::engine_v2::event_loop::EventLoop;

/// Run the event loop until quiescent (no pending work).
pub fn run_until_quiescent(event_loop: &mut EventLoop) {
    // Run ticks until no more dirty nodes
    loop {
        let had_dirty = !event_loop.dirty_nodes.is_empty();
        event_loop.run_tick();

        if !had_dirty && event_loop.dirty_nodes.is_empty() {
            break;
        }
    }
}
