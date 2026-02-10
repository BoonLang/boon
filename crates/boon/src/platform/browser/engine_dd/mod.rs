//! Differential Dataflow engine for Boon.
//!
//! This engine uses Timely Dataflow and Differential Dataflow for
//! reactive evaluation with incremental computation capabilities.
//!
//! # Architecture (Pure DD - Anti-Cheat Design)
//!
//! - `core/` - Pure DD, no Zoon dependencies, no Mutable/RefCell
//!   - `types.rs` - Input/Output with no sync access
//!   - `guards.rs` - Runtime cheat detection
//!   - `worker.rs` - Async DD worker with event loop
//!   - `value.rs` - Pure data types for DD (Value)
//!   - `operators.rs` - DD operators (hold, etc.)
//! - `io/` - Input/output channels for DD communication
//! - `eval/` - Evaluation layer
//!   - `evaluator.rs` - Expression evaluation
//!   - `interpreter.rs` - Program interpretation
//! - `render/` - Rendering layer
//!   - `bridge.rs` - Value → Zoon element conversion
//!
//! ## Anti-Cheat Constraints
//!
//! - NO `Mutable<T>` - Use Output streams instead
//! - NO `RefCell<T>` - All state in DD collections
//! - NO `.get()` - Never read state synchronously
//! - NO `trigger_render()` - DD outputs drive rendering automatically
//!
//! Run `makers verify-dd-no-cheats` to check for violations.

/// Master debug logging flag for the DD engine.
/// When enabled, prints detailed information about DD operations.
pub const LOG_DD_DEBUG: bool = true;

/// Debug logging macro for the DD engine. Only prints when `LOG_DD_DEBUG` is true.
macro_rules! dd_log {
    ($($arg:tt)*) => {
        if $crate::platform::browser::engine_dd::LOG_DD_DEBUG {
            zoon::println!($($arg)*);
        }
    };
}
pub(crate) use dd_log;

// Core DD modules (anti-cheat compliant)
pub mod core;
pub mod io;

// Evaluation layer
pub mod eval;

// Rendering layer
pub mod render;

// Re-export commonly used types
pub use core::{Event, EventValue, Input, Output, Worker, WorkerHandle, CellId, LinkId};
pub use core::value::{Value, CollectionHandle, CollectionId};
pub use eval::interpreter::{DdResult, run_dd_reactive_with_persistence};
pub use eval::evaluator::BoonDdRuntime;
pub use render::bridge::{render_dd_result_reactive_signal, render_dd_document_reactive_signal};
pub use io::{clear_dd_persisted_states, clear_cells_memory};
