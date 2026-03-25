//! Common browser-facing types shared across engine implementations.

use serde::{Deserialize, Serialize};

/// The type of engine used to run Boon code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineType {
    /// Actor-based reactive streams engine (push-based, fine-grained subscriptions)
    Actors,
    /// Virtual-actor runtime with renderer-agnostic retained bridge
    ActorsLite,
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
            Self::ActorsLite => "ActorsLite",
            Self::DifferentialDataflow => "DD",
            Self::Wasm => "Wasm",
        }
    }

    /// Returns the preferred short UI label for engine pickers.
    pub fn picker_label(&self) -> &'static str {
        match self {
            Self::Actors => "Actors",
            Self::ActorsLite => "ActorsLite",
            Self::DifferentialDataflow => "DD",
            Self::Wasm => "Wasm",
        }
    }

    /// Returns the full descriptive name for the engine.
    pub fn full_name(&self) -> &'static str {
        match self {
            Self::Actors => "Actor-based reactive streams",
            Self::ActorsLite => "Virtual actors with retained bridge",
            Self::DifferentialDataflow => "Differential Dataflow",
            Self::Wasm => "Compiled WASM",
        }
    }

    /// Returns a brief description of how the engine works (used for tooltips).
    pub fn description(&self) -> &'static str {
        match self {
            Self::Actors => "Reactive actor subscriptions (mixed push/pull)",
            Self::ActorsLite => "Virtual-actor runtime with retained/keyed host bridge",
            Self::DifferentialDataflow => {
                "Incremental computation based on the Differential Dataflow library"
            }
            Self::Wasm => "WebAssembly backend with renderer-agnostic diffs",
        }
    }
}
