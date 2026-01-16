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

pub use inputs::{EventInjector, fire_global_link, fire_global_link_with_text, fire_global_link_with_bool, fire_global_blur, fire_global_key_down, clear_editing_grace_period_for_link, set_global_dispatcher, clear_global_dispatcher, set_task_handle, clear_task_handle, set_output_listener_handle, clear_output_listener_handle, set_timer_handle, clear_timer_handle, add_router_mapping, clear_router_mappings, DynamicLinkAction, add_dynamic_link_action, clear_dynamic_link_actions, get_dynamic_link_action, get_all_link_mappings};
// ═══════════════════════════════════════════════════════════════════════════
// Phase 7 Cell Functions:
//   - init_cell, init_cell_with_persist: INITIALIZATION ONLY (before DD starts)
//   - sync_cell_from_dd, sync_cell_from_dd_with_persist: DD output → CELL_STATES
//   - update_cell_no_persist: Internal IO layer updates (e.g., routing state)
//
// SURGICALLY REMOVED (Phase 6.5):
//   - update_cell, clear_cell, toggle_cell_bool
// These old functions directly mutated CELL_STATES, bypassing DD dataflow.
// All runtime updates MUST flow through DD events now.
// ═══════════════════════════════════════════════════════════════════════════
pub use outputs::{OutputObserver, cell_states_signal, get_cell_value, get_all_cell_states, load_persisted_cell_value, clear_dd_persisted_states, clear_cells_memory, init_cell, init_cell_with_persist, sync_cell_from_dd, sync_cell_from_dd_with_persist, update_cell_no_persist, set_checkbox_toggle_holds, clear_checkbox_toggle_holds, checkbox_toggle_holds_signal, get_checkbox_toggle_holds, add_text_clear_cell, is_text_clear_cell, clear_text_clear_cells, set_filter_from_route, get_current_route, init_current_route, clear_current_route, set_list_var_name, get_list_var_name, set_elements_field_name, set_remove_event_path, get_remove_event_path, clear_remove_event_path, set_bulk_remove_event_path, get_bulk_remove_event_path, clear_bulk_remove_event_path, EditingEventBindings, set_editing_event_bindings, get_editing_event_bindings, clear_editing_event_bindings, ToggleEventBinding, add_toggle_event_binding, get_toggle_event_bindings, clear_toggle_event_bindings, GlobalToggleEventBinding, add_global_toggle_binding, get_global_toggle_bindings, clear_global_toggle_bindings, set_text_input_key_down_link, get_text_input_key_down_link, clear_text_input_key_down_link, enter_while_preeval, exit_while_preeval, set_list_clear_link, get_list_clear_link, clear_list_clear_link, set_has_template_list, get_has_template_list, clear_has_template_list};
// DEAD CODE REMOVED: get_unchecked_checkbox_count, clear_list_var_name, get_elements_field_name, clear_elements_field_name
