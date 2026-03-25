use crate::runtime::RuntimeTelemetrySnapshot;
use crate::{cells_preview, preview, todo_preview};
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct CounterMetricsReport {
    pub startup_millis: f64,
    pub press_to_paint: LatencySummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct InteractionMetricsReport {
    pub startup_millis: f64,
    pub add_to_paint: LatencySummary,
    pub toggle_to_paint: LatencySummary,
    pub filter_to_paint: LatencySummary,
    pub edit_to_paint: LatencySummary,
}

pub type TodoMetricsReport = InteractionMetricsReport;

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeCoreMetricsReport {
    pub actor_creation_latency: LatencySummary,
    pub send_latency: LatencySummary,
    pub messages_per_second: f64,
    pub peak_actor_count: usize,
    pub peak_queue_depth: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CellsMetricsReport {
    pub cold_mount_to_stable_first_paint_millis: f64,
    pub steady_state_single_cell_edit_to_paint: LatencySummary,
    pub retained_node_creations_per_edit_max: usize,
    pub retained_node_deletions_per_edit_max: usize,
    pub dirty_sink_or_export_count_per_edit_max: usize,
    pub function_instance_reuse_hit_rate_min: f64,
    pub recreated_mapped_scope_count_max: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActorsLiteMetricsReport {
    pub runtime_core: RuntimeCoreMetricsReport,
    pub counter: CounterMetricsReport,
    pub todo_mvc: TodoMetricsReport,
    pub cells: CellsMetricsReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActorsLiteMetricsComparison {
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
    pub cells_retained_creations_under_budget: bool,
    pub cells_retained_deletions_under_budget: bool,
    pub cells_dirty_sink_or_export_under_budget: bool,
    pub cells_function_instance_reuse_under_budget: bool,
    pub cells_recreated_mapped_scope_under_budget: bool,
}

impl ActorsLiteMetricsComparison {
    #[must_use]
    pub fn from_report(report: &ActorsLiteMetricsReport) -> Self {
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
            && self.cells_retained_creations_under_budget
            && self.cells_retained_deletions_under_budget
            && self.cells_dirty_sink_or_export_under_budget
            && self.cells_function_instance_reuse_under_budget
            && self.cells_recreated_mapped_scope_under_budget
    }
}

pub fn actors_lite_metrics_snapshot() -> Result<ActorsLiteMetricsReport, String> {
    let (counter, counter_runtime) = preview::counter_metrics_capture()?;
    let (todo_mvc, todo_runtime) = todo_preview::todo_metrics_capture()?;
    Ok(ActorsLiteMetricsReport {
        runtime_core: runtime_core_metrics_report([counter_runtime, todo_runtime]),
        counter,
        todo_mvc,
        cells: cells_preview::cells_metrics_snapshot()?,
    })
}

fn runtime_core_metrics_report(
    snapshots: [RuntimeTelemetrySnapshot; 2],
) -> RuntimeCoreMetricsReport {
    let actor_creation_samples = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.actor_creation_samples.iter().copied())
        .collect::<Vec<_>>();
    let send_samples = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.send_samples.iter().copied())
        .collect::<Vec<_>>();
    let send_count = snapshots
        .iter()
        .map(|snapshot| snapshot.send_count)
        .sum::<usize>();
    let send_duration_secs = send_samples.iter().map(Duration::as_secs_f64).sum::<f64>();
    let peak_actor_count = snapshots
        .iter()
        .map(|snapshot| snapshot.peak_actor_count)
        .max()
        .unwrap_or(0);
    let peak_queue_depth = snapshots
        .iter()
        .map(|snapshot| snapshot.peak_ready_queue_depth)
        .max()
        .unwrap_or(0);

    RuntimeCoreMetricsReport {
        actor_creation_latency: LatencySummary::from_durations(&actor_creation_samples),
        send_latency: LatencySummary::from_durations(&send_samples),
        messages_per_second: if send_duration_secs > 0.0 {
            send_count as f64 / send_duration_secs
        } else {
            0.0
        },
        peak_actor_count,
        peak_queue_depth,
    }
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
    fn comparison_passes_for_zeroish_report() {
        let report = ActorsLiteMetricsReport {
            runtime_core: RuntimeCoreMetricsReport {
                actor_creation_latency: LatencySummary {
                    samples_ms: vec![0.1],
                    p50_ms: 0.1,
                    p95_ms: 0.1,
                    max_ms: 0.1,
                },
                send_latency: LatencySummary {
                    samples_ms: vec![0.1],
                    p50_ms: 0.1,
                    p95_ms: 0.1,
                    max_ms: 0.1,
                },
                messages_per_second: 10_000.0,
                peak_actor_count: 8,
                peak_queue_depth: 2,
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
                startup_millis: 1.0,
                add_to_paint: LatencySummary {
                    samples_ms: vec![2.0],
                    p50_ms: 2.0,
                    p95_ms: 2.0,
                    max_ms: 2.0,
                },
                toggle_to_paint: LatencySummary {
                    samples_ms: vec![2.0],
                    p50_ms: 2.0,
                    p95_ms: 2.0,
                    max_ms: 2.0,
                },
                filter_to_paint: LatencySummary {
                    samples_ms: vec![2.0],
                    p50_ms: 2.0,
                    p95_ms: 2.0,
                    max_ms: 2.0,
                },
                edit_to_paint: LatencySummary {
                    samples_ms: vec![2.0],
                    p50_ms: 2.0,
                    p95_ms: 2.0,
                    max_ms: 2.0,
                },
            },
            cells: CellsMetricsReport {
                cold_mount_to_stable_first_paint_millis: 20.0,
                steady_state_single_cell_edit_to_paint: LatencySummary {
                    samples_ms: vec![4.0],
                    p50_ms: 4.0,
                    p95_ms: 4.0,
                    max_ms: 4.0,
                },
                retained_node_creations_per_edit_max: 1,
                retained_node_deletions_per_edit_max: 1,
                dirty_sink_or_export_count_per_edit_max: 3,
                function_instance_reuse_hit_rate_min: 0.99,
                recreated_mapped_scope_count_max: 0,
            },
        };

        assert!(ActorsLiteMetricsComparison::from_report(&report).all_pass());
    }
}
