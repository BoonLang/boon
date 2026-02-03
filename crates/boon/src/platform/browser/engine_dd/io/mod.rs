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

// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED (Phase 11a): add_router_mapping, clear_router_mappings
// These allowed I/O layer to intercept link events before DD, bypassing dataflow.
// All link events now go to DD; Router/go_to() outputs navigation commands.
// ═══════════════════════════════════════════════════════════════════════════
pub use inputs::{EventInjector, fire_global_link, fire_global_link_with_text, fire_global_link_with_bool, fire_global_blur, fire_global_key_down, set_global_dispatcher, clear_global_dispatcher, set_task_handle, clear_task_handle, set_output_listener_handle, clear_output_listener_handle, set_timer_handle, clear_timer_handle};
// ═══════════════════════════════════════════════════════════════════════════
// Phase 7 Cell Functions:
//   - sync_cell_from_dd, sync_cell_from_dd_with_persist: DD output → CELL_STATES
//
// SURGICALLY REMOVED (Phase 6.5):
//   - update_cell, clear_cell, toggle_cell_bool
// These old functions directly mutated CELL_STATES, bypassing DD dataflow.
// All runtime updates MUST flow through DD events now.
// ═══════════════════════════════════════════════════════════════════════════
// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED: cell_states_signal
// This was the global broadcast anti-pattern that fired on ANY cell change.
// Use cell_signal(cell_id) or cells_signal(cell_ids) for granular updates.
// ═══════════════════════════════════════════════════════════════════════════
// ============================================================================
// Phase 7.3: Cleaned up exports - deleted setter/clear functions migrated to DataflowConfig
// Kept: getters (delegate to config), route state, and IO functions
// DELETED: CHECKBOX_TOGGLE_HOLDS registry (2026-01-18) - was dead code
// ============================================================================
pub use outputs::{
    // Cell state functions
    OutputObserver, cell_signal, cells_signal, list_signal_vec, is_list_cell,
    get_cell_value, get_all_cell_states,
    load_persisted_cell_value, load_persisted_cell_value_with_collections,
    load_persisted_list_items, load_persisted_list_items_with_collections,
    PersistedListItems, PersistedValue,
    clear_dd_persisted_states, clear_cells_memory,
    sync_cell_from_dd, sync_cell_from_dd_with_persist,
    sync_list_state_from_dd, sync_list_state_from_dd_with_persist,
    // DELETED: checkbox toggle holds functions - were set but never read (dead code)
    // Route state
    get_current_route, init_current_route, clear_current_route,
    // Editing/toggle bindings removed; link actions use LinkCellMapping
    // Evaluator-only config fields now live in DataflowConfig (no IO registry)
};
// SURGICALLY REMOVED: enter_while_preeval, exit_while_preeval (Phase 11b - broadcast anti-pattern fix made them unnecessary)
// DEAD CODE REMOVED: get_unchecked_checkbox_count, clear_list_var_name, get_elements_field_name, clear_elements_field_name, set_elements_field_name, set_list_var_name, get_list_var_name
