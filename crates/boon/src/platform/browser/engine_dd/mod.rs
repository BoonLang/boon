//! Differential Dataflow engine for Boon.
//!
//! This is an alternative engine using Timely Dataflow and Differential Dataflow
//! for pull-based evaluation with incremental computation capabilities.

pub mod dd_bridge;
pub mod dd_evaluator;
pub mod dd_interpreter;
pub mod dd_runtime;
pub mod dd_value;
