use anyhow::{bail, Context, Result};
use boon_engine_actors_lite::{
    actors_lite_metrics_snapshot, ActorsLiteMetricsComparison, ActorsLiteMetricsReport,
};
use boon_engine_factory_fabric::{
    factory_fabric_metrics_snapshot, FactoryFabricMetricsComparison, FactoryFabricMetricsReport,
};
use boon_engine_wasm::{
    cells_backend_metrics_snapshot, CellsBackendComparison, CellsBackendMetricsReport,
};
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct ActorsLitePinnedEnvironmentReport {
    pub os_pretty_name: Option<String>,
    pub os_version_id: Option<String>,
    pub architecture: String,
    pub cpu_model_name: Option<String>,
    pub chromium_version: Option<String>,
    pub chromium_major: Option<u32>,
    pub tool_release_build: bool,
    pub warmed_session: bool,
    pub single_visible_tab: bool,
    pub no_devtools: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActorsLitePinnedEnvironmentComparison {
    pub linux_24_04_x86_64_under_budget: bool,
    pub cpu_matches_i7_9700k: bool,
    pub chromium_146_detected: bool,
    pub tool_release_build: bool,
    pub warmed_session: bool,
    pub single_visible_tab: bool,
    pub no_devtools: bool,
}

impl ActorsLitePinnedEnvironmentComparison {
    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.linux_24_04_x86_64_under_budget
            && self.cpu_matches_i7_9700k
            && self.chromium_146_detected
            && self.tool_release_build
            && self.warmed_session
            && self.single_visible_tab
            && self.no_devtools
    }
}

fn parse_os_release() -> (Option<String>, Option<String>) {
    let Ok(contents) = std::fs::read_to_string("/etc/os-release") else {
        return (None, None);
    };
    let mut pretty = None;
    let mut version_id = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
            pretty = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("VERSION_ID=") {
            version_id = Some(value.trim_matches('"').to_string());
        }
    }
    (pretty, version_id)
}

fn parse_lscpu_model_name() -> Option<String> {
    let output = Command::new("lscpu").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("Model name:")
                .map(|value| value.trim().to_string())
        })
}

fn detect_chromium_version() -> Option<String> {
    for binary in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ] {
        let output = Command::new(binary).arg("--version").output().ok()?;
        if output.status.success() {
            return String::from_utf8(output.stdout)
                .ok()
                .map(|value| value.trim().to_string());
        }
    }
    None
}

fn chromium_major(version: Option<&str>) -> Option<u32> {
    let version = version?;
    let digits = version
        .split_whitespace()
        .find(|part| part.chars().next().is_some_and(|ch| ch.is_ascii_digit()))?;
    digits.split('.').next()?.parse().ok()
}

pub fn detect_actors_lite_pinned_environment(
    warmed_session: bool,
    single_visible_tab: bool,
    no_devtools: bool,
) -> (
    ActorsLitePinnedEnvironmentReport,
    ActorsLitePinnedEnvironmentComparison,
) {
    let (os_pretty_name, os_version_id) = parse_os_release();
    let architecture = std::env::consts::ARCH.to_string();
    let cpu_model_name = parse_lscpu_model_name();
    let chromium_version = detect_chromium_version();
    let chromium_major = chromium_major(chromium_version.as_deref());
    let tool_release_build = !cfg!(debug_assertions);

    let report = ActorsLitePinnedEnvironmentReport {
        os_pretty_name: os_pretty_name.clone(),
        os_version_id: os_version_id.clone(),
        architecture: architecture.clone(),
        cpu_model_name: cpu_model_name.clone(),
        chromium_version: chromium_version.clone(),
        chromium_major,
        tool_release_build,
        warmed_session,
        single_visible_tab,
        no_devtools,
    };

    let comparison = ActorsLitePinnedEnvironmentComparison {
        linux_24_04_x86_64_under_budget: os_version_id.as_deref() == Some("24.04")
            && architecture == "x86_64",
        cpu_matches_i7_9700k: cpu_model_name
            .as_deref()
            .is_some_and(|cpu| cpu.contains("i7-9700K")),
        chromium_146_detected: chromium_major == Some(146),
        tool_release_build,
        warmed_session,
        single_visible_tab,
        no_devtools,
    };

    (report, comparison)
}

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

pub fn run_actors_lite_metrics(
    json: bool,
    check: bool,
    pinned_env: bool,
    warmed_session: bool,
    single_visible_tab: bool,
    no_devtools: bool,
) -> Result<()> {
    let (report, comparison) = run_actors_lite_metrics_capture()?;
    let (environment, environment_comparison) =
        detect_actors_lite_pinned_environment(warmed_session, single_visible_tab, no_devtools);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "report": &report,
                "comparison": &comparison,
                "environment": &environment,
                "environment_comparison": &environment_comparison,
            }))?
        );
    } else {
        println!("ActorsLite Metrics");
        println!(
            "Environment: {} | CPU: {} | Browser: {} | Tool profile: {}",
            environment
                .os_pretty_name
                .as_deref()
                .unwrap_or("unknown os"),
            environment
                .cpu_model_name
                .as_deref()
                .unwrap_or("unknown cpu"),
            environment
                .chromium_version
                .as_deref()
                .unwrap_or("unknown browser"),
            if environment.tool_release_build {
                "release"
            } else {
                "debug"
            }
        );
        println!(
            "Pinned session flags: warmed={}, single_visible_tab={}, no_devtools={}",
            environment.warmed_session, environment.single_visible_tab, environment.no_devtools
        );
        println!(
            "RuntimeCore: actor creation p50 {:.3} ms, send latency p50 {:.3} ms, throughput {:.1} msg/s, peak actors {}, peak queue depth {}",
            report.runtime_core.actor_creation_latency.p50_ms,
            report.runtime_core.send_latency.p50_ms,
            report.runtime_core.messages_per_second,
            report.runtime_core.peak_actor_count,
            report.runtime_core.peak_queue_depth
        );
        println!(
            "Counter: startup {:.3} ms, press-to-paint p50 {:.3} ms, p95 {:.3} ms",
            report.counter.startup_millis,
            report.counter.press_to_paint.p50_ms,
            report.counter.press_to_paint.p95_ms
        );
        println!(
            "TodoMVC: startup {:.3} ms, add/toggle/filter/edit p50 {:.3}/{:.3}/{:.3}/{:.3} ms",
            report.todo_mvc.startup_millis,
            report.todo_mvc.add_to_paint.p50_ms,
            report.todo_mvc.toggle_to_paint.p50_ms,
            report.todo_mvc.filter_to_paint.p50_ms,
            report.todo_mvc.edit_to_paint.p50_ms
        );
        println!(
            "Cells: cold mount {:.3} ms, steady-state edit p50 {:.3} ms, p95 {:.3} ms",
            report.cells.cold_mount_to_stable_first_paint_millis,
            report.cells.steady_state_single_cell_edit_to_paint.p50_ms,
            report.cells.steady_state_single_cell_edit_to_paint.p95_ms
        );
        println!(
            "  retained creates/deletes max: {}/{}, dirty count max: {}, function-instance reuse min: {:.3}, recreated mapped scopes max: {}",
            report.cells.retained_node_creations_per_edit_max,
            report.cells.retained_node_deletions_per_edit_max,
            report.cells.dirty_sink_or_export_count_per_edit_max,
            report.cells.function_instance_reuse_hit_rate_min,
            report.cells.recreated_mapped_scope_count_max
        );
        println!(
            "Pinned environment gate: {}",
            if environment_comparison.all_pass() {
                "PASS"
            } else {
                "FAIL"
            }
        );
    }

    if check && !comparison.all_pass() {
        bail!(
            "ActorsLite budget gate failed:\n{}",
            serde_json::to_string_pretty(&comparison)?
        );
    }

    if check && pinned_env && !environment_comparison.all_pass() {
        bail!(
            "ActorsLite pinned environment gate failed:\n{}",
            serde_json::to_string_pretty(&environment_comparison)?
        );
    }

    Ok(())
}

pub fn run_actors_lite_metrics_capture(
) -> Result<(ActorsLiteMetricsReport, ActorsLiteMetricsComparison)> {
    let report: ActorsLiteMetricsReport = actors_lite_metrics_snapshot()
        .map_err(anyhow::Error::msg)
        .context("failed to compute ActorsLite metrics")?;
    let comparison = ActorsLiteMetricsComparison::from_report(&report);
    Ok((report, comparison))
}

pub fn run_factory_fabric_metrics(json: bool, check: bool) -> Result<()> {
    let report: FactoryFabricMetricsReport = factory_fabric_metrics_snapshot()
        .map_err(anyhow::Error::msg)
        .context("failed to compute FactoryFabric metrics")?;
    let comparison = FactoryFabricMetricsComparison::from_report(&report);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "report": &report,
                "comparison": &comparison,
            }))?
        );
    } else {
        println!("FactoryFabric Metrics");
        println!(
            "  RuntimeCore: region creation p50 {:.3} ms, host batch p50 {:.3} ms, throughput {:.1} msg/s",
            report.runtime_core.region_creation_latency.p50_ms,
            report.runtime_core.host_batch_processing.p50_ms,
            report.runtime_core.messages_per_second
        );
        println!(
            "    cross-region wakes max: {}, bus writes max: {}, machine tasks max: {}, conveyor ops max: {}",
            report.runtime_core.cross_region_wake_count_per_host_event_max,
            report.runtime_core.bus_writes_per_host_event_max,
            report.runtime_core.machine_task_count_per_host_event_max,
            report.runtime_core.conveyor_ops_per_host_event_max
        );
        println!(
            "  Counter: startup {:.3} ms, press-to-paint p50 {:.3} ms, p95 {:.3} ms",
            report.counter.startup_millis,
            report.counter.press_to_paint.p50_ms,
            report.counter.press_to_paint.p95_ms
        );
        println!(
            "  TodoMVC: startup {:.3} ms, add/toggle/filter/edit p50 {:.3}/{:.3}/{:.3}/{:.3} ms",
            report.todo_mvc.startup_millis,
            report.todo_mvc.add_to_paint.p50_ms,
            report.todo_mvc.toggle_to_paint.p50_ms,
            report.todo_mvc.filter_to_paint.p50_ms,
            report.todo_mvc.edit_to_paint.p50_ms
        );
        println!(
            "  Cells: cold mount {:.3} ms, steady-state edit p50 {:.3} ms, p95 {:.3} ms",
            report.cells.cold_mount_to_stable_first_paint_millis,
            report.cells.steady_state_single_cell_edit_to_paint.p50_ms,
            report.cells.steady_state_single_cell_edit_to_paint.p95_ms
        );
        println!(
            "    retained creates/deletes max: {}/{}, dirty count max: {}, function-instance reuse min: {:.3}, recreated mapped scopes max: {}",
            report.cells.retained_node_creations_per_edit_max,
            report.cells.retained_node_deletions_per_edit_max,
            report.cells.dirty_sink_or_export_count_per_edit_max,
            report.cells.function_instance_reuse_hit_rate_min,
            report.cells.recreated_mapped_scope_count_max
        );
        println!(
            "  Cells Dynamic: cold mount {:.3} ms, steady-state edit p50 {:.3} ms, p95 {:.3} ms",
            report.cells_dynamic.cold_mount_to_stable_first_paint_millis,
            report
                .cells_dynamic
                .steady_state_single_cell_edit_to_paint
                .p50_ms,
            report
                .cells_dynamic
                .steady_state_single_cell_edit_to_paint
                .p95_ms
        );
        println!(
            "    retained creates/deletes max: {}/{}, dirty count max: {}, function-instance reuse min: {:.3}, recreated mapped scopes max: {}",
            report.cells_dynamic.retained_node_creations_per_edit_max,
            report.cells_dynamic.retained_node_deletions_per_edit_max,
            report.cells_dynamic.dirty_sink_or_export_count_per_edit_max,
            report.cells_dynamic.function_instance_reuse_hit_rate_min,
            report.cells_dynamic.recreated_mapped_scope_count_max
        );
        println!(
            "  Supported examples: {}",
            report.supported_examples.join(", ")
        );
    }

    if check && !comparison.all_pass() {
        bail!(
            "FactoryFabric budget gate failed:\n{}",
            serde_json::to_string_pretty(&comparison)?
        );
    }

    Ok(())
}
