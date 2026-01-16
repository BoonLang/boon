//! Core DD module - Pure Differential Dataflow, no Zoon dependencies.
//!
//! # Anti-Cheat Architecture
//!
//! This module is designed with strict constraints:
//! - NO `Mutable<T>` from Zoon
//! - NO `RefCell<T>` for state
//! - NO synchronous state access (`.get()`, `.borrow()`)
//!
//! All state lives in DD collections. All observation is through async streams.
//!
//! # Module Structure
//!
//! - `types`: Core type definitions (Input, Output, CellId, LinkId, etc.)
//! - `operators`: DD operators for Boon constructs (hold, latest, etc.)
//! - `worker`: DD worker controller with event loop
//!
//! # Dependencies
//!
//! This module ONLY depends on:
//! - `timely` - Timely Dataflow
//! - `differential-dataflow` - Differential Dataflow
//! - `futures` - Async primitives (channels, streams)
//!
//! It does NOT depend on:
//! - `zoon` - UI framework (no Mutable, no Signal)
//! - Any UI-related types

pub mod dataflow;
pub mod guards;
pub mod operators;
pub mod types;
pub mod value;
pub mod worker;

pub use guards::{
    assert_in_dd_context, assert_not_in_dd_context, dd_operation_count, DdContextGuard,
};
pub use types::{
    channel, Event, EventValue, Input, Output, CellId, LinkId, TimerId,
    BoolTag, ElementTag, EventPayload,
    // Phase 8: DD-native LINK handling
    LinkAction, LinkCellMapping, EditingHandlerConfig,
};
pub use worker::{
    DataflowConfig, Worker, WorkerHandle, EventFilter, CellConfig,
    StateTransform, reconstruct_persisted_item, instantiate_fresh_item,
    // Generic template system exports
    FieldPath, ItemIdentitySpec, FieldInitializer, LinkActionSpec, LinkActionConfig,
    ListItemTemplate, InstantiatedItem, FieldUpdate,
    instantiate_template, get_at_path, get_link_ref_at_path, get_hold_ref_at_path, update_field_at_path,
};
// Note: DocumentUpdate is now internal-only (Phase 6 cleanup)
pub use dataflow::{
    DataflowBuilder, DdCellConfig, DdFirstHandle, DdOutput, DdTransform,
    run_dd_first_batch, merge_latest,
    // Phase 8: DD-native link action processing
    apply_link_action, mapping_matches_event,
};
