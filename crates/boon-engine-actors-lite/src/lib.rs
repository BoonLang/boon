//! ActorsLite browser engine crate.
//!
//! This crate intentionally starts with the Phase 1 skeleton from
//! `docs/plans/actors_lite.md`:
//! - explicit renderer-agnostic IR
//! - generational ids for actors and scopes
//! - minimal runtime core with ready queue
//! - kernel-aligned semantic helpers/tests

use boon::zoon::*;

pub mod acceptance;
pub mod append_list_preview_runtime;
pub mod append_list_runtime;
pub mod bridge;
pub mod browser_debug;
pub mod cells_acceptance;
pub mod cells_lower;
pub mod cells_preview;
pub mod cells_runtime;
pub mod chained_list_remove_bug_preview;
pub mod checkbox_test_preview;
pub mod circle_drawer_preview;
pub mod complex_counter_preview;
pub mod counter_acceptance;
pub mod crud_preview;
pub mod dispatch;
pub mod edit_session;
pub mod editable_list_actions;
pub mod editable_mapped_list_preview_runtime;
pub mod editable_mapped_list_runtime;
pub mod fibonacci_preview;
pub mod filter_checkbox_bug_preview;
pub mod filtered_list_view;
pub mod flight_booker_preview;
pub mod host_view_preview;
pub mod ids;
pub mod input_form_runtime;
pub mod interactive_preview;
pub mod interval_preview;
pub mod ir;
pub mod ir_executor;
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
pub mod mapped_click_runtime;
pub mod mapped_item_state_runtime;
pub mod mapped_list_runtime;
pub mod mapped_list_view_runtime;
pub mod metrics;
pub mod multi_input_state;
pub mod pages_preview;
pub mod parse;
pub mod preview;
pub mod preview_runtime;
pub mod preview_shell;
pub mod retained_ui_state;
pub mod runtime;
pub mod runtime_backed_domain;
pub mod runtime_backed_preview;
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
pub mod toggle_filtered_list_preview_runtime;
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
    match dispatch::classify_source(source) {
        Ok(dispatch::ActorsLiteSourceKind::Counter) => match preview::CounterPreview::new(source) {
            Ok(preview) => preview::render_counter_preview(preview).unify(),
            Err(error) => render_dispatch_error(format!("counter preview: {error}")).unify(),
        },
        Ok(dispatch::ActorsLiteSourceKind::ComplexCounter) => {
            match complex_counter_preview::ComplexCounterPreview::new(source) {
                Ok(preview) => {
                    complex_counter_preview::render_complex_counter_preview(preview).unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("complex_counter preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::Interval) => {
            match interval_preview::IntervalPreview::new(source) {
                Ok(preview) => interval_preview::render_interval_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("interval preview: {error}")).unify(),
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::IntervalHold) => {
            match interval_preview::IntervalPreview::new(source) {
                Ok(preview) => interval_preview::render_interval_preview(preview).unify(),
                Err(error) => {
                    render_dispatch_error(format!("interval_hold preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::Fibonacci) => {
            match fibonacci_preview::FibonacciPreview::new(source) {
                Ok(preview) => fibonacci_preview::render_fibonacci_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("fibonacci preview: {error}")).unify(),
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::Layers) => {
            match layers_preview::LayersPreview::new(source) {
                Ok(preview) => layers_preview::render_layers_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("layers preview: {error}")).unify(),
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::Pages) => {
            match pages_preview::PagesPreview::new(source) {
                Ok(preview) => pages_preview::render_pages_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("pages preview: {error}")).unify(),
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::Latest) => {
            match latest_preview::LatestPreview::new(source) {
                Ok(preview) => latest_preview::render_latest_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("latest preview: {error}")).unify(),
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::TextInterpolationUpdate) => {
            match text_interpolation_update_preview::TextInterpolationUpdatePreview::new(source) {
                Ok(preview) => {
                    text_interpolation_update_preview::render_text_interpolation_update_preview(
                        preview,
                    )
                    .unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("text_interpolation_update preview: {error}"))
                        .unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::Then) => {
            match timed_math_preview::ThenPreview::new(source) {
                Ok(preview) => timed_math_preview::render_then_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("then preview: {error}")).unify(),
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::When) => {
            match timed_math_preview::WhenPreview::new(source) {
                Ok(preview) => timed_math_preview::render_when_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("when preview: {error}")).unify(),
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::While) => {
            match timed_math_preview::WhilePreview::new(source) {
                Ok(preview) => timed_math_preview::render_while_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("while preview: {error}")).unify(),
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::WhileFunctionCall) => {
            match toggle_examples_preview::WhileFunctionCallPreview::new(source) {
                Ok(preview) => {
                    toggle_examples_preview::render_while_function_call_preview(preview).unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("while_function_call preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ButtonHoverTest) => {
            match toggle_examples_preview::ButtonHoverTestPreview::new(source) {
                Ok(preview) => {
                    toggle_examples_preview::render_button_hover_test_preview(preview).unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("button_hover_test preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ButtonHoverToClickTest) => {
            match toggle_examples_preview::ButtonHoverToClickTestPreview::new(source) {
                Ok(preview) => {
                    toggle_examples_preview::render_button_hover_to_click_test_preview(preview)
                        .unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("button_hover_to_click_test preview: {error}"))
                        .unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::SwitchHoldTest) => {
            match toggle_examples_preview::SwitchHoldTestPreview::new(source) {
                Ok(preview) => {
                    toggle_examples_preview::render_switch_hold_test_preview(preview).unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("switch_hold_test preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ListRetainReactive) => {
            match list_retain_reactive_preview::ListRetainReactivePreview::new(source) {
                Ok(preview) => {
                    list_retain_reactive_preview::render_list_retain_reactive_preview(preview)
                        .unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("list_retain_reactive preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ListMapExternalDep) => {
            match list_map_external_dep_preview::ListMapExternalDepPreview::new(source) {
                Ok(preview) => {
                    list_map_external_dep_preview::render_list_map_external_dep_preview(preview)
                        .unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("list_map_external_dep preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ListMapBlock) => {
            match list_map_block_preview::ListMapBlockPreview::new(source) {
                Ok(preview) => {
                    list_map_block_preview::render_list_map_block_preview(preview).unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("list_map_block preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ListRetainCount) => {
            match list_retain_count_preview::ListRetainCountPreview::new(source) {
                Ok(preview) => {
                    list_retain_count_preview::render_list_retain_count_preview(preview).unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("list_retain_count preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ListObjectState) => {
            match list_object_state_preview::ListObjectStatePreview::new(source) {
                Ok(preview) => {
                    list_object_state_preview::render_list_object_state_preview(preview).unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("list_object_state preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ListRetainRemove) => {
            match list_retain_remove_preview::ListRetainRemovePreview::new(source) {
                Ok(preview) => {
                    list_retain_remove_preview::render_list_retain_remove_preview(preview).unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("list_retain_remove preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ShoppingList) => {
            match shopping_list_preview::ShoppingListPreview::new(source) {
                Ok(preview) => shopping_list_preview::render_shopping_list_preview(preview).unify(),
                Err(error) => {
                    render_dispatch_error(format!("shopping_list preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::FilterCheckboxBug) => {
            match filter_checkbox_bug_preview::FilterCheckboxBugPreview::new(source) {
                Ok(preview) => {
                    filter_checkbox_bug_preview::render_filter_checkbox_bug_preview(preview).unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("filter_checkbox_bug preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::CheckboxTest) => {
            match checkbox_test_preview::CheckboxTestPreview::new(source) {
                Ok(preview) => checkbox_test_preview::render_checkbox_test_preview(preview).unify(),
                Err(error) => {
                    render_dispatch_error(format!("checkbox_test preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::ChainedListRemoveBug) => {
            match chained_list_remove_bug_preview::ChainedListRemoveBugPreview::new(source) {
                Ok(preview) => {
                    chained_list_remove_bug_preview::render_chained_list_remove_bug_preview(preview)
                        .unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("chained_list_remove_bug preview: {error}"))
                        .unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::CircleDrawer) => {
            match circle_drawer_preview::CircleDrawerPreview::new(source) {
                Ok(preview) => circle_drawer_preview::render_circle_drawer_preview(preview).unify(),
                Err(error) => {
                    render_dispatch_error(format!("circle_drawer preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::Crud) => match crud_preview::CrudPreview::new(source) {
            Ok(preview) => crud_preview::render_crud_preview(preview).unify(),
            Err(error) => render_dispatch_error(format!("crud preview: {error}")).unify(),
        },
        Ok(dispatch::ActorsLiteSourceKind::TemperatureConverter) => {
            match temperature_converter_preview::TemperatureConverterPreview::new(source) {
                Ok(preview) => {
                    temperature_converter_preview::render_temperature_converter_preview(preview)
                        .unify()
                }
                Err(error) => {
                    render_dispatch_error(format!("temperature_converter preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::FlightBooker) => {
            match flight_booker_preview::FlightBookerPreview::new(source) {
                Ok(preview) => flight_booker_preview::render_flight_booker_preview(preview).unify(),
                Err(error) => {
                    render_dispatch_error(format!("flight_booker preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::Timer) => match timer_preview::TimerPreview::new(source)
        {
            Ok(preview) => timer_preview::render_timer_preview(preview).unify(),
            Err(error) => render_dispatch_error(format!("timer preview: {error}")).unify(),
        },
        Ok(dispatch::ActorsLiteSourceKind::TodoMvcPhysical) => {
            match todo_physical_preview::TodoPhysicalPreview::new(source) {
                Ok(preview) => todo_physical_preview::render_todo_physical_preview(preview).unify(),
                Err(error) => {
                    render_dispatch_error(format!("todo_mvc_physical preview: {error}")).unify()
                }
            }
        }
        Ok(dispatch::ActorsLiteSourceKind::TodoMvc) => match todo_preview::TodoPreview::new(source)
        {
            Ok(preview) => todo_preview::render_todo_preview(preview).unify(),
            Err(error) => render_dispatch_error(format!("todo_mvc preview: {error}")).unify(),
        },
        Ok(dispatch::ActorsLiteSourceKind::Cells) => match cells_preview::CellsPreview::new(source)
        {
            Ok(preview) => cells_preview::render_cells_preview(preview).unify(),
            Err(error) => render_dispatch_error(format!("cells preview: {error}")).unify(),
        },
        Ok(dispatch::ActorsLiteSourceKind::StaticDocument) => {
            match static_preview::StaticPreview::new(source) {
                Ok(preview) => static_preview::render_static_preview(preview).unify(),
                Err(error) => render_dispatch_error(format!("static preview: {error}")).unify(),
            }
        }
        Err(errors) => {
            browser_debug::set_debug_marker("run_actors_lite:unsupported");
            render_dispatch_error(format!("unsupported source: {}", errors.join("; "))).unify()
        }
    }
}

fn render_dispatch_error(message: String) -> impl Element {
    El::new()
        .s(Font::new().color(color!("LightCoral")))
        .child(format!("ActorsLite: {message}"))
}
