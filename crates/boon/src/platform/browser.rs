pub mod common;

// Engine-specific modules (feature-gated)
// NOTE: engine-actors is legacy; engine-dd is default.
#[cfg(feature = "engine-actors")]
pub mod engine_actors;

#[cfg(feature = "engine-dd")]
pub mod engine_dd;

// Actor engine modules (legacy) - api and interpreter depend on actor engine types
#[cfg(feature = "engine-actors")]
pub mod api;
#[cfg(feature = "engine-actors")]
pub mod interpreter;

// Backward-compatible re-exports for the actor engine
// These allow existing code using `crate::platform::browser::engine` etc. to work
#[cfg(feature = "engine-actors")]
pub use engine_actors::bridge;
#[cfg(feature = "engine-actors")]
pub use engine_actors::engine;
#[cfg(feature = "engine-actors")]
pub use engine_actors::evaluator;
