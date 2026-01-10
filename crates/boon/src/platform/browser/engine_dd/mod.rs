//! Differential Dataflow engine for Boon.
//!
//! This is an alternative engine using Timely Dataflow and Differential Dataflow
//! for pull-based evaluation with incremental computation capabilities.
//!
//! # Architecture
//!
//! The DD engine has two layers:
//!
//! 1. **Static evaluation** (`dd_evaluator`) - Evaluates pure expressions to `DdValue`
//! 2. **Reactive layer** (`dd_stream`, `dd_scope`) - Lightweight signals for UI reactivity
//!
//! The `dd_runtime` module provides actual Timely/DD operators for batch processing,
//! while `dd_stream` provides browser-friendly reactivity via Zoon's Mutable/Signal.

pub mod dd_bridge;
pub mod dd_evaluator;
pub mod dd_interpreter;
pub mod dd_link;
pub mod dd_reactive;
pub mod dd_reactive_eval;
pub mod dd_runtime;
pub mod dd_scope;
pub mod dd_stream;
pub mod dd_value;
