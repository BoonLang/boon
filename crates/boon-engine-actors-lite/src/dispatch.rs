use crate::acceptance::actors_lite_public_exposure_enabled;
use crate::lower::{LoweredProgram, lower_program};

pub const SUPPORTED_PLAYGROUND_EXAMPLES: &[&str] = &[
    "minimal",
    "hello_world",
    "counter",
    "complex_counter",
    "counter_hold",
    "fibonacci",
    "interval",
    "interval_hold",
    "layers",
    "pages",
    "latest",
    "text_interpolation_update",
    "then",
    "when",
    "while",
    "while_function_call",
    "button_hover_test",
    "button_hover_to_click_test",
    "switch_hold_test",
    "shopping_list",
    "todo_mvc",
    "list_retain_reactive",
    "list_map_external_dep",
    "list_map_block",
    "list_retain_count",
    "list_object_state",
    "list_retain_remove",
    "filter_checkbox_bug",
    "checkbox_test",
    "chained_list_remove_bug",
    "temperature_converter",
    "crud",
    "timer",
    "flight_booker",
    "circle_drawer",
    "cells",
    "cells_dynamic",
];

pub const MILESTONE_PLAYGROUND_EXAMPLES: &[&str] =
    &["counter", "todo_mvc", "cells", "cells_dynamic"];

pub const PUBLIC_PLAYGROUND_EXAMPLES: &[&str] = &[
    "minimal",
    "hello_world",
    "counter",
    "counter_hold",
    "text_interpolation_update",
    "button_hover_test",
    "button_hover_to_click_test",
    "switch_hold_test",
    "filter_checkbox_bug",
    "checkbox_test",
    "chained_list_remove_bug",
    "complex_counter",
    "fibonacci",
    "interval",
    "interval_hold",
    "then",
    "when",
    "while",
    "while_function_call",
    "todo_mvc",
    "cells",
    "cells_dynamic",
    "list_retain_reactive",
    "list_map_external_dep",
    "list_map_block",
    "list_retain_count",
    "list_object_state",
    "list_retain_remove",
    "temperature_converter",
    "flight_booker",
    "timer",
    "crud",
    "circle_drawer",
    "latest",
    "layers",
    "pages",
    "shopping_list",
];

#[must_use]
pub fn is_supported_playground_example(name: &str) -> bool {
    SUPPORTED_PLAYGROUND_EXAMPLES.contains(&name)
}

#[must_use]
pub fn is_public_playground_example(name: &str) -> bool {
    actors_lite_public_exposure_enabled() && PUBLIC_PLAYGROUND_EXAMPLES.contains(&name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorsLiteSourceKind {
    Counter,
    ComplexCounter,
    Interval,
    IntervalHold,
    Fibonacci,
    Layers,
    Pages,
    Latest,
    TextInterpolationUpdate,
    Then,
    When,
    While,
    WhileFunctionCall,
    ButtonHoverTest,
    ButtonHoverToClickTest,
    SwitchHoldTest,
    ListMapBlock,
    ListMapExternalDep,
    ListObjectState,
    ListRetainCount,
    ListRetainReactive,
    ListRetainRemove,
    ShoppingList,
    FilterCheckboxBug,
    CheckboxTest,
    ChainedListRemoveBug,
    Crud,
    CircleDrawer,
    TemperatureConverter,
    FlightBooker,
    Timer,
    TodoMvc,
    Cells,
    StaticDocument,
}

pub fn classify_source(source: &str) -> Result<ActorsLiteSourceKind, Vec<String>> {
    let mut errors = Vec::new();

    match lower_program(source) {
        Ok(LoweredProgram::Counter(_)) => return Ok(ActorsLiteSourceKind::Counter),
        Ok(LoweredProgram::ComplexCounter(_)) => return Ok(ActorsLiteSourceKind::ComplexCounter),
        Ok(LoweredProgram::TodoMvc(_)) => return Ok(ActorsLiteSourceKind::TodoMvc),
        Ok(LoweredProgram::Interval(_)) => return Ok(ActorsLiteSourceKind::Interval),
        Ok(LoweredProgram::IntervalHold(_)) => return Ok(ActorsLiteSourceKind::IntervalHold),
        Ok(LoweredProgram::Fibonacci(_)) => return Ok(ActorsLiteSourceKind::Fibonacci),
        Ok(LoweredProgram::Layers(_)) => return Ok(ActorsLiteSourceKind::Layers),
        Ok(LoweredProgram::Pages(_)) => return Ok(ActorsLiteSourceKind::Pages),
        Ok(LoweredProgram::Latest(_)) => return Ok(ActorsLiteSourceKind::Latest),
        Ok(LoweredProgram::TextInterpolationUpdate(_)) => {
            return Ok(ActorsLiteSourceKind::TextInterpolationUpdate);
        }
        Ok(LoweredProgram::ButtonHoverToClickTest(_)) => {
            return Ok(ActorsLiteSourceKind::ButtonHoverToClickTest);
        }
        Ok(LoweredProgram::ButtonHoverTest(_)) => {
            return Ok(ActorsLiteSourceKind::ButtonHoverTest);
        }
        Ok(LoweredProgram::FilterCheckboxBug(_)) => {
            return Ok(ActorsLiteSourceKind::FilterCheckboxBug);
        }
        Ok(LoweredProgram::CheckboxTest(_)) => {
            return Ok(ActorsLiteSourceKind::CheckboxTest);
        }
        Ok(LoweredProgram::TemperatureConverter(_)) => {
            return Ok(ActorsLiteSourceKind::TemperatureConverter);
        }
        Ok(LoweredProgram::FlightBooker(_)) => {
            return Ok(ActorsLiteSourceKind::FlightBooker);
        }
        Ok(LoweredProgram::Timer(_)) => return Ok(ActorsLiteSourceKind::Timer),
        Ok(LoweredProgram::ListMapExternalDep(_)) => {
            return Ok(ActorsLiteSourceKind::ListMapExternalDep);
        }
        Ok(LoweredProgram::ListMapBlock(_)) => return Ok(ActorsLiteSourceKind::ListMapBlock),
        Ok(LoweredProgram::ListRetainCount(_)) => {
            return Ok(ActorsLiteSourceKind::ListRetainCount);
        }
        Ok(LoweredProgram::ListObjectState(_)) => return Ok(ActorsLiteSourceKind::ListObjectState),
        Ok(LoweredProgram::ChainedListRemoveBug(_)) => {
            return Ok(ActorsLiteSourceKind::ChainedListRemoveBug);
        }
        Ok(LoweredProgram::Crud(_)) => return Ok(ActorsLiteSourceKind::Crud),
        Ok(LoweredProgram::ListRetainRemove(_)) => {
            return Ok(ActorsLiteSourceKind::ListRetainRemove);
        }
        Ok(LoweredProgram::ShoppingList(_)) => return Ok(ActorsLiteSourceKind::ShoppingList),
        Ok(LoweredProgram::ListRetainReactive(_)) => {
            return Ok(ActorsLiteSourceKind::ListRetainReactive);
        }
        Ok(LoweredProgram::Then(_)) => return Ok(ActorsLiteSourceKind::Then),
        Ok(LoweredProgram::When(_)) => return Ok(ActorsLiteSourceKind::When),
        Ok(LoweredProgram::While(_)) => return Ok(ActorsLiteSourceKind::While),
        Ok(LoweredProgram::WhileFunctionCall(_)) => {
            return Ok(ActorsLiteSourceKind::WhileFunctionCall);
        }
        Ok(LoweredProgram::SwitchHoldTest(_)) => {
            return Ok(ActorsLiteSourceKind::SwitchHoldTest);
        }
        Ok(LoweredProgram::CircleDrawer(_)) => return Ok(ActorsLiteSourceKind::CircleDrawer),
        Ok(LoweredProgram::Cells(_)) => return Ok(ActorsLiteSourceKind::Cells),
        Ok(LoweredProgram::StaticDocument(_)) => return Ok(ActorsLiteSourceKind::StaticDocument),
        Err(error) => errors.push(format!("generic: {error}")),
    }

    Err(errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn example_source(name: &str) -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../../playground/frontend/src/examples");
        path.push(name);
        path.push(format!("{name}.bn"));
        if !path.exists() {
            path.pop();
            path.push("RUN.bn");
        }
        std::fs::read_to_string(path).expect("supported playground example should exist")
    }

    #[test]
    fn classifies_supported_examples_explicitly() {
        let counter = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let todo = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let cells = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let cells_dynamic = include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        );
        let complex_counter = include_str!(
            "../../../playground/frontend/src/examples/complex_counter/complex_counter.bn"
        );
        let interval =
            include_str!("../../../playground/frontend/src/examples/interval/interval.bn");
        let interval_hold = include_str!(
            "../../../playground/frontend/src/examples/interval_hold/interval_hold.bn"
        );
        let fibonacci =
            include_str!("../../../playground/frontend/src/examples/fibonacci/fibonacci.bn");
        let layers = include_str!("../../../playground/frontend/src/examples/layers/layers.bn");
        let pages = include_str!("../../../playground/frontend/src/examples/pages/pages.bn");
        let latest = include_str!("../../../playground/frontend/src/examples/latest/latest.bn");
        let text_interpolation_update = include_str!(
            "../../../playground/frontend/src/examples/text_interpolation_update/text_interpolation_update.bn"
        );
        let then = include_str!("../../../playground/frontend/src/examples/then/then.bn");
        let when = include_str!("../../../playground/frontend/src/examples/when/when.bn");
        let while_source = include_str!("../../../playground/frontend/src/examples/while/while.bn");
        let while_function_call = include_str!(
            "../../../playground/frontend/src/examples/while_function_call/while_function_call.bn"
        );
        let button_hover_test = include_str!(
            "../../../playground/frontend/src/examples/button_hover_test/button_hover_test.bn"
        );
        let button_hover_to_click_test = include_str!(
            "../../../playground/frontend/src/examples/button_hover_to_click_test/button_hover_to_click_test.bn"
        );
        let switch_hold_test = include_str!(
            "../../../playground/frontend/src/examples/switch_hold_test/switch_hold_test.bn"
        );
        let list_retain_reactive = include_str!(
            "../../../playground/frontend/src/examples/list_retain_reactive/list_retain_reactive.bn"
        );
        let list_map_external_dep = include_str!(
            "../../../playground/frontend/src/examples/list_map_external_dep/list_map_external_dep.bn"
        );
        let list_map_block = include_str!(
            "../../../playground/frontend/src/examples/list_map_block/list_map_block.bn"
        );
        let list_retain_count = include_str!(
            "../../../playground/frontend/src/examples/list_retain_count/list_retain_count.bn"
        );
        let list_object_state = include_str!(
            "../../../playground/frontend/src/examples/list_object_state/list_object_state.bn"
        );
        let list_retain_remove = include_str!(
            "../../../playground/frontend/src/examples/list_retain_remove/list_retain_remove.bn"
        );
        let shopping_list = include_str!(
            "../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
        );
        let filter_checkbox_bug = include_str!(
            "../../../playground/frontend/src/examples/filter_checkbox_bug/filter_checkbox_bug.bn"
        );
        let checkbox_test = include_str!(
            "../../../playground/frontend/src/examples/checkbox_test/checkbox_test.bn"
        );
        let chained_list_remove_bug = include_str!(
            "../../../playground/frontend/src/examples/chained_list_remove_bug/chained_list_remove_bug.bn"
        );
        let circle_drawer = include_str!(
            "../../../playground/frontend/src/examples/circle_drawer/circle_drawer.bn"
        );
        let crud = include_str!("../../../playground/frontend/src/examples/crud/crud.bn");
        let temperature_converter = include_str!(
            "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        );
        let flight_booker = include_str!(
            "../../../playground/frontend/src/examples/flight_booker/flight_booker.bn"
        );
        let timer = include_str!("../../../playground/frontend/src/examples/timer/timer.bn");
        let hello_world =
            include_str!("../../../playground/frontend/src/examples/hello_world/hello_world.bn");

        assert_eq!(classify_source(counter), Ok(ActorsLiteSourceKind::Counter));
        assert_eq!(
            classify_source(complex_counter),
            Ok(ActorsLiteSourceKind::ComplexCounter)
        );
        assert_eq!(
            classify_source(interval),
            Ok(ActorsLiteSourceKind::Interval)
        );
        assert_eq!(
            classify_source(interval_hold),
            Ok(ActorsLiteSourceKind::IntervalHold)
        );
        assert_eq!(
            classify_source(fibonacci),
            Ok(ActorsLiteSourceKind::Fibonacci)
        );
        assert_eq!(classify_source(layers), Ok(ActorsLiteSourceKind::Layers));
        assert_eq!(classify_source(pages), Ok(ActorsLiteSourceKind::Pages));
        assert_eq!(classify_source(latest), Ok(ActorsLiteSourceKind::Latest));
        assert_eq!(
            classify_source(text_interpolation_update),
            Ok(ActorsLiteSourceKind::TextInterpolationUpdate)
        );
        assert_eq!(classify_source(then), Ok(ActorsLiteSourceKind::Then));
        assert_eq!(classify_source(when), Ok(ActorsLiteSourceKind::When));
        assert_eq!(
            classify_source(while_source),
            Ok(ActorsLiteSourceKind::While)
        );
        assert_eq!(
            classify_source(while_function_call),
            Ok(ActorsLiteSourceKind::WhileFunctionCall)
        );
        assert_eq!(
            classify_source(button_hover_test),
            Ok(ActorsLiteSourceKind::ButtonHoverTest)
        );
        assert_eq!(
            classify_source(button_hover_to_click_test),
            Ok(ActorsLiteSourceKind::ButtonHoverToClickTest)
        );
        assert_eq!(
            classify_source(switch_hold_test),
            Ok(ActorsLiteSourceKind::SwitchHoldTest)
        );
        assert_eq!(
            classify_source(list_retain_reactive),
            Ok(ActorsLiteSourceKind::ListRetainReactive)
        );
        assert_eq!(
            classify_source(list_map_external_dep),
            Ok(ActorsLiteSourceKind::ListMapExternalDep)
        );
        assert_eq!(
            classify_source(list_map_block),
            Ok(ActorsLiteSourceKind::ListMapBlock)
        );
        assert_eq!(
            classify_source(list_retain_count),
            Ok(ActorsLiteSourceKind::ListRetainCount)
        );
        assert_eq!(
            classify_source(list_object_state),
            Ok(ActorsLiteSourceKind::ListObjectState)
        );
        assert_eq!(
            classify_source(list_retain_remove),
            Ok(ActorsLiteSourceKind::ListRetainRemove)
        );
        assert_eq!(
            classify_source(shopping_list),
            Ok(ActorsLiteSourceKind::ShoppingList)
        );
        assert_eq!(
            classify_source(filter_checkbox_bug),
            Ok(ActorsLiteSourceKind::FilterCheckboxBug)
        );
        assert_eq!(
            classify_source(checkbox_test),
            Ok(ActorsLiteSourceKind::CheckboxTest)
        );
        assert_eq!(
            classify_source(chained_list_remove_bug),
            Ok(ActorsLiteSourceKind::ChainedListRemoveBug)
        );
        assert_eq!(
            classify_source(circle_drawer),
            Ok(ActorsLiteSourceKind::CircleDrawer)
        );
        assert_eq!(classify_source(crud), Ok(ActorsLiteSourceKind::Crud));
        assert_eq!(
            classify_source(temperature_converter),
            Ok(ActorsLiteSourceKind::TemperatureConverter)
        );
        assert_eq!(
            classify_source(flight_booker),
            Ok(ActorsLiteSourceKind::FlightBooker)
        );
        assert_eq!(classify_source(timer), Ok(ActorsLiteSourceKind::Timer));
        assert_eq!(classify_source(todo), Ok(ActorsLiteSourceKind::TodoMvc));
        assert_eq!(classify_source(cells), Ok(ActorsLiteSourceKind::Cells));
        assert_eq!(
            classify_source(cells_dynamic),
            Ok(ActorsLiteSourceKind::Cells)
        );
        assert_eq!(
            classify_source(hello_world),
            Ok(ActorsLiteSourceKind::StaticDocument)
        );
    }

    #[test]
    fn supported_playground_example_list_matches_classifier() {
        for example_name in SUPPORTED_PLAYGROUND_EXAMPLES {
            let source = example_source(example_name);
            assert!(
                classify_source(&source).is_ok(),
                "supported playground example '{example_name}' must classify successfully"
            );
        }
    }

    #[test]
    fn unsupported_source_returns_classifier_errors() {
        let unsupported = "FUNCTION unsupported() { True }";
        let errors = classify_source(unsupported).expect_err("unsupported source should fail");
        assert!(errors.iter().any(|error| error.starts_with("generic:")));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("single_action_accumulator_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("editable_filterable_list_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("persistent_indexed_text_grid_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("dual_action_accumulator_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("retained_toggle_filter_list_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("external_mode_mapped_items_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("dual_mapped_label_stripes_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("counted_filtered_append_list_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("independent_object_counters_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("removable_append_list_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("clearable_append_list_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("filterable_checkbox_list_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("independent_checkbox_list_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("removable_checkbox_list_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("canvas_history_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("selectable_record_column_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("bidirectional_conversion_form_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("selectable_dual_date_form_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("resettable_timed_progress_document:"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("static_document_display:"))
        );
    }

    #[test]
    fn public_playground_examples_match_proven_phase_five_subset() {
        assert!(actors_lite_public_exposure_enabled());
        for example_name in MILESTONE_PLAYGROUND_EXAMPLES {
            assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(example_name));
        }
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"minimal"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"hello_world"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"counter_hold"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"text_interpolation_update"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"button_hover_test"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"button_hover_to_click_test"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"switch_hold_test"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"filter_checkbox_bug"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"checkbox_test"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"chained_list_remove_bug"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"complex_counter"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"fibonacci"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"interval"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"interval_hold"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"then"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"when"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"while"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"while_function_call"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"list_retain_reactive"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"list_map_external_dep"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"list_map_block"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"list_retain_count"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"list_object_state"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"list_retain_remove"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"temperature_converter"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"flight_booker"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"timer"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"crud"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"circle_drawer"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"latest"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"layers"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"pages"));
        assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(&"shopping_list"));
        assert_eq!(
            PUBLIC_PLAYGROUND_EXAMPLES.len(),
            SUPPORTED_PLAYGROUND_EXAMPLES.len()
        );
        for example_name in SUPPORTED_PLAYGROUND_EXAMPLES {
            assert!(PUBLIC_PLAYGROUND_EXAMPLES.contains(example_name));
        }
        for example_name in PUBLIC_PLAYGROUND_EXAMPLES {
            assert!(SUPPORTED_PLAYGROUND_EXAMPLES.contains(example_name));
        }
    }

    #[test]
    fn public_playground_examples_require_phase4_acceptance_record() {
        assert!(actors_lite_public_exposure_enabled());
        assert!(is_public_playground_example("counter"));
        assert!(!is_public_playground_example("todo_mvc_physical"));
        assert!(!is_public_playground_example("not_a_real_example"));
    }
}
