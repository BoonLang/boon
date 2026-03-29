//! ActorsLite browser engine crate.
//!
//! Current source of truth:
//! `docs/plans/ACTORSLITE_STRICT_UNIFIED_ENGINE_PLAN.md`
//!
//! The current preview-dispatch architecture in this crate is transitional.
//! Keep the repo aligned with the strict unified-engine plan and avoid adding
//! new example-specific lowering or acceptance shortcuts.

use boon::zoon::*;

pub mod acceptance;
#[cfg(test)]
mod append_list_runtime;
pub mod bridge;
pub mod browser_debug;
pub mod cells_acceptance;
pub mod cells_lower;
pub mod cells_preview;
pub mod cells_runtime;
pub mod chained_list_remove_bug_preview;
pub mod checkbox_test_preview;
pub mod circle_drawer_preview;
pub mod clock;
pub mod complex_counter_preview;
pub mod counter_acceptance;
pub mod crud_preview;
pub mod dispatch;
pub mod edit_session;
pub mod editable_list_actions;
mod editable_mapped_list_preview_runtime;
pub mod editable_mapped_list_runtime;
pub mod fibonacci_preview;
pub mod filter_checkbox_bug_preview;
pub mod filtered_list_view;
pub mod flight_booker_preview;
mod host_view_preview;
pub mod host_view_template;
pub mod ids;
pub mod input_form_runtime;
mod interactive_preview;
pub mod interval_preview;
pub mod ir;
mod ir_executor;
pub mod latest_preview;
pub mod layers_preview;
pub mod list_form_actions;
pub mod list_map_block_preview;
pub mod list_map_external_dep_preview;
pub mod list_object_state_preview;
pub mod list_retain_count_preview;
pub mod list_retain_reactive_preview;
pub mod list_retain_remove_preview;
pub mod list_semantics;
pub mod lower;
pub mod lowered_preview;
pub mod mapped_click_runtime;
pub mod mapped_item_state_runtime;
pub mod mapped_list_runtime;
pub mod mapped_list_view_runtime;
pub mod metrics;
pub mod multi_input_state;
pub mod pages_preview;
pub mod parse;
pub mod preview;
mod preview_runtime;
pub mod preview_shell;
mod retained_ui_state;
mod runtime;
mod runtime_backed_domain;
mod runtime_backed_preview;
pub mod selected_filter_click_runtime;
pub mod selected_list_filter;
pub mod semantics;
pub mod shopping_list_preview;
pub mod slot_projection;
pub mod static_preview;
pub mod targeted_list_runtime;
pub mod temperature_converter_preview;
pub mod text_filtered_editable_list_preview_runtime;
pub mod text_input;
pub mod text_interpolation_update_preview;
pub mod timed_math_preview;
pub mod timer_preview;
pub mod todo_acceptance;
pub mod todo_physical_preview;
pub mod todo_preview;
pub mod toggle_examples_preview;
pub mod validated_form_runtime;

pub use acceptance::{
    ActorsLitePhase4AcceptanceRecord, actors_lite_phase4_acceptance_is_green,
    actors_lite_phase4_acceptance_record, actors_lite_public_exposure_enabled,
};
pub use dispatch::{
    MILESTONE_PLAYGROUND_EXAMPLES, PUBLIC_PLAYGROUND_EXAMPLES, SUPPORTED_PLAYGROUND_EXAMPLES,
    is_public_playground_example, is_supported_playground_example,
};
pub use metrics::{
    ActorsLiteMetricsComparison, ActorsLiteMetricsReport, CellsMetricsReport, CounterMetricsReport,
    InteractionMetricsReport, LatencySummary, RuntimeCoreMetricsReport, TodoMetricsReport,
    actors_lite_metrics_snapshot,
};

pub fn run_actors_lite(source: &str) -> impl Element {
    browser_debug::clear_debug_marker();
    browser_debug::set_debug_marker("run_actors_lite:start");
    match lower::lower_program(source) {
        Ok(program) => {
            browser_debug::set_debug_marker("run_actors_lite:lowered");
            match lowered_preview::LoweredPreview::from_program(program) {
                Ok(preview) => lowered_preview::render_lowered_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("lowered preview: {error}")).unify(),
            }
        }
        Err(error) => {
            browser_debug::set_debug_marker("run_actors_lite:unsupported");
            render_dispatch_error(format!("unsupported source: {error}")).unify()
        }
    }
}

fn render_dispatch_error(message: String) -> impl Element {
    El::new()
        .s(Font::new().color(color!("LightCoral")))
        .child(format!("ActorsLite: {message}"))
}
