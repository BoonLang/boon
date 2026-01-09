//! Actor-based reactive engine for Boon.
//!
//! This is the original Boon engine using ValueActor, LazyValueActor, and ActorLoop
//! for push-based reactive streams with fine-grained subscriptions.

pub mod bridge;
pub mod engine;
pub mod evaluator;
