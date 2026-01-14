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

pub use inputs::{EventInjector, fire_global_link, fire_global_link_with_text, fire_global_link_with_bool, fire_global_blur, fire_global_key_down, clear_editing_grace_period_for_link, set_global_dispatcher, clear_global_dispatcher, set_task_handle, clear_task_handle, set_output_listener_handle, clear_output_listener_handle, set_timer_handle, clear_timer_handle, add_router_mapping, clear_router_mappings, DynamicLinkAction, add_dynamic_link_action, clear_dynamic_link_actions, get_dynamic_link_action};
pub use outputs::{OutputObserver, update_hold_state, update_hold_state_no_persist, clear_hold_state, toggle_hold_bool, hold_states_signal, get_hold_value, get_all_hold_states, init_hold_state, load_persisted_hold_value, clear_dd_persisted_states, clear_hold_states_memory, set_checkbox_toggle_holds, clear_checkbox_toggle_holds, checkbox_toggle_holds_signal, get_checkbox_toggle_holds, get_unchecked_checkbox_count, set_filter_from_route, get_current_route, init_current_route, clear_current_route, set_list_var_name, get_list_var_name, clear_list_var_name, set_elements_field_name, get_elements_field_name, clear_elements_field_name, set_remove_event_path, get_remove_event_path, clear_remove_event_path, set_bulk_remove_event_path, get_bulk_remove_event_path, clear_bulk_remove_event_path, EditingEventBindings, set_editing_event_bindings, get_editing_event_bindings, clear_editing_event_bindings, ToggleEventBinding, add_toggle_event_binding, get_toggle_event_bindings, clear_toggle_event_bindings, GlobalToggleEventBinding, add_global_toggle_binding, get_global_toggle_bindings, clear_global_toggle_bindings};
