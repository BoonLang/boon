//! Common types shared across engine implementations.

use serde::{Deserialize, Serialize};

#[cfg(feature = "engine-dd")]
use super::engine_dd::clear_dd_persisted_states;
#[cfg(feature = "engine-wasm")]
use super::engine_wasm_pro::clear_wasm_persisted_states;

/// The type of engine used to run Boon code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineType {
    /// Actor-based reactive streams engine (push-based, fine-grained subscriptions)
    Actors,
    /// Differential Dataflow engine (pull-based, incremental computation)
    DifferentialDataflow,
    /// Compiled WASM engine with renderer-agnostic diff output
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

    /// Returns the preferred short UI label for engine pickers.
    pub fn picker_label(&self) -> &'static str {
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

    /// Returns a brief description of how the engine works (used for tooltips).
    pub fn description(&self) -> &'static str {
        match self {
            Self::Actors => "Reactive actor subscriptions (mixed push/pull)",
            Self::DifferentialDataflow => {
                "Incremental computation based on the Differential Dataflow library"
            }
            Self::Wasm => "WebAssembly backend with renderer-agnostic diffs",
        }
    }
}

impl Default for EngineType {
    fn default() -> Self {
        default_engine()
    }
}

/// Returns true when the given engine is compiled into the current build.
pub fn is_engine_available(engine: EngineType) -> bool {
    match engine {
        EngineType::Actors => cfg!(feature = "engine-actors"),
        EngineType::DifferentialDataflow => cfg!(feature = "engine-dd"),
        EngineType::Wasm => cfg!(feature = "engine-wasm"),
    }
}

/// Returns the compiled Wasm-family engine, if any.
pub fn preferred_wasm_family_engine() -> Option<EngineType> {
    if is_engine_available(EngineType::Wasm) {
        Some(EngineType::Wasm)
    } else {
        None
    }
}

/// Resolves a requested engine against the current build.
///
/// Exact matches are preserved. If a Wasm-family engine is unavailable, fall
/// back to the preferred compiled Wasm-family engine. Other unavailable engines
/// return `None` so callers can apply their broader default behavior.
pub fn resolve_engine_for_current_build(engine: EngineType) -> Option<EngineType> {
    if is_engine_available(engine) {
        return Some(engine);
    }

    match engine {
        EngineType::Wasm => preferred_wasm_family_engine(),
        EngineType::Actors | EngineType::DifferentialDataflow => None,
    }
}

/// Returns all engines available in this build, based on compile-time feature flags.
/// Order: Actors, DD, Wasm.
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

/// Returns the engine list to surface in the playground picker.
pub fn picker_engines(selected_engine: Option<EngineType>) -> Vec<EngineType> {
    let _ = selected_engine;
    available_engines()
}

/// Clear persisted and in-memory state for the selected engine.
pub fn clear_selected_engine_persisted_states(engine: EngineType) {
    #[cfg(feature = "engine-dd")]
    if engine == EngineType::DifferentialDataflow {
        clear_dd_persisted_states();
    }

    #[cfg(feature = "engine-wasm")]
    if engine == EngineType::Wasm {
        clear_wasm_persisted_states();
    }
}

/// Clear persisted and in-memory state for all compiled engines.
pub fn clear_all_compiled_engine_persisted_states() {
    #[cfg(feature = "engine-dd")]
    clear_dd_persisted_states();

    #[cfg(feature = "engine-wasm")]
    clear_wasm_persisted_states();
}
