//! Common types shared across engine implementations.

use serde::{Deserialize, Serialize};

/// The type of engine used to run Boon code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineType {
    /// Actor-based reactive streams engine (push-based, fine-grained subscriptions)
    Actors,
    /// Differential Dataflow engine (pull-based, incremental computation)
    DifferentialDataflow,
}

impl EngineType {
    /// Returns a short display name for the engine.
    pub fn short_name(&self) -> &'static str {
        match self {
            Self::Actors => "Actors",
            Self::DifferentialDataflow => "DD",
        }
    }

    /// Returns the full descriptive name for the engine.
    pub fn full_name(&self) -> &'static str {
        match self {
            Self::Actors => "Actor-based reactive streams",
            Self::DifferentialDataflow => "Differential Dataflow",
        }
    }
}

impl Default for EngineType {
    fn default() -> Self {
        default_engine()
    }
}

/// Returns the default engine based on compile-time feature flags.
pub fn default_engine() -> EngineType {
    // When both engines are available (either via engine-both or both individual features)
    #[cfg(all(feature = "engine-actors", feature = "engine-dd"))]
    {
        EngineType::Actors // Default to Actors when both are available
    }

    // Actors-only (DD not available)
    #[cfg(all(feature = "engine-actors", not(feature = "engine-dd")))]
    {
        EngineType::Actors
    }

    // DD-only (Actors not available)
    #[cfg(all(feature = "engine-dd", not(feature = "engine-actors")))]
    {
        EngineType::DifferentialDataflow
    }
}

/// Returns true if both engines are available for runtime switching.
pub fn is_engine_switchable() -> bool {
    cfg!(all(feature = "engine-actors", feature = "engine-dd"))
}
