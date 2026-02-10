//! DD I/O module - Input/Output channels for DD communication.
//!
//! # Anti-Cheat Architecture
//!
//! This module provides the I/O layer between the bridge and DD core:
//! - Receives Input channels from the bridge for event injection
//! - Provides Output streams to the bridge for value observation
//!
//! # Dependencies
//!
//! This module depends on:
//! - `core` - Core types (Input, Output, Event)
//! - `futures` - Async primitives
//!
//! It does NOT depend on:
//! - `zoon` - UI framework
//! - `timely`/`differential-dataflow` - DD internals (that's core's job)

pub mod inputs;
pub mod outputs;

pub use inputs::{EventInjector, fire_global_link, fire_global_link_with_text, fire_global_link_with_bool, fire_global_key_down, set_global_dispatcher, clear_global_dispatcher, set_task_handle, clear_task_handle, set_output_listener_handle, clear_output_listener_handle, set_timer_handle, clear_timer_handle};
pub use outputs::{
    // Cell state functions
    cell_signal, list_signal_vec, is_list_cell,
    get_cell_value,
    load_persisted_cell_value, load_persisted_cell_value_with_collections,
    load_persisted_list_items, load_persisted_list_items_with_collections,
    PersistedListItems, PersistedValue,
    clear_dd_persisted_states, clear_cells_memory,
    sync_cell_from_dd, sync_cell_from_dd_with_persist,
    sync_list_state_from_dd, sync_list_state_from_dd_with_persist,
    // Route state
    get_current_route, init_current_route, clear_current_route, navigate_to_route,
};
