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
pub mod collection_ops;
pub mod operators;
pub mod types;
pub mod value;
pub mod worker;

pub use guards::{
    assert_in_dd_context, assert_not_in_dd_context, dd_operation_count, DdContextGuard,
};
pub use types::{
    channel, Event, EventValue, Input, Output, CellId, LinkId, TimerId, Key,
    BoolTag, ElementTag, EventFilter, ITEM_KEY_FIELD, ROUTE_CHANGE_LINK_ID,
    // DD-native LINK handling
    LinkAction, LinkCellMapping,
};
pub use worker::{
    DataflowConfig, Worker, WorkerHandle, CellConfig,
    StateTransform, reconstruct_persisted_item, instantiate_fresh_item,
    // Generic template system exports
    FieldPath, ItemIdentitySpec, FieldInitializer, LinkActionSpec, LinkActionConfig,
    ListItemTemplate, InstantiatedItem, FieldUpdate, ListAppendBinding,
    instantiate_template, remap_link_mappings_for_item, get_at_path, get_link_ref_at_path, get_hold_ref_at_path, update_field_at_path,
};
// Note: DocumentUpdate is now internal-only
pub use dataflow::{
    DdCellConfig, DdCollectionConfig, DdOutput, DdTransform,
    run_dd_first_batch, merge_latest,
    // DD-native link action processing
    apply_link_action, mapping_matches_event,
};
// DD Collection types for incremental list operations
pub use value::{CollectionId, CollectionHandle, TemplateValue};
// Collection op config shared between worker/dataflow
pub use collection_ops::{CollectionOp, CollectionOpConfig};
// CellUpdate for pure DD operations
pub use value::CellUpdate;
