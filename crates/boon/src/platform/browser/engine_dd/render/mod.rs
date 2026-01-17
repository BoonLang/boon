//! Rendering layer for the DD engine.
//!
//! This module handles rendering DD values to Zoon UI elements.
//!
//! # Module Structure
//!
//! - `bridge` - Converts DD Value types to Zoon UI elements
//! - `list_adapter` - Phase 12: Converts DD collection diffs to SignalVec for incremental rendering

pub mod bridge;
pub mod list_adapter;

pub use bridge::*;
pub use list_adapter::{
    dd_diffs_to_vec_diff_stream, dd_captured_to_vec_diff_stream, process_diff_batch_sync,
    DdDiff, DdDiffEntry, DdDiffBatch, KeyedListAdapter,
};
