//! Differential Dataflow engine for Boon.
//!
//! This engine uses Timely Dataflow and Differential Dataflow for
//! reactive evaluation with incremental computation capabilities.
//!
//! # Architecture (Pure DD - Anti-Cheat Design)
//!
//! - `core/` - Pure DD, no Zoon dependencies, no Mutable/RefCell
//!   - `types.rs` - DdInput/DdOutput with no sync access
//!   - `guards.rs` - Runtime cheat detection
//!   - `worker.rs` - Async DD worker with event loop
//! - `io/` - Input/output channels for DD communication
//! - `bridge/` - Zoon integration, receives streams only
//! - `dd_value.rs` - Pure data types for DD
//! - `dd_runtime.rs` - DD operators (hold, etc.)
//! - `dd_evaluator.rs` - Static expression evaluation
//!
//! ## Anti-Cheat Constraints
//!
//! - NO `Mutable<T>` - Use DdOutput streams instead
//! - NO `RefCell<T>` - All state in DD collections
//! - NO `.get()` - Never read state synchronously
//! - NO `trigger_render()` - DD outputs drive rendering automatically
//!
//! Run `makers verify-dd-no-cheats` to check for violations.

// Pure DD modules (anti-cheat compliant)
pub mod core;
pub mod io;
pub mod bridge;

// Core modules (no cheats)
pub mod dd_value;      // Pure data types
pub mod dd_runtime;    // DD operators (hold, etc.)
pub mod dd_evaluator;  // Static expression evaluation

// Stub modules for frontend compatibility (to be replaced in Phase 4)
pub mod dd_bridge;       // Stub: render DD output to Zoon elements
pub mod dd_interpreter;  // Stub: parse Boon and run DD dataflow
pub mod dd_reactive_eval; // Stub: timer invalidation

// Re-export commonly used types
pub use core::{DdEvent, DdEventValue, DdInput, DdOutput, DdWorker, DdWorkerHandle, HoldId, LinkId};
pub use dd_value::DdValue;
pub use io::{clear_dd_persisted_states, clear_hold_states_memory};
