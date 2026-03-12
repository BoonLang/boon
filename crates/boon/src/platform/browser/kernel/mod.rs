//! Reference semantic-kernel foundations.
//!
//! This module is intentionally engine-agnostic and side-effect light, but it
//! is not the start of a shared production runtime for all engines. It is a
//! reference/oracle surface for semantics, diagnostics, and tests while the
//! independent engines keep their own execution models.

mod ids;
mod runtime;
mod semantics;
mod ui;
mod value;

#[allow(unused_imports)]
pub use ids::{ElementId, ExprId, ItemKey, ScopeId, SlotKey, SourceId, TickId, TickSeq};
#[allow(unused_imports)]
pub use runtime::{
    AppliedUpdate, LinkBinding, LinkCell, ListCell, ListEntry, Runtime, RuntimeUpdate, Trigger,
};
#[allow(unused_imports)]
pub use semantics::{LatestCandidate, select_latest};
#[allow(unused_imports)]
pub use ui::{EventPortId, EventPortState, EventType, UiStore};
#[allow(unused_imports)]
pub use value::KernelValue;
