//! DD I/O module - Input/Output channels for DD communication.
//!
//! # Anti-Cheat Architecture
//!
//! This module provides the I/O layer between the bridge and DD core:
//! - Receives DdInput channels from the bridge for event injection
//! - Provides DdOutput streams to the bridge for value observation
//!
//! # Dependencies
//!
//! This module depends on:
//! - `core` - Core types (DdInput, DdOutput, DdEvent)
//! - `futures` - Async primitives
//!
//! It does NOT depend on:
//! - `zoon` - UI framework
//! - `timely`/`differential-dataflow` - DD internals (that's core's job)

pub mod inputs;
pub mod outputs;

pub use inputs::{EventInjector, fire_global_link, fire_global_link_with_text, fire_global_link_with_bool, fire_global_key_down, set_global_dispatcher, clear_global_dispatcher, set_task_handle, clear_task_handle, set_output_listener_handle, clear_output_listener_handle, set_timer_handle, clear_timer_handle, add_router_mapping, clear_router_mappings};
pub use outputs::{OutputObserver, update_hold_state, update_hold_state_no_persist, hold_states_signal, get_hold_value, init_hold_state, load_persisted_hold_value, clear_dd_persisted_states, clear_hold_states_memory, set_checkbox_toggle_holds, clear_checkbox_toggle_holds, checkbox_toggle_holds_signal, get_checkbox_toggle_holds, get_unchecked_checkbox_count, TodoFilter, set_filter_from_route, selected_filter_signal, clear_selected_filter, get_current_route, init_current_route, clear_current_route};
