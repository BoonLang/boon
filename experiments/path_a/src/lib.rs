//! Path A: Dirty Propagation + Explicit Captures
//!
//! This prototype keeps arena-based nodes but makes template dependencies
//! explicit at compile time through CaptureSpec.

pub mod arena;
pub mod engine;
pub mod evaluator;
pub mod ledger;
pub mod node;
pub mod template;
pub mod value;

pub use engine::Engine;
pub use shared::test_harness::Value;
