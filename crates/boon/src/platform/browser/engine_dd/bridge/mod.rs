//! DD Bridge module - Connects DD outputs to Zoon UI.
//!
//! # Anti-Cheat Architecture
//!
//! This module is the ONLY place where DD outputs meet Zoon:
//! - Receives DdOutput streams from io module
//! - Converts streams to Zoon signals via `signal::from_stream()`
//! - Renders DdValue to Zoon elements
//!
//! # Key Constraint
//!
//! This module CANNOT import from `core` directly - only from `io`.
//! This ensures the bridge can't access DD internals or attempt
//! synchronous state reads.
//!
//! # Dependencies
//!
//! This module depends on:
//! - `io` - I/O layer (EventInjector, OutputObserver)
//! - `zoon` - UI framework (only place where Zoon is used)
//! - `futures` - For stream conversion
//!
//! It does NOT depend on:
//! - `timely`/`differential-dataflow` - DD internals
//! - `core` directly - Must go through io layer

pub mod render;
pub mod events;

pub use render::DdBridge;
pub use events::DomEventHandler;
