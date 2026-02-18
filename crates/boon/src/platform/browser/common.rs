//! Common types shared across engine implementations.

use serde::{Deserialize, Serialize};

/// The type of engine used to run Boon code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineType {
    /// Actor-based reactive streams engine (push-based, fine-grained subscriptions)
    Actors,
    /// Differential Dataflow engine (pull-based, incremental computation)
    DifferentialDataflow,
    /// Compiled WASM engine (direct compilation to WebAssembly bytecode)
    Wasm,
}

impl EngineType {
    /// Returns a short display name for the engine.
    pub fn short_name(&self) -> &'static str {
        match self {
            Self::Actors => "Actors",
            Self::DifferentialDataflow => "DD",
            Self::Wasm => "Wasm",
        }
    }

    /// Returns the full descriptive name for the engine.
    pub fn full_name(&self) -> &'static str {
        match self {
            Self::Actors => "Actor-based reactive streams",
            Self::DifferentialDataflow => "Differential Dataflow",
            Self::Wasm => "Compiled WASM",
        }
    }
}

impl Default for EngineType {
    fn default() -> Self {
        default_engine()
    }
}

/// Returns all engines available in this build, based on compile-time feature flags.
/// Order: Actors, DD, Wasm (priority order for default selection).
pub fn available_engines() -> Vec<EngineType> {
    let mut engines = Vec::new();
    #[cfg(feature = "engine-actors")]
    engines.push(EngineType::Actors);
    #[cfg(feature = "engine-dd")]
    engines.push(EngineType::DifferentialDataflow);
    #[cfg(feature = "engine-wasm")]
    engines.push(EngineType::Wasm);
    engines
}

/// Returns the default engine based on compile-time feature flags.
/// First available engine wins (priority: Actors > DD > Wasm).
pub fn default_engine() -> EngineType {
    available_engines()
        .into_iter()
        .next()
        .expect("At least one engine must be enabled via feature flags")
}

/// Returns true if more than one engine is available for runtime switching.
pub fn is_engine_switchable() -> bool {
    available_engines().len() > 1
}
