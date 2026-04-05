//! Virtual time clock for deterministic testing.
//!
//! Stub implementation - the engine_v2 dependency was removed.
//! Full implementation pending engine_v2 availability.

/// Stub slot ID placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotId {
    pub index: u32,
    pub generation: u32,
}
