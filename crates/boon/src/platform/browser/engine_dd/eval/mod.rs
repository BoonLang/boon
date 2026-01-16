//! Evaluation layer for the DD engine.
//!
//! This module handles:
//! - `evaluator`: Static expression evaluation to DD values
//! - `interpreter`: Parse Boon code and run DD dataflow

pub mod evaluator;
pub mod interpreter;

pub use evaluator::*;
pub use interpreter::*;
