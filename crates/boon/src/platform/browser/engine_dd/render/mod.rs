//! Rendering layer for the DD engine.
//!
//! This module handles rendering DD values to Zoon UI elements.
//!
//! # Module Structure
//!
//! - `bridge` - Converts DD Value types to Zoon UI elements
//! - `list_adapter` - Phase 12: (removed) diff adapter was unused in bridge

pub mod bridge;

pub use bridge::*;
