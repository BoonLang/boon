//! DD output diffs → Mutable/Signal for Zoon rendering.

use zoon::Mutable;
use super::super::core::value::Value;

/// Holds the reactive output from the DD engine.
/// Zoon elements observe this via `.signal_cloned()`.
pub struct DdOutput {
    pub document: Mutable<Value>,
}

impl DdOutput {
    pub fn new(initial: Value) -> Self {
        DdOutput {
            document: Mutable::new(initial),
        }
    }
}
