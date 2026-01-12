//! DD Reactive Evaluation - Timer and engine management module.
//!
//! This module provides management functions for the DD engine,
//! including invalidation of running timers and full engine stop.

use super::io::{clear_timer_handle, clear_output_listener_handle, clear_task_handle, clear_global_dispatcher, clear_router_mappings};

/// Invalidate all DD engine timers.
///
/// Called before clearing persisted state to prevent race conditions
/// where old timers re-save values before the new run starts.
///
/// This stops the JavaScript timer task by dropping its handle.
pub fn invalidate_timers() {
    clear_timer_handle();
    zoon::println!("[DD Timer] Timer invalidated");
}

/// Stop the entire DD engine.
///
/// Called when clearing state to ensure no background tasks can
/// re-persist values after the clear.
///
/// Stops: timer, output listener, worker task, event dispatcher, router.
pub fn stop_dd_engine() {
    clear_timer_handle();
    clear_output_listener_handle();
    clear_task_handle();
    clear_global_dispatcher();
    clear_router_mappings();
    zoon::println!("[DD Engine] Engine stopped");
}
