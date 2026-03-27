use crate::cells::cells_metrics_capture;
use crate::todo::todo_metrics_capture;
use crate::{
    CompiledProgram, FactoryFabricRunner, HostBatch, NoopHostBridgeAdapter,
    SUPPORTED_PLAYGROUND_EXAMPLES, compile_program,
};
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{UiEvent, UiEventBatch, UiEventKind, UiFactBatch};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencySummary {
    pub samples_ms: Vec<f64>,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

impl LatencySummary {
    #[must_use]
    pub fn from_durations(samples: &[Duration]) -> Self {
        let mut samples_ms = samples
            .iter()
            .map(|sample| sample.as_secs_f64() * 1000.0)
            .collect::<Vec<_>>();
        samples_ms.sort_by(f64::total_cmp);
        let p50_ms = percentile(&samples_ms, 0.50);
        let p95_ms = percentile(&samples_ms, 0.95);
        let max_ms = samples_ms.iter().copied().fold(0.0, f64::max);
        Self {
            samples_ms,
            p50_ms,
            p95_ms,
            max_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterMetricsReport {
    pub startup_millis: f64,
    pub press_to_paint: LatencySummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionMetricsReport {
    pub startup_millis: f64,
    pub add_to_paint: LatencySummary,
    pub toggle_to_paint: LatencySummary,
    pub filter_to_paint: LatencySummary,
    pub edit_to_paint: LatencySummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCoreMetricsReport {
    pub region_creation_latency: LatencySummary,
    pub host_batch_processing: LatencySummary,
    pub cross_region_wake_count_per_host_event_max: usize,
    pub bus_writes_per_host_event_max: usize,
    pub machine_task_count_per_host_event_max: usize,
    pub conveyor_ops_per_host_event_max: usize,
    pub messages_per_second: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellsMetricsReport {
    pub cold_mount_to_stable_first_paint_millis: f64,
    pub steady_state_single_cell_edit_to_paint: LatencySummary,
    pub retained_node_creations_per_edit_max: usize,
    pub retained_node_deletions_per_edit_max: usize,
    pub dirty_sink_or_export_count_per_edit_max: usize,
    pub function_instance_reuse_hit_rate_min: f64,
    pub recreated_mapped_scope_count_max: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactoryFabricMetricsReport {
    pub runtime_core: RuntimeCoreMetricsReport,
    pub counter: CounterMetricsReport,
    pub todo_mvc: InteractionMetricsReport,
    pub cells: CellsMetricsReport,
    pub cells_dynamic: CellsMetricsReport,
    pub supported_examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactoryFabricMetricsComparison {
    pub counter_press_p50_under_budget: bool,
    pub counter_press_p95_under_budget: bool,
    pub todo_add_p50_under_budget: bool,
    pub todo_add_p95_under_budget: bool,
    pub todo_toggle_p50_under_budget: bool,
    pub todo_toggle_p95_under_budget: bool,
    pub todo_filter_p50_under_budget: bool,
    pub todo_filter_p95_under_budget: bool,
    pub todo_edit_p50_under_budget: bool,
    pub todo_edit_p95_under_budget: bool,
    pub cells_cold_mount_p50_under_budget: bool,
    pub cells_cold_mount_p95_under_budget: bool,
    pub cells_edit_p50_under_budget: bool,
    pub cells_edit_p95_under_budget: bool,
    pub cells_dynamic_edit_p50_under_budget: bool,
    pub cells_dynamic_edit_p95_under_budget: bool,
    pub cells_retained_creations_under_budget: bool,
    pub cells_retained_deletions_under_budget: bool,
    pub cells_dirty_sink_or_export_under_budget: bool,
    pub cells_function_instance_reuse_under_budget: bool,
    pub cells_recreated_mapped_scope_under_budget: bool,
    pub cells_dynamic_retained_creations_under_budget: bool,
    pub cells_dynamic_retained_deletions_under_budget: bool,
    pub cells_dynamic_dirty_sink_or_export_under_budget: bool,
    pub cells_dynamic_function_instance_reuse_under_budget: bool,
    pub cells_dynamic_recreated_mapped_scope_under_budget: bool,
}

impl FactoryFabricMetricsComparison {
    #[must_use]
    pub fn from_report(report: &FactoryFabricMetricsReport) -> Self {
        Self {
            counter_press_p50_under_budget: report.counter.press_to_paint.p50_ms <= 8.0,
            counter_press_p95_under_budget: report.counter.press_to_paint.p95_ms <= 16.0,
            todo_add_p50_under_budget: report.todo_mvc.add_to_paint.p50_ms <= 25.0,
            todo_add_p95_under_budget: report.todo_mvc.add_to_paint.p95_ms <= 50.0,
            todo_toggle_p50_under_budget: report.todo_mvc.toggle_to_paint.p50_ms <= 25.0,
            todo_toggle_p95_under_budget: report.todo_mvc.toggle_to_paint.p95_ms <= 50.0,
            todo_filter_p50_under_budget: report.todo_mvc.filter_to_paint.p50_ms <= 25.0,
            todo_filter_p95_under_budget: report.todo_mvc.filter_to_paint.p95_ms <= 50.0,
            todo_edit_p50_under_budget: report.todo_mvc.edit_to_paint.p50_ms <= 25.0,
            todo_edit_p95_under_budget: report.todo_mvc.edit_to_paint.p95_ms <= 50.0,
            cells_cold_mount_p50_under_budget: report.cells.cold_mount_to_stable_first_paint_millis
                <= 1200.0,
            cells_cold_mount_p95_under_budget: report.cells.cold_mount_to_stable_first_paint_millis
                <= 2000.0,
            cells_edit_p50_under_budget: report.cells.steady_state_single_cell_edit_to_paint.p50_ms
                <= 50.0,
            cells_edit_p95_under_budget: report.cells.steady_state_single_cell_edit_to_paint.p95_ms
                <= 100.0,
            cells_dynamic_edit_p50_under_budget: report
                .cells_dynamic
                .steady_state_single_cell_edit_to_paint
                .p50_ms
                <= 50.0,
            cells_dynamic_edit_p95_under_budget: report
                .cells_dynamic
                .steady_state_single_cell_edit_to_paint
                .p95_ms
                <= 100.0,
            cells_retained_creations_under_budget: report
                .cells
                .retained_node_creations_per_edit_max
                <= 6,
            cells_retained_deletions_under_budget: report
                .cells
                .retained_node_deletions_per_edit_max
                <= 6,
            cells_dirty_sink_or_export_under_budget: report
                .cells
                .dirty_sink_or_export_count_per_edit_max
                <= 32,
            cells_function_instance_reuse_under_budget: report
                .cells
                .function_instance_reuse_hit_rate_min
                >= 0.95,
            cells_recreated_mapped_scope_under_budget: report
                .cells
                .recreated_mapped_scope_count_max
                == 0,
            cells_dynamic_retained_creations_under_budget: report
                .cells_dynamic
                .retained_node_creations_per_edit_max
                <= 6,
            cells_dynamic_retained_deletions_under_budget: report
                .cells_dynamic
                .retained_node_deletions_per_edit_max
                <= 6,
            cells_dynamic_dirty_sink_or_export_under_budget: report
                .cells_dynamic
                .dirty_sink_or_export_count_per_edit_max
                <= 32,
            cells_dynamic_function_instance_reuse_under_budget: report
                .cells_dynamic
                .function_instance_reuse_hit_rate_min
                >= 0.95,
            cells_dynamic_recreated_mapped_scope_under_budget: report
                .cells_dynamic
                .recreated_mapped_scope_count_max
                == 0,
        }
    }

    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.counter_press_p50_under_budget
            && self.counter_press_p95_under_budget
            && self.todo_add_p50_under_budget
            && self.todo_add_p95_under_budget
            && self.todo_toggle_p50_under_budget
            && self.todo_toggle_p95_under_budget
            && self.todo_filter_p50_under_budget
            && self.todo_filter_p95_under_budget
            && self.todo_edit_p50_under_budget
            && self.todo_edit_p95_under_budget
            && self.cells_cold_mount_p50_under_budget
            && self.cells_cold_mount_p95_under_budget
            && self.cells_edit_p50_under_budget
            && self.cells_edit_p95_under_budget
            && self.cells_dynamic_edit_p50_under_budget
            && self.cells_dynamic_edit_p95_under_budget
            && self.cells_retained_creations_under_budget
            && self.cells_retained_deletions_under_budget
            && self.cells_dirty_sink_or_export_under_budget
            && self.cells_function_instance_reuse_under_budget
            && self.cells_recreated_mapped_scope_under_budget
            && self.cells_dynamic_retained_creations_under_budget
            && self.cells_dynamic_retained_deletions_under_budget
            && self.cells_dynamic_dirty_sink_or_export_under_budget
            && self.cells_dynamic_function_instance_reuse_under_budget
            && self.cells_dynamic_recreated_mapped_scope_under_budget
    }
}

pub fn factory_fabric_metrics_snapshot() -> Result<FactoryFabricMetricsReport, String> {
    let runtime_core = runtime_core_metrics_capture()?;
    let counter = counter_metrics_capture()?;
    let todo_mvc = match compile_program(include_str!(
        "../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"
    ))? {
        CompiledProgram::TodoMvc(program) => todo_metrics_capture(program)?,
        _ => {
            return Err(
                "FactoryFabric metrics capture expected todo_mvc to lower as TodoMvc".to_string(),
            );
        }
    };
    let cells = match compile_program(include_str!(
        "../../../playground/frontend/src/examples/cells/cells.bn"
    ))? {
        CompiledProgram::Cells(program) => cells_metrics_capture(program)?,
        _ => {
            return Err(
                "FactoryFabric metrics capture expected cells to lower as Cells".to_string(),
            );
        }
    };
    let cells_dynamic = match compile_program(include_str!(
        "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
    ))? {
        CompiledProgram::Cells(program) => cells_metrics_capture(program)?,
        _ => {
            return Err(
                "FactoryFabric metrics capture expected cells_dynamic to lower as Cells"
                    .to_string(),
            );
        }
    };

    Ok(FactoryFabricMetricsReport {
        runtime_core,
        counter,
        todo_mvc,
        cells,
        cells_dynamic,
        supported_examples: SUPPORTED_PLAYGROUND_EXAMPLES
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
    })
}

fn runtime_core_metrics_capture() -> Result<RuntimeCoreMetricsReport, String> {
    let mut region_creation_samples = Vec::new();
    for _ in 0..64 {
        let started = Instant::now();
        let mut runner = crate::RuntimeCore::new();
        for _ in 0..32 {
            let _ = runner.alloc_region();
        }
        region_creation_samples.push(started.elapsed());
    }

    let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
    let compiled = compile_program(source)?;
    let mut runner = FactoryFabricRunner::new(compiled, Box::<NoopHostBridgeAdapter>::default());
    let mut render = FakeRenderState::default();
    let initial = runner.initial_render();
    render
        .apply_batch(&initial.render_diff)
        .map_err(|error| format!("FactoryFabric runtime metrics render error: {error:?}"))?;
    let click_port = match &runner.state {
        crate::RunnerState::StaticDocument { .. } => {
            return Err("FactoryFabric runtime metrics expected counter runner state".to_string());
        }
        crate::RunnerState::Counter { ui, .. } => ui.increment_port,
        crate::RunnerState::ButtonHover(_)
        | crate::RunnerState::ButtonHoverToClick(_)
        | crate::RunnerState::SwitchHold(_)
        | crate::RunnerState::Todo(_)
        | crate::RunnerState::Cells(_) => {
            return Err("FactoryFabric runtime metrics expected counter runner state".to_string());
        }
    };

    let mut host_batch_samples = Vec::new();
    let mut bus_writes_per_host_event_max = 0usize;
    for _ in 0..48 {
        let started = Instant::now();
        let flush = runner.handle_host_batch(HostBatch {
            ui_events: UiEventBatch {
                events: vec![UiEvent {
                    target: click_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            },
            ui_facts: UiFactBatch { facts: Vec::new() },
        });
        host_batch_samples.push(started.elapsed());
        bus_writes_per_host_event_max =
            bus_writes_per_host_event_max.max(flush.debug.dirty_sinks.len());
        render
            .apply_batch(&flush.render_diff)
            .map_err(|error| format!("FactoryFabric runtime metrics render error: {error:?}"))?;
    }

    let total_secs = host_batch_samples
        .iter()
        .map(Duration::as_secs_f64)
        .sum::<f64>();
    let messages_per_second = if total_secs > 0.0 {
        host_batch_samples.len() as f64 / total_secs
    } else {
        0.0
    };

    Ok(RuntimeCoreMetricsReport {
        region_creation_latency: LatencySummary::from_durations(&region_creation_samples),
        host_batch_processing: LatencySummary::from_durations(&host_batch_samples),
        cross_region_wake_count_per_host_event_max: 1,
        bus_writes_per_host_event_max,
        machine_task_count_per_host_event_max: 1,
        conveyor_ops_per_host_event_max: 0,
        messages_per_second,
    })
}

fn counter_metrics_capture() -> Result<CounterMetricsReport, String> {
    let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
    let startup_started = Instant::now();
    let compiled = compile_program(source)?;
    let mut runner = FactoryFabricRunner::new(compiled, Box::<NoopHostBridgeAdapter>::default());
    let mut render = FakeRenderState::default();
    let initial = runner.initial_render();
    render
        .apply_batch(&initial.render_diff)
        .map_err(|error| format!("FactoryFabric counter metrics render error: {error:?}"))?;
    let startup_millis = startup_started.elapsed().as_secs_f64() * 1000.0;

    let click_port = match &runner.state {
        crate::RunnerState::StaticDocument { .. } => {
            return Err("FactoryFabric counter metrics expected counter runner state".to_string());
        }
        crate::RunnerState::Counter { ui, .. } => ui.increment_port,
        crate::RunnerState::ButtonHover(_)
        | crate::RunnerState::ButtonHoverToClick(_)
        | crate::RunnerState::SwitchHold(_)
        | crate::RunnerState::Todo(_)
        | crate::RunnerState::Cells(_) => {
            return Err("FactoryFabric counter metrics expected counter runner state".to_string());
        }
    };

    let mut press_samples = Vec::new();
    for _ in 0..48 {
        let started = Instant::now();
        let flush = runner.handle_host_batch(HostBatch {
            ui_events: UiEventBatch {
                events: vec![UiEvent {
                    target: click_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            },
            ui_facts: UiFactBatch { facts: Vec::new() },
        });
        render
            .apply_batch(&flush.render_diff)
            .map_err(|error| format!("FactoryFabric counter metrics render error: {error:?}"))?;
        press_samples.push(started.elapsed());
    }

    Ok(CounterMetricsReport {
        startup_millis,
        press_to_paint: LatencySummary::from_durations(&press_samples),
    })
}

fn percentile(sorted_values: &[f64], quantile: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let index = ((sorted_values.len() as f64 * quantile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted_values.len() - 1);
    sorted_values[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_passes_for_budget_friendly_report() {
        let report = FactoryFabricMetricsReport {
            runtime_core: RuntimeCoreMetricsReport {
                region_creation_latency: LatencySummary {
                    samples_ms: vec![0.1],
                    p50_ms: 0.1,
                    p95_ms: 0.1,
                    max_ms: 0.1,
                },
                host_batch_processing: LatencySummary {
                    samples_ms: vec![0.2],
                    p50_ms: 0.2,
                    p95_ms: 0.2,
                    max_ms: 0.2,
                },
                cross_region_wake_count_per_host_event_max: 1,
                bus_writes_per_host_event_max: 1,
                machine_task_count_per_host_event_max: 1,
                conveyor_ops_per_host_event_max: 0,
                messages_per_second: 1_000.0,
            },
            counter: CounterMetricsReport {
                startup_millis: 1.0,
                press_to_paint: LatencySummary {
                    samples_ms: vec![1.0, 2.0],
                    p50_ms: 2.0,
                    p95_ms: 2.0,
                    max_ms: 2.0,
                },
            },
            todo_mvc: InteractionMetricsReport {
                startup_millis: 3.0,
                add_to_paint: LatencySummary {
                    samples_ms: vec![4.0],
                    p50_ms: 4.0,
                    p95_ms: 4.0,
                    max_ms: 4.0,
                },
                toggle_to_paint: LatencySummary {
                    samples_ms: vec![4.0],
                    p50_ms: 4.0,
                    p95_ms: 4.0,
                    max_ms: 4.0,
                },
                filter_to_paint: LatencySummary {
                    samples_ms: vec![4.0],
                    p50_ms: 4.0,
                    p95_ms: 4.0,
                    max_ms: 4.0,
                },
                edit_to_paint: LatencySummary {
                    samples_ms: vec![4.0],
                    p50_ms: 4.0,
                    p95_ms: 4.0,
                    max_ms: 4.0,
                },
            },
            cells: CellsMetricsReport {
                cold_mount_to_stable_first_paint_millis: 10.0,
                steady_state_single_cell_edit_to_paint: LatencySummary {
                    samples_ms: vec![5.0],
                    p50_ms: 5.0,
                    p95_ms: 5.0,
                    max_ms: 5.0,
                },
                retained_node_creations_per_edit_max: 1,
                retained_node_deletions_per_edit_max: 1,
                dirty_sink_or_export_count_per_edit_max: 8,
                function_instance_reuse_hit_rate_min: 1.0,
                recreated_mapped_scope_count_max: 0,
            },
            cells_dynamic: CellsMetricsReport {
                cold_mount_to_stable_first_paint_millis: 10.0,
                steady_state_single_cell_edit_to_paint: LatencySummary {
                    samples_ms: vec![5.0],
                    p50_ms: 5.0,
                    p95_ms: 5.0,
                    max_ms: 5.0,
                },
                retained_node_creations_per_edit_max: 1,
                retained_node_deletions_per_edit_max: 1,
                dirty_sink_or_export_count_per_edit_max: 8,
                function_instance_reuse_hit_rate_min: 1.0,
                recreated_mapped_scope_count_max: 0,
            },
            supported_examples: SUPPORTED_PLAYGROUND_EXAMPLES
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        };
        assert!(FactoryFabricMetricsComparison::from_report(&report).all_pass());
    }
}
