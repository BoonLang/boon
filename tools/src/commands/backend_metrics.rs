use anyhow::{bail, Context, Result};
use boon_engine_wasm::{
    cells_backend_metrics_snapshot, CellsBackendComparison, CellsBackendMetricsReport,
};
use std::path::PathBuf;

pub fn run_cells_backend_metrics(
    json: bool,
    check: bool,
    target_dir: Option<PathBuf>,
) -> Result<()> {
    let _ = target_dir;

    let report: CellsBackendMetricsReport = cells_backend_metrics_snapshot()
        .map_err(anyhow::Error::msg)
        .context("failed to compute cells backend metrics")?;
    let comparison = CellsBackendComparison::from_report(&report);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "report": &report,
                "comparison": &comparison,
            }))?
        );
    } else {
        println!("Cells Backend Metrics");
        println!(
            "Wasm: {} ms lower+exec, {} ms first render total, {} ms A1 edit path total",
            report.wasm.lower_exec_millis,
            report.wasm.first_render_total_millis,
            report.wasm.a1_commit_millis
        );
        println!(
            "  lower: {} ms, exec-build: {} ms",
            report.wasm.lower_millis, report.wasm.exec_build_millis
        );
        println!(
            "  init runtime: {} ms, init decode+apply: {} ms",
            report.wasm.init_runtime_millis, report.wasm.init_decode_apply_millis
        );
        println!(
            "  edit entry: {} ms, input update: {} ms, commit runtime: {} ms, commit decode+apply: {} ms",
            report.wasm.edit_entry_millis,
            report.wasm.input_update_millis,
            report.wasm.commit_runtime_millis,
            report.wasm.commit_decode_apply_millis
        );
        println!(
            "  dependent recompute (A2 -> B1/C1): {} ms total, {} ms runtime, {} ms decode+apply",
            report.wasm.a2_recompute_millis,
            report.wasm.dependent_recompute_runtime_millis,
            report.wasm.dependent_recompute_decode_apply_millis
        );
        println!(
            "Wasm init batch: {}",
            serde_json::to_string(&report.wasm.init_batch).unwrap_or_else(|_| "null".to_string())
        );
        println!(
            "Wasm A1 commit batch: {}",
            serde_json::to_string(&report.wasm.a1_commit_batch)
                .unwrap_or_else(|_| "null".to_string())
        );
        println!(
            "Wasm A2 dependent recompute batch: {}",
            serde_json::to_string(&report.wasm.a2_recompute_batch)
                .unwrap_or_else(|_| "null".to_string())
        );
        println!("Wasm budget gate:");
        println!(
            "  module under size budget: {}",
            comparison.module_under_size_budget
        );
        println!(
            "  incremental commit under size budget: {}",
            comparison.incremental_commit_under_size_budget
        );
        println!(
            "  incremental commit under op budget: {}",
            comparison.incremental_commit_under_op_budget
        );
        println!(
            "  first render under time budget: {}",
            comparison.first_render_under_time_budget
        );
        println!(
            "  edit path under time budget: {}",
            comparison.edit_path_under_time_budget
        );
        println!(
            "  dependent recompute under time budget: {}",
            comparison.dependent_recompute_under_time_budget
        );
    }

    if check && !comparison.all_pass() {
        bail!(
            "cells backend Wasm budget gate failed:\n{}",
            serde_json::to_string_pretty(&comparison)?
        );
    }

    Ok(())
}
