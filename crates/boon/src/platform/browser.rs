pub mod common;
pub(crate) mod kernel;

use crate::parser::static_expression::{Expression, Spanned};
use zoon::*;

// Engine-specific modules (feature-gated)
// NOTE: engine-actors is legacy; engine-dd is default.
#[cfg(feature = "engine-actors")]
pub mod engine_actors;

#[cfg(feature = "engine-dd")]
pub mod engine_dd;

#[cfg(feature = "engine-wasm")]
pub(crate) mod engine_wasm_pro;

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

type ExternalFunctionDef = (String, Vec<String>, Spanned<Expression>, Option<String>);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackendBatchMetrics {
    pub encoded_bytes: usize,
    pub op_count: usize,
    pub ui_node_count: usize,
    pub double_click_ports: usize,
    pub input_ports: usize,
    pub key_down_ports: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WasmPipelineMetrics {
    pub lower_millis: u128,
    pub exec_build_millis: u128,
    pub lower_exec_millis: u128,
    pub init_runtime_millis: u128,
    pub init_decode_apply_millis: u128,
    pub init_millis: u128,
    pub first_render_total_millis: u128,
    pub edit_entry_millis: u128,
    pub input_update_millis: u128,
    pub commit_runtime_millis: u128,
    pub commit_decode_apply_millis: u128,
    pub a1_commit_millis: u128,
    pub dependent_recompute_runtime_millis: u128,
    pub dependent_recompute_decode_apply_millis: u128,
    pub a2_recompute_millis: u128,
    pub init_batch: BackendBatchMetrics,
    pub a1_commit_batch: BackendBatchMetrics,
    pub a2_recompute_batch: BackendBatchMetrics,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CellsBackendMetricsReport {
    pub wasm: WasmPipelineMetrics,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CellsBackendComparison {
    pub module_under_size_budget: bool,
    pub incremental_commit_under_size_budget: bool,
    pub incremental_commit_under_op_budget: bool,
    pub first_render_under_time_budget: bool,
    pub edit_path_under_time_budget: bool,
    pub dependent_recompute_under_time_budget: bool,
}

impl CellsBackendComparison {
    #[must_use]
    pub fn from_report(report: &CellsBackendMetricsReport) -> Self {
        Self {
            module_under_size_budget: report.wasm.init_batch.encoded_bytes <= 1_000_000,
            incremental_commit_under_size_budget: report.wasm.a1_commit_batch.encoded_bytes
                <= 2_000,
            incremental_commit_under_op_budget: report.wasm.a1_commit_batch.op_count <= 16,
            first_render_under_time_budget: report.wasm.first_render_total_millis <= 300,
            edit_path_under_time_budget: report.wasm.a1_commit_millis <= 300,
            dependent_recompute_under_time_budget: report.wasm.a2_recompute_millis <= 300,
        }
    }

    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.module_under_size_budget
            && self.incremental_commit_under_size_budget
            && self.incremental_commit_under_op_budget
            && self.first_render_under_time_budget
            && self.edit_path_under_time_budget
            && self.dependent_recompute_under_time_budget
    }
}

/// Shared Wasm-family browser entrypoint.
///
/// This keeps backend-specific Wasm runners behind the browser module boundary
/// so external callers do not depend on legacy-vs-pro internal modules.
pub fn run_wasm_family_engine(
    engine: common::EngineType,
    source: &str,
    external_functions: Option<&[ExternalFunctionDef]>,
    persistence_enabled: bool,
) -> RawElOrText {
    match engine {
        #[cfg(feature = "engine-wasm")]
        common::EngineType::Wasm => {
            engine_wasm_pro::run_wasm(source, external_functions, persistence_enabled)
        }
        #[cfg(not(feature = "engine-wasm"))]
        common::EngineType::Wasm => unsupported_engine_element("Wasm"),

        other => unsupported_engine_element(other.short_name()),
    }
}

#[cfg(feature = "engine-wasm")]
pub fn cells_backend_metrics_snapshot() -> Result<CellsBackendMetricsReport, String> {
    Ok(CellsBackendMetricsReport {
        wasm: engine_wasm_pro::wasm_pipeline_metrics_for_cells()?,
    })
}

fn unsupported_engine_element(engine: &str) -> RawElOrText {
    El::new()
        .s(Font::new().color(color!("Red")))
        .child(format!("{engine} is not a Wasm-family engine"))
        .unify()
}
